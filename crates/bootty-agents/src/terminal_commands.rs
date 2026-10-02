use bootty_control::{
    ArgumentSchema, CommandDescriptor, CompactSchema, MutationClass, ResourceKind, ValueType,
};

use crate::AgentKind;

#[must_use]
pub fn terminal_command_descriptors() -> Vec<CommandDescriptor> {
    let mut descriptors = Vec::new();
    for provider in AgentKind::ALL {
        let label = match provider {
            AgentKind::Codex => "Codex",
            AgentKind::Claude => "Claude",
            AgentKind::Pi => "Pi",
        };
        let specs = launch_specs(label)
            .into_iter()
            .chain(control_specs(label))
            .chain(history_account_specs(label));
        for (operation, title, names, required, target, mutation, palette) in specs {
            descriptors.push(CommandDescriptor {
                id: format!("agents.{provider}.{operation}"), title,
                description: "Uses the provider's native terminal interface; backend terminals own tabs, splits and processes.".to_owned(),
                mutation,
                arguments: CompactSchema { arguments: names.into_iter().enumerate().map(|(index, name)| ArgumentSchema { name: name.to_owned(), value_type: ValueType::String, required: index < required, choices: Vec::new(), minimum: None, maximum: None }).collect() },
                target, palette,
            });
        }
    }
    descriptors
}

type TerminalCommandSpec = (
    &'static str,
    String,
    Vec<&'static str>,
    usize,
    Option<ResourceKind>,
    MutationClass,
    bool,
);

fn launch_specs(label: &str) -> [TerminalCommandSpec; 4] {
    [
        (
            "start",
            format!("Open {label} terminal"),
            vec!["cwd", "program", "argv"],
            0,
            Some(ResourceKind::Binding),
            MutationClass::Write,
            true,
        ),
        (
            "tab",
            format!("Open {label} terminal tab"),
            vec!["cwd", "program", "argv"],
            0,
            Some(ResourceKind::Session),
            MutationClass::Write,
            true,
        ),
        (
            "resume",
            format!("Resume {label} session"),
            vec!["session", "cwd", "program", "argv"],
            1,
            Some(ResourceKind::Binding),
            MutationClass::Write,
            false,
        ),
        (
            "fork",
            format!("Fork {label} session"),
            vec!["session", "cwd", "program", "argv"],
            1,
            Some(ResourceKind::Binding),
            MutationClass::Write,
            false,
        ),
    ]
}

fn control_specs(label: &str) -> [TerminalCommandSpec; 7] {
    [
        (
            "state",
            format!("Inspect {label} activity"),
            vec![],
            0,
            Some(ResourceKind::Terminal),
            MutationClass::Read,
            false,
        ),
        (
            "prompt",
            format!("Send prompt to {label}"),
            vec!["message"],
            1,
            Some(ResourceKind::Terminal),
            MutationClass::Write,
            false,
        ),
        (
            "follow_up",
            format!("Follow up with {label}"),
            vec!["message"],
            1,
            Some(ResourceKind::Terminal),
            MutationClass::Write,
            false,
        ),
        (
            "steer",
            format!("Send text to {label}"),
            vec!["message"],
            1,
            Some(ResourceKind::Terminal),
            MutationClass::Write,
            false,
        ),
        (
            "abort",
            format!("Interrupt {label}"),
            vec![],
            0,
            Some(ResourceKind::Terminal),
            MutationClass::Destructive,
            false,
        ),
        (
            "interrupt",
            format!("Interrupt {label}"),
            vec![],
            0,
            Some(ResourceKind::Terminal),
            MutationClass::Destructive,
            false,
        ),
        (
            "stop",
            format!("Close {label} terminal"),
            vec![],
            0,
            Some(ResourceKind::Terminal),
            MutationClass::Destructive,
            false,
        ),
    ]
}

fn history_account_specs(label: &str) -> [TerminalCommandSpec; 4] {
    [
        (
            "history",
            format!("{label} session history"),
            vec![],
            0,
            None,
            MutationClass::Read,
            true,
        ),
        (
            "account.status",
            format!("{label} account status"),
            vec!["provider", "program"],
            0,
            None,
            MutationClass::Read,
            false,
        ),
        (
            "account.login",
            format!("Sign in to {label}"),
            vec!["cwd", "program"],
            0,
            Some(ResourceKind::Binding),
            MutationClass::Write,
            true,
        ),
        (
            "account.logout",
            format!("Sign out of {label}"),
            vec!["cwd", "program"],
            0,
            Some(ResourceKind::Binding),
            MutationClass::Destructive,
            false,
        ),
    ]
}
