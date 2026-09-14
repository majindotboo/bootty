use bootty_config::config::ConfigDocument;

use crate::settings_session::RemoteProfile;

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

/// Outcome supplied by an owner after handling a [`SettingsEffect`].
#[derive(Clone, Debug)]
pub enum SettingsOutcome {
    DocumentAccepted {
        revision: u64,
        document: ConfigDocument,
        warning: Option<String>,
    },
    DocumentRejected(String),
    Module(ModuleOutcome),
    Remote(RemoteOutcome),
}
