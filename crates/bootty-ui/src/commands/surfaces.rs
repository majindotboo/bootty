use std::collections::BTreeMap;

use bootty_control::{
    CommandDescriptor, CommandInvocation, CommandOutcome, CompactSchema, MutationClass, ValueType,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceCommand {
    Navigate(SurfaceChooserAction),
    Choose {
        id: u64,
        kind: String,
        argv: Option<String>,
    },
    Cancel(u64),
    CreateAgent {
        id: u64,
        arguments: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceChooserAction {
    Previous,
    Next,
    First,
    Last,
    Confirm,
    Agent,
    Terminal,
    Claude,
    Codex,
    Pi,
    EditProfiles,
    Cancel,
}

impl SurfaceChooserAction {
    pub const ALL: [Self; 12] = [
        Self::Previous,
        Self::Next,
        Self::First,
        Self::Last,
        Self::Confirm,
        Self::Agent,
        Self::Terminal,
        Self::Claude,
        Self::Codex,
        Self::Pi,
        Self::EditProfiles,
        Self::Cancel,
    ];

    #[must_use]
    pub const fn command(self) -> &'static str {
        match self {
            Self::Previous => "ui.surface.previous",
            Self::Next => "ui.surface.next",
            Self::First => "ui.surface.first",
            Self::Last => "ui.surface.last",
            Self::Confirm => "ui.surface.confirm",
            Self::Agent => "ui.surface.agent",
            Self::Terminal => "ui.surface.terminal",
            Self::Claude => "ui.surface.claude",
            Self::Codex => "ui.surface.codex",
            Self::Pi => "ui.surface.pi",
            Self::EditProfiles => "ui.surface.edit_profiles",
            Self::Cancel => "ui.surface.cancel",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Previous => "Previous surface choice",
            Self::Next => "Next surface choice",
            Self::First => "First surface choice",
            Self::Last => "Last surface choice",
            Self::Confirm => "Confirm surface choice",
            Self::Agent => "Choose Agent",
            Self::Terminal => "Choose Terminal",
            Self::Claude => "Choose Claude Code terminal profile",
            Self::Codex => "Choose Codex terminal profile",
            Self::Pi => "Choose Pi terminal profile",
            Self::EditProfiles => "Edit terminal profiles",
            Self::Cancel => "Cancel surface choice",
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "One table owns the captured surface command schema"
)]
pub(super) fn register_commands(commands: &mut BTreeMap<String, super::RegisteredCommand>) {
    for action in SurfaceChooserAction::ALL {
        commands.insert(
            action.command().to_owned(),
            super::RegisteredCommand {
                descriptor: CommandDescriptor {
                    id: action.command().to_owned(),
                    title: action.title().to_owned(),
                    description: "Navigate the focused new tab or split chooser.".to_owned(),
                    mutation: MutationClass::Write,
                    arguments: CompactSchema::default(),
                    target: None,
                    palette: false,
                },
                executor: super::CommandExecutorResolver::Surface,
            },
        );
    }
    for (id, title, names) in [
        (
            "surface.choose",
            "Choose new tab or pane",
            vec!["request_id", "kind", "argv"],
        ),
        (
            "surface.cancel",
            "Cancel new tab or pane",
            vec!["request_id"],
        ),
        (
            "surface.create_agent",
            "Create captured agent tab or pane",
            vec![
                "request_id",
                "provider",
                "cwd",
                "model",
                "thinking",
                "mode",
                "budget",
                "identity",
                "title",
                "prompt",
                "account",
                "attachments",
                "selection",
                "applications",
                "attachment_ranges",
                "permissions",
            ],
        ),
    ] {
        let arguments = names
            .into_iter()
            .map(|name| {
                let mut argument = super::argument(
                    name,
                    if name == "request_id" {
                        ValueType::Integer
                    } else {
                        ValueType::String
                    },
                );
                argument.required = !matches!(
                    name,
                    "argv"
                        | "account"
                        | "attachments"
                        | "selection"
                        | "applications"
                        | "attachment_ranges"
                        | "permissions"
                );
                if name == "request_id" {
                    argument.minimum = Some(1);
                }
                if name == "kind" {
                    argument.choices = vec![
                        "terminal".to_owned(),
                        "agent".to_owned(),
                        "profile".to_owned(),
                    ];
                }
                argument
            })
            .collect();
        commands.insert(
            id.to_owned(),
            super::RegisteredCommand {
                descriptor: CommandDescriptor {
                    id: id.to_owned(),
                    title: title.to_owned(),
                    description: "Use the retained destination of the new surface chooser."
                        .to_owned(),
                    mutation: MutationClass::Write,
                    arguments: CompactSchema { arguments },
                    target: None,
                    palette: false,
                },
                executor: super::CommandExecutorResolver::Surface,
            },
        );
    }
}

pub(super) fn resolve(invocation: &CommandInvocation) -> Result<SurfaceCommand, CommandOutcome> {
    if let Some(action) = SurfaceChooserAction::ALL
        .into_iter()
        .find(|action| action.command() == invocation.command)
    {
        return Ok(SurfaceCommand::Navigate(action));
    }
    let invalid = || CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: "A surface command requires its retained request id".to_owned(),
    };
    let id = invocation
        .arguments
        .first()
        .and_then(|id| id.parse::<u64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(invalid)?;
    match invocation.command.as_str() {
        "surface.cancel" => Ok(SurfaceCommand::Cancel(id)),
        "surface.choose" => Ok(SurfaceCommand::Choose {
            id,
            kind: invocation.arguments.get(1).ok_or_else(invalid)?.clone(),
            argv: invocation.arguments.get(2).cloned(),
        }),
        "surface.create_agent" => Ok(SurfaceCommand::CreateAgent {
            id,
            arguments: invocation.arguments.iter().skip(1).cloned().collect(),
        }),
        _ => Err(invalid()),
    }
}
