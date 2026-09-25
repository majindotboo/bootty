use bootty_config::config::ConfigDocument;

use crate::settings_session::{AcceptedSettings, RemoteProfile};

/// Work the application owner must perform outside the settings session.
#[derive(Clone, Debug)]
pub enum SettingsEffect {
    SubmitDocument(ConfigDocument),
    InstallIntegration {
        identity: String,
        module: String,
        id: String,
    },
    UninstallIntegration {
        identity: String,
        module: String,
        id: String,
    },
    UpsertRemote(RemoteProfile),
    SetDefaultRemote(RemoteProfile),
    ClearDefaultRemote,
    RemoveRemote {
        id: String,
    },
    TestRemote {
        request_id: u64,
        profile: RemoteProfile,
    },
}

/// Result of native integration work performed by the application owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleOutcome {
    IntegrationUpdated {
        identity: String,
    },
    Failed {
        identity: Option<String>,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteOutcome {
    pub request_id: u64,
    pub result: Result<(), String>,
}

/// The editor whose draft was submitted by a config write.
/// Writes finish synchronously; asynchronous writes would also need a draft revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsWriteSource {
    Document,
    DefaultRemote,
    RemoteProfile(String),
}

/// Outcome supplied by an owner after handling a [`SettingsEffect`].
#[derive(Clone, Debug)]
pub enum SettingsOutcome {
    DocumentAccepted {
        source: SettingsWriteSource,
        accepted: Box<AcceptedSettings>,
        warning: Option<String>,
    },
    DocumentRejected {
        source: SettingsWriteSource,
        error: String,
    },
    Module(ModuleOutcome),
    Remote(RemoteOutcome),
}
