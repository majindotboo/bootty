#![cfg(unix)]

use bootty_agents::{AgentLaunch, CodexTerminalObserver};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    fs,
    io::{Read as _, Write as _},
    os::unix::{
        fs::PermissionsExt as _,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    time::Duration,
};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;
const OWNER_LAUNCH: &str = "BOOTTY_TEST_CODEX_OWNER_LAUNCH";
const LIMIT: Duration = Duration::from_secs(5);

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[rstest]
fn codex_owner_subprocess() -> TestResult<()> {
    let Some(launch) = std::env::var_os(OWNER_LAUNCH) else {
        return Ok(());
    };
    let launch: AgentLaunch = serde_json::from_str(launch.to_str().ok_or("owner launch UTF-8")?)?;
    let directory = Path::new(launch.cwd.as_deref().ok_or("owner cwd")?);
    let mut observer = CodexTerminalObserver::prepare(&launch, directory, Arc::new(|_| {}))?;
    let mut completion = [0];
    std::io::stdin().read_exact(&mut completion)?;
    observer.stop_and_wait()?;
    Ok(())
}

fn accepted(listener: &UnixListener) -> TestResult<UnixStream> {
    let listener = listener.try_clone()?;
    let (sender, response) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(listener.accept());
    });
    let (stream, _) = response.recv_timeout(LIMIT)??;
    stream.set_read_timeout(Some(LIMIT))?;
    Ok(stream)
}

fn provider(directory: &Path) -> TestResult<AgentLaunch> {
    let program = directory.join("provider 'literal'.py");
    fs::write(
        &program,
        r"#!/usr/bin/env python3
import fcntl, json, os, pathlib, socket, subprocess, sys
root = pathlib.Path(__file__).parent
lock = open(root / 'same-session-writer.lock', 'w')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
os.set_inheritable(lock.fileno(), True)
peer = socket.socket(socket.AF_UNIX)
peer.connect(str(root / 'ready.sock'))
os.set_inheritable(peer.fileno(), True)
child = subprocess.Popen([sys.executable, '-c', 'import sys; sys.stdin.read()'], stdin=subprocess.PIPE, close_fds=False)
peer.sendall((json.dumps({'argv': sys.argv[1:], 'account': os.environ.get('CODEX_HOME'), 'cwd': os.getcwd(), 'pid': os.getpid(), 'descendant': child.pid}) + '\n').encode())
while peer.recv(1): pass
",
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    Ok(AgentLaunch {
        program: program.to_str().ok_or("program path")?.to_owned(),
        cwd: Some(directory.to_str().ok_or("cwd path")?.to_owned()),
        arguments: vec![
            "-c".to_owned(),
            "session='same-id'; $HOME `uname`\nnext".to_owned(),
            "-c".to_owned(),
            String::new(),
        ],
        ephemeral: false,
        account_directory: Some(
            directory
                .join("captured-account")
                .to_string_lossy()
                .into_owned(),
        ),
    })
}

fn metadata(peer: &mut UnixStream, launch: &AgentLaunch) -> TestResult<()> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        peer.read_exact(&mut byte)?;
        if byte == *b"\n" {
            break;
        }
        line.extend(byte);
    }
    let value: serde_json::Value = serde_json::from_slice(&line)?;
    let arguments: Vec<String> =
        serde_json::from_value(value.get("argv").ok_or("observed argv")?.clone())?;
    assert_eq!(arguments.get(3..), Some(launch.arguments.as_slice()));
    assert_eq!(
        value.get("account").and_then(serde_json::Value::as_str),
        launch.account_directory.as_deref()
    );
    assert_eq!(
        fs::canonicalize(
            value
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .ok_or("observed cwd")?
        )?,
        fs::canonicalize(launch.cwd.as_deref().ok_or("captured cwd")?)?
    );
    Ok(())
}

#[rstest]
#[case::sigkill(true)]
#[case::normal_shutdown(false)]
fn owner_loss_reaps_private_provider_descendants_and_releases_same_session_writer(
    #[case] crash: bool,
) -> TestResult<()> {
    let directory = assert_fs::TempDir::new()?;
    let launch = provider(directory.path())?;
    let listener = UnixListener::bind(directory.path().join("ready.sock"))?;
    let mut owner = OwnedChild(
        Command::new(std::env::current_exe()?)
            .args(["--exact", "codex_owner_subprocess", "--nocapture"])
            .env(OWNER_LAUNCH, serde_json::to_string(&launch)?)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut unrelated = OwnedChild(
        Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut peer = accepted(&listener)?;
    metadata(&mut peer, &launch)?;
    if crash {
        owner.0.kill()?;
    } else {
        owner
            .0
            .stdin
            .as_mut()
            .ok_or("owner stdin")?
            .write_all(b"q")?;
    }
    let status = owner.0.wait()?;
    assert_eq!(status.success(), !crash);
    let mut byte = [0];
    assert_eq!(
        peer.read(&mut byte)?,
        0,
        "provider and descendant must close their inherited completion socket"
    );
    assert_eq!(
        unrelated.0.try_wait()?,
        None,
        "unrelated process must survive owner loss"
    );
    // The same provider lock and captured native session/config must be available after owner loss.
    let mut replacement =
        CodexTerminalObserver::prepare(&launch, directory.path(), Arc::new(|_| {}))?;
    let mut resumed = accepted(&listener)?;
    metadata(&mut resumed, &launch)?;
    replacement.stop_and_wait()?;
    assert_eq!(resumed.read(&mut byte)?, 0);
    Ok(())
}

#[rstest]
fn failed_provider_exit_is_observed_after_supervisor_reaps_its_watcher() -> TestResult<()> {
    let directory = assert_fs::TempDir::new()?;
    let mut launch = provider(directory.path())?;
    fs::write(&launch.program, "#!/bin/sh\nexit 37\n")?;
    launch.arguments.clear();
    let (sender, observed) = mpsc::channel();
    let mut observer = CodexTerminalObserver::prepare(
        &launch,
        directory.path(),
        Arc::new(move |observation| {
            let _ = sender.send(observation);
        }),
    )?;
    let observation = observed.recv_timeout(LIMIT)?;
    assert_eq!(
        observation.status,
        bootty_agents::TerminalAgentStatus::Unavailable
    );
    let detail = observation.detail.ok_or("provider exit detail")?;
    if !detail.contains("37") {
        return Err(format!("provider exit status was lost: {detail}").into());
    }
    observer.stop_and_wait()?;
    Ok(())
}
