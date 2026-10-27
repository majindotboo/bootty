use anyhow::{Context as _, Result};
use bootty_agents::{TerminalProviderTransport, TerminalQueryOutput};
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    capability::BindingCapabilityDescriptor,
    controller::SpaceId,
    native::NativeProvider,
    provider::{
        MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider, MuxBackendRegistry,
        MuxCommandDispatch,
    },
    terminal::BackendPanePolicy,
};
use serde_json::json;
use std::{
    collections::HashMap,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
};

#[derive(Clone)]
pub struct Fixture {
    root: PathBuf,
    live_program: Option<PathBuf>,
    sessions: Arc<(Mutex<HashMap<PathBuf, String>>, Condvar)>,
}

impl Fixture {
    pub fn new(root: &Path, live_program: Option<PathBuf>) -> Self {
        Self {
            root: root.to_owned(),
            live_program,
            sessions: Arc::new((Mutex::default(), Condvar::new())),
        }
    }

    pub fn backends(&self) -> Result<Arc<MuxBackendRegistry>> {
        Ok(Arc::new(MuxBackendRegistry::from_app_providers(
            [Arc::new(self.clone())],
            [MuxBackendKind::Native],
        )?))
    }

    pub fn transport(&self) -> TerminalProviderTransport {
        let mut transport = super::provider_queries::terminal_transport(None);
        let fixture = self.clone();
        transport.output = Arc::new(move |command, cancelled| {
            if cancelled() {
                return Err("provider query cancelled".into());
            }
            let args = command.get_args().collect::<Vec<_>>();
            if !(args == ["agents", "--json"] || args == ["agents", "--json", "--all"]) {
                return Err("unexpected provider query".into());
            }
            let account = command
                .get_envs()
                .find_map(|(key, value)| (key == "CLAUDE_CONFIG_DIR").then_some(value).flatten())
                .ok_or("missing captured account")?;
            fixture
                .query(&account.to_string_lossy())
                .map_err(|error| error.to_string())
        });
        transport
    }

    fn query(&self, account: &str) -> Result<TerminalQueryOutput> {
        let account = std::fs::canonicalize(account)?;
        let (sessions, wake) = &*self.sessions;
        let (observed, _) = wake
            .wait_timeout_while(
                sessions
                    .lock()
                    .map_err(|error| anyhow::anyhow!("{error}"))?,
                std::time::Duration::from_secs(30),
                |sessions| !sessions.contains_key(&account),
            )
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let id = observed
            .get(&account)
            .cloned()
            .context("provider has not started")?;
        drop(observed);
        let state = std::fs::read_to_string(account.join("state"))?;
        let detail = account.join("detail");
        let detail = if detail.exists() {
            Some(std::fs::read_to_string(detail)?)
        } else {
            None
        };
        Ok(TerminalQueryOutput {
            stdout: serde_json::to_vec(
                &json!([{"sessionId":id,"state":state,"waitingFor":detail}]),
            )?,
            stderr: Vec::new(),
            exit_code: Some(0),
            successful: true,
        })
    }

    fn start(&self, argv: &[String]) -> Result<()> {
        let Some(id) = argv
            .windows(2)
            .find_map(|pair| (pair[0] == "--session-id").then_some(&pair[1]))
        else {
            anyhow::ensure!(
                argv.is_empty(),
                "fixture command omitted its session selector"
            );
            return Ok(());
        };
        let account = argv
            .iter()
            .find_map(|arg| arg.strip_prefix("CLAUDE_CONFIG_DIR="))
            .context("missing account in captured argv")?;
        anyhow::ensure!(
            Path::new(account).starts_with(&self.root),
            "account escaped fixture"
        );
        let name = Path::new(account)
            .file_name()
            .context("account name")?
            .to_string_lossy();
        let mut order = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("order"))?;
        writeln!(order, "{name}")?;
        let (sessions, wake) = &*self.sessions;
        sessions
            .lock()
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .insert(std::fs::canonicalize(account)?, id.clone());
        wake.notify_all();
        Ok(())
    }
}

impl MuxBackendProvider for Fixture {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Native
    }
    fn command_dispatch(&self) -> MuxCommandDispatch {
        NativeProvider.command_dispatch()
    }
    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        NativeProvider.build_backend(config, workspace)
    }
}

impl MuxAppBackendProvider for Fixture {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        NativeProvider.app_policy()
    }
    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        NativeProvider.capabilities(scope)
    }
    fn build_pane_policy(&self, _config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(PanePolicy(self.clone()))
    }
}

struct PanePolicy(Fixture);
impl BackendPanePolicy for PanePolicy {
    fn remote_target(&self) -> Option<bootty_mux::RemoteTarget> {
        None
    }
    fn start_terminal(
        &mut self,
        request: bootty_mux::terminal::PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn bootty_mux::terminal::TerminalRuntime>>> {
        let command = &request.terminal_config.launch.command;
        let program = if command.first().is_some_and(|program| program == "env") {
            command
                .iter()
                .skip(1)
                .find(|argument| !argument.contains('='))
        } else {
            command.first()
        };
        if self
            .0
            .live_program
            .as_ref()
            .is_some_and(|expected| program.is_some_and(|program| Path::new(program) == expected))
        {
            return bootty_mux::native::NativePanePolicy.start_terminal(request);
        }
        self.0.start(&request.terminal_config.launch.command)?;
        Ok(Some(Box::new(
            super::naming_backend::placeholder_terminal::PlaceholderTerminal::new(
                request.spawn_geometry,
                request.target.cwd().map(str::to_owned),
            ),
        )))
    }
    fn sync_target(
        &mut self,
        _target: Option<&bootty_mux::terminal::ScopedMuxPaneTarget>,
        _hide_tmux_status: bool,
    ) {
    }
    fn set_layout_window(&mut self, _window_id: Option<&str>) {}
    fn resize_layout_window(
        &mut self,
        _request: bootty_mux::terminal::PaneLayoutResizeRequest<'_>,
    ) -> Result<bool> {
        Ok(false)
    }
    fn deactivate(&mut self) {}
}
