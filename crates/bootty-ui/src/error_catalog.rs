use thiserror::Error;

/// A failure that the app may publish through a window notification.
///
/// The `Display` implementation is the short user-facing message. Variants carrying a `String`
/// retain the originating message as expandable technical detail. `Technical` is the boundary for
/// errors owned by another crate or an external process.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ErrorNotice {
    #[error("the last space cannot be closed")]
    LastSpaceCannotClose,
    #[error("command requires confirmation")]
    CommandRequiresConfirmation,
    #[error("mux operation is unavailable")]
    MuxOperationUnavailable,
    #[error("mux command worker stopped")]
    MuxCommandWorkerStopped,
    #[error("command worker stopped")]
    CommandWorkerStopped,
    #[error("command does not accept a target")]
    CommandDoesNotAcceptTarget,
    #[error("unknown command")]
    UnknownCommand(String),
    #[error("command is unavailable")]
    CommandHasNoAppExecutor(String),
    #[error("command arguments are invalid")]
    InvalidCommandArguments(String),
    #[error("configuration reload failed")]
    ConfigurationReloadFailed,
    #[error("command requires a target")]
    CommandRequiresTarget(String),
    #[error("the selected target is stale")]
    StaleCommandTarget(String),
    #[error("no current target is available")]
    NoCurrentTarget(String),
    #[error("File handoff to remote Spaces is not supported.")]
    RemoteFileHandoffUnsupported,
    #[error("file handoff rejected")]
    FileHandoffRejected(String),
    #[error("this session is no longer available")]
    SessionUnavailable,
    #[error("there is nowhere else to move it yet")]
    NoSpaceToMoveSession,
    #[error("session name cannot be empty")]
    SessionNameEmpty,
    #[error("this Space does not hold the session")]
    SessionNotHeldBySpace(String),
    #[error("remote project task stopped")]
    RemoteProjectTaskStopped,
    #[error("SSH profile is unavailable")]
    SshProfileUnavailable(String),
    #[error("the terminal configuration could not be applied")]
    TerminalConfigPublicationFailed(String),
    #[error("the worktree operation failed")]
    Worktree(String),
    /// Bootty's own wording, shown as written.
    #[error("{0}")]
    Message(String),
    #[error("{summary}")]
    Technical { summary: String, details: String },
}

impl ErrorNotice {
    /// A failure of `action`, which names what failed: "Could not open the keymap". A cause the
    /// catalog recognizes follows it. The original text, which can hold provider output and
    /// paths, stays in the details.
    pub fn failed(action: &str, details: impl Into<String>) -> Self {
        let details = details.into().trim().to_owned();
        let summary = known_cause(&details).map_or_else(
            || format!("{action}."),
            |cause| format!("{action}. {cause}"),
        );
        Self::Technical { summary, details }
    }

    /// Return the original failure text for diagnostics and command responses.
    pub fn raw_message(&self) -> String {
        self.details()
            .map_or_else(|| self.to_string(), str::to_owned)
    }

    #[must_use]
    pub fn details(&self) -> Option<&str> {
        match self {
            Self::UnknownCommand(message)
            | Self::CommandHasNoAppExecutor(message)
            | Self::InvalidCommandArguments(message)
            | Self::CommandRequiresTarget(message)
            | Self::StaleCommandTarget(message)
            | Self::NoCurrentTarget(message)
            | Self::FileHandoffRejected(message)
            | Self::SessionNotHeldBySpace(message)
            | Self::SshProfileUnavailable(message)
            | Self::TerminalConfigPublicationFailed(message)
            | Self::Worktree(message) => Some(message),
            Self::Technical { summary, details } if summary != details => Some(details),
            _ => None,
        }
    }
}

/// What a person sees when a command can't even be queued.
#[must_use]
pub const fn command_send_message(error: bootty_control::AppCommandSendError) -> &'static str {
    match error {
        bootty_control::AppCommandSendError::Overloaded => "Bootty is busy. Try again in a moment.",
        bootty_control::AppCommandSendError::Shutdown => "Bootty is shutting down.",
    }
}

/// A cause people can act on, recognized in a failure's text.
fn known_cause(message: &str) -> Option<&'static str> {
    let lower = message.to_ascii_lowercase();
    if lower.contains("command deadline expired") {
        return Some("The command took too long. Try again.");
    }
    if lower.contains("ssh") {
        return Some("Could not reach the remote workspace.");
    }
    if lower.contains("connection") {
        return Some("The connection was lost. Try again.");
    }
    if lower.contains("target is stale") {
        return Some("That pane or session no longer exists.");
    }
    if lower.contains("no such file or directory") {
        return Some("It no longer exists.");
    }
    if lower.contains("permission denied") {
        return Some("Permission to it was denied.");
    }
    None
}
