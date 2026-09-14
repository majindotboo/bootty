//! Application effects for the renderer-neutral settings session.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::gpui::{ModuleIntegrationSnapshot, ModuleIntegrationStatus, ModuleIntegrationsSnapshot};
use crate::settings_session::{
    ModuleOutcome, RemoteOutcome, RemoteProfile, SettingsEffect, SettingsOutcome,
};
use bootty_agents::{
    AgentIntegration, IntegrationStatus, agent_integrations, install_integration,
    integration_status, uninstall_integration,
};
use bootty_config::config::{
    SshAuthenticationConfig, SshHostKeyPolicyConfig, SshProfileConfig, SshRemoteConfig,
};
use bootty_mux::RepaintHandle;

use crate::{
    AppEffect,
    gpui_settings_catalog::{UnsupportedModuleDiagnostic, scan_unsupported_module_sources},
    state::AppState,
};

#[derive(Clone, Debug, Default)]
pub struct NativeSettingsCatalog {
    pub(crate) unsupported_sources: Vec<UnsupportedModuleDiagnostic>,
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
    integration_lock: Arc<Mutex<()>>,
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
            integration_lock: Arc::new(Mutex::new(())),
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
            SettingsEffect::SubmitDocument(document) => commit_document(state, document),
            SettingsEffect::InstallIntegration {
                identity,
                module,
                id,
            } => {
                self.request_integration(
                    state.config().config_path.clone(),
                    identity,
                    module,
                    id,
                    true,
                    repaint,
                );
                (Vec::new(), Vec::new())
            }
            SettingsEffect::UninstallIntegration {
                identity,
                module,
                id,
            } => {
                self.request_integration(
                    state.config().config_path.clone(),
                    identity,
                    module,
                    id,
                    false,
                    repaint,
                );
                (Vec::new(), Vec::new())
            }
            SettingsEffect::UpsertRemote(profile) => {
                let mut document = state.config_document();
                let id = profile.id.clone();
                let result = remote_config(&profile).and_then(|profile| {
                    document
                        .set_ssh_profile(&id, &profile)
                        .map_err(|error| error.to_string())
                });
                match result {
                    Ok(()) => commit_document(state, document),
                    Err(error) => (vec![SettingsOutcome::DocumentRejected(error)], Vec::new()),
                }
            }
            SettingsEffect::SetDefaultRemote(profile) => {
                let mut document = state.config_document();
                let remote = SshRemoteConfig {
                    host: profile.host,
                    user: profile.user,
                    port: profile.port,
                    program: profile.program,
                    args: profile.args,
                };
                match document.set_multiplexer_remote(&remote) {
                    Ok(()) => commit_document(state, document),
                    Err(error) => (
                        vec![SettingsOutcome::DocumentRejected(error.to_string())],
                        Vec::new(),
                    ),
                }
            }
            SettingsEffect::ClearDefaultRemote => {
                let mut document = state.config_document();
                match document.remove_multiplexer_remote() {
                    Ok(()) => commit_document(state, document),
                    Err(error) => (
                        vec![SettingsOutcome::DocumentRejected(error.to_string())],
                        Vec::new(),
                    ),
                }
            }
            SettingsEffect::RemoveRemote { id } => {
                let mut document = state.config_document();
                match document.remove_ssh_profile(&id) {
                    Ok(()) => commit_document(state, document),
                    Err(error) => (
                        vec![SettingsOutcome::DocumentRejected(error.to_string())],
                        Vec::new(),
                    ),
                }
            }
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
            let _ = sender.try_send(SettingsOutcome::Remote(RemoteOutcome {
                request_id,
                result,
            }));
            repaint();
        });
    }

    /// Refresh native settings facts away from the UI thread.
    pub(crate) fn request_catalog(&self, config_path: &Path, repaint: &RepaintHandle) {
        let generation = self
            .catalog_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let config_path = config_path.to_owned();
        let sender = self.catalog_results_tx.clone();
        let repaint = Arc::clone(repaint);
        std::thread::spawn(move || {
            let catalog = load_native_settings_catalog(&config_path);
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

    fn request_integration(
        &self,
        config_path: PathBuf,
        identity: String,
        module: String,
        id: String,
        install: bool,
        repaint: &RepaintHandle,
    ) {
        let outcome_sender = self.remote_results_tx.clone();
        let catalog_sender = self.catalog_results_tx.clone();
        let integration_lock = Arc::clone(&self.integration_lock);
        let generation = self
            .catalog_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let repaint = Arc::clone(repaint);
        std::thread::spawn(move || {
            let _guard = integration_lock.lock().ok();
            let config_dir = config_path
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            let integration_dir = config_dir.join("integrations");
            let home = bootty_git::home_dir();
            let declaration = agent_integrations(&integration_dir)
                .into_iter()
                .find(|integration| {
                    integration.provider.module() == identity
                        && integration.declaration.module == module
                        && integration.declaration.id == id
                })
                .map(|integration| integration.declaration);
            let result = declaration.map_or_else(
                || {
                    Err(format!(
                        "no native integration `{id}` declared by `{module}`"
                    ))
                },
                |declaration| {
                    if install {
                        install_integration(&integration_dir, home.as_deref(), &declaration)
                    } else {
                        uninstall_integration(&integration_dir, home.as_deref(), &declaration)
                    }
                },
            );
            let outcome = SettingsOutcome::Module(match result {
                Ok(()) => ModuleOutcome::IntegrationUpdated { identity },
                Err(message) => ModuleOutcome::Failed {
                    identity: Some(identity),
                    message,
                },
            });
            let _ = outcome_sender.send_blocking(outcome);
            let _ = catalog_sender.send_blocking(CatalogResult {
                generation,
                catalog: load_native_settings_catalog(&config_path),
            });
            repaint();
        });
    }
}

fn commit_document(
    state: &mut AppState,
    document: bootty_config::config::ConfigDocument,
) -> (Vec<SettingsOutcome>, Vec<AppEffect>) {
    match state.commit_settings_document(document) {
        Ok((document, warning, effects)) => (
            vec![SettingsOutcome::DocumentAccepted {
                revision: state.config_revision(),
                document,
                warning,
            }],
            effects,
        ),
        Err(error) => (
            vec![SettingsOutcome::DocumentRejected(error.to_string())],
            Vec::new(),
        ),
    }
}

fn load_native_settings_catalog(config_path: &Path) -> NativeSettingsCatalog {
    let config_dir = config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let unsupported_sources =
        scan_unsupported_module_sources(&config_dir).unwrap_or_else(|error| {
            vec![UnsupportedModuleDiagnostic {
                path: config_dir.clone(),
                detail: format!("scan failed: {error}"),
            }]
        });
    let integration_rows = native_integration_rows(&config_dir);
    NativeSettingsCatalog {
        unsupported_sources,
        integration_rows,
    }
}

fn native_integration_rows(config_dir: &Path) -> Vec<ModuleIntegrationsSnapshot> {
    let integration_dir = config_dir.join("integrations");
    let home = bootty_git::home_dir();
    agent_integrations(&integration_dir)
        .into_iter()
        .map(|integration| {
            let AgentIntegration {
                provider,
                declaration,
            } = integration;
            let status = match integration_status(&integration_dir, home.as_deref(), &declaration) {
                IntegrationStatus::Missing => ModuleIntegrationStatus::Missing,
                IntegrationStatus::Partial => ModuleIntegrationStatus::Partial,
                IntegrationStatus::Installed => ModuleIntegrationStatus::Installed,
            };
            ModuleIntegrationsSnapshot {
                identity: provider.module().to_owned(),
                error: None,
                integrations: vec![ModuleIntegrationSnapshot {
                    module: declaration.module,
                    id: declaration.id,
                    title: declaration.title,
                    summary: declaration.summary,
                    status,
                }],
            }
        })
        .collect()
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
