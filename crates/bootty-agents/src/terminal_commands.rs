use bootty_control::{
    ArgumentSchema, CommandDescriptor, CompactSchema, MutationClass, ResourceKind, ValueType,
};

use crate::AgentKind;

#[must_use]
pub fn terminal_command_descriptors() -> Vec<CommandDescriptor> {
    let mut descriptors = vec![CommandDescriptor {
        id: "agents.spawn".to_owned(),
        title: "Create a child task".to_owned(),
        description: "Create a detached child of an opted-in live agent attachment.".to_owned(),
        mutation: MutationClass::Write,
        arguments: CompactSchema {
            arguments: vec![
                ArgumentSchema {
                    name: "request".to_owned(),
                    value_type: ValueType::String,
                    required: true,
                    choices: Vec::new(),
                    minimum: None,
                    maximum: None,
                },
                ArgumentSchema {
                    name: "attachment".to_owned(),
                    value_type: ValueType::String,
                    required: false,
                    choices: Vec::new(),
                    minimum: None,
                    maximum: None,
                },
            ],
        },
        target: Some(ResourceKind::Terminal),
        palette: false,
    }];
    descriptors.push(provider_update_descriptor());
    for provider in AgentKind::ALL {
        let label = match provider {
            AgentKind::Codex => "Codex",
            AgentKind::Claude => "Claude",
            AgentKind::Pi => "Pi",
        };
        let specs = launch_specs(label)
            .into_iter()
            .chain(control_specs(label))
            .chain(recovery_specs(label))
            .chain(history_account_specs(label))
            .chain(connector_specs(provider, label));
        for (operation, title, names, required, target, mutation, palette) in specs {
            descriptors.push(CommandDescriptor {
                id: format!("agents.{provider}.{operation}"),
                title,
                description: match operation {
                    "start" => format!("Start {label} in the selected project."),
                    "tab" => format!("Start {label} in a new tab in this session."),
                    "pane" => format!("Start {label} in a split of the captured terminal."),
                    "account.login" => format!("Open {label}'s sign-in flow in a terminal."),
                    "connectors" => format!("List the MCP connectors {label} has."),
                    "connector.add" => format!(
                        "Add an MCP connector, reached at a URL or started by a command, with {label}'s own command in a terminal."
                    ),
                    "connector.login" => {
                        format!("Open {label}'s sign-in flow for an MCP connector in a terminal.")
                    }
                    "history.open" => format!(
                        "Browse saved {label} conversations in the selected account and project."
                    ),
                    _ => format!("Use {label}'s terminal interface."),
                },
                mutation,
                arguments: CompactSchema {
                    arguments: names
                        .into_iter()
                        .enumerate()
                        .map(|(index, name)| ArgumentSchema {
                            name: name.to_owned(),
                            value_type: ValueType::String,
                            required: index < required,
                            choices: Vec::new(),
                            minimum: None,
                            maximum: None,
                        })
                        .collect(),
                },
                target,
                palette,
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

fn launch_specs(label: &str) -> [TerminalCommandSpec; 5] {
    [
        (
            "start",
            format!("Open {label} terminal"),
            vec![
                "cwd", "program", "argv", "name", "profile", "identity", "title",
            ],
            0,
            Some(ResourceKind::Session),
            MutationClass::Write,
            false,
        ),
        (
            "tab",
            format!("Open {label} terminal"),
            vec!["cwd", "program", "argv", "profile"],
            0,
            Some(ResourceKind::Session),
            MutationClass::Write,
            true,
        ),
        (
            "pane",
            format!("Open {label} in a split"),
            vec!["direction", "cwd", "program", "argv", "profile"],
            1,
            Some(ResourceKind::Terminal),
            MutationClass::Write,
            false,
        ),
        (
            "resume",
            format!("Resume {label} session"),
            vec![
                "session",
                "cwd",
                "program",
                "argv",
                "profile",
                "account_directory",
            ],
            1,
            Some(ResourceKind::Session),
            MutationClass::Write,
            false,
        ),
        (
            "fork",
            format!("Fork {label} session"),
            vec![
                "session",
                "cwd",
                "program",
                "argv",
                "profile",
                "account_directory",
            ],
            1,
            Some(ResourceKind::Session),
            MutationClass::Write,
            false,
        ),
    ]
}

fn recovery_specs(label: &str) -> [TerminalCommandSpec; 3] {
    [
        (
            "associate",
            format!("Associate {label} saved pane"),
            vec!["catalog_handle"],
            1,
            Some(ResourceKind::Terminal),
            MutationClass::Write,
            false,
        ),
        (
            "restore",
            format!("Restore {label} terminal"),
            vec!["catalog_handle"],
            1,
            Some(ResourceKind::Terminal),
            MutationClass::Write,
            false,
        ),
        (
            "restore.launch",
            format!("Launch restored {label} terminal"),
            vec!["preparation"],
            1,
            Some(ResourceKind::Terminal),
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

/// A provider's MCP connectors stay its own: listed through its CLI, and added and signed in to
/// in a terminal it runs.
fn connector_specs(provider: AgentKind, label: &str) -> Vec<TerminalCommandSpec> {
    if !crate::manages_connectors(provider) {
        return Vec::new();
    }
    vec![
        (
            "connectors",
            format!("List {label} connectors"),
            vec![],
            0,
            Some(ResourceKind::Binding),
            MutationClass::Read,
            false,
        ),
        (
            "connector.add",
            format!("Add a {label} connector"),
            vec!["name", "target"],
            2,
            Some(ResourceKind::Session),
            MutationClass::Write,
            false,
        ),
        (
            "connector.login",
            format!("Sign in to a {label} connector"),
            vec!["name"],
            1,
            Some(ResourceKind::Session),
            MutationClass::Write,
            false,
        ),
    ]
}

fn history_account_specs(label: &str) -> [TerminalCommandSpec; 7] {
    [
        (
            "provider.status",
            format!("Inspect {label} installation"),
            vec!["provider", "program"],
            0,
            Some(ResourceKind::Binding),
            MutationClass::Read,
            false,
        ),
        (
            "provider.update",
            format!("Update {label}"),
            vec!["cwd", "program"],
            0,
            Some(ResourceKind::Session),
            MutationClass::Write,
            false,
        ),
        (
            "history.open",
            format!("Open {label} history"),
            vec![],
            0,
            Some(ResourceKind::Session),
            MutationClass::Read,
            false,
        ),
        (
            "history",
            format!("{label} session history"),
            vec!["cwd", "profile", "account_directory", "before"],
            0,
            Some(ResourceKind::Binding),
            MutationClass::Read,
            // Keep the query on the command API until a history view can present its result.
            false,
        ),
        (
            "account.status",
            format!("{label} account status"),
            vec!["provider", "program"],
            0,
            Some(ResourceKind::Binding),
            MutationClass::Read,
            false,
        ),
        (
            "account.login",
            format!("Sign in to {label}"),
            vec!["cwd", "program", "profile"],
            0,
            Some(ResourceKind::Session),
            MutationClass::Write,
            false,
        ),
        (
            "account.logout",
            format!("Sign out of {label}"),
            vec!["cwd", "program"],
            0,
            Some(ResourceKind::Session),
            MutationClass::Destructive,
            false,
        ),
    ]
}

fn provider_update_descriptor() -> CommandDescriptor {
    CommandDescriptor {
        id: "agents.providers.update".into(),
        title: "Update a local provider".into(),
        description: "Update a recognized installation when it has no active processes.".into(),
        mutation: MutationClass::Write,
        arguments: CompactSchema {
            arguments: vec![ArgumentSchema {
                name: "provider".into(),
                value_type: ValueType::String,
                required: true,
                choices: AgentKind::ALL
                    .into_iter()
                    .map(|provider| provider.to_string())
                    .collect(),
                minimum: None,
                maximum: None,
            }],
        },
        target: None,
        palette: false,
    }
}
