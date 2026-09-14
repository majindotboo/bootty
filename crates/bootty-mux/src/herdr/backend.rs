use crate::{
    backend::MuxBackend,
    command::MuxCommand,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxSnapshot, MuxWindow},
};
#[cfg(feature = "terminal-runtime")]
use crate::{
    capability::BindingCapabilityDescriptor,
    controller::SpaceId,
    terminal::{
        AttachLaunch, BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest,
        ScopedMuxPaneTarget, TerminalRuntime, start_attach_terminal,
    },
};
use anyhow::{Context, Result, bail};
use bootty_host::remote::RemoteHost;
use bootty_host::{CommandOutput, CommandRunner, SystemCommandRunner, require_success};
use serde::Deserialize;
const HERDR_PROGRAM: &str = "herdr";
const DEFAULT_SESSION: &str = "default";

#[cfg(feature = "terminal-runtime")]
const HERDR_ATTACH_ENVIRONMENT: [&str; 7] = [
    "HERDR_ENV",
    "HERDR_SESSION",
    "HERDR_SOCKET_PATH",
    "HERDR_CLIENT_SOCKET_PATH",
    "HERDR_WORKSPACE_ID",
    "HERDR_TAB_ID",
    "HERDR_PANE_ID",
];

#[derive(Clone, Debug)]
pub struct HerdrBackend<R = SystemCommandRunner> {
    runner: R,
}

impl HerdrBackend<SystemCommandRunner> {
    #[must_use]
    pub const fn new() -> Self {
        Self::with_runner(SystemCommandRunner)
    }

    #[must_use]
    pub const fn for_remote(remote: RemoteHost) -> HerdrBackend<RemoteHerdrRunner> {
        HerdrBackend::with_runner(RemoteHerdrRunner::new(remote))
    }
}

impl Default for HerdrBackend<SystemCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> HerdrBackend<R> {
    pub const fn with_runner(runner: R) -> Self {
        Self { runner }
    }

    pub const fn runner(&self) -> &R {
        &self.runner
    }
}

impl<R: CommandRunner> HerdrBackend<R> {
    /// # Errors
    /// Returns command or response parsing errors while reading opaque attachments.
    pub fn snapshot(&self) -> Result<MuxSnapshot> {
        let args = ["session", "list", "--json"].map(str::to_owned);
        let output = self.runner.run(HERDR_PROGRAM, &args)?;
        let stdout = require_success(HERDR_PROGRAM, &args, output)
            .context("list Herdr sessions (requires Herdr 0.8 or newer)")?;
        parse_session_list(&stdout)
    }
    /// # Errors
    /// Returns unsupported command, invalid target, or backend execution errors.
    pub fn execute(&mut self, command: &MuxCommand) -> Result<()> {
        bail!(
            "Herdr owns its UI and topology; Bootty does not support {command:?} through this backend"
        )
    }
}

#[derive(Deserialize)]
struct HerdrSessionList {
    #[serde(default)]
    sessions: Vec<HerdrSessionRow>,
}

#[derive(Deserialize)]
struct HerdrSessionRow {
    name: String,
    #[serde(default)]
    default: bool,
}

fn parse_session_list(stdout: &str) -> Result<MuxSnapshot> {
    let listed: HerdrSessionList =
        serde_json::from_str(stdout).context("decode `herdr session list --json`")?;
    let mut sessions = listed.sessions;

    // Attaching a named Herdr session starts it when needed. Keep a fresh backend attachable even
    // before Herdr has written its first session record.
    if sessions.is_empty() {
        sessions.push(HerdrSessionRow {
            name: DEFAULT_SESSION.to_owned(),
            default: true,
        });
    }

    let active_index = sessions
        .iter()
        .position(|session| session.default)
        .unwrap_or(0);
    let sessions = sessions
        .into_iter()
        .enumerate()
        .map(|(index, session)| synthetic_session(session.name, index == active_index))
        .collect::<Vec<_>>();
    Ok(MuxSnapshot {
        active_session_id: sessions
            .iter()
            .find(|session| session.active)
            .map(|session| session.id.clone()),
        sessions,
        ..MuxSnapshot::default()
    })
}

