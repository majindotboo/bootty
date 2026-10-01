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
                title: format!("{provider} {}", operation.replace('.', " ")),
                description: "Use the agent’s terminal interface".to_owned(),
                mutation,
                target: Some(target),
                palette: matches!(operation, "start" | "history" | "account.login"),
                arguments: CompactSchema {
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
                },
            });
        }
    }
    descriptors
}
