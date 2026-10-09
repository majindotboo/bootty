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
    #[error("the session cleanup could not complete")]
    Ditch(String),
    #[error("the worktree was removed, but a branch remains")]
    DitchPartial(String),
    #[error("{summary}")]
    Technical { summary: String, details: String },
}

impl ErrorNotice {
    pub fn from_text(message: impl Into<String>) -> Self {
        let details = message.into().trim().to_owned();
        let summary = technical_summary(&details);
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
            | Self::Worktree(message)
            | Self::Ditch(message)
            | Self::DitchPartial(message) => Some(message),
            Self::Technical { summary, details } if summary != details => Some(details),
            _ => None,
        }
    }
}

fn technical_summary(message: &str) -> String {
    let message = bootty_agents::native_error_message(message);
    let lower = message.to_ascii_lowercase();
    if lower.contains("active writer") || lower.contains("open in another app") {
        return "This conversation is open in another app. Close it there, then retry.".into();
    }
    if lower.contains("archived") {
        return "This conversation is archived. Unarchive it to continue.".into();
    }
    if lower.contains("not logged in")
        || lower.contains("sign in")
        || lower.contains("unauthorized")
    {
        return "Sign in to the selected provider account, then retry.".into();
    }
    if lower.contains("api key") {
        return "Configure an OpenAI API key to use dictation and voice.".into();
    }
    if lower.contains("rate limit") || lower.contains("usage limit") {
        return "The provider's usage limit was reached. Retry when it resets.".into();
    }
    if lower.contains("microphone") || lower.contains("speech was detected") {
        return "Check microphone access and the input device, then try recording again.".into();
    }
    if lower.contains("command deadline expired") {
        return "The command took too long. Try again.".to_owned();
    }
    if lower.contains("rmux pane output ended: transportlost") {
        return "The terminal connection was lost. Try reopening this pane.".to_owned();
    }
    if lower.contains("ssh") || lower.contains("connection") {
        return "Could not reach the remote workspace.".to_owned();
    }
    if lower.contains("target is stale") {
        return "That pane or session no longer exists.".to_owned();
    }
    // Untrusted provider and transport diagnostics remain available through details().
    "Could not complete the operation. Try again.".into()
}
