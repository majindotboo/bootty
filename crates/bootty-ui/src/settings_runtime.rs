//! Application effects for the renderer-neutral settings session.

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::gpui::ModuleIntegrationsSnapshot;
use crate::settings_session::{
    AcceptedSettings, ModuleOutcome, RemoteOutcome, RemoteProfile, SettingsEffect, SettingsOutcome,
    SettingsWriteSource,
};
use bootty_config::config::{
    ConfigDocument, SshAuthenticationConfig, SshHostKeyPolicyConfig, SshProfileConfig,
    SshRemoteConfig,
};
use bootty_mux::RepaintHandle;

use crate::{AppEffect, state::AppState};

#[derive(Clone, Debug, Default)]
pub struct NativeSettingsCatalog {
    pub(crate) integration_rows: Vec<ModuleIntegrationsSnapshot>,
}

struct CatalogResult {
    generation: u64,
    catalog: NativeSettingsCatalog,
}

pub struct SettingsRuntime {
    remote_results_tx: async_channel::Sender<SettingsOutcome>,
    remote_results_rx: async_channel::Receiver<SettingsOutcome>,
    catalog_results_tx: async_channel::Sender<CatalogResult>,
    catalog_results_rx: async_channel::Receiver<CatalogResult>,
    catalog: Mutex<NativeSettingsCatalog>,
    catalog_generation: AtomicU64,
    published_catalog_generation: AtomicU64,
}

impl Default for SettingsRuntime {
    fn default() -> Self {
        let (remote_results_tx, remote_results_rx) = async_channel::bounded(8);
        let (catalog_results_tx, catalog_results_rx) = async_channel::bounded(4);
        Self {
            remote_results_tx,
            remote_results_rx,
            catalog_results_tx,
            catalog_results_rx,
            catalog: Mutex::new(NativeSettingsCatalog::default()),
            catalog_generation: AtomicU64::new(0),
            published_catalog_generation: AtomicU64::new(0),
        }
    }
}

impl SettingsRuntime {
    pub(crate) fn apply(
        &self,
        effect: SettingsEffect,
        state: &mut AppState,
        repaint: &RepaintHandle,
    ) -> (Vec<SettingsOutcome>, Vec<AppEffect>) {
        match effect {
            SettingsEffect::SubmitDocument(document) => {
                commit_document(state, document, SettingsWriteSource::Document)
            }
            SettingsEffect::InstallIntegration { identity, .. }
            | SettingsEffect::UninstallIntegration { identity, .. } => (
                vec![SettingsOutcome::Module(ModuleOutcome::Failed {
                    identity: Some(identity),
                    message: "Legacy agent adapters are unsupported. Open an agent terminal; no hook installation is required.".to_owned(),
                })],
                Vec::new(),
            ),
            SettingsEffect::UpsertRemote(profile) => update_remote_document(
                state,
                SettingsWriteSource::RemoteProfile(profile.id.clone()),
                |document| {
                    let config = remote_config(&profile)?;
                    document
                        .set_ssh_profile(&profile.id, &config)
                        .map_err(|error| error.to_string())
                },
            ),
            SettingsEffect::SetDefaultRemote(profile) => {
                let remote = SshRemoteConfig {
                    host: profile.host,
                    user: profile.user,
                    port: profile.port,
                    program: profile.program,
                    args: profile.args,
                };
                update_remote_document(state, SettingsWriteSource::DefaultRemote, |document| {
                    document
                        .set_multiplexer_remote(&remote)
                        .map_err(|error| error.to_string())
                })
            }
            SettingsEffect::ClearDefaultRemote => {
                update_remote_document(state, SettingsWriteSource::DefaultRemote, |document| {
                    document
                        .remove_multiplexer_remote()
                        .map_err(|error| error.to_string())
                })
            }
            SettingsEffect::RemoveRemote { id } => update_remote_document(
                state,
                SettingsWriteSource::RemoteProfile(id.clone()),
                |document| {
                    document
                        .remove_ssh_profile(&id)
                        .map_err(|error| error.to_string())
                },
            ),
            SettingsEffect::TestRemote {
                request_id,
                profile,
            } => {
                self.test_remote(request_id, profile, repaint);
                (Vec::new(), Vec::new())
            }
        }
    }

