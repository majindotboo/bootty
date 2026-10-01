use crate::AgentKind;
use bootty_control::{
    ArgumentSchema, CommandDescriptor, CompactSchema, MutationClass, ResourceKind, ValueType,
};

#[must_use]
pub fn native_command_descriptors() -> Vec<CommandDescriptor> {
    let mut descriptors = vec![descriptor(
        "agents.native.list".to_owned(),
        "List agent sessions",
        MutationClass::Read,
        None,
        &[],
    )];
    for provider in AgentKind::ALL {
        for (operation, mutation, arguments) in [
            (
                "start",
                MutationClass::Write,
                vec![("cwd", false), ("program", false), ("argv", false)],
            ),
            ("resume", MutationClass::Write, vec![("session", false)]),
            ("fork", MutationClass::Write, vec![("session", false)]),
            ("state", MutationClass::Read, vec![]),
            ("rename", MutationClass::Write, vec![("title", true)]),
            ("remove", MutationClass::Destructive, vec![]),
            ("prompt", MutationClass::Write, vec![("message", true)]),
            ("interrupt", MutationClass::Write, vec![]),
            ("abort", MutationClass::Write, vec![]),
            ("stop", MutationClass::Destructive, vec![]),
            ("history", MutationClass::Read, vec![]),
            (
                "approve",
                MutationClass::Write,
                vec![("request", true), ("allow", true)],
            ),
            (
                "respond",
                MutationClass::Write,
                vec![("request", true), ("response", true)],
            ),
            ("account.status", MutationClass::Read, vec![]),
            (
                "account.login",
                MutationClass::Write,
                vec![("provider", false)],
            ),
            ("account.logout", MutationClass::Destructive, vec![]),
        ] {
            let target = Some(if operation == "start" {
                ResourceKind::Binding
            } else {
                ResourceKind::Session
            });
            descriptors.push(descriptor(
                format!("agents.{provider}.{operation}"),
                &format!("{provider} {}", operation.replace('.', " ")),
                mutation,
                target,
                &arguments,
            ));
        }
    }
    descriptors
}

fn descriptor(
    id: String,
    title: &str,
    mutation: MutationClass,
    target: Option<ResourceKind>,
    arguments: &[(&str, bool)],
) -> CommandDescriptor {
    CommandDescriptor {
        id,
        title: title.to_owned(),
        description: String::new(),
        mutation,
        target,
        palette: false,
        arguments: CompactSchema {
            arguments: arguments
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
    }
}