impl<R: CommandRunner> MuxBackend for HerdrBackend<R> {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Self::snapshot(self)
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        Self::execute(self, &command)
    }
}

fn synthetic_session(name: String, active: bool) -> MuxSession {
    let window_id = format!("herdr:{name}");
    let anchor = MuxPaneAnchor {
        session_id: name.clone(),
        pane_id: None,
        pane_pid: None,
        cwd: None,
        process: Some(HERDR_PROGRAM.to_owned()),
    };
    MuxSession {
        id: name.clone(),
        name: name.clone(),
        active,
        anchor: anchor.clone(),
        active_window_id: Some(window_id.clone()),
        windows: vec![MuxWindow {
            id: window_id,
            index: 0,
            name,
            active: true,
            anchor: anchor.clone(),
            panes: vec![anchor],
            layout: None,
            progress: None,
        }],
        tag: MuxSessionTag::default(),
    }
}

#[derive(Clone, Debug)]
pub struct RemoteHerdrRunner {
    remote: RemoteHost,
}

impl RemoteHerdrRunner {
    #[must_use]
    pub const fn new(remote: RemoteHost) -> Self {
        Self { remote }
    }
}

impl CommandRunner for RemoteHerdrRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let (program, args) = self.remote.command(program, args);
        SystemCommandRunner.run(&program, &args)
    }
}

#[cfg(feature = "terminal-runtime")]
#[must_use]
pub fn herdr_capabilities(scope: SpaceId) -> BindingCapabilityDescriptor {
    BindingCapabilityDescriptor::new(scope, [])
}

#[cfg(feature = "terminal-runtime")]
pub struct HerdrPanePolicy {
    remote: Option<RemoteHost>,
}

#[cfg(feature = "terminal-runtime")]
impl HerdrPanePolicy {
    #[must_use]
    pub const fn new(remote: Option<RemoteHost>) -> Self {
        Self { remote }
    }
    /// # Errors
    /// Returns an error if the requested session cannot be resolved for attachment.
    pub fn attach_launch(&self, session: &str) -> Result<AttachLaunch> {
        let args = vec!["--session".to_owned(), session.to_owned()];
        let (program, args, remote) = match &self.remote {
            Some(remote) => {
                let Some(ssh) = remote.as_ssh() else {
                    bail!("Herdr public-client attachment does not support WSL");
                };
                let target = ssh.target();
                if target.program != "ssh" || target.port.is_some() || !target.args.is_empty() {
                    bail!(
                        "Herdr remote UI supports host and user here; put custom SSH options in ~/.ssh/config"
                    )
                }
                (
                    HERDR_PROGRAM.to_owned(),
                    vec![
                        "--remote".to_owned(),
                        remote.destination(),
                        "--session".to_owned(),
                        session.to_owned(),
                    ],
                    true,
                )
            }
            None => (HERDR_PROGRAM.to_owned(), args, false),
        };
        Ok(AttachLaunch {
            program,
            args,
            env_remove: HERDR_ATTACH_ENVIRONMENT.map(str::to_owned).to_vec(),
            env: Vec::new(),
            // Herdr enables its stock direct Kitty relay for known compatible terminal hosts.
            // Remove this override once Herdr recognizes Bootty itself.
            term_program: (!remote).then(|| "Ghostty".to_owned()),
            remote,
        })
    }
}

#[cfg(feature = "terminal-runtime")]
impl BackendPanePolicy for HerdrPanePolicy {
    fn remote_target(&self) -> Option<crate::RemoteTarget> {
        self.remote.as_ref().map(RemoteHost::target)
    }

    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        let launch = self.attach_launch(request.target.session_id())?;
        start_attach_terminal(request, launch).map(Some)
    }

    fn sync_target(&mut self, _target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {}

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(&mut self, _request: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }

    fn deactivate(&mut self) {}
}
