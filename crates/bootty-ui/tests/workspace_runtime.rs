#![cfg(test)]
use bootty_mux::workspace::ScopedSessionTarget;

use bootty_config::config::BoottyConfig;
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use bootty_control::{
    AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_mux::repository::{
    SpaceMuxOverride, SpaceRemoteOverride, WorkspaceRepository, WorkspaceSpace,
};
use bootty_mux::{command::MuxCommand, snapshot::MuxSessionTag};
use bootty_ui::AppState;
use rusqlite::Connection;

use crate::support::close_transport;

use crate::support::idle_frames as frames;

use crate::support::config as test_config;

const fn python_program() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn python_argv(script: &str, arguments: &[&str]) -> Vec<String> {
    [python_program(), "-u", "-c", script]
        .into_iter()
        .chain(arguments.iter().copied())
        .map(str::to_owned)
        .collect()
}

fn idle_argv() -> Vec<String> {
    if cfg!(unix) {
        vec!["/bin/sh".into(), "-c".into(), "read ignored".into()]
    } else {
        python_argv("import sys; sys.stdin.read()", &[])
    }
}

fn create_space(
    repository: &mut WorkspaceRepository,
    name: &str,
    sort_key: &str,
    color: [u8; 3],
    mux: SpaceMuxOverride,
) -> WorkspaceSpace {
    repository
        .create_space(name, sort_key, color, false, mux)
        .expect("create Space")
        .expect("valid Space")
}

fn event_repaint(repaint: impl Fn() + Send + Sync + 'static) -> bootty_mux::RepaintHandle {
    let host = std::thread::current();
    Arc::new(move || {
        repaint();
        host.unpark();
    })
}

fn repaint_on_event() -> bootty_mux::RepaintHandle {
    event_repaint(|| {})
}

fn app_state(config: bootty_config::config::BoottyConfig) -> AppState {
    AppState::new(config, repaint_on_event(), None, None).expect("app state")
}

// Fail real SQLite writes immediately instead of waiting for its busy retry timeout.

#[fixture]
fn directory() -> assert_fs::TempDir {
    assert_fs::TempDir::new().expect("temporary workspace")
}

fn claimed_session(
    identity: &str,
    backend_name: &str,
    cwd: &str,
) -> bootty_mux::session_membership::WorkspaceSession {
    bootty_mux::session_membership::WorkspaceSession {
        identity: identity.to_owned(),
        backend_name: backend_name.to_owned(),
        display_name: backend_name.to_owned(),
        explicit: false,
        cwd: cwd.to_owned(),
        state: bootty_mux::session_membership::SessionState::default(),
        terminal_snapshot: None,
    }
}

fn claim_first_space(
    config_path: &Path,
    identity: &str,
    backend_name: &str,
    cwd: &str,
) -> (WorkspaceRepository, WorkspaceSpace) {
    let (mut repository, snapshot) = WorkspaceRepository::open(config_path).expect("workspace");
    let space = snapshot.spaces()[0].clone();
    let mut sessions = space.binding().sessions().clone();
    assert!(sessions.claim(claimed_session(identity, backend_name, cwd)));
    repository
        .commit_binding_state(space.binding().mux_scope(), &sessions)
        .expect("persist claimed session");
    (repository, space)
}

fn submit_command(
    state: &mut AppState,
    invocation: CommandInvocation,
    started: Instant,
) -> CommandOutcome {
    submit_command_as(state, invocation, Caller::Socket, started)
}

fn submit_command_as(
    state: &mut AppState,
    invocation: CommandInvocation,
    caller: Caller,
    started: Instant,
) -> CommandOutcome {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("command deadline");
    let outcomes = state
        .app_command_sender(caller)
        .submit(invocation, deadline, CommandCancellation::new())
        .expect("submit command");
    let mut frame_at = started;
    loop {
        state.update_frame(frames::idle_frame(frame_at));
        match outcomes.try_recv() {
            Ok(outcome) => return outcome,
            Err(mpsc::TryRecvError::Disconnected) => panic!("command reply disconnected"),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        assert!(Instant::now() < deadline, "command did not answer");
        frame_at = frame_at
            .checked_add(Duration::from_millis(1))
            .expect("frame timestamp");
        std::thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

/// Run frames until `ready` answers, bounded for real process startup.
fn wait_until<T>(
    state: &mut AppState,
    what: &str,
    mut ready: impl FnMut(&mut AppState) -> Option<T>,
) -> T {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("test deadline");
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if let Some(value) = ready(state) {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

fn targeted(command: &str, arguments: Vec<String>, target: serde_json::Value) -> CommandInvocation {
    CommandInvocation {
        target: Some(serde_json::from_value(target).expect("command target")),
        ..CommandInvocation::new(command, arguments, Caller::Socket)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetSpace {
    /// The active Space, which has no session yet and so shows the new one at once.
    EmptyActive,
    /// The active Space, which keeps showing the session it already has.
    Active,
    /// A native Space that is not active.
    Inactive,
}

/// A native session's command runs from the moment the session is created, whether or not its
/// pane is shown, and showing the pane later presents that same process: in the active Space and
/// in an inactive one.
#[rstest]
#[case::in_an_empty_active_space(TargetSpace::EmptyActive)]
#[case::in_the_active_space(TargetSpace::Active)]
#[case::in_an_inactive_space(TargetSpace::Inactive)]
fn a_native_session_command_starts_at_once_and_is_the_process_shown_later(
    directory: assert_fs::TempDir,
    #[case] created_in: TargetSpace,
) {
    let config_path = directory.path().join("config.toml");
    let (mut repository, _) = WorkspaceRepository::open(&config_path).expect("workspace");
    let native = SpaceMuxOverride {
        remote: SpaceRemoteOverride::Local,
    };
    let scripts = create_space(&mut repository, "Scripts", "2", [2; 3], native.clone()).id();
    let other = create_space(&mut repository, "Other", "3", [3; 3], native).id();
    drop(repository);
    let mut state = app_state(test_config::config(config_path));
    let home = state.active_space_id();
    let space_target = |state: &mut AppState, name: &str| {
        let outcome = submit_command(
            state,
            CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
            Instant::now(),
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            panic!("spaces.list failed: {outcome:?}");
        };
        value
            .as_array()
            .expect("a list of Spaces")
            .iter()
            .find(|space| space["name"] == name || (name.is_empty() && space["active"] == true))
            .map(|space| space["target"].clone())
            .expect("the Space is listed")
    };
    let project = directory.path().join("project");
    std::fs::create_dir(&project).expect("project directory");
    let cwd = project.to_string_lossy().into_owned();
    let home_target = space_target(&mut state, "");
    if created_in != TargetSpace::EmptyActive {
        let outcome = submit_command(
            &mut state,
            targeted(
                "session.create",
                vec!["project".to_owned(), cwd.clone()],
                home_target.clone(),
            ),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    let selection = |state: &AppState| {
        (
            state.mux().selected_session().map(str::to_owned),
            state.mux().selected_window().map(str::to_owned),
        )
    };
    let before = selection(&state);

    let marker = directory.path().join("started");
    let received = directory.path().join("received");
    let argv = python_argv(
        "import os,sys; open(sys.argv[1], 'a').write(os.environ['BOOTTY_PANE'] + '|' + os.getcwd() + '\\n'); print('eager-token', end='', flush=True)\nwith open(sys.argv[2], 'a', buffering=1) as output:\n for line in sys.stdin: output.write(line); print('input-recorded', flush=True)",
        &[&marker.to_string_lossy(), &received.to_string_lossy()],
    );
    let target = if created_in == TargetSpace::Inactive {
        space_target(&mut state, "Scripts")
    } else {
        home_target
    };
    let outcome = submit_command(
        &mut state,
        targeted(
            "session.create",
            vec![
                "agent".to_owned(),
                cwd,
                serde_json::to_string(&argv).expect("encode argv"),
            ],
            target,
        ),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("session.create failed: {outcome:?}");
    };
    let terminal = value["terminal"].clone();

    wait_until(
        &mut state,
        "the command starts when the session is created",
        |_| {
            std::fs::read_to_string(&marker)
                .ok()
                .filter(|text| text.ends_with('\n'))
        },
    );
    match created_in {
        // An empty Space shows the first session it gets, so this pane is on screen at once.
        TargetSpace::EmptyActive => {
            assert_eq!(selection(&state).0.as_deref(), Some("agent"));
        }
        TargetSpace::Active | TargetSpace::Inactive => {
            assert_eq!(selection(&state), before);
        }
    }
    assert_eq!(state.active_space_id(), home);

    let shows_token = |state: &mut AppState| {
        let outcome = submit_command(
            state,
            targeted("terminal.capture", Vec::new(), terminal.clone()),
            Instant::now(),
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            return None;
        };
        value["capture"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("eager-token"))
            .then_some(())
    };
    // Before anything shows it, even in an inactive Space, the pane is readable and takes input.
    wait_until(&mut state, "the hidden pane is readable", shows_token);
    for (command, arguments) in [
        ("terminal.paste", vec!["hidden-input".to_owned()]),
        ("terminal.submit", Vec::new()),
    ] {
        let outcome = submit_command(
            &mut state,
            targeted(command, arguments, terminal.clone()),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{command}: {outcome:?}"
        );
    }
    // PTY echo precedes the child's file write. Wait for its acknowledgement so the
    // predicate has a corresponding repaint event even when this Space is hidden.
    wait_until(
        &mut state,
        "input reaches the hidden pane's process",
        |state| {
            let outcome = submit_command(
                state,
                targeted("terminal.capture", Vec::new(), terminal.clone()),
                Instant::now(),
            );
            let CommandOutcome::Success { value, .. } = outcome else {
                return None;
            };
            value["capture"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("input-recorded"))
                .then_some(())
        },
    );
    assert!(
        std::fs::read_to_string(&received)
            .expect("recorded input")
            .contains("hidden-input")
    );
    assert_eq!(state.active_space_id(), home);

    if created_in == TargetSpace::Inactive {
        // Pass through another native Space first: the pane has to follow its owner each time.
        assert!(state.activate_space_from_ui(other));
        state.update_frame(frames::idle_frame(Instant::now()));
        assert!(state.activate_space_from_ui(scripts));
    }
    state.activate_session_from_ui("agent");
    wait_until(
        &mut state,
        "the shown pane is the process the create started",
        shows_token,
    );
    let pane = state
        .mux()
        .backend_session_by_id_or_name("agent")
        .and_then(|session| session.windows.first()?.panes.first()?.pane_id.clone())
        .expect("the session's pane");
    let marker = std::fs::read_to_string(&marker).expect("marker");
    let lines = marker.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "the command ran once");
    let (started_pane, cwd) = lines[0].split_once('|').expect("pane and cwd");
    assert_eq!(started_pane, pane, "the pane Bootty shows owns the process");
    assert_eq!(
        Path::new(cwd).canonicalize().unwrap(),
        project.canonicalize().unwrap()
    );
}

/// `pane.close` and `session.close` close a hidden native pane's transport and drop the pane, leaving
/// selection alone. Closing a session's only pane ends the session, as in tmux and rmux.
#[rstest]
#[case::pane("pane.close")]
#[case::session("session.close")]
fn closing_ends_a_hidden_native_pane_without_moving_selection(
    directory: assert_fs::TempDir,
    #[case] close_command: &str,
) {
    let config_path = directory.path().join("config.toml");
    let (launcher, closed) = close_transport::launcher();
    bootty_test_support::native_mux::configure(&config_path, launcher)
        .expect("observable native transport");
    let mut state = app_state(test_config::config(config_path));
    let outcome = submit_command(
        &mut state,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("spaces.list failed: {outcome:?}");
    };
    let home = value[0]["target"].clone();
    let cwd = directory.path().to_string_lossy().into_owned();
    for name in ["shown", "agent"] {
        let arguments = vec![name.to_owned(), cwd.clone()];
        let outcome = submit_command(
            &mut state,
            targeted("session.create", arguments, home.clone()),
            Instant::now(),
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            panic!("session.create {name} failed: {outcome:?}");
        };
        if name == "agent" {
            let terminal = value["terminal"].clone();
            let selected = state.mux().selected_session().map(str::to_owned);
            let close_target = if close_command == "pane.close" {
                terminal.clone()
            } else {
                value["created"].clone()
            };
            let mut close = targeted(close_command, Vec::new(), close_target);
            let outcome = submit_command(&mut state, close.clone(), Instant::now());
            let CommandOutcome::ConfirmationRequired { confirmation } = outcome else {
                panic!("expected close confirmation: {outcome:?}");
            };
            close.confirmation = Some(*confirmation);
            let outcome = submit_command(&mut state, close, Instant::now());
            assert!(
                matches!(outcome, CommandOutcome::Success { .. }),
                "{outcome:?}"
            );
            let pane = terminal["handle"]
                .as_str()
                .and_then(|handle| serde_json::from_str::<Vec<String>>(handle).ok())
                .and_then(|path| path.last().cloned())
                .expect("terminal target names its pane");
            assert_eq!(
                closed
                    .recv_timeout(Duration::from_secs(5))
                    .expect("native pane closes its transport"),
                pane
            );
            assert!(
                !state
                    .mux()
                    .all_sessions()
                    .iter()
                    .flat_map(|session| &session.windows)
                    .flat_map(|window| std::iter::once(&window.anchor).chain(&window.panes))
                    .any(|anchor| anchor.pane_id.as_deref() == Some(pane.as_str())),
                "the closed pane is gone from the snapshot"
            );
            assert_eq!(state.mux().selected_session().map(str::to_owned), selected);
            // The session ended with its only pane, so its name is free again.
            assert!(
                !state
                    .mux()
                    .all_sessions()
                    .iter()
                    .any(|session| session.name == "agent"),
                "the session ends with its last pane"
            );
            let reused = submit_command(
                &mut state,
                targeted(
                    "session.create",
                    vec!["agent".to_owned(), cwd.clone()],
                    home.clone(),
                ),
                Instant::now(),
            );
            assert!(
                matches!(reused, CommandOutcome::Success { .. }),
                "{reused:?}"
            );
        }
    }
}

/// Every Space in `spaces.list` order: its name, scope, and Binding target.
fn listed_spaces(state: &mut AppState) -> Vec<(String, String, serde_json::Value)> {
    let outcome = submit_command(
        state,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("spaces.list failed: {outcome:?}");
    };
    value
        .as_array()
        .expect("a list of Spaces")
        .iter()
        .map(|space| {
            (
                space["name"].as_str().expect("Space name").to_owned(),
                space["scope"].as_str().expect("Space scope").to_owned(),
                space["target"].clone(),
            )
        })
        .collect()
}

/// Submit through the socket mailbox and run frames in real time until the command answers.
fn submit_and_wait(state: &mut AppState, invocation: CommandInvocation) -> CommandOutcome {
    let (response, outcomes) = mpsc::channel();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("test deadline");
    state
        .app_command_sender(Caller::Socket)
        .try_send(AppCommandRequest {
            creation_receipt: None,
            invocation,
            deadline,
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");
    wait_until(state, "the command answers", |_| outcomes.try_recv().ok())
}

/// A native create whose first pane cannot start fails instead of reporting a dead pane as
/// created, and the session it made is closed so the name is free again. A cwd that is not a
/// directory fails before anything is created.
#[rstest]
fn a_native_session_that_cannot_start_fails_its_create_and_frees_its_name(
    directory: assert_fs::TempDir,
) {
    let mut state = app_state(test_config::config(directory.path().join("config.toml")));
    let (_, _, home) = listed_spaces(&mut state).remove(0);
    let cwd = directory.path().to_string_lossy().into_owned();
    let create = |cwd: &str, argv: serde_json::Value| {
        targeted(
            "session.create",
            vec!["agent".to_owned(), cwd.to_owned(), argv.to_string()],
            home.clone(),
        )
    };
    let has_agent = |state: &AppState| {
        state
            .mux()
            .all_sessions()
            .iter()
            .any(|session| session.name == "agent")
    };

    let missing = submit_and_wait(
        &mut state,
        create(
            &cwd,
            serde_json::json!(["bootty-no-such-program", "--flag"]),
        ),
    );
    assert!(
        matches!(&missing, CommandOutcome::Failed { code, message }
            if code == "session_start_failed" && message.contains("bootty-no-such-program")),
        "{missing:?}"
    );
    assert!(!has_agent(&state), "the failed session is closed");

    let not_a_directory = directory.path().join("file");
    std::fs::write(&not_a_directory, "").expect("plain file");
    let invalid = submit_and_wait(
        &mut state,
        create(
            &not_a_directory.to_string_lossy(),
            serde_json::json!(["bootty-never-start-invalid-directory"]),
        ),
    );
    assert!(
        matches!(&invalid, CommandOutcome::Failed { code, .. } if code == "invalid_arguments"),
        "{invalid:?}"
    );
    assert!(!has_agent(&state), "nothing was created");

    let started = submit_and_wait(&mut state, create(&cwd, serde_json::json!(idle_argv())));
    assert!(
        matches!(started, CommandOutcome::Success { .. }),
        "{started:?}"
    );
    assert!(has_agent(&state));
}

/// A native create still starting when its session is closed and a new one takes the name
/// answers for its own session only: it fails, and the new session keeps running.
#[rstest]
fn a_starting_native_create_never_answers_for_or_closes_a_recreated_session(
    directory: assert_fs::TempDir,
) {
    let mut state = app_state(test_config::config(directory.path().join("config.toml")));
    let (_, _, home) = listed_spaces(&mut state).remove(0);
    let cwd = directory.path().to_string_lossy().into_owned();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("test deadline");
    // Queued together, so the first create is still starting when the others run.
    let queue = |invocation| {
        let (response, outcome) = mpsc::channel();
        state
            .app_command_sender(Caller::Socket)
            .try_send(AppCommandRequest {
                creation_receipt: None,
                invocation,
                deadline,
                cancellation: CommandCancellation::new(),
                response,
            })
            .expect("submit command");
        outcome
    };
    let create = |argv: serde_json::Value| {
        targeted(
            "session.create",
            vec!["agent".to_owned(), cwd.clone(), argv.to_string()],
            home.clone(),
        )
    };
    let session = serde_json::json!({
        "kind": "session",
        "handle": serde_json::to_string(&[home["handle"].as_str().expect("Space handle"), "agent"])
            .expect("session handle"),
        "generation": home["generation"],
    });
    let mut close = targeted("session.close", Vec::new(), session);
    close.confirmation = Some(close.confirmation());

    let first = queue(create(serde_json::json!([
        "bootty-no-such-program",
        "--flag"
    ])));
    let closed = queue(close);
    let second = queue(create(serde_json::json!(idle_argv())));

    let first = wait_until(&mut state, "the first create answers", |_| {
        first.try_recv().ok()
    });
    // Normally the close finds the first create still starting. If its program fails first, the
    // create closes its own session and this close finds nothing; either way the rest holds.
    wait_until(&mut state, "the close answers", |_| closed.try_recv().ok());
    let second = wait_until(&mut state, "the second create answers", |_| {
        second.try_recv().ok()
    });
    assert!(
        matches!(&first, CommandOutcome::Failed { code, .. } if code == "session_start_failed"),
        "{first:?}"
    );
    assert!(
        matches!(second, CommandOutcome::Success { .. }),
        "{second:?}"
    );
    assert!(
        state
            .mux()
            .all_sessions()
            .iter()
            .any(|session| session.name == "agent"),
        "the recreated session keeps running"
    );
}

/// Saved metadata commands complete synchronously through the same socket mailbox.
fn saved_metadata_command(
    state: &mut AppState,
    command: &str,
    arguments: Vec<String>,
    binding: serde_json::Value,
) -> CommandOutcome {
    let mut invocation = targeted(command, arguments, binding);
    if command == "session.delete" {
        invocation.confirmation = Some(invocation.confirmation());
    }
    submit_and_wait(state, invocation)
}

fn saved_metadata_listing(
    state: &mut AppState,
    binding: serde_json::Value,
) -> Vec<serde_json::Value> {
    let CommandOutcome::Success { value, .. } =
        saved_metadata_command(state, "session.saved", Vec::new(), binding)
    else {
        panic!("saved metadata listing failed");
    };
    value.as_array().expect("saved sessions").clone()
}

struct SavedLifecycleWorkspace {
    config: BoottyConfig,
    native: bootty_mux::native::NativeBackend,
    state: AppState,
}

fn retained_create(
    binding: &serde_json::Value,
    identity: &str,
    cwd: &str,
    argv: &serde_json::Value,
    title: &str,
) -> CommandInvocation {
    targeted(
        "session.create",
        vec![
            "retained-task".to_owned(),
            cwd.to_owned(),
            argv.to_string(),
            identity.to_owned(),
            title.to_owned(),
        ],
        binding.clone(),
    )
}

fn saved_lifecycle_workspace(directory: &Path, attached: bool) -> SavedLifecycleWorkspace {
    let config_path = directory.join("config.toml");
    let cwd = directory.to_string_lossy().into_owned();
    let (mut repository, space) = claim_first_space(&config_path, "saved", "shared-title", &cwd);
    let mut membership = space.binding().sessions().clone();
    membership.claim(claimed_session("saved", "shared-title", &cwd));
    membership.claim(claimed_session("sibling", "shared-title", &cwd));
    repository
        .commit_binding_state(space.id(), &membership)
        .expect("persist sibling");
    create_space(
        &mut repository,
        "Other",
        "2",
        [1, 2, 3],
        SpaceMuxOverride::default(),
    );
    drop(repository);
    let config = test_config::config(config_path.clone());
    let native = bootty_mux::native::NativeBackend::for_workspace(&config_path);
    if attached {
        native
            .execute(MuxCommand::CreateProjectSession {
                session_id: "backend-id".to_owned(),
                cwd,
                tag: MuxSessionTag {
                    identity: Some("saved".to_owned()),
                    space: Some(space.remote_id().to_owned()),
                },
                argv: None,
            })
            .expect("native saved attachment");
        native
            .execute(MuxCommand::RenameSession {
                session_id: "backend-id".to_owned(),
                name: "shared-title".to_owned(),
            })
            .unwrap();
    }
    let state = app_state(config.clone());
    SavedLifecycleWorkspace {
        config,
        native,
        state,
    }
}

#[rstest]
fn saved_retry_cannot_claim_an_identity_owned_by_another_space(directory: assert_fs::TempDir) {
    let mut state = saved_lifecycle_workspace(directory.path(), true).state;
    let mut spaces = listed_spaces(&mut state);
    let (_, _, home) = spaces.remove(0);
    let (_, _, other) = spaces.remove(0);
    let before = saved_metadata_listing(&mut state, home.clone());
    let rejected = submit_and_wait(
        &mut state,
        retained_create(
            &other,
            "saved",
            &directory.path().to_string_lossy(),
            &serde_json::json!([]),
            "Replacement",
        ),
    );
    assert!(
        matches!(rejected, CommandOutcome::Unavailable { .. }),
        "{rejected:?}"
    );
    assert_eq!(saved_metadata_listing(&mut state, home), before);
    assert_eq!(
        saved_metadata_listing(&mut state, other),
        Vec::<serde_json::Value>::new()
    );
}

#[rstest]
#[case::command("close_session")]
#[case::wire("session.close")]
#[case::server("server")]
fn closing_releases_session_membership(directory: assert_fs::TempDir, #[case] command: &str) {
    let SavedLifecycleWorkspace {
        config,
        mut state,
        native,
    } = saved_lifecycle_workspace(directory.path(), true);
    let binding = listed_spaces(&mut state)[0].2.clone();
    if command == "server" {
        native
            .execute(MuxCommand::DitchSession {
                session_id: "backend-id".into(),
            })
            .unwrap();
        wait_until(&mut state, "ended session disappears", |state| {
            state.mux().sessions().is_empty().then_some(())
        });
    } else {
        let target = serde_json::json!({ "kind": "session", "handle": serde_json::to_string(&[binding["handle"].as_str().unwrap(), "backend-id"]).unwrap(), "generation": binding["generation"] });
        let mut invocation = targeted(command, Vec::new(), target);
        invocation.confirmation = Some(invocation.confirmation());
        let outcome = submit_and_wait(&mut state, invocation);
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    assert_eq!(native.snapshot().unwrap().sessions, []);
    assert_eq!(
        saved_metadata_listing(&mut state, binding),
        Vec::<serde_json::Value>::new()
    );
    drop(state);
    let mut reopened = app_state(config);
    let binding = listed_spaces(&mut reopened)[0].2.clone();
    assert_eq!(
        saved_metadata_listing(&mut reopened, binding),
        Vec::<serde_json::Value>::new()
    );
}

#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
fn selecting_existing_content_dismisses_creation(
    directory: assert_fs::TempDir,
    #[case] select_tab: bool,
    #[case] current_tab_binding: bool,
) {
    let mut config = test_config::config(directory.path().join("config.toml"));
    config.input.keybind.push("alt+1=select_tab:1".into());
    let cwd = directory.path().to_string_lossy().into_owned();
    let (repository, space) = claim_first_space(&config.config_path, "task", "first", &cwd);
    drop(repository);
    let native = bootty_mux::native::NativeBackend::for_workspace(&config.config_path);
    native
        .execute(MuxCommand::CreateProjectSession {
            session_id: "first".to_owned(),
            cwd,
            tag: MuxSessionTag {
                identity: Some("task".to_owned()),
                space: Some(space.remote_id().to_owned()),
            },
            argv: None,
        })
        .unwrap();
    native
        .execute(MuxCommand::NewWindow {
            session_id: "first".to_owned(),
            cwd: None,
            argv: None,
        })
        .unwrap();
    native
        .execute(MuxCommand::ActivateWindow {
            session_id: "first".to_owned(),
            window_id: "tab-1".to_owned(),
        })
        .unwrap();
    let mut state = app_state(config);
    state.update_frame(frames::idle_frame(Instant::now()));
    let open = |state: &mut AppState| {
        let outcome = submit_command(
            state,
            CommandInvocation::new("new_mux_session", Vec::new(), Caller::Socket),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    };
    open(&mut state);
    assert_eq!(
        state.keymap_focus(),
        bootty_ui::keymap_runtime::KeymapFocus::Other
    );
    if current_tab_binding {
        let mut frame = frames::idle_frame(Instant::now());
        frame.input.events.push(bootty_ui::gpui::InputEvent::Key {
            key: bootty_ui::gpui::Key::Digit(1),
            pressed: true,
            repeat: false,
            modifiers: bootty_ui::gpui::Modifiers {
                alt: true,
                ..Default::default()
            },
        });
        state.update_frame(frame);
        let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
        while state.modal_dialog().is_some() {
            state.update_frame(frames::idle_frame(Instant::now()));
            assert!(
                Instant::now() < deadline,
                "window selection did not finish: {:?}",
                state.last_error()
            );
            if state.modal_dialog().is_some() {
                std::thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
            }
        }
        assert_eq!(state.mux().selected_window(), Some("tab-1"));
    } else if select_tab {
        let outcome = submit_command(
            &mut state,
            CommandInvocation::new("next_tab", Vec::new(), Caller::Socket),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert_eq!(state.mux().selected_window(), Some("tab-2"));
    } else {
        assert!(
            state.activate_scoped_session_from_ui(&ScopedSessionTarget::new(
                state.mux_scope(),
                "first",
            ))
        );
    }
    assert!(state.modal_dialog().is_none(), "{:?}", state.last_error());
}

#[rstest]
fn failed_active_space_delete_preserves_selection(
    directory: assert_fs::TempDir,
    #[values(true, false)] reject_delete: bool,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone());
    let (mut repository, snapshot) = WorkspaceRepository::open(&config_path).unwrap();
    let first = snapshot.spaces()[0].id();
    create_space(
        &mut repository,
        "Second",
        "2",
        [0x22, 0x44, 0x66],
        SpaceMuxOverride::default(),
    );
    repository.set_selected_space("close-test", first).unwrap();
    let mut state = AppState::new_for_window(
        config,
        "close-test".to_owned(),
        repaint_on_event(),
        None,
        None,
    )
    .unwrap();
    let database = Connection::open(directory.path().join("session-order.sqlite3")).unwrap();
    let trigger = if reject_delete {
        "CREATE TRIGGER reject_space_delete BEFORE DELETE ON workspace_spaces
         BEGIN SELECT RAISE(ABORT, 'Space close rejected'); END;"
    } else {
        "CREATE TRIGGER reject_space_selection BEFORE INSERT ON workspace_window_state
         BEGIN SELECT RAISE(ABORT, 'Space close rejected'); END;"
    };
    database.execute_batch(trigger).unwrap();

    assert!(!state.close_space_from_ui(first));
    assert_eq!(state.active_space_id(), first);
    assert_eq!(state.space_summaries().len(), 2);
    assert!(state.last_error().unwrap().contains("Space close rejected"));
    let (_, reopened) = WorkspaceRepository::open(&config_path).unwrap();
    assert_eq!(reopened.selected_space("close-test"), Some(first));
    assert_eq!(reopened.spaces().len(), 2);

    let mut invocation = CommandInvocation::from_action("close_space", Caller::Socket);
    let outcome = submit_command(&mut state, invocation.clone(), Instant::now());
    let CommandOutcome::ConfirmationRequired { confirmation } = outcome else {
        panic!("expected close confirmation: {outcome:?}");
    };
    invocation.target.clone_from(&confirmation.target);
    invocation.confirmation = Some(*confirmation);
    let outcome = submit_command(&mut state, invocation, Instant::now());
    assert!(
        matches!(&outcome, CommandOutcome::Failed { message, .. } if message.contains("Space close rejected")),
        "report the actual persistence failure: {outcome:?}"
    );
    assert_eq!(state.active_space_id(), first);
}

#[rstest]
#[case::first(0, 1)]
#[case::middle(1, 2)]
#[case::last(2, 1)]
fn closing_active_space_selects_the_neighbor_and_preserves_the_last_space(
    directory: assert_fs::TempDir,
    #[case] closing: usize,
    #[case] neighbor: usize,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone());
    let (mut repository, snapshot) = WorkspaceRepository::open(&config_path).unwrap();
    let spaces = [
        snapshot.spaces()[0].id(),
        create_space(
            &mut repository,
            "Second",
            "2",
            [2; 3],
            SpaceMuxOverride::default(),
        )
        .id(),
        create_space(
            &mut repository,
            "Third",
            "3",
            [3; 3],
            SpaceMuxOverride::default(),
        )
        .id(),
    ];
    repository
        .set_selected_space("close-test", spaces[closing])
        .unwrap();
    let mut state = AppState::new_for_window(
        config,
        "close-test".to_owned(),
        repaint_on_event(),
        None,
        None,
    )
    .unwrap();

    assert!(state.close_space_from_ui(spaces[closing]));
    assert_eq!(state.active_space_id(), spaces[neighbor]);
    let (_, reopened) = WorkspaceRepository::open(&config_path).unwrap();
    assert_eq!(
        reopened.selected_space("close-test"),
        Some(spaces[neighbor])
    );
    assert_eq!(reopened.spaces().len(), 2);
    assert!(
        reopened
            .spaces()
            .iter()
            .all(|space| space.id() != spaces[closing])
    );

    assert!(state.close_space_from_ui(spaces[neighbor]));
    let last = state.active_space_id();
    assert!(!state.close_space_from_ui(last));
    assert_eq!(
        state.last_error(),
        Some("the last space cannot be closed".to_owned())
    );
    assert_eq!(state.active_space_id(), last);
    assert_eq!(state.space_summaries().len(), 1);
    let (_, reopened) = WorkspaceRepository::open(&config_path).unwrap();
    assert_eq!(reopened.selected_space("close-test"), Some(last));
    assert_eq!(reopened.spaces().len(), 1);
}
