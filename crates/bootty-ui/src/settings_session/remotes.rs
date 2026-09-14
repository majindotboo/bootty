use std::path::PathBuf;

use crate::settings_session::{RemoteOutcome, SettingsEffect};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DefaultRemote {
    pub host: String,
    pub user: String,
    pub port: String,
    pub program: String,
    pub args: Vec<String>,
    pub error: Option<String>,
    dirty: bool,
}

impl DefaultRemote {
    #[must_use]
    pub fn from_remote(remote: Option<&RemoteProfile>) -> Self {
        let Some(remote) = remote else {
            return Self {
                program: "ssh".to_owned(),
                ..Self::default()
            };
        };
        Self {
            host: remote.host.clone(),
            user: remote.user.clone().unwrap_or_default(),
            port: remote.port.map(|port| port.to_string()).unwrap_or_default(),
            program: remote.program.clone(),
            args: remote.args.clone(),
            error: None,
            dirty: false,
        }
    }

    fn validate(&self) -> Result<RemoteProfile, String> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Default remote needs a host name.".to_owned());
        }
        let port = nonempty(&self.port)
            .map(|port| {
                port.parse::<std::num::NonZeroU16>()
                    .map(std::num::NonZeroU16::get)
                    .map_err(|_| "Port must be between 1 and 65535.".to_owned())
            })
            .transpose()?;
        Ok(RemoteProfile {
            id: "default".to_owned(),
            name: "Default remote".to_owned(),
            host: host.to_owned(),
            user: nonempty(&self.user),
            port,
            authentication: "auto".to_owned(),
            host_key_policy: "strict".to_owned(),
            identity_file: None,
            proxy_jump: None,
            program: nonempty(&self.program).unwrap_or_else(|| "ssh".to_owned()),
            args: self.args.iter().filter_map(|arg| nonempty(arg)).collect(),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub authentication: String,
    pub host_key_policy: String,
    pub identity_file: Option<PathBuf>,
    pub proxy_jump: Option<String>,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteDraft {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    pub port: String,
    pub authentication: String,
    pub host_key_policy: String,
    pub identity_file: String,
    pub proxy_jump: String,
    pub program: String,
    pub args: Vec<String>,
    pub error: Option<String>,
}

impl RemoteDraft {
    #[must_use]
    pub fn from_profile(profile: &RemoteProfile) -> Self {
        Self {
            id: profile.id.clone(),
            name: profile.name.clone(),
            host: profile.host.clone(),
            user: profile.user.clone().unwrap_or_default(),
            port: profile
                .port
                .map(|port| port.to_string())
                .unwrap_or_default(),
            authentication: profile.authentication.clone(),
            host_key_policy: profile.host_key_policy.clone(),
            identity_file: profile
                .identity_file
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            proxy_jump: profile.proxy_jump.clone().unwrap_or_default(),
            program: profile.program.clone(),
            args: profile.args.clone(),
            error: None,
        }
    }

    /// Validate the remote draft and construct its saved profile.
    ///
    /// # Errors
    /// Rejects missing names or hosts, invalid ports, and authentication without a key file.
    pub fn validate(&self) -> Result<RemoteProfile, String> {
        let name = self.name.trim();
        let host = self.host.trim();
        if name.is_empty() || host.is_empty() {
            return Err("Profile name and host name are required.".to_owned());
        }
        let port = nonempty(&self.port)
            .map(|port| {
                port.parse::<std::num::NonZeroU16>()
                    .map(std::num::NonZeroU16::get)
                    .map_err(|_| "Port must be between 1 and 65535.".to_owned())
            })
            .transpose()?;
        let identity_file = nonempty(&self.identity_file).map(PathBuf::from);
        if self.authentication != "auto" && identity_file.is_none() {
            let required = if self.authentication == "agent" {
                "an SSH-agent public key file"
            } else {
                "a private key file"
            };
            return Err(format!("Choose {required}."));
        }
        Ok(RemoteProfile {
            id: self.id.clone(),
            name: name.to_owned(),
            host: host.to_owned(),
            user: nonempty(&self.user),
            port,
            authentication: self.authentication.clone(),
            host_key_policy: self.host_key_policy.clone(),
            identity_file,
            proxy_jump: nonempty(&self.proxy_jump),
            program: nonempty(&self.program).unwrap_or_else(|| "ssh".to_owned()),
            args: self.args.iter().filter_map(|arg| nonempty(arg)).collect(),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteEditorSnapshot {
    pub default: DefaultRemote,
    pub profiles: Vec<RemoteProfile>,
    pub selected: Option<String>,
    pub draft: Option<RemoteDraft>,
    pub testing: Option<u64>,
    pub message: Option<Result<(), String>>,
}

#[derive(Default)]
pub struct RemoteState {
    default: DefaultRemote,
    profiles: Vec<RemoteProfile>,
    selected: Option<String>,
    draft: Option<RemoteDraft>,
    next_request_id: u64,
    testing: Option<u64>,
    message: Option<Result<(), String>>,
}

impl RemoteState {
    pub(crate) fn set_default(&mut self, default: DefaultRemote) {
        if !self.default.dirty {
            self.default = default;
        }
    }

    pub(crate) fn edit_default(&mut self, field: &str, value: String) -> bool {
        match field {
            "host" => self.default.host = value,
            "user" => self.default.user = value,
            "port" => self.default.port = value,
            "program" => self.default.program = value,
            "args" => self.default.args = value.lines().map(str::to_owned).collect(),
            _ => return false,
        }
        self.default.error = None;
        self.default.dirty = true;
        true
    }

    pub(crate) fn edit_argument(&mut self, id: &str, index: usize, value: String) -> bool {
        if id == "default" {
            let Some(argument) = self.default.args.get_mut(index) else {
                return false;
            };
            *argument = value;
            self.default.error = None;
            self.default.dirty = true;
            return true;
        }
        let Some(draft) = self.draft.as_mut().filter(|draft| draft.id == id) else {
            return false;
        };
        let Some(argument) = draft.args.get_mut(index) else {
            return false;
        };
        *argument = value;
        draft.error = None;
        self.testing = None;
        self.message = None;
        true
    }

    pub(crate) fn add_argument(&mut self, id: &str) -> bool {
        if id == "default" {
            self.default.args.push(String::new());
            self.default.error = None;
            self.default.dirty = true;
            return true;
        }
        let Some(draft) = self.draft.as_mut().filter(|draft| draft.id == id) else {
            return false;
        };
        draft.args.push(String::new());
        draft.error = None;
        self.testing = None;
        self.message = None;
        true
    }

    pub(crate) fn remove_argument(&mut self, id: &str, index: usize) -> bool {
        if id == "default" {
            if index >= self.default.args.len() {
                return false;
            }
            self.default.args.remove(index);
            self.default.error = None;
            self.default.dirty = true;
            return true;
        }
        let Some(draft) = self.draft.as_mut().filter(|draft| draft.id == id) else {
            return false;
        };
        if index >= draft.args.len() {
            return false;
        }
        draft.args.remove(index);
        draft.error = None;
        self.testing = None;
        self.message = None;
        true
    }

    pub(crate) fn save_default(&mut self) -> Option<SettingsEffect> {
        match self.default.validate() {
            Ok(profile) => {
                self.default.error = None;
                self.default.dirty = false;
                Some(SettingsEffect::SetDefaultRemote(profile))
            }
            Err(error) => {
                self.default.error = Some(error);
                None
            }
        }
    }

    pub(crate) fn reject_default(&mut self, error: impl Into<String>) {
        self.default.error = Some(error.into());
    }

    pub(crate) fn clear_default(&mut self) -> SettingsEffect {
        self.default = DefaultRemote {
            program: "ssh".to_owned(),
            dirty: false,
            ..DefaultRemote::default()
        };
        SettingsEffect::ClearDefaultRemote
    }
    pub(crate) fn set_profiles(&mut self, profiles: Vec<RemoteProfile>) {
        self.profiles = profiles;
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.profiles.iter().any(|profile| &profile.id == id))
        {
            self.selected = None;
            self.draft = None;
            self.testing = None;
            self.message = None;
        }
    }

    pub(crate) fn select(&mut self, id: &str) -> bool {
        let Some(profile) = self.profiles.iter().find(|profile| profile.id == id) else {
            return false;
        };
        self.selected = Some(id.to_owned());
        self.draft = Some(RemoteDraft::from_profile(profile));
        self.testing = None;
        self.message = None;
        true
    }

    pub(crate) fn new_draft(&mut self, id: String) {
        self.testing = None;
        self.message = None;
        self.selected = None;
        self.draft = Some(RemoteDraft {
            id,
            name: "New Remote".to_owned(),
            authentication: "auto".to_owned(),
            host_key_policy: "strict".to_owned(),
            program: "ssh".to_owned(),
            ..RemoteDraft::default()
        });
    }

    pub(crate) fn set_draft(&mut self, draft: RemoteDraft) {
        self.testing = None;
        self.draft = Some(draft);
        self.message = None;
    }

    pub(crate) fn validated_draft(&mut self) -> Option<RemoteProfile> {
        let draft = self.draft.as_mut()?;
        match draft.validate() {
            Ok(profile) => {
                draft.error = None;
                Some(profile)
            }
            Err(error) => {
                draft.error = Some(error);
                None
            }
        }
    }

    pub(crate) fn test(&mut self) -> Option<SettingsEffect> {
        let profile = self.validated_draft()?;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        self.testing = Some(self.next_request_id);
        self.message = None;
        Some(SettingsEffect::TestRemote {
            request_id: self.next_request_id,
            profile,
        })
    }

    pub(crate) fn test_with_fields(
        &mut self,
        id: &str,
        fields: Vec<(String, String)>,
    ) -> Option<SettingsEffect> {
        let draft = self.draft.as_mut().filter(|draft| draft.id == id)?;
        for (field, value) in fields {
            match field.as_str() {
                "name" => draft.name = value,
                "host" => draft.host = value,
                "user" => draft.user = value,
                "port" => draft.port = value,
                "authentication" => draft.authentication = value,
                "host-key-policy" => draft.host_key_policy = value,
                "identity-file" => draft.identity_file = value,
                "proxy-jump" => draft.proxy_jump = value,
                "program" => draft.program = value,
                _ => {}
            }
        }
        self.test()
    }

    pub(crate) fn apply(&mut self, outcome: RemoteOutcome) {
        if self.testing != Some(outcome.request_id) {
            return;
        }
        self.testing = None;
        self.message = Some(outcome.result);
    }

    pub(crate) fn snapshot(&self) -> RemoteEditorSnapshot {
        RemoteEditorSnapshot {
            default: self.default.clone(),
            profiles: self.profiles.clone(),
            selected: self.selected.clone(),
            draft: self.draft.clone(),
            testing: self.testing,
            message: self.message.clone(),
        }
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
