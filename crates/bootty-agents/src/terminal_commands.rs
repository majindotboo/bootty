use crate::AgentKind;
use bootty_control::{
    ArgumentSchema, CommandDescriptor, CompactSchema, MutationClass, ResourceKind, ValueType,
};

#[must_use]
pub fn terminal_command_descriptors() -> Vec<CommandDescriptor> {
    let mut descriptors = Vec::new();
    for provider in AgentKind::ALL {
        for (operation, mutation, target, arguments) in [
            (
                "start",
                MutationClass::Write,
                ResourceKind::Binding,
                vec![("cwd", false), ("program", false), ("argv", false)],
            ),
            (
                "tab",
                MutationClass::Write,
                ResourceKind::Session,
                vec![("cwd", false), ("program", false), ("argv", false)],
            ),
            (
                "resume",
                MutationClass::Write,
                ResourceKind::Binding,
                vec![("session", false), ("cwd", false)],
            ),
            (
                "fork",
                MutationClass::Write,
                ResourceKind::Binding,
                vec![("session", false), ("cwd", false)],
            ),
            (
                "history",
                MutationClass::Write,
                ResourceKind::Binding,
                vec![("cwd", false)],
            ),
            (
                "sessions",
                MutationClass::Read,
                ResourceKind::Binding,
                vec![("cwd", false)],
            ),
            (
                "account.status",
                MutationClass::Read,
                ResourceKind::Binding,
                vec![("provider", false)],
            ),
            (
                "account.login",
                MutationClass::Write,
                ResourceKind::Binding,
                vec![],
            ),
            (
                "account.logout",
                MutationClass::Destructive,
                ResourceKind::Binding,
                vec![],
            ),
            (
                "prompt",
                MutationClass::Write,
                ResourceKind::Terminal,
                vec![("message", true)],
            ),
            (
                "interrupt",
                MutationClass::Write,
                ResourceKind::Terminal,
                vec![],
            ),
            (
                "abort",
                MutationClass::Write,
                ResourceKind::Terminal,
                vec![],
            ),
            (
                "stop",
                MutationClass::Destructive,
                ResourceKind::Terminal,
                vec![],
            ),
        ] {
            descriptors.push(CommandDescriptor {
                id: format!("agents.{provider}.{operation}"),
                title: terminal_command_title(provider, operation),
                description: "Use the agent’s terminal interface".to_owned(),
                mutation,
                target: Some(target),
                palette: terminal_command_in_palette(provider, operation),
                arguments: terminal_arguments(arguments),
            });
        }
        descriptors.push(terminal_tool_descriptor(provider));
    }
    descriptors
}

fn terminal_tool_descriptor(provider: AgentKind) -> CommandDescriptor {
    CommandDescriptor {
        id: format!("agents.{provider}.tools"),
        title: format!("Read {provider} terminal tool"),
        description: "Use a live own-terminal read attachment".to_owned(),
        mutation: MutationClass::Read,
        arguments: terminal_arguments(vec![("request", true)]),
        target: None,
        palette: false,
    }
}

fn terminal_arguments(arguments: Vec<(&str, bool)>) -> CompactSchema {
    CompactSchema {
        arguments: arguments
            .into_iter()
            .map(|(name, required)| ArgumentSchema {
                name: name.to_owned(),
                value_type: ValueType::String,
                required,
                choices: Vec::new(),
                minimum: None,
                maximum: None,
            })
            .collect(),
    }
}

fn terminal_command_title(provider: AgentKind, operation: &str) -> String {
    let name = match provider {
        AgentKind::Codex => "Codex",
        AgentKind::Claude => "Claude",
        AgentKind::Pi => "Pi",
    };
    match operation {
        "start" => format!("Open {name} terminal"),
        "tab" => format!("Open {name} tab"),
        "history" => format!("{name} session history"),
        "resume" => format!("Resume {name} session"),
        "fork" => format!("Fork {name} session"),
        "sessions" => format!("List {name} sessions"),
        "account.status" => format!("{name} account status"),
        "account.login" => format!("Sign in to {name}"),
        "account.logout" => format!("Sign out of {name}"),
        "prompt" => format!("Send prompt to {name}"),
        "interrupt" | "abort" => format!("Interrupt {name}"),
        "stop" => format!("Close {name} terminal"),
        _ => format!("{name} {operation}"),
    }
}

fn terminal_command_in_palette(provider: AgentKind, operation: &str) -> bool {
    matches!(
        operation,
        "start" | "tab" | "history" | "resume" | "account.login"
    ) || (operation == "fork" && provider != AgentKind::Pi)
}
