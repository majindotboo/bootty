#![cfg(test)]
#![cfg(unix)]

//! A catalog-backed remote Space driven through the real client transport: `RemoteSpaceBackend`
//! runs the versioned daemon through `ssh`, here a fake one that runs the remote command line on
//! this machine, against a private tmux server.

use std::{
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
    remote_space::{RemoteSpaceBackend, RemoteSpaceTransport},
    snapshot::{MuxSessionTag, new_session_identity},
};
use pretty_assertions::assert_eq;

const TIMEOUT: Duration = Duration::from_secs(30);

struct FakeRemote {
    root: PathBuf,
    cleanup: bool,
}

impl FakeRemote {
    fn new(root: &Path) -> Result<Self> {
        // Unix sockets have a short path limit, and tmux keeps its socket under TMUX_TMPDIR.
        let remote = Self {
            root: root.to_owned(),
            cleanup: false,
        };
        let bin = remote.home().join(".bootty/bin");
        std::fs::create_dir_all(&bin)?;
        std::fs::create_dir_all(remote.tmux_tmpdir())?;
        let search = std::env::var_os("PATH").unwrap_or_default();
        let search = std::env::join_paths(
            std::env::split_paths(&search).filter(|path| !path.ends_with("shims")),
        )?;
        let tmux =
            bootty_host::find_executable_in(std::ffi::OsStr::new("tmux"), Some(&search), None)
                .context("tmux is required for remote backend acceptance")?;
        std::os::unix::fs::symlink(tmux, bin.join("tmux"))?;
        // Installed where the client looks for exactly this protocol and version.
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_bootty-daemon"),
            bin.join(format!(
                "bootty-daemon-{}-{}.exe",
                bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
                env!("CARGO_PKG_VERSION")
            )),
        )?;
        let ssh = remote.root.as_path().join("ssh");
        bootty_test_support::write_executable(
            &ssh,
            r#"#!/bin/sh
for line; do
    case "$line" in --bootty-test-root=*) root=${line#--bootty-test-root=};; esac
done
unset TMUX TMUX_PANE XDG_CONFIG_HOME XDG_STATE_HOME XDG_DATA_HOME
export HOME="$root/home" TMUX_TMPDIR="$root/tmux" BOOTTY_DAEMON_STATE="$root/state/daemon.sqlite" SHELL=/bin/sh
cd "$HOME" && exec /bin/sh -c "$line"
"#,
        )?;
        Ok(remote)
    }

    fn home(&self) -> PathBuf {
        self.root.as_path().join("home")
    }

    fn tmux_tmpdir(&self) -> PathBuf {
        self.root.as_path().join("tmux")
    }

    /// The remote login environment. `TMUX` goes so tmux never finds the server this test runs
    /// in; `TMUX_TMPDIR` keeps the daemon's tmux on a private socket.
    fn environment(&self) -> String {
        let value = |path: &Path| shell_quote(&path.to_string_lossy());
        format!(
            // XDG paths would otherwise lead the daemon to the developer's real configuration.
            "unset TMUX TMUX_PANE XDG_CONFIG_HOME XDG_STATE_HOME XDG_DATA_HOME\nexport HOME={} TMUX_TMPDIR={} BOOTTY_DAEMON_STATE={} SHELL=/bin/sh",
            value(&self.home()),
            value(&self.tmux_tmpdir()),
            value(&self.root.as_path().join("state/daemon.sqlite")),
        )
    }

    fn host(&self) -> RemoteHost {
        RemoteHost::from(SshRemote::new(SshRemoteConfig {
            host: "fake-remote".to_owned(),
            user: None,
            port: None,
            program: self
                .root
                .as_path()
                .join("ssh")
                .to_string_lossy()
                .into_owned(),
            args: vec![format!(
                "--bootty-test-root={}",
                self.root.as_path().display()
            )],
        }))
    }

    /// Run a shell command in the remote environment.
    fn run(&self, command: &str) -> Result<Output> {
        Ok(Command::new("/bin/sh")
            .args(["-c", &format!("{}\n{command}", self.environment())])
            .output()?)
    }

    fn create_space(name: &str) -> Result<String> {
        let output = InProcessDaemon.run(
            &[
                "remote-space".into(),
                "create".into(),
                "--name".into(),
                name.into(),
                "--backend".into(),
                "tmux".into(),
            ],
            None,
        )?;
        let summary: serde_json::Value = serde_json::from_str(&output)?;
        summary["id"]
            .as_str()
            .map(str::to_owned)
            .context("created Space id")
    }
}

