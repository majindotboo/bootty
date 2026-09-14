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

#[must_use]
pub fn command_descriptors() -> Vec<CommandDescriptor> {
    descriptors().to_vec()
}

pub fn descriptors() -> &'static [CommandDescriptor] {
    static DESCRIPTORS: OnceLock<Vec<CommandDescriptor>> = OnceLock::new();
    DESCRIPTORS.get_or_init(build_descriptors)
}

fn build_descriptors() -> Vec<CommandDescriptor> {
    let mut descriptors = pi_descriptors()
        .into_iter()
        .chain(codex_descriptors())
        .chain(claude_descriptors())
        .collect::<Vec<_>>();
    for provider in crate::AgentKind::ALL {
        descriptors.push(terminal_descriptor(
            &format!("agents.{provider}.acknowledge"),
            &format!("Acknowledge {provider} attention"),
            MutationClass::Write,
            ResourceKind::Terminal,
            vec![string_argument("sequence", true)],
        ));
        for action in ["resume", "fork"] {
            descriptors.push(terminal_descriptor(
                &format!("agents.{provider}.{action}"),
                &format!("{action} {provider} session"),
                MutationClass::Write,
                ResourceKind::Terminal,
                vec![
                    string_argument("session", false),
                    string_argument("cwd", false),
                    string_argument("program", false),
                    string_argument("argv", false),
                ],
            ));
        }
    }
    descriptors
}

fn start_descriptor(provider: &str, title: &str) -> CommandDescriptor {
    terminal_descriptor(
        &format!("agents.{provider}.start"),
        title,
        MutationClass::Write,
        ResourceKind::Terminal,
        vec![
            string_argument("cwd", false),
            string_argument("program", false),
            string_argument("argv", false),
        ],
    )
}

fn state_descriptor(id: &str, title: &str) -> CommandDescriptor {
    CommandDescriptor {
        id: id.to_owned(),
        title: title.to_owned(),
        description: String::new(),
        mutation: MutationClass::Read,
        arguments: CompactSchema {
            arguments: vec![string_argument("pane", false)],
        },
        target: None,
        palette: false,
    }
}

fn ingest_descriptor(id: &str, title: &str) -> CommandDescriptor {
    CommandDescriptor {
        id: id.to_owned(),
        title: title.to_owned(),
        description: String::new(),
        mutation: MutationClass::Write,
        arguments: CompactSchema {
            arguments: vec![
                string_argument("event", true),
                string_argument("pane", false),
                string_argument("launch", false),
            ],
        },
        target: None,
        palette: false,
    }
}

fn terminal_descriptor(
    id: &str,
    title: &str,
    mutation: MutationClass,
    target: ResourceKind,
    arguments: Vec<ArgumentSchema>,
) -> CommandDescriptor {
    CommandDescriptor {
        id: id.to_owned(),
        title: title.to_owned(),
        description: String::new(),
        mutation,
        arguments: CompactSchema { arguments },
        target: Some(target),
        palette: false,
    }
}

fn string_argument(name: &str, required: bool) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type: ValueType::String,
        required,
        choices: Vec::new(),
        minimum: None,
        maximum: None,
    }
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

fn pi_descriptors() -> [CommandDescriptor; 8] {
    [
        start_descriptor("pi", "Start Pi"),
        terminal_descriptor(
            "agents.pi.prompt",
            "Prompt Pi",
            MutationClass::Write,
            ResourceKind::Terminal,
            vec![string_argument("message", true)],
        ),
        terminal_descriptor(
            "agents.pi.steer",
            "Steer Pi",
            MutationClass::Write,
            ResourceKind::Terminal,
            vec![string_argument("message", true)],
        ),
        terminal_descriptor(
            "agents.pi.follow_up",
            "Follow up with Pi",
            MutationClass::Write,
            ResourceKind::Terminal,
            vec![string_argument("message", true)],
        ),
        terminal_descriptor(
            "agents.pi.abort",
            "Abort Pi",
            MutationClass::Destructive,
            ResourceKind::Terminal,
            Vec::new(),
        ),
        state_descriptor("agents.pi.state", "Inspect Pi state"),
        terminal_descriptor(
            "agents.pi.stop",
            "Stop Pi pane",
            MutationClass::Destructive,
            ResourceKind::Pane,
            Vec::new(),
        ),
        ingest_descriptor("agents.pi.ingest", "Ingest a Pi native event"),
    ]
}

fn codex_descriptors() -> [CommandDescriptor; 7] {
    [
        start_descriptor("codex", "Start Codex"),
        terminal_descriptor(
            "agents.codex.prompt",
            "Prompt Codex",
            MutationClass::Write,
            ResourceKind::Terminal,
            vec![string_argument("message", true)],
        ),
        terminal_descriptor(
            "agents.codex.steer",
            "Steer Codex",
            MutationClass::Write,
            ResourceKind::Terminal,
            vec![string_argument("message", true)],
        ),
        terminal_descriptor(
            "agents.codex.interrupt",
            "Interrupt Codex",
            MutationClass::Destructive,
            ResourceKind::Terminal,
            Vec::new(),
        ),
        state_descriptor("agents.codex.state", "Inspect Codex state"),
        terminal_descriptor(
            "agents.codex.stop",
            "Stop Codex pane",
            MutationClass::Destructive,
            ResourceKind::Pane,
            Vec::new(),
        ),
        ingest_descriptor("agents.codex.ingest", "Ingest a Codex hook event"),
    ]
}

fn claude_descriptors() -> [CommandDescriptor; 7] {
    [
        start_descriptor("claude", "Start Claude Code"),
        terminal_descriptor(
            "agents.claude.prompt",
            "Send text to Claude Code",
            MutationClass::Write,
            ResourceKind::Terminal,
            vec![string_argument("message", true)],
        ),
        terminal_descriptor(
            "agents.claude.steer",
            "Send text to Claude Code",
            MutationClass::Write,
            ResourceKind::Terminal,
            vec![string_argument("message", true)],
        ),
        terminal_descriptor(
            "agents.claude.follow_up",
            "Send text to Claude Code",
            MutationClass::Write,
            ResourceKind::Terminal,
            vec![string_argument("message", true)],
        ),
        terminal_descriptor(
            "agents.claude.abort",
            "Interrupt Claude Code",
            MutationClass::Destructive,
            ResourceKind::Terminal,
            Vec::new(),
        ),
        ingest_descriptor("agents.claude.ingest", "Ingest a Claude Code hook event"),
        state_descriptor("agents.claude.state", "Inspect Claude Code state"),
    ]
}
