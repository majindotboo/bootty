#![cfg(test)]
#![cfg(unix)]

//! A catalog-backed remote Space driven through the real client transport: `RemoteSpaceBackend`
//! runs the versioned daemon through `ssh`, here a fake one that runs the remote command line on
//! this machine, against a private tmux server.

use std::{
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bootty_config::config::SshRemoteConfig;
use bootty_host::{remote::RemoteHost, shell_quote, ssh::SshRemote};
use bootty_mux::{
    MuxBackendKind,
    backend::{MuxBackend, PaneCapture, PaneInput},
    command::MuxCommand,
    remote_space::RemoteSpaceBackend,
    snapshot::{MuxSessionTag, new_session_identity},
};
use pretty_assertions::assert_eq;

const TIMEOUT: Duration = Duration::from_secs(10);

struct FakeRemote {
    root: assert_fs::TempDir,
}

impl FakeRemote {
    fn new() -> Result<Self> {
        // Unix sockets have a short path limit, and tmux keeps its socket under TMUX_TMPDIR.
        let root = assert_fs::TempDir::new_in("/tmp")?;
        let remote = Self { root };
        let bin = remote.home().join(".bootty/bin");
        std::fs::create_dir_all(&bin)?;
        std::fs::create_dir_all(remote.tmux_tmpdir())?;
        // Installed where the client looks for exactly this protocol and version.
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_bootty-daemon"),
            bin.join(format!(
                "bootty-daemon-{}-{}.exe",
                bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
                env!("CARGO_PKG_VERSION")
            )),
        )?;
        let ssh = remote.root.path().join("ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nfor line; do :; done\n{}\ncd \"$HOME\" && exec /bin/sh -c \"$line\"\n",
                remote.environment()
            ),
        )?;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
        Ok(remote)
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn tmux_tmpdir(&self) -> PathBuf {
        self.root.path().join("tmux")
    }

    /// The remote login environment. `TMUX` goes so tmux never finds the server this test runs
    /// in; `TMUX_TMPDIR` keeps the daemon's tmux on a private socket.
    fn environment(&self) -> String {
        let value = |path: &Path| shell_quote(&path.to_string_lossy());
        format!(
            "unset TMUX TMUX_PANE\nexport HOME={} TMUX_TMPDIR={} BOOTTY_DAEMON_STATE={} SHELL=/bin/sh",
            value(&self.home()),
            value(&self.tmux_tmpdir()),
            value(&self.root.path().join("state/daemon.sqlite")),
        )
    }

    fn host(&self) -> RemoteHost {
        RemoteHost::from(SshRemote::new(SshRemoteConfig {
            host: "fake-remote".to_owned(),
            user: None,
            port: None,
            program: self.root.path().join("ssh").to_string_lossy().into_owned(),
            args: Vec::new(),
        }))
    }

    /// Run a shell command in the remote environment.
    fn run(&self, command: &str) -> Result<Output> {
        Ok(Command::new("/bin/sh")
            .args(["-c", &format!("{}\n{command}", self.environment())])
            .output()?)
    }

    fn create_space(&self, name: &str) -> Result<String> {
        let daemon = shell_quote(env!("CARGO_BIN_EXE_bootty-daemon"));
        let output = self.run(&format!(
            "{daemon} remote-space create --name {} --backend tmux",
            shell_quote(name)
        ))?;
        anyhow::ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let summary: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        summary["id"]
            .as_str()
            .map(str::to_owned)
            .context("created Space id")
    }
}

impl Drop for FakeRemote {
    fn drop(&mut self) {
        let _ = self.run("tmux kill-server");
    }
}

fn tag(space: &str) -> MuxSessionTag {
    MuxSessionTag {
        identity: Some(new_session_identity()),
        space: Some(space.to_owned()),
    }
}

fn wait_for(what: &str, mut ready: impl FnMut() -> Result<bool>) -> Result<()> {
    let started = Instant::now();
    while !ready()? {
        anyhow::ensure!(started.elapsed() < TIMEOUT, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// An explicit create runs its argv on the remote host and never reuses a name another Space
/// holds. Its pane then takes input and capture through the daemon, and only its own Space may
/// address it.
#[test]
fn a_remote_space_runs_explicit_creates_and_pane_io_through_the_daemon() -> Result<()> {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux unavailable; remote Space acceptance not run");
        return Ok(());
    }
    let remote = FakeRemote::new()?;
    let (first, second) = (
        remote.create_space("First")?,
        remote.create_space("Second")?,
    );
    let mut owner = RemoteSpaceBackend::new(remote.host(), first.clone(), MuxBackendKind::Tmux);
    let mut other = RemoteSpaceBackend::new(remote.host(), second.clone(), MuxBackendKind::Tmux);
    let received = remote.root.path().join("received");
    let cwd = remote.root.path().to_string_lossy().into_owned();
    owner.execute(MuxCommand::CreateProjectSession {
        session_id: "agent".to_owned(),
        cwd: cwd.clone(),
        tag: tag(&first),
        argv: Some(vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf remote-ready; exec cat > \"$0\"".to_owned(),
            received.to_string_lossy().into_owned(),
        ]),
    })?;
    let session = owner
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.name == "agent")
        .context("the created session")?;
    let pane = session
        .windows
        .first()
        .and_then(|window| window.panes.first()?.pane_id.clone())
        .context("the created session's pane")?;
    let screen = PaneCapture {
        history: false,
        max_lines: 100,
        ansi: false,
    };
    wait_for("the argv runs", || {
        Ok(owner
            .capture_pane(&pane, screen)?
            .text
            .contains("remote-ready"))
    })?;
    for input in [
        PaneInput::Paste("from the script".to_owned()),
        PaneInput::Submit,
    ] {
        owner.send_pane_input(&pane, &input)?;
    }
    wait_for("input reaches the remote pane", || {
        Ok(std::fs::read_to_string(&received).unwrap_or_default() == "from the script\n")
    })?;

    let refused = [
        other
            .send_pane_input(&pane, &PaneInput::Write(b"x".to_vec()))
            .err(),
        other.capture_pane(&pane, screen).err(),
    ];
    for error in refused {
        let error = error.context("another Space must not reach the pane")?;
        anyhow::ensure!(
            format!("{error:#}").contains("does not belong to remote Space"),
            "{error:#}"
        );
    }
    let duplicate = other.execute(MuxCommand::CreateProjectSession {
        session_id: "agent".to_owned(),
        cwd,
        tag: tag(&second),
        argv: Some(Vec::new()),
    });
    anyhow::ensure!(
        duplicate.is_err(),
        "an explicit create must refuse a taken name"
    );
    let owners = owner
        .snapshot()?
        .sessions
        .into_iter()
        .filter(|candidate| candidate.name == "agent")
        .map(|candidate| candidate.tag)
        .collect::<Vec<_>>();
    assert_eq!(owners, [session.tag], "the session keeps its Space's stamp");
    anyhow::ensure!(
        other.snapshot()?.sessions.is_empty(),
        "the other Space holds no session"
    );
    Ok(())
}
