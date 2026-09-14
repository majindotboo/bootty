use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

use crate::{
    action_catalog::Command,
    app_actions::{KeybindAction, SidebarAction, keybind_action_for_name},
    error_catalog::ErrorNotice,
    gpui::CommandAction,
};
use bootty_agents::{AgentKind, AgentService, command_descriptors as agent_command_descriptors};
use bootty_control::{
    ArgumentSchema, Caller, CommandCatalogSource, CommandDescriptor, CommandInvocation,
    CommandOutcome, CompactSchema, ControlCatalog, MutationClass, ResourceKind, ValueType,
};

// One declaration owns each action, its catalog order, and its wire metadata.
macro_rules! command_actions {
    ($name:ident { $($variant:ident => ($id:literal, $title:literal, [$($argument:literal),*], $mutation:ident)),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: [Self; [$(stringify!($variant)),+].len()] = [$(Self::$variant),+];

            const fn metadata(self) -> (&'static str, &'static str, &'static [&'static str], bootty_control::MutationClass) {
                match self {
                    $(Self::$variant => ($id, $title, &[$($argument),*], bootty_control::MutationClass::$mutation)),+
                }
            }
        }
    };
}

mod dock;
mod files;
mod git;
mod jobs;
mod panes;
pub(crate) mod runtime;
mod themes;

pub use dock::{DockAction, DockRequest, PANELS, PanelCreation, PanelDescriptor, panel_descriptor};
pub use files::FileAction;
pub use git::GitAction;
pub use jobs::JobAction;
pub use panes::PaneAction;
pub use themes::ThemeAction;

pub(crate) use runtime::{CommandRuntime, command_outcome_message};

pub(crate) use bootty_mux::target::ExactMuxTarget;

#[must_use]
pub fn command_invocation_from_catalog(
    command: Command,
    caller: Caller,
) -> Option<CommandInvocation> {
    command
        .palette_action()
        .map(|action| CommandInvocation::from_action(action, caller))
}

