//! Exercise the real tmux server on a private socket; transfers must preserve process identity.
#![cfg(unix)]
use anyhow::{Context as _, Result};
use bootty_host::{CommandOutput, CommandRunner, SystemCommandRunner};
use bootty_mux::{
    command::{MuxCommand, MuxDirection},
    tmux::TmuxBackend,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{collections::BTreeMap, process::Command};

struct PrivateTmux {
    directory: assert_fs::TempDir,
}
impl PrivateTmux {
    fn args(&self, args: &[String]) -> Vec<String> {
        [
            vec![
                "-S".to_owned(),
                self.directory
                    .path()
                    .join("socket")
                    .to_string_lossy()
                    .into_owned(),
                "-f".to_owned(),
                "/dev/null".to_owned(),
            ],
            args.to_vec(),
        ]
        .concat()
    }
    fn run_checked(&self, args: &[&str]) -> Result<String> {
        let output = self.run(
            "tmux",
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )?;
        anyhow::ensure!(output.success, "{}", output.stderr);
        Ok(output.stdout)
    }
    fn processes(&self) -> Result<BTreeMap<String, String>> {
        self.run_checked(&["list-panes", "-a", "-F", "#{pane_id} #{pane_pid}"])?
            .lines()
            .map(|line| {
                let (id, pid) = line.split_once(' ').context("pane process")?;
                Ok((id.to_owned(), pid.to_owned()))
            })
            .collect()
    }
}
impl CommandRunner for PrivateTmux {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        SystemCommandRunner.run(program, &self.args(args))
    }
}
impl Drop for PrivateTmux {
    fn drop(&mut self) {
        let _ = self.run("tmux", &["kill-server".to_owned()]);
    }
}
#[rstest]
fn tmux_transfers_keep_processes_and_reject_foreign_panes() -> Result<()> {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux unavailable; real-server acceptance not run");
        return Ok(());
    }
    let server = PrivateTmux {
        directory: assert_fs::TempDir::new().expect("private socket"),
    };
    server.run_checked(&[
        "new-session",
        "-d",
        "-s",
        "transfer",
        "-x",
        "160",
        "-y",
        "80",
        "sh",
    ])?;
    server.run_checked(&["split-window", "-h", "-t", "transfer", "sh"])?;
    server.run_checked(&["new-session", "-d", "-s", "foreign", "sh"])?;
    let before = server.processes()?;
    let ids = server
        .run_checked(&["list-panes", "-t", "transfer", "-F", "#{pane_id}"])?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let foreign = server
        .run_checked(&["display-message", "-p", "-t", "foreign", "#{pane_id}"])?
        .trim()
        .to_owned();
    let mut backend = TmuxBackend::with_runner("tmux", server);
    backend
        .execute(MuxCommand::SwapPanes {
            session_id: "transfer".to_owned(),
            source_pane_id: ids[0].clone(),
            target_pane_id: ids[1].clone(),
        })
        .expect("swap");
    backend
        .execute(MuxCommand::ExtractPane {
            session_id: "transfer".to_owned(),
            pane_id: ids[0].clone(),
        })
        .expect("extract");
    let snapshot = backend.snapshot().expect("extracted snapshot");
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.name == "transfer")
            .expect("session")
            .windows
            .len(),
        2
    );
    backend
        .execute(MuxCommand::MovePane {
            session_id: "transfer".to_owned(),
            pane_id: ids[0].clone(),
            target_pane_id: ids[1].clone(),
            direction: MuxDirection::Down,
        })
        .expect("move back");
    assert_eq!(backend.runner().processes()?, before);
    let before_rejection = backend.snapshot().expect("snapshot");
    anyhow::ensure!(
        backend
            .execute(MuxCommand::SwapPanes {
                session_id: "transfer".to_owned(),
                source_pane_id: ids[0].clone(),
                target_pane_id: foreign
            })
            .is_err()
    );
    assert_eq!(
        backend.snapshot().expect("unchanged snapshot"),
        before_rejection
    );
    assert_eq!(backend.runner().processes()?, before);
    Ok(())
}
