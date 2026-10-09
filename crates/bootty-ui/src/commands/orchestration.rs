//! Finite Runs commands. Wire requests select task intent, never host paths, argv or authority.

use std::collections::BTreeMap;

use bootty_agents::{
    AgentKind, AgentLaunch, AgentPrompt, MAX_RUN_NODES, OrchestrationLaunch, OrchestrationNodeSpec,
    OrchestrationPlan,
};
use bootty_config::config::{AgentProfileConfig, AgentProvidersConfig};
use bootty_control::{
    CommandDescriptor, CommandInvocation, CommandOutcome, CompactSchema, ResourceKind, ValueType,
};
use serde::{Deserialize, Serialize};

command_actions! {
    RunAction {
        Create => ("runs.create", "Create Run", ["nodes"], Write),
        CreateForSession => ("runs.create_for_session", "Create Session Tasks", ["nodes"], Write),
        List => ("runs.list", "List Runs", [], Read),
        Read => ("runs.read", "Read Run", ["run"], Read),
        Dispatch => ("runs.dispatch", "Start Ready Tasks", ["run"], Write),
        Cancel => ("runs.cancel", "Cancel Run", ["run"], Write),
        Retry => ("runs.retry", "Retry Run Task", ["run", "node"], Write),
        Restart => ("runs.restart", "Restart Unfinished Tasks", ["run"], Write),
    }
}

impl RunAction {
    #[must_use]
    pub fn descriptor(self) -> CommandDescriptor {
        let (id, title, names, mutation) = self.metadata();
        CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description: "Run bounded dependent agent tasks on their captured Binding; recovery never replays a prompt.".to_owned(),
            mutation,
            arguments: CompactSchema { arguments: names.iter().map(|name| super::argument(name, ValueType::String)).collect() },
            target: Some(if self == Self::CreateForSession { ResourceKind::Session } else if matches!(self, Self::List | Self::Read) { ResourceKind::ApplicationWindow } else { ResourceKind::Binding }),
            palette: false,
        }
    }
}

