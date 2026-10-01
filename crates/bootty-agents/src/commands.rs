use std::{sync::OnceLock, time::Instant};

use bootty_control::{
    ArgumentSchema, Caller, CommandCancellation, CommandDescriptor, CommandInvocation,
    CommandOutcome, CommandTarget, CompactSchema, MutationClass, ResourceKind, ValueType,
};
use serde_json::Value;

/// Commands needed by the provider adapters are injected at the host boundary.  This keeps the
/// agent crate independent of GPUI, mux implementations, and the control server.
pub trait AgentCommandExecutor: Send + Sync {
    fn execute(
        &self,
        invocation: CommandInvocation,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> CommandOutcome;
}

impl<F> AgentCommandExecutor for F
where
    F: Fn(CommandInvocation, Instant, CommandCancellation) -> CommandOutcome + Send + Sync,
{
    fn execute(
        &self,
        invocation: CommandInvocation,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> CommandOutcome {
        self(invocation, deadline, cancellation)
    }
}

/// The captured invocation context used by the service.  `target_supplied` retains the
/// distinction between a host-captured pane and the implicit `new_tab` path for `.start`.
#[derive(Clone)]
pub struct AgentInvocation {
    pub invocation: CommandInvocation,
    pub target_supplied: bool,
    /// Scope captured by the host while resolving the command target. Hook invocations may leave
    /// this empty; the service then asks its injected pane resolver.
    pub scope: Option<String>,
    pub launch_context: crate::AgentLaunchContext,
    pub deadline: Instant,
    pub cancellation: CommandCancellation,
}

impl AgentInvocation {
    #[must_use]
    pub fn new(
        invocation: CommandInvocation,
        target_supplied: bool,
        scope: Option<String>,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> Self {
        Self {
            invocation,
            target_supplied,
            scope,
            launch_context: crate::AgentLaunchContext::default(),
            deadline,
            cancellation,
        }
    }
}

#[derive(Clone, Copy)]
pub enum Operation {
    Start,
    Resume,
    Fork,
    Prompt,
    Steer,
    FollowUp,
    Abort,
    Interrupt,
    State,
    Stop,
    Ingest,
    Acknowledge,
}

impl Operation {
    const fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Resume => "resume",
            Self::Fork => "fork",
            Self::Prompt => "prompt",
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
            Self::Abort => "abort",
            Self::Interrupt => "interrupt",
            Self::State => "state",
            Self::Stop => "stop",
            Self::Ingest => "ingest",
            Self::Acknowledge => "acknowledge",
        }
    }

    const fn arguments(self) -> &'static [(&'static str, bool)] {
        match self {
            Self::Start => &[("cwd", false), ("program", false), ("argv", false)],
            Self::Resume | Self::Fork => &[
                ("session", false),
                ("cwd", false),
                ("program", false),
                ("argv", false),
            ],
            Self::Prompt | Self::Steer | Self::FollowUp => &[("message", true)],
            Self::State => &[("pane", false)],
            Self::Ingest => &[
                ("event", true),
                ("pane", false),
                ("launch", false),
                ("server", false),
            ],
            Self::Acknowledge => &[("sequence", true)],
            Self::Abort | Self::Interrupt | Self::Stop => &[],
        }
    }
}

/// One registration owns both the advertised schema and the executable operation.
pub struct AgentCommand {
    pub provider: crate::AgentKind,
    pub operation: Operation,
    pub descriptor: CommandDescriptor,
}

impl AgentCommand {
    fn new(provider: crate::AgentKind, operation: Operation, title: &str) -> Self {
        let mutation = match operation {
            Operation::State => MutationClass::Read,
            Operation::Abort | Operation::Interrupt | Operation::Stop => MutationClass::Destructive,
            _ => MutationClass::Write,
        };
        let target = match operation {
            Operation::Ingest => None,
            Operation::Stop => Some(ResourceKind::Pane),
            _ => Some(ResourceKind::Terminal),
        };
        Self {
            provider,
            operation,
            descriptor: CommandDescriptor {
                id: format!("agents.{provider}.{}", operation.name()),
                title: title.to_owned(),
                description: String::new(),
                mutation,
                arguments: CompactSchema {
                    arguments: operation
                        .arguments()
                        .iter()
                        .map(|(name, required)| ArgumentSchema {
                            name: (*name).to_owned(),
                            value_type: ValueType::String,
                            required: *required,
                            choices: Vec::new(),
                            minimum: None,
                            maximum: None,
                        })
                        .collect(),
                },
                target,
                palette: false,
            },
        }
    }

