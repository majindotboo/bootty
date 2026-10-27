#![cfg(test)]
#![cfg(unix)]

use bootty_agents::{
    AgentLaunch, CodexTerminalObserver, NativeChild, NativeChildControl, TerminalAgentStatus,
    TerminalLauncher,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io,
    sync::{Arc, mpsc},
    time::Duration,
};

struct ExitedPeer(mpsc::Sender<&'static str>);

impl NativeChildControl for ExitedPeer {
    fn try_wait(&mut self) -> Result<Option<String>, String> {
        Ok(Some("exit status: 37".to_owned()))
    }
    fn terminate(&mut self) -> Result<(), String> {
        self.0.send("terminate").map_err(|error| error.to_string())
    }
    fn wait(&mut self) -> Result<String, String> {
        self.0.send("wait").map_err(|error| error.to_string())?;
        Ok("exit status: 37".to_owned())
    }
}

#[rstest]
fn injected_observer_reports_exit_before_a_client_connects_and_reaps_its_peer() {
    let directory = assert_fs::TempDir::new().unwrap();
    let launch = AgentLaunch {
        program: "/fixture/codex".to_owned(),
        cwd: Some(directory.path().to_str().unwrap().to_owned()),
        arguments: vec!["--model".to_owned(), "selected-model".to_owned()],
        ephemeral: false,
        account_directory: Some("/fixture/account".to_owned()),
    };
    let expected = launch.clone();
    let (cleanup, cleaned) = mpsc::channel();
    let launcher: TerminalLauncher = Arc::new(move |command| {
        assert_eq!(command.get_program(), expected.program.as_str());
        assert_eq!(
            command.get_current_dir(),
            expected.cwd.as_deref().map(std::path::Path::new)
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(name, _)| *name == "CODEX_HOME")
                .and_then(|(_, value)| value)
                .and_then(|value| value.to_str()),
            Some("/fixture/account")
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(&args[..2], ["app-server", "--listen"]);
        assert!(args[2].starts_with("unix://"));
        assert_eq!(&args[3..], ["--config", "model=\"selected-model\""]);
        Ok(NativeChild {
            stdin: Box::new(io::sink()),
            stdout: Box::new(io::empty()),
            stderr: Box::new(io::empty()),
            control: Box::new(ExitedPeer(cleanup.clone())),
        })
    });
    let (observations, observed) = mpsc::channel();
    let mut observer = CodexTerminalObserver::prepare_with_launcher(
        &launch,
        directory.path(),
        Arc::new(move |value| {
            observations.send(value).unwrap();
        }),
        &launcher,
    )
    .unwrap();
    let observation = observed.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(observation.status, TerminalAgentStatus::Unavailable);
    assert!(observation.detail.unwrap().contains("37"));
    observer.stop_and_wait().unwrap();
    assert_eq!(
        cleaned.try_iter().collect::<Vec<_>>(),
        ["terminate", "wait"]
    );
}