pub(super) fn register_commands(commands: &mut BTreeMap<String, super::RegisteredCommand>) {
    for action in RunAction::ALL {
        let descriptor = action.descriptor();
        commands.insert(
            descriptor.id.clone(),
            super::RegisteredCommand {
                descriptor,
                executor: super::CommandExecutorResolver::Orchestration,
            },
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunNodeRequest {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub provider: AgentKind,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Existing saved task for a new native conversation; absent keeps terminal launches.
    #[serde(default)]
    pub task_identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunCommand {
    Create { nodes: Vec<RunNodeRequest> },
    CreateForSession { nodes: Vec<RunNodeRequest> },
    List,
    Read { run: String },
    Dispatch { run: String },
    Cancel { run: String },
    Retry { run: String, node: String },
    Restart { run: String },
}

impl RunCommand {
    /// # Errors
    /// Rejects unknown commands, malformed/oversized requests and wire authority fields.
    pub fn parse(invocation: &CommandInvocation) -> Result<Self, CommandOutcome> {
        let invalid = || CommandOutcome::Failed {
            code: "invalid_arguments".to_owned(),
            message: "Choose a bounded typed Runs command".to_owned(),
        };
        let args = &invocation.arguments;
        let one = || {
            args.first()
                .filter(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
                .cloned()
                .ok_or_else(invalid)
        };
        Ok(match invocation.command.as_str() {
            "runs.create" | "runs.create_for_session" if args.len() == 1 => {
                let encoded = args.first().ok_or_else(invalid)?;
                // Below the shared 1 MiB envelope; raise only with a bounded transport upgrade.
                if encoded.len() > 384 * 1024 {
                    return Err(invalid());
                }
                let nodes: Vec<RunNodeRequest> =
                    serde_json::from_str(encoded).map_err(|_| invalid())?;
                if nodes.is_empty() || nodes.len() > MAX_RUN_NODES {
                    return Err(invalid());
                }
                for node in &nodes {
                    AgentPrompt::new(node.prompt.clone()).map_err(|_| invalid())?;
                }
                if invocation.command == "runs.create_for_session" {
                    Self::CreateForSession { nodes }
                } else {
                    Self::Create { nodes }
                }
            }
            "runs.list" if args.is_empty() => Self::List,
            "runs.read" if args.len() == 1 => Self::Read { run: one()? },
            "runs.dispatch" if args.len() == 1 => Self::Dispatch { run: one()? },
            "runs.cancel" if args.len() == 1 => Self::Cancel { run: one()? },
            "runs.restart" if args.len() == 1 => Self::Restart { run: one()? },
            "runs.retry" if args.len() == 2 => Self::Retry {
                run: one()?,
                node: args
                    .get(1)
                    .filter(|id| {
                        !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
                    })
                    .cloned()
                    .ok_or_else(invalid)?,
            },
            _ => return Err(invalid()),
        })
    }
}

/// Resolve only host configuration captured before creation. The account resolver is injected at I/O.
/// # Errors
/// Rejects disabled/missing profiles, invalid captured host values or an invalid dependency plan.
pub fn capture_run_plan(
    requests: Vec<RunNodeRequest>,
    cwd: &str,
    task_directories: &BTreeMap<String, String>,
    providers: &AgentProvidersConfig,
    mut account: impl FnMut(AgentKind, Option<&AgentProfileConfig>) -> Result<String, String>,
) -> Result<OrchestrationPlan, String> {
    if !std::path::Path::new(cwd).is_absolute() {
        return Err("Runs require the Binding's captured absolute project directory".to_owned());
    }
    let mut nodes = Vec::new();
    for request in requests {
        let directory = match request.task_identity.as_deref() {
            Some(identity) => task_directories
                .get(identity)
                .map(String::as_str)
                .ok_or("The exact saved task is not attached")?,
            None => cwd,
        };
        if !std::path::Path::new(directory).is_absolute() {
            return Err("The captured saved task has no absolute directory".to_owned());
        }
        let preferences = providers
            .provider(&request.provider.to_string())
            .ok_or("Unsupported run provider")?;
        if !preferences.enabled {
            return Err("Run provider is disabled in Settings".to_owned());
        }
        let id = request.profile.as_deref().unwrap_or(&preferences.selected);
        let profile = if id.is_empty() {
            None
        } else {
            Some(
                preferences
                    .profiles
                    .get(id)
                    .ok_or("The captured run profile is unavailable")?,
            )
        };
        let arguments = profile.map_or_else(Vec::new, |profile| profile.arguments.clone());
        if arguments.iter().any(|argument| {
            matches!(
                argument.split('=').next(),
                Some(
                    "--resume"
                        | "resume"
                        | "fork"
                        | "--continue"
                        | "-c"
                        | "--session-id"
                        | "--session"
                        | "--fork-session"
                        | "--fork"
                )
            )
        }) {
            // Reuse requires provider turn identity, rather than a previous session's Done status.
            return Err(
                "Run profiles with preselected sessions require supported turn identity".to_owned(),
            );
        }
        let launch = AgentLaunch {
            program: if preferences.program.is_empty() {
                request.provider.default_program().to_owned()
            } else {
                preferences.program.clone()
            },
            cwd: Some(directory.to_owned()),
            arguments,
            ephemeral: false,
            account_directory: Some(account(request.provider, profile)?),
        };
        let profile = (!id.is_empty()).then(|| id.to_owned());
        let launch = match request.task_identity {
            Some(task) => {
                OrchestrationLaunch::capture_for_task(request.provider, profile, launch, task)?
            }
            None => OrchestrationLaunch::capture(request.provider, profile, launch)?,
        };
        nodes.push(OrchestrationNodeSpec {
            id: request.id,
            title: request.title,
            dependencies: request.dependencies,
            prompt: AgentPrompt::new(request.prompt)?,
            launch,
        });
    }
    OrchestrationPlan::new(nodes)
}
