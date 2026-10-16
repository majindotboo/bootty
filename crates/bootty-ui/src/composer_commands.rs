//! Bootty's own slash commands in a conversation composer. Each one runs an existing command, so
//! a typed `/stop` and the Stop button share one invocation path.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposerCommand {
    New,
    Model,
    Plan,
    Rename,
    Stop,
    Diff,
    Settings,
}

impl ComposerCommand {
    pub const ALL: [Self; 7] = [
        Self::New,
        Self::Model,
        Self::Plan,
        Self::Rename,
        Self::Stop,
        Self::Diff,
        Self::Settings,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Model => "model",
            Self::Plan => "plan",
            Self::Rename => "rename",
            Self::Stop => "stop",
            Self::Diff => "diff",
            Self::Settings => "settings",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::New => "Start a new agent tab",
            Self::Model => "Choose the model and effort",
            Self::Plan => "Turn plan mode on or off",
            Self::Rename => "Rename this conversation",
            Self::Stop => "Stop the current turn",
            Self::Diff => "Show Git changes",
            Self::Settings => "Open Settings",
        }
    }

    /// The name this command answers to. A provider command with the same name keeps it, and the
    /// built-in moves to `bootty:NAME`.
    #[must_use]
    pub fn label(self, provider_has: impl Fn(&str) -> bool) -> String {
        if provider_has(self.name()) {
            format!("bootty:{}", self.name())
        } else {
            self.name().to_owned()
        }
    }

    /// The built-in a whole draft names, and the text after it. `bootty:NAME` always reaches the
    /// built-in; the bare name does only while no provider command has it.
    #[must_use]
    pub fn parse(draft: &str, provider_has: impl Fn(&str) -> bool) -> Option<(Self, &str)> {
        let rest = draft.trim().strip_prefix('/')?;
        let (word, argument) = rest
            .split_once(char::is_whitespace)
            .map_or((rest, ""), |(word, argument)| (word, argument.trim()));
        let command = Self::ALL.into_iter().find(|command| {
            word.strip_prefix("bootty:") == Some(command.name())
                || (word == command.name() && !provider_has(word))
        })?;
        Some((command, argument))
    }
}