    fn test_remote(&self, request_id: u64, profile: RemoteProfile, repaint: &RepaintHandle) {
        let sender = self.remote_results_tx.clone();
        let repaint = Arc::clone(repaint);
        std::thread::spawn(move || {
            let result = remote_config(&profile).and_then(|profile| {
                bootty_mux::remote_space::list_remote(&profile)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            });
            let _ = sender.send_blocking(SettingsOutcome::Remote(RemoteOutcome {
                request_id,
                result,
            }));
            repaint();
        });
    }

    /// Refresh native settings facts away from the UI thread.
    pub(crate) fn request_catalog(&self, _config_path: &Path, repaint: &RepaintHandle) {
        let generation = self
            .catalog_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let sender = self.catalog_results_tx.clone();
        let repaint = Arc::clone(repaint);
        std::thread::spawn(move || {
            let catalog = NativeSettingsCatalog::default();
            let _ = sender.send_blocking(CatalogResult {
                generation,
                catalog,
            });
            repaint();
        });
    }

    pub(crate) fn drain_outcomes(&self) -> Vec<SettingsOutcome> {
        std::iter::from_fn(|| self.remote_results_rx.try_recv().ok()).collect()
    }

    /// Publish the newest completed catalog, rejecting results from older requests.
    pub(crate) fn drain_catalog(&self) -> Option<NativeSettingsCatalog> {
        let mut newest = None;
        while let Ok(result) = self.catalog_results_rx.try_recv() {
            if newest
                .as_ref()
                .is_none_or(|current: &CatalogResult| result.generation > current.generation)
            {
                newest = Some(result);
            }
        }
        let result = newest?;
        if result.generation <= self.published_catalog_generation.load(Ordering::Acquire) {
            return None;
        }
        if let Ok(mut catalog) = self.catalog.lock() {
            *catalog = result.catalog.clone();
        }
        self.published_catalog_generation
            .store(result.generation, Ordering::Release);
        Some(result.catalog)
    }

    /// Return the last completed native catalog without touching the filesystem.
    pub(crate) fn current_catalog(&self) -> NativeSettingsCatalog {
        self.catalog.lock().map_or_else(
            |_| NativeSettingsCatalog::default(),
            |catalog| catalog.clone(),
        )
    }
}

impl AppState {
    pub(crate) fn accepted_settings(&self) -> AcceptedSettings {
        AcceptedSettings {
            revision: self.config_revision(),
            config: Arc::new(self.config().clone()),
            document: self.config_document(),
            schema: self.settings_schema(),
        }
    }
}

fn update_remote_document(
    state: &mut AppState,
    source: SettingsWriteSource,
    update: impl FnOnce(&mut ConfigDocument) -> Result<(), String>,
) -> (Vec<SettingsOutcome>, Vec<AppEffect>) {
    let mut document = state.config_document();
    match update(&mut document) {
        Ok(()) => commit_document(state, document, source),
        Err(error) => (
            vec![SettingsOutcome::DocumentRejected { source, error }],
            Vec::new(),
        ),
    }
}

fn commit_document(
    state: &mut AppState,
    document: ConfigDocument,
    source: SettingsWriteSource,
) -> (Vec<SettingsOutcome>, Vec<AppEffect>) {
    match state.commit_settings_document(document) {
        Ok((_, warning, effects)) => (
            vec![SettingsOutcome::DocumentAccepted {
                source,
                accepted: Box::new(state.accepted_settings()),
                warning,
            }],
            effects,
        ),
        Err(error) => (
            vec![SettingsOutcome::DocumentRejected {
                source,
                error: error.to_string(),
            }],
            Vec::new(),
        ),
    }
}

fn remote_config(profile: &RemoteProfile) -> Result<SshProfileConfig, String> {
    let authentication = match profile.authentication.as_str() {
        "auto" => SshAuthenticationConfig::Auto,
        "agent" => SshAuthenticationConfig::Agent,
        "key-file" => SshAuthenticationConfig::KeyFile,
        value => return Err(format!("unknown SSH authentication {value:?}")),
    };
    let host_key_policy = match profile.host_key_policy.as_str() {
        "strict" => SshHostKeyPolicyConfig::Strict,
        "accept-new" => SshHostKeyPolicyConfig::AcceptNew,
        value => return Err(format!("unknown SSH host-key policy {value:?}")),
    };
    Ok(SshProfileConfig {
        name: profile.name.clone(),
        host: profile.host.clone(),
        user: profile.user.clone(),
        port: profile.port,
        authentication,
        host_key_policy,
        identity_file: profile.identity_file.clone(),
        proxy_jump: profile.proxy_jump.clone(),
        program: profile.program.clone(),
        args: profile.args.clone(),
    })
}
