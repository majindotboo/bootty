use bootty_control::{
    ArgumentSchema, CommandDescriptor, CompactSchema, MutationClass, ResourceKind, ValueType,
};

#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "One table owns native command wire metadata"
)]
pub fn native_command_descriptors() -> Vec<CommandDescriptor> {
    let launch_arguments = launch_arguments();
    [
        ("start", launch_arguments.clone(), ResourceKind::Binding),
        ("tab", launch_arguments.clone(), ResourceKind::Binding),
        (
            "pane",
            launch_arguments.into_iter().chain(["direction"]).collect(),
            ResourceKind::Terminal,
        ),
        ("list", vec![], ResourceKind::Binding),
        ("activities", vec![], ResourceKind::Binding),
        (
            "names",
            catalog_arguments(Some("prompt")),
            ResourceKind::Binding,
        ),
        ("catalog", catalog_arguments(None), ResourceKind::Binding),
        (
            "catalog-completions",
            catalog_arguments(None),
            ResourceKind::Binding,
        ),
        (
            "completions",
            vec!["id", "generation"],
            ResourceKind::Session,
        ),
        (
            "catalog-favorite",
            catalog_arguments(Some("model")),
            ResourceKind::Binding,
        ),
        (
            "spawn",
            vec!["id", "generation", "request", "attachment"],
            ResourceKind::Session,
        ),
        (
            "control-child",
            vec!["id", "generation", "request", "attachment"],
            ResourceKind::Session,
        ),
        ("focus", vec![], ResourceKind::Session),
        ("close", vec![], ResourceKind::Session),
        (
            "panel",
            vec!["id", "generation", "terminal"],
            ResourceKind::Session,
        ),
        (
            "prompt",
            vec![
                "id",
                "generation",
                "prompt",
                "annotations",
                "attachments",
                "citations",
                "applications",
                "authored_bytes",
                "attachment_ranges",
            ],
            ResourceKind::Session,
        ),
        (
            "import",
            vec!["id", "generation", "path"],
            ResourceKind::Session,
        ),
        (
            "attachment-preview",
            vec!["id", "generation", "attachment_id"],
            ResourceKind::Session,
        ),
        (
            "computer",
            vec!["id", "generation", "application", "action"],
            ResourceKind::Session,
        ),
        (
            "fork",
            vec!["id", "generation", "response"],
            ResourceKind::Session,
        ),
        (
            "subagent-read",
            vec!["id", "generation", "subagent"],
            ResourceKind::Session,
        ),
        ("interrupt", vec!["id", "generation"], ResourceKind::Session),
        ("stop", vec!["id", "generation"], ResourceKind::Session),
        (
            "rename",
            vec!["id", "generation", "title", "expected_title"],
            ResourceKind::Session,
        ),
        ("resume", vec!["id", "generation"], ResourceKind::Session),
        ("models", vec!["id", "generation"], ResourceKind::Session),
        ("provider", vec!["id", "generation"], ResourceKind::Session),
        ("profiles", vec!["id", "generation"], ResourceKind::Session),
        ("status", vec!["id", "generation"], ResourceKind::Session),
        (
            "activity",
            vec!["id", "generation", "limit"],
            ResourceKind::Session,
        ),
        ("terminal", vec!["id", "generation"], ResourceKind::Session),
        (
            "history",
            vec!["id", "generation", "direction"],
            ResourceKind::Session,
        ),
        (
            "favorite",
            vec!["id", "generation", "model"],
            ResourceKind::Session,
        ),
        (
            "configure",
            vec!["id", "generation", "selection"],
            ResourceKind::Session,
        ),
        (
            "permissions",
            vec!["id", "generation", "permissions"],
            ResourceKind::Session,
        ),
        (
            "approve",
            vec!["id", "generation", "request", "decision"],
            ResourceKind::Session,
        ),
        (
            "browser-attach",
            vec!["id", "generation", "attachment"],
            ResourceKind::Session,
        ),
        (
            "respond",
            vec!["id", "generation", "request", "response"],
            ResourceKind::Session,
        ),
    ]
    .into_iter()
    .map(|(operation, names, target)| descriptor(operation, names, target))
    .collect()
}

fn descriptor(operation: &str, names: Vec<&str>, target: ResourceKind) -> CommandDescriptor {
    CommandDescriptor {
        id: format!("agents.native.{operation}"),
        title: match operation {
            "status" => "Agent status".into(),
            "activities" => "List agents".into(),
            _ => format!("Native conversation {operation}"),
        },
        description: "Use the exact native conversation owner.".to_owned(),
        mutation: if matches!(
            operation,
            "list"
                | "activities"
                | "status"
                | "activity"
                | "models"
                | "provider"
                | "profiles"
                | "catalog"
                | "names"
                | "completions"
                | "catalog-completions"
                | "subagent-read"
                | "history"
                | "terminal"
        ) {
            MutationClass::Read
        } else {
            MutationClass::Write
        },
        arguments: CompactSchema {
            arguments: names
                .into_iter()
                .map(|name| ArgumentSchema {
                    name: name.to_owned(),
                    value_type: ValueType::String,
                    required: !(matches!(
                        name,
                        "expected_account"
                            | "expected_title"
                            | "annotations"
                            | "attachments"
                            | "citations"
                            | "authored_bytes"
                            | "attachment_ranges"
                            | "applications"
                            | "selection"
                    ) || name == "permissions" && operation != "permissions"
                        || name == "response" && operation == "fork"),
                    choices: if name == "direction" && operation == "pane" {
                        vec!["right".into(), "down".into()]
                    } else {
                        Vec::new()
                    },
                    minimum: None,
                    maximum: None,
                })
                .collect(),
        },
        target: Some(target),
        palette: false,
    }
}

fn catalog_arguments(extra: Option<&'static str>) -> Vec<&'static str> {
    launch_arguments()
        .into_iter()
        .take(6)
        .chain(extra)
        .collect()
}

fn launch_arguments() -> Vec<&'static str> {
    vec![
        "provider",
        "cwd",
        "program",
        "argv",
        "name",
        "profile",
        "identity",
        "title",
        "prompt",
        "expected_account",
        "attachments",
        "selection",
        "applications",
        "attachment_ranges",
        "permissions",
    ]
}
