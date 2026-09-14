use std::sync::Mutex;

use anyhow::Result;
use bootty_config::config::SshRemoteConfig as SshTarget;
use bootty_host::ssh::SshRemote;
use bootty_host::{CommandOutput, CommandRunner};
use bootty_mux::herdr::{HerdrBackend, HerdrPanePolicy};
use bootty_mux::{backend::MuxBackend, command::MuxCommand};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[derive(Debug)]
struct FakeRunner {
    output: CommandOutput,
    calls: Mutex<Vec<(String, Vec<String>)>>,
}

impl FakeRunner {
    fn success(stdout: &str) -> Self {
        Self {
            output: CommandOutput {
                success: true,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            },
            calls: Mutex::default(),
        }
    }

    fn failure(stderr: &str) -> Self {
        Self {
            output: CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: stderr.to_owned(),
            },
            calls: Mutex::default(),
        }
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        self.calls
            .lock()
            .map_err(|error| anyhow::anyhow!("calls lock: {error}"))?
            .push((program.to_owned(), args.to_vec()));
        Ok(self.output.clone())
    }
}

#[rstest]
fn snapshot_uses_public_session_list_and_keeps_stopped_sessions_attachable() {
    let runner = FakeRunner::success(
        r#"{"sessions":[
            {"name":"sleeping","default":false,"running":false,"socket_path":"/tmp/a","session_dir":"/tmp/a"},
            {"name":"work","default":false,"running":true,"socket_path":"/tmp/b","session_dir":"/tmp/b"},
            {"name":"main","default":true,"running":true,"socket_path":"/tmp/c","session_dir":"/tmp/c"}
        ]}"#,
    );
    let backend = HerdrBackend::with_runner(runner);

    let snapshot = backend.snapshot().expect("Herdr snapshot");

    assert_eq!(
        backend
            .runner()
            .calls
            .lock()
            .expect("calls lock")
            .as_slice(),
        &[(
            "herdr".to_owned(),
            vec!["session".to_owned(), "list".to_owned(), "--json".to_owned()]
        )]
    );
    assert_eq!(snapshot.active_session_id.as_deref(), Some("main"));
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        ["sleeping", "work", "main"]
    );
    for session in &snapshot.sessions {
        assert_eq!(session.windows.len(), 1);
        assert_eq!(session.windows[0].panes.len(), 1);
        assert_eq!(session.anchor.session_id, session.id);
        assert_eq!(session.anchor.pane_id, None);
    }
}

#[rstest]
fn empty_session_list_keeps_the_default_session_attachable() {
    let backend = HerdrBackend::with_runner(FakeRunner::success(r#"{"sessions":[]}"#));

    let snapshot = backend.snapshot().expect("fresh Herdr snapshot");

    assert_eq!(snapshot.active_session_id.as_deref(), Some("default"));
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.sessions[0].name, "default");
}

#[rstest]
fn snapshot_reports_cli_and_json_failures() {
    let cli = HerdrBackend::with_runner(FakeRunner::failure("Herdr isn't installed"));
    let json = HerdrBackend::with_runner(FakeRunner::success("not json"));

    assert!(
        cli.snapshot()
            .expect_err("CLI failure")
            .to_string()
            .contains("list Herdr sessions")
    );
    assert!(
        json.snapshot()
            .expect_err("JSON failure")
            .to_string()
            .contains("decode `herdr session list --json`")
    );
}

#[rstest]
fn attach_launch_selects_the_session_and_scrubs_nested_herdr_state() {
    let launch = HerdrPanePolicy::new(None)
        .attach_launch("review")
        .expect("attach launch");

    assert_eq!(launch.program, "herdr");
    assert_eq!(launch.args, ["--session", "review"]);
    assert_eq!(
        launch.env_remove,
        [
            "HERDR_ENV",
            "HERDR_SESSION",
            "HERDR_SOCKET_PATH",
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_WORKSPACE_ID",
            "HERDR_TAB_ID",
            "HERDR_PANE_ID",
        ]
    );
    assert!(!launch.remote);
}

#[rstest]
fn remote_attach_uses_herdrs_local_composite_ui() {
    let remote = SshRemote::new(SshTarget {
        host: "herd.example".to_owned(),
        user: Some("dev".to_owned()),
        ..SshTarget::for_host("ignored")
    });

    let launch = HerdrPanePolicy::new(Some(remote.into()))
        .attach_launch("review")
        .expect("remote attach launch");

    assert_eq!(launch.program, "herdr");
    assert_eq!(
        launch.args,
        ["--remote", "dev@herd.example", "--session", "review"]
    );
    assert!(launch.remote);
}

#[rstest]
fn remote_attach_rejects_ssh_options_that_herdr_cannot_represent() {
    let remote = SshRemote::new(SshTarget {
        host: "herd.example".to_owned(),
        port: Some(2202),
        ..SshTarget::for_host("ignored")
    });

    let Err(error) = HerdrPanePolicy::new(Some(remote.into())).attach_launch("review") else {
        panic!("custom SSH option should be rejected")
    };

    assert!(error.to_string().contains("~/.ssh/config"));
}

#[rstest]
fn topology_commands_fail_instead_of_mutating_herdr_private_state() {
    let mut backend = HerdrBackend::with_runner(FakeRunner::success(r#"{"sessions":[]}"#));

    let error = MuxBackend::execute(
        &mut backend,
        MuxCommand::SplitPane {
            session_id: "default".to_owned(),
            pane_id: None,
            direction: bootty_mux::command::MuxSplitDirection::Right,
        },
    )
    .expect_err("unsupported command");

    assert!(error.to_string().contains("Herdr owns its UI and topology"));
    assert!(error.to_string().contains("SplitPane"));
}