#[derive(Clone, Debug, PartialEq)]
pub enum CoreCommandExecutor {
    Synchronous(SynchronousCommand),
    Dock(DockAction, Option<u64>),
    Keybind(KeybindAction),
    OpenLink(Vec<String>),
    AgentWorkspace(AgentWorkspaceAction),
    Forward(&'static str, Vec<String>),
    Job(JobAction, Vec<String>),
    Recovery(&'static str, Vec<String>),
    WslList,
    CaptureTerminal(Vec<String>, bool),
    ShellPrompt(&'static str, Vec<String>),
    Git(GitAction, Vec<String>),
    Theme(ThemeAction, Vec<String>),
    File(FileAction, Vec<String>),
    Pane(PaneAction, Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentWorkspaceAction {
    List,
    Focus,
    Next,
}

/// Commands that finish on the app thread after their cancellation token is started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SynchronousCommand {
    ReloadConfig,
    Sidebar(SidebarAction),
    Command(crate::gpui::CommandAction),
    CurrentResource(ResourceKind),
    PasteTerminal(String),
    Doctor,
    ShellIntegration(String),
    WslSpace(Vec<String>),
    ReadTerminal,
    SubmitTerminal,
}

#[derive(Clone, Debug)]
struct RegisteredCommand {
    descriptor: CommandDescriptor,
    executor: CommandExecutorResolver,
}

#[derive(Clone, Copy, Debug)]
enum CommandExecutorResolver {
    Dock(DockAction),
    Keybind,
    Sidebar(SidebarAction),
    Command(crate::gpui::CommandAction),
    CurrentResource,
    PasteTerminal,
    OpenLink,
    AgentWorkspace(AgentWorkspaceAction),
    Doctor,
    Forward(&'static str),
    Job(JobAction),
    Recovery(&'static str),
    ShellIntegration,
    WslList,
    WslSpace,
    CaptureTerminal(bool),
    ShellPrompt(&'static str),
    ReadTerminal,
    SubmitTerminal,
    WriteTerminal,
    Git(GitAction),
    Theme(ThemeAction),
    File(FileAction),
    Pane(PaneAction),
}

#[derive(Clone, Debug, Default)]
pub struct CommandRegistry {
    commands: BTreeMap<String, RegisteredCommand>,
}

impl CommandRegistry {
    pub fn core() -> &'static Self {
        static REGISTRY: OnceLock<CommandRegistry> = OnceLock::new();
        REGISTRY.get_or_init(Self::from_core_commands)
    }

    pub fn list(&self) -> impl Iterator<Item = &CommandDescriptor> {
        self.commands.values().map(|command| &command.descriptor)
    }

    #[must_use]
    pub fn describe(&self, id: &str) -> Option<&CommandDescriptor> {
        self.commands.get(id).map(|command| &command.descriptor)
    }

    pub fn palette_commands(&self) -> impl Iterator<Item = Command> + '_ {
        Command::all().filter(|command| {
            command.palette_action().is_some()
                && self
                    .describe(command.id())
                    .is_some_and(|descriptor| descriptor.palette)
        })
    }

    /// Resolve a catalog command and validate its arguments.
    ///
    /// # Errors
    /// Returns an unknown-command, unsupported-command, or invalid-arguments outcome.
    pub fn resolve(
        &self,
        invocation: CommandInvocation,
    ) -> Result<ResolvedCommandInvocation, CommandOutcome> {
        let Some(registered) = self.commands.get(&invocation.command) else {
            return Err(CommandOutcome::Failed {
                code: "unknown_command".to_owned(),
                message: ErrorNotice::UnknownCommand(format!(
                    "unknown command {}",
                    invocation.command
                ))
                .raw_message(),
            });
        };
        let descriptor = registered.descriptor.clone();
        validate_arguments(&descriptor, &invocation.arguments)?;
        let invalid_arguments = || CommandOutcome::Failed {
            code: "invalid_arguments".to_owned(),
            message: format!("Invalid arguments for {}", invocation.command),
        };
        let first_argument = || invocation.arguments.first().ok_or_else(invalid_arguments);
        let executor = match registered.executor {
            CommandExecutorResolver::Dock(action) => CoreCommandExecutor::Dock(
                action,
                invocation
                    .arguments
                    .first()
                    .map(|id| id.parse())
                    .transpose()
                    .map_err(|_| invalid_arguments())?,
            ),
            CommandExecutorResolver::Keybind => resolve_keybind_command(&invocation)?,
            CommandExecutorResolver::Sidebar(action) => {
                CoreCommandExecutor::Synchronous(SynchronousCommand::Sidebar(action))
            }
            CommandExecutorResolver::Command(action) => {
                CoreCommandExecutor::Synchronous(SynchronousCommand::Command(action))
            }
            CommandExecutorResolver::CurrentResource => {
                CoreCommandExecutor::Synchronous(SynchronousCommand::CurrentResource(
                    resource_kind(first_argument()?).ok_or_else(invalid_arguments)?,
                ))
            }
            CommandExecutorResolver::PasteTerminal => CoreCommandExecutor::Synchronous(
                SynchronousCommand::PasteTerminal(first_argument()?.clone()),
            ),
            CommandExecutorResolver::Pane(action) => {
                CoreCommandExecutor::Pane(action, invocation.arguments.clone())
            }
            CommandExecutorResolver::File(action) => {
                CoreCommandExecutor::File(action, invocation.arguments.clone())
            }
            CommandExecutorResolver::Theme(action) => {
                CoreCommandExecutor::Theme(action, invocation.arguments.clone())
            }
            CommandExecutorResolver::Git(action) => {
                CoreCommandExecutor::Git(action, invocation.arguments.clone())
            }
            CommandExecutorResolver::ShellPrompt(action) => {
                CoreCommandExecutor::ShellPrompt(action, invocation.arguments.clone())
            }
            CommandExecutorResolver::Forward(action) => {
                CoreCommandExecutor::Forward(action, invocation.arguments.clone())
            }
            CommandExecutorResolver::Job(action) => {
                CoreCommandExecutor::Job(action, invocation.arguments.clone())
            }
            CommandExecutorResolver::Recovery(action) => {
                CoreCommandExecutor::Recovery(action, invocation.arguments.clone())
            }
            CommandExecutorResolver::Doctor => {
                CoreCommandExecutor::Synchronous(SynchronousCommand::Doctor)
            }
            CommandExecutorResolver::AgentWorkspace(action) => {
                CoreCommandExecutor::AgentWorkspace(action)
            }
            CommandExecutorResolver::OpenLink => {
                CoreCommandExecutor::OpenLink(invocation.arguments.clone())
            }
            CommandExecutorResolver::ShellIntegration => CoreCommandExecutor::Synchronous(
                SynchronousCommand::ShellIntegration(first_argument()?.clone()),
            ),
            CommandExecutorResolver::WslList => CoreCommandExecutor::WslList,
            CommandExecutorResolver::WslSpace => CoreCommandExecutor::Synchronous(
                SynchronousCommand::WslSpace(invocation.arguments.clone()),
            ),
            CommandExecutorResolver::CaptureTerminal(export) => {
                CoreCommandExecutor::CaptureTerminal(invocation.arguments.clone(), export)
            }
            CommandExecutorResolver::ReadTerminal => {
                CoreCommandExecutor::Synchronous(SynchronousCommand::ReadTerminal)
            }
            CommandExecutorResolver::SubmitTerminal => {
                CoreCommandExecutor::Synchronous(SynchronousCommand::SubmitTerminal)
            }
            CommandExecutorResolver::WriteTerminal => CoreCommandExecutor::Keybind(
                KeybindAction::Write(first_argument()?.as_bytes().to_vec()),
            ),
        };
        Ok(ResolvedCommandInvocation {
            descriptor,
            executor: CommandExecutor::Core(executor),
            invocation,
        })
    }

    fn from_core_commands() -> Self {
        let mut commands = BTreeMap::new();
        for command in Command::all() {
            let descriptor = command.descriptor();
            commands
                .entry(descriptor.id.clone())
                .and_modify(|existing: &mut RegisteredCommand| {
                    existing.descriptor.palette |= descriptor.palette;
                })
                .or_insert_with(|| RegisteredCommand {
                    descriptor,
                    executor: DockAction::from_command(command).map_or(
                        CommandExecutorResolver::Keybind,
                        CommandExecutorResolver::Dock,
                    ),
                });
        }
        register_navigation_commands(&mut commands);
        register_resource_commands(&mut commands);
        register_capture_commands(&mut commands);
        register_agents_commands(&mut commands);
        register_shell_prompt_commands(&mut commands);
        register_forward_commands(&mut commands);
        register_feature_commands(&mut commands);
        register_host_commands(&mut commands);
        Self { commands }
    }
}

fn register_navigation_commands(commands: &mut BTreeMap<String, RegisteredCommand>) {
    for action in SidebarAction::ALL {
        let descriptor = sidebar_descriptor(action);
        commands.insert(
            descriptor.id.clone(),
            RegisteredCommand {
                descriptor,
                executor: CommandExecutorResolver::Sidebar(action),
            },
        );
    }
    for (action, name, title, description) in [
        (
            CommandAction::Previous,
            "previous",
            "Previous Command Item",
            "Select the previous item in the active palette or picker.",
        ),
        (
            CommandAction::Next,
            "next",
            "Next Command Item",
            "Select the next item in the active palette or picker.",
        ),
        (
            CommandAction::Confirm,
            "confirm",
            "Confirm Command Item",
            "Activate the selected item in the active palette or picker.",
        ),
        (
            CommandAction::Cancel,
            "cancel",
            "Close Command Picker",
            "Close the active palette or picker.",
        ),
        (
            CommandAction::ToggleFavorite,
            "toggle_favorite",
            "Toggle Directory Favorite",
            "Toggle the selected directory's favorite state.",
        ),
    ] {
        let id = format!("ui.command.{name}");
        commands.insert(
            id.clone(),
            RegisteredCommand {
                descriptor: CommandDescriptor {
                    id,
                    title: title.to_owned(),
                    description: description.to_owned(),
                    mutation: MutationClass::Write,
                    arguments: CompactSchema::default(),
                    target: Some(ResourceKind::ApplicationWindow),
                    palette: false,
                },
                executor: CommandExecutorResolver::Command(action),
            },
        );
    }
}

fn register_resource_commands(commands: &mut BTreeMap<String, RegisteredCommand>) {
    let resource_kind_choices = [
        "instance",
        "application_window",
        "binding",
        "session",
        "mux_window",
        "pane",
        "terminal",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (descriptor, executor) = (
        CommandDescriptor {
            id: "resource.current".to_owned(),
            title: "Current Resource".to_owned(),
            description: "Return the current opaque resource target.".to_owned(),
            mutation: MutationClass::Read,
            arguments: CompactSchema {
                arguments: vec![ArgumentSchema {
                    name: "kind".to_owned(),
                    value_type: ValueType::String,
                    required: true,
                    choices: resource_kind_choices,
                    minimum: None,
                    maximum: None,
                }],
            },
            target: None,
            palette: false,
        },
        CommandExecutorResolver::CurrentResource,
    );
    commands.insert(
        descriptor.id.clone(),
        RegisteredCommand {
            descriptor,
            executor,
        },
    );
    for (id, title, description, mutation, arguments, executor) in [
        (
            "terminal.read",
            "Read Terminal",
            "Read the active terminal screen.",
            MutationClass::Read,
            vec![],
            CommandExecutorResolver::ReadTerminal,
        ),
        (
            "terminal.write",
            "Write Terminal",
            "Write literal text to the active terminal.",
            MutationClass::Write,
            vec![argument("text", ValueType::String)],
            CommandExecutorResolver::WriteTerminal,
        ),
        (
            "terminal.paste",
            "Paste Terminal Text",
            "Paste text into the active terminal.",
            MutationClass::Write,
            vec![argument("text", ValueType::String)],
            CommandExecutorResolver::PasteTerminal,
        ),
        (
            "terminal.submit",
            "Submit Terminal Input",
            "Send the terminal's encoded Enter key.",
            MutationClass::Write,
            vec![],
            CommandExecutorResolver::SubmitTerminal,
        ),
    ] {
        let descriptor = CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            mutation,
            arguments: CompactSchema { arguments },
            target: Some(ResourceKind::Terminal),
            palette: false,
        };
        commands.insert(
            descriptor.id.clone(),
            RegisteredCommand {
                descriptor,
                executor,
            },
        );
    }
}

fn register_capture_commands(commands: &mut BTreeMap<String, RegisteredCommand>) {
    for (id, title, export) in [
        ("terminal.capture", "Capture Terminal", false),
        ("terminal.export", "Export Terminal", true),
    ] {
        let mut arguments = Vec::new();
        if export {
            arguments.push(argument("destination", ValueType::String));
        }
        for (name, choices) in [
            ("format", vec!["plain", "ansi", "html"]),
            ("scope", vec!["screen", "history"]),
            ("max_lines", vec![]),
        ] {
            let mut arg = argument(
                name,
                if name == "max_lines" {
                    ValueType::Integer
                } else {
                    ValueType::String
                },
            );
            arg.required = false;
            arg.choices = choices.into_iter().map(str::to_owned).collect();
            if name == "max_lines" {
                arg.minimum = Some(1);
                arg.maximum = Some(100_000);
            }
            arguments.push(arg);
        }
        commands.insert(id.to_owned(), RegisteredCommand {
                descriptor: CommandDescriptor { id: id.to_owned(), title: title.to_owned(), description: "Capture retained rendered state from the target terminal. History includes retained rows, not original process bytes. Export creates a new local file.".to_owned(), mutation: if export { MutationClass::Write } else { MutationClass::Read }, arguments: CompactSchema { arguments }, target: Some(ResourceKind::Terminal), palette: false },
                executor: CommandExecutorResolver::CaptureTerminal(export),
            });
    }
    commands.insert(
        "doctor".to_owned(),
        RegisteredCommand {
            descriptor: CommandDescriptor {
                id: "doctor".to_owned(),
                title: "Workspace Diagnostics".to_owned(),
                description:
                    "Read identity, host bindings, backend capabilities and reported failures."
                        .to_owned(),
                mutation: MutationClass::Read,
                arguments: CompactSchema::default(),
                target: Some(ResourceKind::ApplicationWindow),
                palette: false,
            },
            executor: CommandExecutorResolver::Doctor,
        },
    );
}

fn register_agents_commands(commands: &mut BTreeMap<String, RegisteredCommand>) {
    for (action, id, title, target) in [
        (
            AgentWorkspaceAction::List,
            "agents.list",
            "List Agents",
            ResourceKind::ApplicationWindow,
        ),
        (
            AgentWorkspaceAction::Focus,
            "agents.focus",
            "Focus Agent",
            ResourceKind::Terminal,
        ),
        (
            AgentWorkspaceAction::Next,
            "agents.next",
            "Next Unread Agent",
            ResourceKind::ApplicationWindow,
        ),
    ] {
        commands.insert(
            id.to_owned(),
            RegisteredCommand {
                descriptor: CommandDescriptor {
                    id: id.to_owned(),
                    title: title.to_owned(),
                    description: title.to_owned(),
                    arguments: CompactSchema::default(),
                    mutation: if action == AgentWorkspaceAction::List {
                        MutationClass::Read
                    } else {
                        MutationClass::Write
                    },
                    target: Some(target),
                    palette: false,
                },
                executor: CommandExecutorResolver::AgentWorkspace(action),
            },
        );
    }
}

fn register_shell_prompt_commands(commands: &mut BTreeMap<String, RegisteredCommand>) {
    for (id, names, mutation) in [
        ("shell.prompt", vec![], MutationClass::Read),
        ("history.search", vec!["spec"], MutationClass::Read),
        ("shell.history", vec!["query"], MutationClass::Read),
        (
            "shell.apply",
            vec!["revision", "text", "submit"],
            MutationClass::Write,
        ),
    ] {
        commands.insert(id.to_owned(), RegisteredCommand {
                descriptor: CommandDescriptor {
                    id: id.to_owned(), title: id.to_owned(),
                    description: "Inspect a supported shell prompt, search its history, or atomically hand an edited command to an untouched prompt.".to_owned(),
                    arguments: CompactSchema { arguments: names.into_iter().map(|name| argument(name, ValueType::String)).collect() },
                    mutation, target: Some(if id == "history.search" { ResourceKind::Binding } else { ResourceKind::Terminal }), palette: false,
                },
                executor: CommandExecutorResolver::ShellPrompt(id),
            });
    }
}

fn register_forward_commands(commands: &mut BTreeMap<String, RegisteredCommand>) {
    for (id, names, mutation, target) in [
        (
            "forwards.list",
            vec![],
            MutationClass::Read,
            ResourceKind::ApplicationWindow,
        ),
        (
            "forwards.open",
            vec!["url"],
            MutationClass::Write,
            ResourceKind::Binding,
        ),
        (
            "forwards.retry",
            vec!["id"],
            MutationClass::Write,
            ResourceKind::ApplicationWindow,
        ),
        (
            "forwards.close",
            vec!["id"],
            MutationClass::Write,
            ResourceKind::ApplicationWindow,
        ),
        (
            "forwards.check",
            vec!["id"],
            MutationClass::Read,
            ResourceKind::ApplicationWindow,
        ),
    ] {
        commands.insert(
            id.to_owned(),
            RegisteredCommand {
                descriptor: CommandDescriptor {
                    id: id.to_owned(),
                    title: id.to_owned(),
                    description: "Manage captured-host loopback forwarding leases.".to_owned(),
                    arguments: CompactSchema {
                        arguments: names
                            .into_iter()
                            .map(|name| argument(name, ValueType::String))
                            .collect(),
                    },
                    mutation,
                    target: Some(target),
                    palette: false,
                },
                executor: CommandExecutorResolver::Forward(id),
            },
        );
    }
}

fn register_feature_commands(commands: &mut BTreeMap<String, RegisteredCommand>) {
    for (id, names, mutation) in [
        ("recovery.list", vec![], MutationClass::Read),
        ("recovery.get", vec!["id"], MutationClass::Read),
        ("recovery.export", vec!["id", "path"], MutationClass::Write),
        ("recovery.delete", vec!["id"], MutationClass::Destructive),
        ("recovery.resume", vec!["id"], MutationClass::Write),
        ("recovery.fork", vec!["id"], MutationClass::Write),
    ] {
        commands.insert(id.into(), RegisteredCommand { descriptor: CommandDescriptor {
                id:id.into(), title:id.into(), description:"Browse bounded previous-session output or explicitly relaunch a recorded agent session.".into(),
                arguments:CompactSchema { arguments:names.into_iter().map(|name|argument(name,ValueType::String)).collect() }, mutation,
                target:Some(ResourceKind::ApplicationWindow), palette:false,
            }, executor:CommandExecutorResolver::Recovery(id) });
    }
    for action in JobAction::ALL {
        let descriptor = action.descriptor();
        commands.insert(
            descriptor.id.clone(),
            RegisteredCommand {
                descriptor,
                executor: CommandExecutorResolver::Job(action),
            },
        );
    }
    for action in ThemeAction::ALL {
        let descriptor = action.descriptor();
        commands.insert(
            descriptor.id.clone(),
            RegisteredCommand {
                descriptor,
                executor: CommandExecutorResolver::Theme(action),
            },
        );
    }
    for action in GitAction::ALL {
        let descriptor = action.descriptor();
        commands.insert(
            descriptor.id.clone(),
            RegisteredCommand {
                descriptor,
                executor: CommandExecutorResolver::Git(action),
            },
        );
    }
    for action in FileAction::ALL {
        let descriptor = action.descriptor();
        commands.insert(
            descriptor.id.clone(),
            RegisteredCommand {
                descriptor,
                executor: CommandExecutorResolver::File(action),
            },
        );
    }
    for action in PaneAction::ALL {
        let descriptor = action.descriptor();
        commands.insert(
            descriptor.id.clone(),
            RegisteredCommand {
                descriptor,
                executor: CommandExecutorResolver::Pane(action),
            },
        );
    }
}

fn register_host_commands(commands: &mut BTreeMap<String, RegisteredCommand>) {
    let mut cwd = argument("cwd", ValueType::String);
    cwd.required = false;
    let descriptor = CommandDescriptor {
            id: "link.open".to_owned(), title: "Open Terminal Link".to_owned(),
            description: "Open a URL or file location on the target terminal's host; remote loopback URLs establish a scoped SSH forward first.".to_owned(),
            arguments: CompactSchema { arguments: vec![argument("location", ValueType::String), cwd] },
            mutation: MutationClass::Write, target: Some(ResourceKind::Terminal), palette: false,
        };
    commands.insert(
        descriptor.id.clone(),
        RegisteredCommand {
            descriptor,
            executor: CommandExecutorResolver::OpenLink,
        },
    );
    let descriptor = CommandDescriptor {
        id: "shell.integration".to_owned(),
        title: "Read Shell Integration".to_owned(),
        description: "Read the optional bash, zsh or fish hooks for shells managed outside Bootty."
            .to_owned(),
        mutation: MutationClass::Read,
        target: None,
        palette: false,
        arguments: CompactSchema {
            arguments: vec![argument("shell", ValueType::String)],
        },
    };
    commands.insert(
        descriptor.id.clone(),
        RegisteredCommand {
            descriptor,
            executor: CommandExecutorResolver::ShellIntegration,
        },
    );
    let mut name = argument("name", ValueType::String);
    name.required = false;
    let mut backend = argument("backend", ValueType::String);
    backend.required = false;
    for (id, title, description, mutation, arguments, executor) in [
        (
            "wsl.list",
            "List WSL Distributions",
            "List installed Windows Subsystem for Linux distributions.",
            MutationClass::Read,
            vec![],
            CommandExecutorResolver::WslList,
        ),
        (
            "space.wsl",
            "Create WSL Space",
            "Create and activate a Space in a WSL distribution. Backend defaults to rmux; tmux is also supported. Connection occurs through the binding runtime.",
            MutationClass::Write,
            vec![argument("distribution", ValueType::String), name, backend],
            CommandExecutorResolver::WslSpace,
        ),
    ] {
        let descriptor = CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            arguments: CompactSchema { arguments },
            mutation,
            target: None,
            palette: false,
        };
        commands.insert(
            id.to_owned(),
            RegisteredCommand {
                descriptor,
                executor,
            },
        );
    }
}

#[derive(Clone)]
pub enum CommandExecutor {
    Core(CoreCommandExecutor),
    Agent(Arc<AgentService>),
    /// The static agent catalog remains discoverable in tests and uncomposed app states.
    /// Invocation is rejected explicitly until the host supplies its event transport.
    UncomposedAgent,
}

#[derive(Clone)]
pub struct ResolvedCommandInvocation {
    pub descriptor: CommandDescriptor,
    pub invocation: CommandInvocation,
    pub executor: CommandExecutor,
}

#[derive(Clone)]
pub struct CommandCatalog {
    core: &'static CommandRegistry,
    agents: Option<Arc<AgentService>>,
    control: Arc<ControlCatalog>,
}

struct NativeCatalogSource {
    agents: Option<Arc<AgentService>>,
    jobs: std::sync::Weak<bootty_host::jobs::JobRegistry>,
}

impl CommandCatalogSource for NativeCatalogSource {
    fn list(&self) -> Vec<CommandDescriptor> {
        agent_command_descriptors()
    }

    fn describe(&self, id: &str) -> Option<CommandDescriptor> {
        agent_command_descriptors()
            .into_iter()
            .find(|command| command.id == id)
    }

    fn topics(&self) -> std::collections::BTreeSet<String> {
        let mut topics = self.agents.as_ref().map_or_else(
            || {
                AgentKind::ALL
                    .into_iter()
                    .map(|provider| provider.topic().to_owned())
                    .collect()
            },
            |agents| agents.topics(),
        );
        if self.jobs.upgrade().is_some_and(|jobs| jobs.is_active()) {
            topics.insert("jobs.changed".to_owned());
        }
        topics
    }

    fn with_active_topic(
        &self,
        module: &str,
        generation: u64,
        topic: &str,
        publish: &mut dyn FnMut(),
    ) -> Result<(), String> {
        if module == "bootty.jobs" && topic == "jobs.changed" {
            if self
                .jobs
                .upgrade()
                .is_some_and(|jobs| jobs.is_active() && jobs.generation() == generation)
            {
                publish();
                return Ok(());
            }
            return Err("job owner has retired".to_owned());
        }
        if AgentKind::ALL
            .iter()
            .any(|provider| provider.module() == module && provider.topic() == topic)
        {
            return self.agents.as_ref().map_or_else(
                || Err("agent service is not composed".to_owned()),
                |agents| agents.with_active_topic(module, generation, topic, publish),
            );
        }
        Err(format!(
            "agent event topic `{topic}` is not registered by `{module}`"
        ))
    }
}

impl std::fmt::Debug for CommandCatalog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandCatalog")
            .field("core", &self.core)
            .finish_non_exhaustive()
    }
}

impl Default for CommandCatalog {
    fn default() -> Self {
        Self::with_services(None, std::sync::Weak::new())
    }
}

impl CommandCatalog {
    pub fn with_agent_service(agents: Arc<AgentService>) -> Self {
        Self::with_services(Some(agents), std::sync::Weak::new())
    }

    pub(crate) fn with_services(
        agents: Option<Arc<AgentService>>,
        jobs: std::sync::Weak<bootty_host::jobs::JobRegistry>,
    ) -> Self {
        let core = CommandRegistry::core();
        let source = Arc::new(NativeCatalogSource {
            agents: agents.clone(),
            jobs,
        });
        Self {
            core,
            control: Arc::new(ControlCatalog::new(core.list().cloned().collect(), source)),
            agents,
        }
    }

    #[must_use]
    pub fn list(&self) -> Vec<CommandDescriptor> {
        self.control.list()
    }

    #[must_use]
    pub fn describe(&self, id: &str) -> Option<CommandDescriptor> {
        self.control.describe(id)
    }

    /// Resolve a catalog command and validate its arguments.
    ///
    /// # Errors
    /// Returns an unknown-command, unsupported-command, or invalid-arguments outcome.
    pub fn resolve(
        &self,
        invocation: CommandInvocation,
    ) -> Result<ResolvedCommandInvocation, CommandOutcome> {
        if let Some(descriptor) = agent_command_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.id == invocation.command)
        {
            validate_arguments(&descriptor, &invocation.arguments)?;
            let executor = self
                .agents
                .as_ref()
                .map_or(CommandExecutor::UncomposedAgent, |agents| {
                    CommandExecutor::Agent(Arc::clone(agents))
                });
            return Ok(ResolvedCommandInvocation {
                descriptor,
                invocation,
                executor,
            });
        }
        self.core.resolve(invocation)
    }

    #[must_use]
    pub fn agents(&self) -> Option<Arc<AgentService>> {
        self.agents.clone()
    }

    #[must_use]
    pub fn control_catalog(&self) -> Arc<ControlCatalog> {
        Arc::clone(&self.control)
    }
}

fn sidebar_descriptor(action: SidebarAction) -> CommandDescriptor {
    let (title, description) = match action {
        SidebarAction::Ignore => (
            "Ignore Sidebar Input",
            "Consume a sidebar key without changing the workspace.",
        ),
        SidebarAction::PreviousSession => (
            "Previous Sidebar Session",
            "Move the sidebar session cursor to the previous session.",
        ),
        SidebarAction::NextSession => (
            "Next Sidebar Session",
            "Move the sidebar session cursor to the next session.",
        ),
        SidebarAction::ActivateSession => (
            "Activate Sidebar Session",
            "Open the sidebar session under the cursor.",
        ),
        SidebarAction::FocusTerminal => (
            "Focus Terminal",
            "Return keyboard focus from the sidebar to the terminal.",
        ),
    };
    CommandDescriptor {
        id: action.command_id().to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        mutation: MutationClass::Write,
        arguments: CompactSchema::default(),
        target: Some(ResourceKind::ApplicationWindow),
        palette: false,
    }
}

fn resolve_keybind_command(
    invocation: &CommandInvocation,
) -> Result<CoreCommandExecutor, CommandOutcome> {
    let Some(action) = keybind_action_for_name(&invocation.action_name()) else {
        return Err(CommandOutcome::Unsupported {
            message: ErrorNotice::CommandHasNoAppExecutor(format!(
                "command {} has no app executor",
                invocation.command
            ))
            .raw_message(),
        });
    };
    Ok(match action {
        KeybindAction::App(crate::app_actions::AppAction::ReloadConfig) => {
            CoreCommandExecutor::Synchronous(SynchronousCommand::ReloadConfig)
        }
        action => CoreCommandExecutor::Keybind(action),
    })
}

fn validate_arguments(
    descriptor: &CommandDescriptor,
    arguments: &[String],
) -> Result<(), CommandOutcome> {
    let required = descriptor
        .arguments
        .arguments
        .iter()
        .filter(|argument| argument.required)
        .count();
    if arguments.len() < required || arguments.len() > descriptor.arguments.arguments.len() {
        return Err(CommandOutcome::Failed {
            code: "invalid_arguments".to_owned(),
            message: ErrorNotice::InvalidCommandArguments(format!(
                "command {} expects {} argument(s), got {}",
                descriptor.id,
                descriptor.arguments.arguments.len(),
                arguments.len()
            ))
            .raw_message(),
        });
    }
    for (schema, value) in descriptor.arguments.arguments.iter().zip(arguments) {
        let valid_type = match schema.value_type {
            ValueType::String => true,
            ValueType::Integer => value.parse::<i64>().is_ok(),
            ValueType::Number => value.parse::<f32>().is_ok_and(f32::is_finite),
        };
        let parsed_integer = || value.parse::<i64>().ok();
        let valid_minimum = schema
            .minimum
            .is_none_or(|minimum| parsed_integer().is_some_and(|value| value >= minimum));
        let valid_maximum = schema
            .maximum
            .is_none_or(|maximum| parsed_integer().is_some_and(|value| value <= maximum));
        let valid = valid_type
            && valid_minimum
            && valid_maximum
            && (schema.choices.is_empty() || schema.choices.contains(value));
        if !valid {
            return Err(CommandOutcome::Failed {
                code: "invalid_arguments".to_owned(),
                message: ErrorNotice::InvalidCommandArguments(format!(
                    "invalid {} argument for {}",
                    schema.name, descriptor.id
                ))
                .raw_message(),
            });
        }
    }
    Ok(())
}

fn argument(name: &str, value_type: ValueType) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type,
        required: true,
        choices: Vec::new(),
        minimum: None,
        maximum: None,
    }
}
fn resource_kind(value: &str) -> Option<ResourceKind> {
    match value {
        "instance" => Some(ResourceKind::Instance),
        "application_window" => Some(ResourceKind::ApplicationWindow),
        "binding" => Some(ResourceKind::Binding),
        "session" => Some(ResourceKind::Session),
        "mux_window" => Some(ResourceKind::MuxWindow),
        "pane" => Some(ResourceKind::Pane),
        "terminal" => Some(ResourceKind::Terminal),
        _ => None,
    }
}