impl Drop for FakeRemote {
    fn drop(&mut self) {
        if !self.cleanup {
            return;
        }
        // The daemon names its server after the build's identity, so stop every server on the
        // private socket directory rather than tmux's default one.
        let _ = self.run(
            r#"for socket in "$TMUX_TMPDIR"/tmux-*/*; do tmux -S "$socket" kill-server; done"#,
        );
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
        std::thread::yield_now();
    }
    Ok(())
}

/// An explicit create runs its argv on the remote host and never reuses a name another Space
/// holds. Its pane then takes input and capture through the daemon, and only its own Space may
/// address it.
#[test]
fn a_remote_space_runs_explicit_creates_and_pane_io_through_the_daemon() -> Result<()> {
    let directory = assert_fs::TempDir::new_in("/tmp")?;
    let remote = FakeRemote::new(directory.path())?;
    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", "remote_panes_helper", "--nocapture"])
        .env("BOOTTY_REMOTE_PANES_ROOT", directory.path())
        .env("HOME", remote.home())
        .env("TMUX_TMPDIR", remote.tmux_tmpdir())
        .env(
            "BOOTTY_DAEMON_STATE",
            directory.path().join("state/daemon.sqlite"),
        )
        .env(
            "PATH",
            format!(
                "{}:/usr/bin:/bin",
                remote.home().join(".bootty/bin").display()
            ),
        )
        .env("SHELL", "/bin/sh")
        .env("BOOTTY_SHELL", "/bin/sh")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("ENV", "")
        .env_remove("BASH_ENV")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .status()?;
    anyhow::ensure!(status.success(), "remote pane helper failed");
    Ok(())
}

struct InProcessDaemon;
impl RemoteSpaceTransport for InProcessDaemon {
    fn run(&self, args: &[String], input: Option<Vec<u8>>) -> Result<String> {
        anyhow::ensure!(args.first().is_some_and(|arg| arg == "remote-space"));
        let paths = bootty_daemon::command_runtime::remote_space_paths_from_environment(
            bootty_config::ApplicationIdentity::Production,
        )?;
        let mut output = Vec::new();
        bootty_daemon::command_runtime::run_remote_space_with_io(
            &args[1..],
            &paths,
            &mut std::io::Cursor::new(input.unwrap_or_default()),
            &mut output,
        )?;
        Ok(String::from_utf8(output)?)
    }
}

#[test]
fn remote_panes_helper() -> Result<()> {
    let Some(root) = std::env::var_os("BOOTTY_REMOTE_PANES_ROOT") else {
        return Ok(());
    };
    bootty_config::ApplicationIdentity::Production.initialize_process()?;
    let remote = FakeRemote {
        root: root.into(),
        cleanup: true,
    };
    let (first, second) = (
        FakeRemote::create_space("First")?,
        FakeRemote::create_space("Second")?,
    );
    let mut owner = RemoteSpaceBackend::new(remote.host(), first.clone(), MuxBackendKind::Tmux);
    let mut other =
        RemoteSpaceBackend::with_transport(InProcessDaemon, second.clone(), MuxBackendKind::Tmux);
    let received = remote.root.as_path().join("received");
    let cwd = remote.root.as_path().to_string_lossy().into_owned();
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
    let owner = RemoteSpaceBackend::with_transport(InProcessDaemon, first, MuxBackendKind::Tmux);
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
