use bootty_control::{
    ArgumentSchema, CommandDescriptor, CompactSchema, MutationClass, ResourceKind, ValueType,
};

/// Each native conversation command: its operation, the shared arguments it starts from, the
/// arguments it adds, and what it addresses. Registration keeps this order.
const COMMANDS: &[(&str, &[&str], &[&str], ResourceKind)] = &[
    (
        "start",
        LAUNCH,
        &["projectless", "saved_session", "provider_mentions", "goal"],
        ResourceKind::Binding,
    ),
    (
        "tab",
        LAUNCH,
        &["projectless", "saved_session", "provider_mentions", "goal"],
        ResourceKind::Binding,
    ),
    (
        "pane",
        LAUNCH,
        &["direction", "saved_session", "provider_mentions", "goal"],
        ResourceKind::Terminal,
    ),
    ("list", &[], &[], ResourceKind::Binding),
    ("activities", &[], &[], ResourceKind::Binding),
    ("names", CATALOG, &["prompt"], ResourceKind::Binding),
    ("catalog", CATALOG, &[], ResourceKind::Binding),
    ("catalog-info", CATALOG, &["refresh"], ResourceKind::Binding),
    ("catalog-completions", CATALOG, &[], ResourceKind::Binding),
    ("completions", SESSION, &[], ResourceKind::Session),
    (
        "catalog-favorite",
        CATALOG,
        &["model"],
        ResourceKind::Binding,
    ),
    (
        "spawn",
        SESSION,
        &["request", "attachment"],
        ResourceKind::Session,
    ),
    (
        "control-child",
        SESSION,
        &["request", "attachment"],
        ResourceKind::Session,
    ),
    ("regenerate-title", SESSION, &[], ResourceKind::Session),
    ("focus", &[], &[], ResourceKind::Session),
    ("close", &[], &[], ResourceKind::Session),
    ("panel", SESSION, &["terminal"], ResourceKind::Session),
    ("prompt", PROMPT, &[], ResourceKind::Session),
    ("import", SESSION, &["path"], ResourceKind::Session),
    ("queue", PROMPT, &[], ResourceKind::Session),
    ("interrupt-send", PROMPT, &[], ResourceKind::Session),
    (
        "queue-cancel",
        SESSION,
        &["message_id"],
        ResourceKind::Session,
    ),
    ("draft", SESSION, &["draft"], ResourceKind::Session),
    ("seen", SESSION, &[], ResourceKind::Session),
    ("voice-start", SESSION, &["offer"], ResourceKind::Session),
    ("voice-stop", SESSION, &[], ResourceKind::Session),
    ("mark-unread", SESSION, &[], ResourceKind::Session),
    (
        "queue-retry",
        SESSION,
        &["message_id"],
        ResourceKind::Session,
    ),
    (
        "queue-steer",
        SESSION,
        &["message_id"],
        ResourceKind::Session,
    ),
    ("queue-resume", SESSION, &[], ResourceKind::Session),
    (
        "queue-move",
        SESSION,
        &["message_id", "direction"],
        ResourceKind::Session,
    ),
    (
        "attachment-preview",
        SESSION,
        &["attachment_id"],
        ResourceKind::Session,
    ),
    (
        "attachment-open",
        SESSION,
        &["attachment_id"],
        ResourceKind::Session,
    ),
    (
        "attachment-reveal",
        SESSION,
        &["attachment_id"],
        ResourceKind::Session,
    ),
    (
        "computer",
        SESSION,
        &["application", "action"],
        ResourceKind::Session,
    ),
    (
        "fork",
        SESSION,
        &["response", "provider"],
        ResourceKind::Session,
    ),
    (
        "subagent-read",
        SESSION,
        &["subagent"],
        ResourceKind::Session,
    ),
    (
        "subagent-stop",
        SESSION,
        &["subagent"],
        ResourceKind::Session,
    ),
    (
        "mcp-app-read",
        SESSION,
        &["server", "uri", "call"],
        ResourceKind::Session,
    ),
    (
        "mcp-app-call",
        SESSION,
        &["server", "tool", "arguments", "approved"],
        ResourceKind::Session,
    ),
    ("implement", SESSION, &[], ResourceKind::Session),
    ("interrupt", SESSION, &[], ResourceKind::Session),
    ("stop", SESSION, &[], ResourceKind::Session),
    (
        "rename",
        SESSION,
        &["title", "expected_title"],
        ResourceKind::Session,
    ),
    ("resume", SESSION, &[], ResourceKind::Session),
    ("unarchive", SESSION, &[], ResourceKind::Session),
    ("goal", SESSION, &["objective"], ResourceKind::Session),
    ("goal-status", SESSION, &["status"], ResourceKind::Session),
    ("turn-diff", SESSION, &["reply"], ResourceKind::Session),
    ("rewind-files", SESSION, &["prompt"], ResourceKind::Session),
    (
        "rewind",
        SESSION,
        &["prompt", "files"],
        ResourceKind::Session,
    ),
    ("models", SESSION, &[], ResourceKind::Session),
    ("models-info", SESSION, &[], ResourceKind::Session),
    ("provider", SESSION, &[], ResourceKind::Session),
    ("profiles", SESSION, &[], ResourceKind::Session),
    ("status", SESSION, &[], ResourceKind::Session),
    ("activity", SESSION, &["limit"], ResourceKind::Session),
    ("terminal", SESSION, &[], ResourceKind::Session),
    ("open-link", SESSION, &["location"], ResourceKind::Session),
    ("reveal-link", SESSION, &["location"], ResourceKind::Session),
    (
        "edit-link",
        SESSION,
        &["location", "editor"],
        ResourceKind::Session,
    ),
    ("history", SESSION, &["direction"], ResourceKind::Session),
    ("favorite", SESSION, &["model"], ResourceKind::Session),
    ("configure", SESSION, &["selection"], ResourceKind::Session),
    (
        "permissions",
        SESSION,
        &["permissions"],
        ResourceKind::Session,
    ),
    (
        "approve",
        SESSION,
        &["request", "decision"],
        ResourceKind::Session,
    ),
    ("deny", SESSION, &["request", "note"], ResourceKind::Session),
    (
        "browser-attach",
        SESSION,
        &["attachment"],
        ResourceKind::Session,
    ),
    (
        "respond",
        SESSION,
        &["request", "response"],
        ResourceKind::Session,
    ),
];