    pub fn validate_arguments(&self, arguments: &[String]) -> Option<String> {
        let schema = &self.descriptor.arguments.arguments;
        let maximum = schema.len();
        if arguments.len() > maximum {
            return Some(format!("command accepts at most {maximum} argument(s)"));
        }
        if schema
            .iter()
            .skip(arguments.len())
            .any(|argument| argument.required)
        {
            return Some("required argument is missing".to_owned());
        }
        None
    }
}

#[must_use]
pub fn command_descriptors() -> Vec<CommandDescriptor> {
    catalog()
        .iter()
        .map(|command| command.descriptor.clone())
        .collect()
}

pub fn resolve(command: &str) -> Option<&'static AgentCommand> {
    catalog()
        .iter()
        .find(|entry| entry.descriptor.id == command)
}

fn catalog() -> &'static [AgentCommand] {
    static COMMANDS: OnceLock<Vec<AgentCommand>> = OnceLock::new();
    COMMANDS.get_or_init(|| {
        use crate::AgentKind::{Claude, Codex, Pi};
        use Operation::{Abort, FollowUp, Ingest, Interrupt, Prompt, Start, State, Steer, Stop};

        let mut commands = [
            (Pi, Start, "Start Pi"),
            (Pi, Prompt, "Prompt Pi"),
            (Pi, Steer, "Steer Pi"),
            (Pi, FollowUp, "Follow up with Pi"),
            (Pi, Abort, "Abort Pi"),
            (Pi, State, "Inspect Pi state"),
            (Pi, Stop, "Stop Pi pane"),
            (Pi, Ingest, "Ingest a Pi native event"),
            (Codex, Start, "Start Codex"),
            (Codex, Prompt, "Prompt Codex"),
            (Codex, Steer, "Steer Codex"),
            (Codex, Interrupt, "Interrupt Codex"),
            (Codex, State, "Inspect Codex state"),
            (Codex, Stop, "Stop Codex pane"),
            (Codex, Ingest, "Ingest a Codex hook event"),
            (Claude, Start, "Start Claude Code"),
            (Claude, Prompt, "Send text to Claude Code"),
            (Claude, Steer, "Send text to Claude Code"),
            (Claude, FollowUp, "Send text to Claude Code"),
            (Claude, Abort, "Interrupt Claude Code"),
            (Claude, Ingest, "Ingest a Claude Code hook event"),
            (Claude, State, "Inspect Claude Code state"),
        ]
        .into_iter()
        .map(|(provider, operation, title)| AgentCommand::new(provider, operation, title))
        .collect::<Vec<_>>();
        for provider in crate::AgentKind::ALL {
            commands.push(AgentCommand::new(
                provider,
                Operation::Acknowledge,
                &format!("Acknowledge {provider} attention"),
            ));
            for operation in [Operation::Resume, Operation::Fork] {
                commands.push(AgentCommand::new(
                    provider,
                    operation,
                    &format!("{} {provider} session", operation.name()),
                ));
            }
        }
        commands
    })
}

pub const fn success(value: Value) -> CommandOutcome {
    CommandOutcome::Success {
        value,
        warnings: Vec::new(),
    }
}

pub fn failed(code: &str, message: impl Into<String>) -> CommandOutcome {
    CommandOutcome::Failed {
        code: code.to_owned(),
        message: message.into(),
    }
}

pub fn nested_invocation(
    command: &str,
    arguments: Vec<String>,
    target: Option<CommandTarget>,
) -> CommandInvocation {
    let mut invocation = CommandInvocation::new(command, arguments, Caller::Internal);
    invocation.target = target;
    invocation
}
