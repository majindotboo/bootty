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
    #[error("mux operation is unsupported")]
    MuxOperationUnsupported,
    #[error("mux operation is unavailable")]
    MuxOperationUnavailable,
    #[error("mux operation capability is stale")]
    MuxOperationCapabilityStale,
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
    #[error("remote Space name cannot be empty")]
    RemoteSpaceNameEmpty,
    #[error("remote Space is unavailable")]
    RemoteSpaceUnavailable(String),
    #[error("session does not belong to the remote Space")]
    SessionDoesNotBelongToRemoteSpace(String),
    #[error("this Space does not hold the session")]
    SessionNotHeldBySpace(String),
    #[error("remote Spaces need tmux or rmux")]
    RemoteSpaceBackendUnsupported,
    #[error("remote Space points to another SSH host")]
    RemoteSpacePointsToAnotherHost(String),
    #[error(
        "Remote Space now uses {actual} instead of {expected}. Edit this Space and select it again."
    )]
    RemoteSpaceBackendChanged { actual: String, expected: String },
    #[error("remote Space catalog version is not supported")]
    RemoteSpaceCatalogVersionUnsupported(String),
    #[error("remote project task stopped")]
    RemoteProjectTaskStopped,
    #[error("the previous remote Space operation is still stopping")]
    RemoteSpaceOperationStopping,
    #[error("remote Space task stopped")]
    RemoteSpaceTaskStopped,
    #[error("binding unavailable; reconnect to restore it")]
    BindingUnavailable,
    #[error("SSH profile is unavailable")]
    SshProfileUnavailable(String),
    #[error("persisted Space has no backend binding")]
    PersistedSpaceHasNoBackendBinding,
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
            | Self::RemoteSpaceUnavailable(message)
            | Self::SessionDoesNotBelongToRemoteSpace(message)
            | Self::SessionNotHeldBySpace(message)
            | Self::RemoteSpacePointsToAnotherHost(message)
            | Self::RemoteSpaceCatalogVersionUnsupported(message)
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

/// Whether a failure means the provider account needs signing in, which a Sign in action fixes.
#[must_use]
pub fn needs_sign_in(message: &str) -> bool {
    let lower = bootty_agents::native_error_message(message).to_ascii_lowercase();
    lower.contains("not logged in") || lower.contains("sign in") || lower.contains("unauthorized")
}

/// A cause people can act on, recognized in a failure's text.
fn known_cause(message: &str) -> Option<&'static str> {
    let message = bootty_agents::native_error_message(message);
    let lower = message.to_ascii_lowercase();
    if lower.contains("active writer") || lower.contains("open in another app") {
        return Some("This conversation is open in another app. Close it there, then retry.");
    }
    // Only an archived conversation has an unarchive action; other "archived" text doesn't.
    if lower.contains("is archived")
        && ["session", "conversation", "thread"]
            .iter()
            .any(|subject| lower.contains(subject))
    {
        return Some("This conversation is archived. Unarchive it to continue.");
    }
    if needs_sign_in(&message) {
        return Some("Sign in to the selected provider account, then retry.");
    }
    if lower.contains("api key") {
        return Some("Add the missing API key in Settings, then retry.");
    }
    if lower.contains("rate limit") || lower.contains("usage limit") {
        return Some("The provider's usage limit was reached. Retry when it resets.");
    }
    if lower.contains("current voice call") {
        return Some("Wait for the current voice call to end before starting another.");
    }
    if lower.contains("voice connection failed") {
        return Some("Voice connection failed. Check your network and try again.");
    }
    if lower.contains("voice connection was lost") {
        return Some("Voice connection was lost. Check your network and try again.");
    }
    if lower.contains("microphone") || lower.contains("speech was detected") {
        return Some("Check microphone access and the input device, then try recording again.");
    }
    if lower.contains("command deadline expired") {
        return Some("The command took too long. Try again.");
    }
    if lower.contains("rmux pane output ended: transportlost") {
        return Some("The terminal connection was lost. Try reopening this pane.");
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
    if lower.contains("attachments can be at most 512 mib") {
        return Some("Attachments can be at most 512 MiB.");
    }
    if lower.contains("images can be at most 50 mib") {
        return Some("Images can be at most 50 MiB.");
    }
    None
}