/// What every launch takes.
const LAUNCH: &[&str] = &[
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
];

/// What a catalog query takes: the launch arguments that pick a provider and account.
const CATALOG: &[&str] = &["provider", "cwd", "program", "argv", "name", "profile"];

/// The exact conversation a session command addresses.
const SESSION: &[&str] = &["id", "generation"];

/// What a message to a conversation carries.
const PROMPT: &[&str] = &[
    "id",
    "generation",
    "prompt",
    "annotations",
    "attachments",
    "citations",
    "applications",
    "authored_bytes",
    "attachment_ranges",
    "provider_mentions",
    "conversations",
    "review_comments",
];

#[must_use]
pub fn native_command_descriptors() -> Vec<CommandDescriptor> {
    COMMANDS
        .iter()
        .map(|&(operation, base, extra, target)| {
            descriptor(
                operation,
                base.iter().chain(extra).copied().collect(),
                target,
            )
        })
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
                | "models-info"
                | "provider"
                | "profiles"
                | "catalog"
                | "catalog-info"
                | "names"
                | "completions"
                | "catalog-completions"
                | "subagent-read"
                | "mcp-app-read"
                | "history"
                | "turn-diff"
                | "terminal"
        ) {
            MutationClass::Read
        } else if matches!(operation, "rewind-files" | "rewind") {
            MutationClass::Destructive
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
                            | "projectless"
                            | "saved_session"
                            | "refresh"
                            | "expected_title"
                            | "annotations"
                            | "attachments"
                            | "citations"
                            | "review_comments"
                            | "authored_bytes"
                            | "attachment_ranges"
                            | "applications"
                            | "selection"
                            | "provider_mentions"
                            | "conversations"
                            | "goal"
                            | "files"
                            | "approved"
                    ) || name == "permissions" && operation != "permissions"
                        || matches!(name, "response" | "provider") && operation == "fork"),
                    choices: match (name, operation) {
                        ("direction", "pane") => vec!["right".into(), "down".into()],
                        ("direction", "queue-move") => vec!["earlier".into(), "later".into()],
                        ("status", "goal-status") => vec!["active".into(), "paused".into()],
                        ("files", "rewind") => vec!["files".into()],
                        ("approved", "mcp-app-call") => vec!["approved".into()],
                        _ => Vec::new(),
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
