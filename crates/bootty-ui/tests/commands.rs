#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use pretty_assertions::assert_eq;
use rstest::rstest;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use bootty_config::config::MultiplexerBackendConfig;
use bootty_control::{
    AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
    CommandTarget, MutationClass, ResourceKind, ValueType,
};
use bootty_mux::repository::{SpaceMuxOverride, WorkspaceRepository};
use bootty_mux::workspace::{
    SESSION_ARGV_MAX_BYTES, SESSION_ARGV_MAX_ELEMENTS, SESSION_NAME_MAX_BYTES,
};
use bootty_terminal::geometry::SurfaceRect;
use bootty_ui::commands::{CommandCatalog, CommandExecutor};
use bootty_ui::{
    AppState, ModalDialog,
    presentation::dialogs::{DitchAction, DitchSessionEvent, NewSessionPickerEvent},
};
use rusqlite::Connection;

#[path = "support/idle_frames.rs"]
mod frames;
mod support;
#[path = "support/config.rs"]
mod test_config;

fn native_state(directory: &Path) -> AppState {
    let config = test_config::config(
        directory.join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    AppState::new(config, support::backends(), Arc::new(|| {}), None, None).expect("app state")
}

fn open_native_session(state: &mut AppState, cwd: &Path, started: Instant) {
    let outcome = submit_action(state, "new_mux_session", Caller::Socket, started);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::NewSession(_))
    ));
    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: cwd.to_string_lossy().into_owned(),
    });
    for tick in 1..5 {
        state.update_frame(frames::idle_frame(
            started
                .checked_add(Duration::from_millis(tick))
                .expect("test timestamp fits"),
        ));
    }
}

#[rstest]
fn creating_a_session_selects_it_and_keeps_it_selected_after_refresh() {
    let directory = assert_fs::TempDir::new().expect("isolated config");
    let mut state = native_state(directory.path());
    let started = Instant::now();
    for name in ["first", "second", "third"] {
        let cwd = directory.path().join(name);
        fs::create_dir(&cwd).expect("project directory");
        open_native_session(&mut state, &cwd, started);
        assert_eq!(state.mux().selected_session(), Some(name));
        assert!(state.terminal_focused());
    }
}

fn pane_count(state: &AppState) -> usize {
    state
        .pane_rects(SurfaceRect::from_min_size(0.0, 0.0, 200.0, 100.0), 4.0)
        .len()
}

#[rstest]
#[case(false)]
#[case(true)]
fn modal_dismissal_restores_the_underlying_input_route(#[case] sidebar: bool) {
    let directory = assert_fs::TempDir::new().expect("isolated config");
    let mut state = native_state(directory.path());
    let started = Instant::now();
    if sidebar {
        submit_action(
            &mut state,
            "toggle_sidebar_focus",
            Caller::Keybinding,
            started,
        );
    }
    for _ in 0..3 {
        assert!(state.open_session_picker_dialog_from_ui());
        assert!(!state.terminal_focused());
        assert_eq!(
            state.keymap_focus(),
            bootty_ui::keymap_runtime::KeymapFocus::Command
        );
        let Some(bootty_ui::presentation::dialogs::DialogProjection::Dialog(spec)) =
            state.dialog_projection()
        else {
            panic!("session picker projection");
        };
        state.apply_dialog_intent(
            &bootty_gpui::DialogIntent::Dismiss { dialog: spec.id },
            &mut Vec::new(),
        );
        assert!(state.modal_dialog().is_none());
        assert_eq!(state.terminal_focused(), !sidebar);
        assert_eq!(state.sidebar_focused(), sidebar);
    }
}

#[rstest]
#[case("doctor", MutationClass::Read, Some(ResourceKind::ApplicationWindow))]
#[case("git.status", MutationClass::Read, Some(ResourceKind::Binding))]
#[case("git.stage", MutationClass::Write, Some(ResourceKind::Binding))]
#[case("git.amend", MutationClass::Destructive, Some(ResourceKind::Binding))]
#[case("terminal.read", MutationClass::Read, Some(ResourceKind::Terminal))]
#[case("terminal.write", MutationClass::Write, Some(ResourceKind::Terminal))]
#[case("terminal.paste", MutationClass::Write, Some(ResourceKind::Terminal))]
#[case("terminal.submit", MutationClass::Write, Some(ResourceKind::Terminal))]
#[case("resource.current", MutationClass::Read, None)]
fn core_commands_publish_typed_descriptors(
    #[case] id: &str,
    #[case] mutation: MutationClass,
    #[case] target: Option<ResourceKind>,
) {
    let catalog = CommandCatalog::default();
    let descriptor = catalog.describe(id).expect("core command descriptor");
    assert_eq!((descriptor.mutation, descriptor.target), (mutation, target));
}

#[rstest]
#[case("toggle_search_regex", "regex")]
#[case("toggle_search_case_sensitive", "case_sensitive")]
fn find_switches_share_command_and_button_state(
    #[case] command: &str,
    #[case] row: &str,
    #[values(
        Caller::Cli,
        Caller::Socket,
        Caller::Keybinding,
        Caller::CommandPalette
    )]
    caller: Caller,
) {
    let directory = assert_fs::TempDir::new().expect("isolated workspace");
    let mut state = native_state(directory.path());
    let started = Instant::now();
    open_native_session(&mut state, directory.path(), started);
    let outcome = submit_action(&mut state, command, caller, started);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let spec = state.terminal_find_projection().expect("find bar");
    assert!(
        spec.rows
            .iter()
            .find(|candidate| candidate.id.0 == row)
            .expect("switch")
            .current
    );
    state.apply_terminal_find_dialog_intent(&bootty_gpui::DialogIntent::Activate {
        dialog: spec.id,
        row: bootty_gpui::RowId::new(row),
        action: bootty_gpui::ActionId::new(row),
        payload: bootty_gpui::DialogPayload::default(),
    });
    state.update_frame(frames::idle_frame(
        started
            .checked_add(Duration::from_millis(10))
            .expect("test timestamp fits"),
    ));
    let spec = state
        .terminal_find_projection()
        .expect("find bar remains open");
    assert!(
        !spec
            .rows
            .iter()
            .find(|candidate| candidate.id.0 == row)
            .expect("switch")
            .current
    );
}

#[test]
fn core_command_resolution_reports_executor_and_argument_boundaries() {
    let catalog = CommandCatalog::default();
    assert!(matches!(
        catalog
            .resolve(CommandInvocation::from_action(
                "terminal.read",
                Caller::Socket,
            ))
            .expect("resolve core command")
            .executor,
        CommandExecutor::Core(_)
    ));
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action(
            "terminal.write",
            Caller::Socket,
        )),
        Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
    ));
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action(
            "missing.command",
            Caller::Socket,
        )),
        Err(CommandOutcome::Failed { code, .. }) if code == "unknown_command"
    ));
}

#[rstest]
#[case("orchestration.run.remove", vec!["missing-run".to_owned()])]
fn coordination_destructive_commands_require_exact_confirmation(
    #[case] command: &str,
    #[case] arguments: Vec<String>,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "main".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )
    .unwrap();
    let mut invocation = CommandInvocation::new(command, arguments, Caller::Socket);
    let started = Instant::now();
    let outcome = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        invocation.clone(),
        started,
    );
    let CommandOutcome::ConfirmationRequired { confirmation } = outcome else {
        panic!("{outcome:?}");
    };
    invocation.confirmation = Some(*confirmation);
    let authorized =
        submit_command_from_caller(&mut state, &wakes, Caller::Socket, invocation, started);
    assert!(
        !matches!(authorized, CommandOutcome::ConfirmationRequired { .. }),
        "{authorized:?}"
    );
    assert!(
        !matches!(authorized, CommandOutcome::Success { .. }),
        "Missing session/run must never mutate"
    );
}

#[test]
fn command_specs_keep_presentation_policy_and_arguments_together() {
    let catalog = CommandCatalog::default();

    let appearance = catalog
        .describe("change_appearance")
        .expect("appearance command");
    assert_eq!(appearance.title, "Change Appearance");
    assert_eq!(appearance.mutation, MutationClass::Write);
    assert_eq!(appearance.target, Some(ResourceKind::ApplicationWindow));
    let [appearance_argument] = appearance.arguments.arguments.as_slice() else {
        panic!("appearance argument schema: {:?}", appearance.arguments);
    };
    assert_eq!(appearance_argument.name, "appearance");
    assert_eq!(appearance_argument.value_type, ValueType::String);
    assert!(appearance_argument.required);
    assert_eq!(appearance_argument.choices, ["system", "light", "dark"]);

    let clipboard = catalog
        .describe("copy_to_clipboard")
        .expect("clipboard command");
    let [clipboard_argument] = clipboard.arguments.arguments.as_slice() else {
        panic!("clipboard argument schema: {:?}", clipboard.arguments);
    };
    assert!(!clipboard_argument.required);
    assert_eq!(clipboard_argument.choices, ["plain", "vt", "html", "mixed"]);

    assert!(catalog.describe("move_tab").expect("move tab").palette);
    assert!(!catalog.describe("select_tab").expect("select tab").palette);
    assert_eq!(
        catalog
            .describe("close_surface")
            .expect("close pane")
            .mutation,
        MutationClass::Destructive
    );
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action(
            "change_appearance:sepia",
            Caller::Socket,
        )),
        Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
    ));
    assert!(matches!(
        catalog.resolve(CommandInvocation::from_action("select_tab:0", Caller::Socket)),
        Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
    ));
    assert!(
        catalog
            .resolve(CommandInvocation::from_action(
                "copy_to_clipboard",
                Caller::Socket,
            ))
            .is_ok()
    );
}

#[test]
fn discovered_resource_target_cannot_retarget_a_replacement_binding() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let started = Instant::now();
    let mut first = AppState::new(
        config.clone(),
        support::backends(),
        Arc::new(|| {}),
        None,
        None,
    )
    .expect("first app state");
    let current = submit_command(
        &mut first,
        CommandInvocation::new(
            "resource.current",
            vec!["binding".to_owned()],
            Caller::Socket,
        ),
        started,
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("current binding outcome: {current:?}");
    };
    let target: CommandTarget =
        serde_json::from_value(value["target"].clone()).expect("current binding target");
    drop(first);

    let mut replacement = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("replacement app state");
    let mut invocation = CommandInvocation::new("edit_space", Vec::new(), Caller::Socket);
    invocation.target = Some(target);
    let outcome = submit_command(
        &mut replacement,
        invocation,
        started
            .checked_add(Duration::from_millis(20))
            .expect("test timestamp fits"),
    );
    assert!(matches!(outcome, CommandOutcome::StaleTarget { .. }));
}

#[test]
fn native_split_command_publishes_the_binding_owned_layout() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    open_native_session(&mut state, directory.path(), started);

    let outcome = submit_action(
        &mut state,
        "split_right",
        Caller::Socket,
        started
            .checked_add(Duration::from_millis(10))
            .expect("test timestamp fits"),
    );

    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(state.native_multi_pane());
    assert_eq!(pane_count(&state), 2);
    assert!(state.focused_pane().is_some());

    let direct = submit_action(
        &mut state,
        "split_right",
        Caller::Keybinding,
        started
            .checked_add(Duration::from_millis(15))
            .expect("test timestamp fits"),
    );
    assert!(
        matches!(direct, CommandOutcome::Success { .. }),
        "{direct:?}"
    );
    assert_eq!(pane_count(&state), 3);

    let unsupported = submit_action(
        &mut state,
        "toggle_pane_zoom",
        Caller::Socket,
        started
            .checked_add(Duration::from_millis(20))
            .expect("test timestamp fits"),
    );
    assert!(matches!(unsupported, CommandOutcome::Unsupported { .. }));
    assert_eq!(pane_count(&state), 3);
}

#[test]
fn ditch_session_commits_membership_after_authoritative_command() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    let created = submit_action(&mut state, "new_tab", Caller::Socket, started);
    assert!(
        matches!(created, CommandOutcome::Success { .. }),
        "{created:?}"
    );

    let target = (0..250)
        .find_map(|tick| {
            state.update_frame(frames::idle_frame(
                started
                    .checked_add(Duration::from_millis(
                        250_u64.checked_add(tick).expect("test tick fits"),
                    ))
                    .expect("test timestamp fits"),
            ));
            std::thread::sleep(Duration::from_millis(1));
            state
                .binding_session_groups()
                .into_iter()
                .find_map(|group| group.sessions.first().map(|session| group.target(session)))
        })
        .expect("native session becomes available");
    let original_name = state
        .binding_session_groups()
        .iter()
        .flat_map(|group| group.sessions.iter())
        .find(|session| session.id == target.session_id)
        .expect("live session")
        .name
        .clone();

    assert!(state.open_ditch_session_dialog_for(&target.session_id));
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::DitchSession(_))
    ));
    state.apply_ditch_session_event(DitchSessionEvent::Ditch {
        session_id: target.session_id.clone(),
        cwd: None,
        action: DitchAction::KillOnly,
    });

    let removed = (0..250).any(|tick| {
        state.update_frame(frames::idle_frame(
            started
                .checked_add(Duration::from_millis(500 + tick))
                .expect("test timestamp fits"),
        ));
        std::thread::sleep(Duration::from_millis(1));
        !state
            .binding_session_groups()
            .iter()
            .flat_map(|group| group.sessions.iter())
            .any(|session| session.id == target.session_id)
    });
    assert!(
        removed,
        "authoritative ditch result must remove the live session"
    );

    drop(state);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    assert!(
        !reopened.spaces()[0]
            .binding()
            .sessions()
            .backend_names()
            .contains(&original_name)
    );
}

#[test]
fn ditch_submits_after_worktree_removal_when_branch_deletion_fails() {
    let (_repository, main, worktree, duplicate) = repo_with_duplicate_branch();
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    open_native_session(&mut state, &worktree, started);
    let session_id = state.mux().sessions()[0].id.clone();

    let cwd = worktree.to_string_lossy().into_owned();
    let action = DitchAction::RemoveWorktreeAndBranch {
        force: true,
        branch: "feature".to_owned(),
        repo: main.to_string_lossy().into_owned(),
    };
    let ditch_event = || DitchSessionEvent::Ditch {
        session_id: session_id.clone(),
        cwd: Some(cwd.clone()),
        action: action.clone(),
    };
    let database = directory.path().join("session-order.sqlite3");
    let lock = Connection::open(&database).expect("open lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold workspace write lock");
    assert!(state.open_ditch_session_dialog_for(&session_id));
    state.apply_ditch_session_event(ditch_event());
    assert!(
        worktree.exists(),
        "failed Ditch preparation must not remove the worktree"
    );
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::DitchSession(_))
    ));
    lock.execute_batch("ROLLBACK")
        .expect("release workspace write lock");
    drop(lock);

    state.apply_ditch_session_event(ditch_event());

    assert!(
        (0..250).any(|tick| {
            state.update_frame(frames::idle_frame(
                started
                    .checked_add(Duration::from_millis(10 + tick))
                    .expect("test timestamp fits"),
            ));
            std::thread::sleep(Duration::from_millis(1));
            !state
                .binding_session_groups()
                .iter()
                .flat_map(|group| group.sessions.iter())
                .any(|session| session.id == session_id)
        }),
        "partial cleanup must still submit Ditch"
    );
    assert!(!worktree.exists(), "ditch must remove the linked worktree");
    assert!(
        state
            .last_error()
            .is_some_and(|warning| warning.contains("branch 'feature' remains")),
        "partial cleanup warning must name the remaining branch"
    );
    assert!(duplicate.exists(), "duplicate branch checkout must remain");
    assert!(git_read(&main, &["branch", "--list", "feature"]).contains("feature"));
}

fn repo_with_duplicate_branch() -> (assert_fs::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = assert_fs::TempDir::new().expect("temporary repository root");
    let main = root.path().join("main");
    let worktree = root.path().join("worktree");
    let duplicate = root.path().join("duplicate");
    fs::create_dir(&main).expect("create main worktree");
    git_ok(&main, &["init", "-q", "-b", "main"]);
    git_ok(&main, &["config", "user.email", "test@bootty.dev"]);
    git_ok(&main, &["config", "user.name", "Bootty Test"]);
    fs::write(main.join("README"), "hello").expect("write initial file");
    git_ok(&main, &["add", "."]);
    git_ok(&main, &["commit", "-q", "-m", "init"]);
    git_ok(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().expect("UTF-8 worktree path"),
        ],
    );
    git_ok(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "--force",
            duplicate.to_str().expect("UTF-8 duplicate path"),
            "feature",
        ],
    );
    (root, main, worktree, duplicate)
}

fn git_ok(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_read(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn native_window_actions_use_the_binding_owned_plan() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    open_native_session(&mut state, directory.path(), started);

    let session = state.mux().sessions()[0].clone();
    let first_window = session.windows[0].id.clone();
    let outcome = submit_action(
        &mut state,
        "new_tab",
        Caller::Keybinding,
        started
            .checked_add(Duration::from_millis(5))
            .expect("test timestamp fits"),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("new tab outcome: {outcome:?}");
    };
    let created: CommandTarget =
        serde_json::from_value(value["created"].clone()).expect("created terminal target");
    assert_eq!(created.kind, ResourceKind::Terminal);
    for tick in 5..10 {
        state.update_frame(frames::idle_frame(
            started
                .checked_add(Duration::from_millis(tick))
                .expect("test timestamp fits"),
        ));
    }

    let session = state
        .mux()
        .sessions()
        .iter()
        .find(|candidate| candidate.id == session.id)
        .expect("created session");
    assert_eq!(session.windows.len(), 2);
    let second_window = session
        .windows
        .iter()
        .find(|window| window.id != first_window)
        .expect("new window")
        .id
        .clone();
    let outcome = submit_action(
        &mut state,
        "next_tab",
        Caller::Keybinding,
        started
            .checked_add(Duration::from_millis(10))
            .expect("test timestamp fits"),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.mux().selected_window(), Some(first_window.as_str()));
    let current = submit_command(
        &mut state,
        CommandInvocation::new(
            "resource.current",
            vec!["terminal".to_owned()],
            Caller::Socket,
        ),
        started
            .checked_add(Duration::from_millis(10))
            .expect("test timestamp fits"),
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("current terminal outcome: {current:?}");
    };
    let first_terminal: CommandTarget =
        serde_json::from_value(value["target"].clone()).expect("first terminal target");
    let outcome = submit_action(
        &mut state,
        "last_tab",
        Caller::Keybinding,
        started
            .checked_add(Duration::from_millis(11))
            .expect("test timestamp fits"),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.mux().selected_window(), Some(second_window.as_str()));

    let mut write = CommandInvocation::new("terminal.write", vec![" ".to_owned()], Caller::Socket);
    write.target = Some(first_terminal);
    let outcome = submit_command(
        &mut state,
        write,
        started
            .checked_add(Duration::from_millis(12))
            .expect("test timestamp fits"),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    // A targeted write reaches its pane's own terminal and leaves the selection alone.
    assert_eq!(state.mux().selected_window(), Some(second_window.as_str()));
}

#[test]
fn a_failed_socket_command_reaches_its_caller_not_the_window() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    open_native_session(&mut state, directory.path(), started);
    let mut paste = CommandInvocation::new("terminal.paste", vec!["hi".to_owned()], Caller::Socket);
    paste.target = Some(CommandTarget {
        kind: ResourceKind::Terminal,
        handle: r#"["gone","$9","@9","%9"]"#.to_owned(),
        generation: 1,
    });
    let outcome = submit_command(&mut state, paste, started);
    assert!(
        matches!(outcome, CommandOutcome::StaleTarget { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.last_error(), None);
}

#[rstest]
#[case(1, true)]
#[case(2, false)]
fn close_window_policy_only_applies_to_the_last_workspace_surface(
    #[case] session_count: usize,
    #[case] closes_window: bool,
    #[values(Caller::Keybinding, Caller::Socket)] caller: Caller,
) {
    let directory = assert_fs::TempDir::new().expect("isolated workspace");
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.when_closing_with_no_tabs = bootty_config::config::WhenClosingWithNoTabs::CloseWindow;
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).unwrap();
    let started = Instant::now();
    for index in 0..session_count {
        let cwd = directory.path().join(format!("session-{index}"));
        fs::create_dir(&cwd).unwrap();
        open_native_session(&mut state, &cwd, started);
    }
    assert_eq!(state.mux().sessions().len(), session_count);
    let mut invocation = CommandInvocation::from_action("close_surface", caller);
    if caller == Caller::Socket {
        let outcome = submit_command(&mut state, invocation.clone(), started);
        let CommandOutcome::ConfirmationRequired { confirmation } = outcome else {
            panic!("socket close should require confirmation: {outcome:?}");
        };
        invocation.command.clone_from(&confirmation.command);
        invocation.arguments.clone_from(&confirmation.arguments);
        invocation.target.clone_from(&confirmation.target);
        invocation.confirmation = Some(*confirmation);
    }
    let receiver = state
        .app_command_sender(caller)
        .submit(
            invocation,
            started
                .checked_add(Duration::from_secs(1))
                .expect("test timestamp fits"),
            CommandCancellation::new(),
        )
        .unwrap();
    let mut closed = false;
    let outcome = (0..100)
        .find_map(|tick| {
            closed |= state
                .update_frame(frames::idle_frame(
                    started
                        .checked_add(Duration::from_millis(tick))
                        .expect("test timestamp fits"),
                ))
                .iter()
                .any(|effect| matches!(effect, bootty_ui::AppEffect::CloseWindow));
            receiver.try_recv().ok()
        })
        .expect("close command outcome");
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(closed, closes_window);
    if !closes_window {
        assert_eq!(
            state
                .mux()
                .sessions()
                .iter()
                .map(|session| session.windows.len())
                .sum::<usize>(),
            1
        );
    }
}

fn submit_command(
    state: &mut AppState,
    invocation: CommandInvocation,
    started: Instant,
) -> CommandOutcome {
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation,
            deadline: started
                .checked_add(Duration::from_secs(1))
                .expect("test timestamp fits"),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");
    (0..100)
        .find_map(|tick| {
            state.update_frame(frames::idle_frame(
                started
                    .checked_add(Duration::from_millis(tick))
                    .expect("test timestamp fits"),
            ));
            // A native create answers once its pane's process has started.
            outcomes.recv_timeout(Duration::from_millis(5)).ok()
        })
        .expect("command outcome")
}

fn submit_action(
    state: &mut AppState,
    action: &str,
    caller: Caller,
    started: Instant,
) -> CommandOutcome {
    submit_command(
        state,
        CommandInvocation::from_action(action, caller),
        started,
    )
}

fn submit_command_from_caller(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    caller: Caller,
    invocation: CommandInvocation,
    started: Instant,
) -> CommandOutcome {
    let commands = state.app_command_sender(caller);
    let outcomes = commands
        .submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(5))
                .expect("test timestamp fits"),
            CommandCancellation::new(),
        )
        .expect("submit command");
    loop {
        state.update_frame(frames::idle_frame(started));
        if let Ok(outcome) = outcomes.try_recv() {
            return outcome;
        }
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("command worker publishes completion");
    }
}

#[rstest]
#[case(Caller::Socket)]
#[case(Caller::Keybinding)]
fn git_commands_complete_through_the_shared_mailbox(#[case] caller: Caller) {
    let directory = assert_fs::TempDir::new().unwrap();
    let root = directory.path().to_string_lossy().into_owned();
    assert!(
        Command::new("git")
            .args(["-C", &root, "init", "--quiet"])
            .status()
            .unwrap()
            .success()
    );
    fs::write(directory.path().join("file[a].txt"), "contents\n").unwrap();
    let (wake, wakes) = mpsc::channel();
    let config = test_config::config(
        directory.path().join("settings/config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let mut state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .unwrap();
    let started = Instant::now();
    let mut run = |command: &str, arguments: Vec<String>| {
        let mut invocation = CommandInvocation::from_action(command, caller);
        invocation.arguments = arguments;
        let response = state
            .app_command_sender(caller)
            .submit(
                invocation,
                started
                    .checked_add(Duration::from_secs(5))
                    .expect("test timestamp fits"),
                CommandCancellation::new(),
            )
            .unwrap();
        loop {
            state.update_frame(frames::idle_frame(started));
            if let Ok(outcome) = response.try_recv() {
                break outcome;
            }
            wakes
                .recv_timeout(Duration::from_secs(5))
                .expect("worker publishes completion");
        }
    };
    assert!(matches!(
        run("git.stage", vec![root.clone(), "file[a].txt".to_owned()]),
        CommandOutcome::Success { .. }
    ));
    let CommandOutcome::Success { value, .. } = run("git.status", vec![root.clone()]) else {
        panic!("Git status failed");
    };
    let changes: bootty_git::changes::RepositoryChanges = serde_json::from_value(value).unwrap();
    let file = changes
        .files
        .iter()
        .find(|file| file.path == "file[a].txt")
        .unwrap();
    assert_eq!(file.index, 'A');
    assert_eq!(
        fs::read_to_string(directory.path().join("file[a].txt")).unwrap(),
        "contents\n"
    );
    assert!(
        Command::new("git")
            .args([
                "-C",
                &root,
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "initial"
            ])
            .status()
            .unwrap()
            .success()
    );
    let name = format!(
        "{}-checkout",
        directory.path().file_name().unwrap().to_str().unwrap()
    );
    let CommandOutcome::Success { value, .. } = run(
        "worktree.create",
        vec![
            root.clone(),
            "topic/new".to_owned(),
            name,
            "HEAD".to_owned(),
        ],
    ) else {
        panic!("worktree creation failed");
    };
    let checkout = value.as_str().unwrap();
    assert_eq!(
        fs::read_to_string(std::path::Path::new(checkout).join("file[a].txt")).unwrap(),
        "contents\n"
    );
    assert!(
        Command::new("git")
            .args(["-C", &root, "worktree", "remove", "--", checkout])
            .status()
            .unwrap()
            .success()
    );
    // Amend keeps the existing confirmation contract for external callers.
    if caller == Caller::Socket {
        assert!(matches!(
            run("git.amend", vec![root, "message".to_owned()]),
            CommandOutcome::ConfirmationRequired { .. }
        ));
    }
}

#[rstest]
#[case(Caller::Socket)]
#[case(Caller::Keybinding)]
fn document_commands_keep_revision_checks_on_the_shared_invocation_path(#[case] caller: Caller) {
    use bootty_host::files::{FileResponse, decode_document, encode_document};
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("document.rs");
    fs::write(&path, "original").unwrap();
    let name = path.to_str().unwrap().to_owned();
    let (wake, wakes) = mpsc::channel();
    let config = test_config::config(
        directory.path().join("settings/config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let mut state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .unwrap();
    let started = Instant::now();
    let mut run = |command: &str, arguments: Vec<String>| {
        let mut invocation = CommandInvocation::from_action(command, caller);
        invocation.arguments = arguments;
        let response = state
            .app_command_sender(caller)
            .submit(
                invocation,
                started
                    .checked_add(Duration::from_secs(5))
                    .expect("test timestamp fits"),
                CommandCancellation::new(),
            )
            .unwrap();
        loop {
            state.update_frame(frames::idle_frame(started));
            if let Ok(outcome) = response.try_recv() {
                break outcome;
            }
            wakes
                .recv_timeout(Duration::from_secs(5))
                .expect("worker completion");
        }
    };
    let CommandOutcome::Success { value, .. } = run("files.read", vec![name.clone()]) else {
        panic!("read");
    };
    let FileResponse::Document(snapshot) = serde_json::from_value(value).unwrap() else {
        panic!("document");
    };
    assert_eq!(snapshot.contents().unwrap(), "original");
    let CommandOutcome::Success { value, .. } = run(
        "files.format",
        vec![
            name.clone(),
            encode_document("fn main(){println!(\"hi\");}").unwrap(),
        ],
    ) else {
        panic!("format");
    };
    let FileResponse::Formatted { content_base64 } = serde_json::from_value(value).unwrap() else {
        panic!("formatted document");
    };
    assert_eq!(
        decode_document(&content_base64).unwrap(),
        "fn main() {\n    println!(\"hi\");\n}\n"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    let arguments = vec![name, snapshot.digest, encode_document("saved").unwrap()];
    assert!(matches!(
        run("files.save", arguments.clone()),
        CommandOutcome::Success { .. }
    ));
    fs::write(&path, "external").unwrap();
    assert!(
        matches!(run("files.save",arguments),CommandOutcome::Failed{code,..} if code=="file_failed")
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "external");
    let image = b"\x89PNG\r\n\x1a\n\0\xff";
    fs::write(&path, image).unwrap();
    let CommandOutcome::Success { value, .. } =
        run("files.read", vec![path.to_string_lossy().into_owned()])
    else {
        panic!("read image")
    };
    let FileResponse::Media(snapshot) = serde_json::from_value(value).unwrap() else {
        panic!("image");
    };
    assert_eq!(snapshot.len, u64::try_from(image.len()).unwrap());
    assert_eq!(snapshot.kind, bootty_host::media::MediaKind::Image);
}

#[rstest]
fn native_pane_arrangement_preserves_ids_and_publishes_only_completed_layouts() {
    let directory = assert_fs::TempDir::new().expect("isolated workspace");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    open_native_session(&mut state, directory.path(), started);
    let first_window = state.mux().selected_window().expect("window").to_owned();
    let first = state.focused_pane().expect("first pane");
    assert!(matches!(
        submit_action(&mut state, "split_right", Caller::Socket, started),
        CommandOutcome::Success { .. }
    ));
    let second = state.focused_pane().expect("second pane");
    let area = SurfaceRect::from_min_size(0., 0., 600., 400.);
    state.set_pane_ratio(&[], 0.3, 0.05);
    let before = state.pane_rects(area, 0.);
    let invoke = |state: &mut AppState, command: &str, args: Vec<String>| {
        let outcome = submit_command(
            state,
            CommandInvocation::new(command, args, Caller::Socket),
            started,
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    };
    invoke(&mut state, "pane.swap", vec![first.clone(), second.clone()]);
    let swapped = state.pane_rects(area, 0.);
    assert_eq!(swapped[0], (second.clone(), before[0].1));
    assert_eq!(swapped[1], (first.clone(), before[1].1));
    assert_eq!(state.focused_pane(), Some(first.clone()));
    invoke(
        &mut state,
        "pane.move",
        vec![first.clone(), second.clone(), "down".to_owned()],
    );
    let moved = state.pane_rects(area, 0.);
    assert_eq!(moved[0].0, second);
    assert_eq!(moved[1].0, first);
    assert!(moved[0].1.max_y <= moved[1].1.min_y);
    let rejected = submit_command(
        &mut state,
        CommandInvocation::new(
            "pane.move",
            vec![first.clone(), "absent".to_owned(), "left".to_owned()],
            Caller::Socket,
        ),
        started,
    );
    assert!(
        matches!(rejected, CommandOutcome::Failed { .. }),
        "{rejected:?}"
    );
    assert_eq!(state.pane_rects(area, 0.), moved);
    invoke(&mut state, "pane.extract", vec![first.clone()]);
    let extracted_window = state
        .mux()
        .selected_window()
        .expect("extracted window")
        .to_owned();
    assert_ne!(first_window, extracted_window);
    assert_eq!(state.pane_rects(area, 0.), vec![(first.clone(), area)]);
    invoke(
        &mut state,
        "pane.merge",
        vec![extracted_window.clone(), first_window.clone()],
    );
    assert_eq!(state.mux().selected_window(), Some(first_window.as_str()));
    assert_eq!(state.focused_pane(), Some(first.clone()));
    let mut panes = state
        .pane_rects(area, 0.)
        .into_iter()
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    panes.sort();
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(panes, expected);
    assert_eq!(state.mux().all_sessions()[0].windows.len(), 1);
    assert!(matches!(
        submit_action(&mut state, "new_tab", Caller::Socket, started),
        CommandOutcome::Success { .. }
    ));
    assert_ne!(
        state.mux().selected_window(),
        Some(extracted_window.as_str())
    );
    let before = state.mux().all_sessions().to_vec();
    let stale = submit_command(
        &mut state,
        CommandInvocation::new(
            "pane.merge",
            vec![extracted_window, first_window],
            Caller::Socket,
        ),
        started,
    );
    assert!(matches!(stale, CommandOutcome::Failed { .. }), "{stale:?}");
    assert_eq!(state.mux().all_sessions(), before);
}

#[rstest]
#[case(false, false)]
#[case(false, true)]
#[case(true, false)]
fn link_commands_open_the_resolved_document_and_honor_cancellation(
    #[case] cancel: bool,
    #[case] uri_cwd: bool,
) {
    use bootty_ui::AppEffect;
    let directory = assert_fs::TempDir::new().unwrap();
    fs::write(directory.path().join("target.rs"), "first\nsecond\n").unwrap();
    let (wake, wakes) = mpsc::channel();
    let config = test_config::config(
        directory.path().join("settings/config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let mut state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .unwrap();
    let started = Instant::now();
    open_native_session(&mut state, directory.path(), started);
    let cancellation = CommandCancellation::new();
    if cancel {
        let _ = cancellation.cancel();
    }
    let response = state
        .app_command_sender(Caller::Socket)
        .submit(
            CommandInvocation::new(
                "link.open",
                vec![
                    "target.rs:2:3".to_owned(),
                    if uri_cwd {
                        url::Url::from_directory_path(directory.path())
                            .unwrap()
                            .to_string()
                    } else {
                        directory.path().to_string_lossy().into_owned()
                    },
                ],
                Caller::Socket,
            ),
            started
                .checked_add(Duration::from_secs(5))
                .expect("test timestamp fits"),
            cancellation,
        )
        .unwrap();
    let mut effects = Vec::new();
    let outcome = loop {
        effects.extend(state.update_frame(frames::idle_frame(started)));
        if let Ok(outcome) = response.try_recv() {
            break outcome;
        }
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("link worker completion");
    };
    let documents = effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::OpenFiles(request) => Some((
                request.path.clone(),
                request.document,
                request.line,
                request.column,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    if cancel {
        assert!(
            !matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            documents,
            Vec::<(std::string::String, bool, u32, u32)>::new()
        );
    } else {
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            documents,
            vec![(
                fs::canonicalize(directory.path().join("target.rs"))
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                true,
                2,
                3
            )]
        );
    }
}

#[cfg(unix)]
#[rstest]
fn capture_and_export_use_the_attached_pane_and_never_replace_a_file() {
    use bootty_terminal::frame_source::TerminalFrameSource as _;
    use std::os::unix::fs::PermissionsExt as _;
    let directory = assert_fs::TempDir::new().unwrap();
    let shell = directory.path().join("capture-shell");
    fs::write(&shell, "#!/bin/sh\nprintf 'first-capture-line\\r\\n'\ni=0; while [ $i -lt 100 ]; do printf 'line-%s\\r\\n' \"$i\"; i=$((i + 1)); done\nprintf 'capture-ready'\nread line\n").unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    let (wake, wakes) = mpsc::channel();
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.shell = Some(shell.to_string_lossy().into_owned());
    let mut state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .unwrap();
    let started = Instant::now();
    open_native_session(&mut state, directory.path(), started);
    loop {
        state.update_frame(frames::idle_frame(started));
        if state
            .terminal_mut()
            .extract_frame()
            .unwrap()
            .text_rows()
            .join("\n")
            .contains("capture-ready")
        {
            break;
        }
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("terminal publishes fixture output");
    }
    let mut run = |name: &str, arguments: Vec<String>| {
        let mut command = CommandInvocation::from_action(name, Caller::Socket);
        command.arguments = arguments;
        submit_command_from_caller(&mut state, &wakes, Caller::Socket, command, started)
    };
    let outcome = run(
        "terminal.capture",
        vec!["plain".to_owned(), "history".to_owned()],
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("capture failed: {outcome:?}");
    };
    let text = value["capture"]["text"].as_str().unwrap();
    assert!(text.contains("first-capture-line"));
    assert!(text.contains("capture-ready"));
    assert_eq!(value["source"]["kind"], "pane_render_state");
    let destination = directory.path().join("capture.txt");
    let args = vec![
        destination.to_string_lossy().into_owned(),
        "plain".to_owned(),
        "history".to_owned(),
    ];
    assert!(matches!(
        run("terminal.export", args.clone()),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(fs::read_to_string(&destination).unwrap(), text);
    fs::write(&destination, "keep existing content").unwrap();
    assert!(matches!(
        run("terminal.export", args),
        CommandOutcome::Failed { .. }
    ));
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "keep existing content"
    );
}

#[rstest]
fn authored_theme_preview_restore_save_and_apply_share_command_path() {
    let directory = assert_fs::TempDir::new().unwrap();
    let (wake, wakes) = mpsc::channel();
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let mut state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .unwrap();
    let original = state.config().clone();
    let variant = state.active_appearance_variant();
    let appearance = match variant {
        bootty_config::config::AppearanceVariant::Light => "light",
        bootty_config::config::AppearanceVariant::Dark => "dark",
    };
    let source = "[metadata]\nname='Authored'\nsource='Test'\nlicense='MIT'\n[colors]\nbackground='#112233'\nforeground='#eeeeee'\n";
    let started = Instant::now();
    for (command, args) in [
        ("theme.preview", vec![source, appearance]),
        ("theme.restore", vec![]),
        ("theme.save", vec!["Authored", source]),
        ("theme.apply", vec!["Authored", appearance]),
    ] {
        let outcome = submit_command_from_caller(
            &mut state,
            &wakes,
            Caller::Socket,
            CommandInvocation::new(
                command,
                args.into_iter().map(str::to_owned).collect(),
                Caller::Socket,
            ),
            started,
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{command}: {outcome:?}"
        );
        if command == "theme.preview" || command == "theme.apply" {
            assert_eq!(
                state.config().colors_for_appearance(variant).background,
                Some(bootty_config::color::Color::from_hex("#112233").unwrap())
            );
        }
        if command == "theme.restore" {
            assert_eq!(state.config().appearance, original.appearance);
        }
    }
    assert_eq!(
        state.config().theme_for_appearance(variant),
        Some("Authored")
    );
    let saved = fs::read_to_string(directory.path().join("config.toml")).unwrap();
    assert!(saved.contains("Authored"));
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn agent_start_uses_a_backend_pty_with_literal_arguments_and_captured_directory(
    #[case] single_executable: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "main".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )
    .unwrap();
    let output = directory.path().join("agent-output");
    let listener = std::os::unix::net::UnixListener::bind(&output).unwrap();
    let provider = directory.path().join("agent executable.py");
    fs::write(
        &provider,
        r"import json,os,sys,socket
with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as output:
 output.connect(sys.argv[2])
 output.sendall(json.dumps({'literal':sys.argv[1], 'cwd':os.getcwd(), 'tty':sys.stdin.isatty() and sys.stdout.isatty(), 'term':os.environ.get('TERM'), 'colorterm':os.environ.get('COLORTERM'), 'no_color':os.environ.get('NO_COLOR')}).encode())
for line in sys.stdin: print(line,flush=True)
",
    )
    .unwrap();
    let literal = "quoted ' value; $HOME `uname`";
    let mut argv = serde_json::to_string(&[
        provider.to_str().unwrap(),
        literal,
        output.to_str().unwrap(),
    ])
    .unwrap();
    let (provider_kind, program) = if single_executable {
        use std::os::unix::fs::PermissionsExt as _;
        let script = fs::read_to_string(&provider).unwrap();
        fs::write(
            &provider,
            format!(
                "#!/usr/bin/env python3\nimport sys\nsys.argv.extend([{},{}])\n{script}",
                serde_json::to_string(literal).unwrap(),
                serde_json::to_string(output.to_str().unwrap()).unwrap()
            ),
        )
        .unwrap();
        fs::set_permissions(&provider, fs::Permissions::from_mode(0o700)).unwrap();
        argv = "[]".to_owned();
        (
            bootty_agents::AgentKind::Codex,
            provider.to_string_lossy().into_owned(),
        )
    } else {
        (bootty_agents::AgentKind::Pi, "/usr/bin/python3".to_owned())
    };
    let started = Instant::now();
    let outcome = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            format!("agents.{provider_kind}.start"),
            vec![
                directory.path().to_string_lossy().into_owned(),
                program,
                argv,
            ],
            Caller::Socket,
        ),
        started,
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let (mut connection, _) = listener.accept().unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut connection, &mut bytes).unwrap();
    let facts: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        facts.get("literal").and_then(serde_json::Value::as_str),
        Some(literal)
    );
    assert_eq!(
        std::fs::canonicalize(
            facts
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .unwrap()
        )
        .unwrap(),
        std::fs::canonicalize(directory.path()).unwrap()
    );
    assert_eq!(facts["tty"], true);
    assert_eq!(facts["term"], "xterm-bootty");
    assert_eq!(facts["colorterm"], "truecolor");
    assert_eq!(facts["no_color"], serde_json::Value::Null);
    let value = match outcome {
        CommandOutcome::Success { value, .. } => Some(value),
        _ => None,
    }
    .expect("the provider launch succeeded");
    let target: CommandTarget = serde_json::from_value(value["terminal_target"].clone()).unwrap();
    assert_eq!(target.kind, ResourceKind::Terminal);
    assert_eq!(
        state
            .terminal_agent_service()
            .unwrap()
            .record(&target)
            .unwrap()
            .provider,
        provider_kind
    );
    for caller in [
        Caller::CommandPalette,
        Caller::Keybinding,
        Caller::BuiltinKeybinding,
        Caller::Cli,
        Caller::Socket,
        Caller::Luau,
        Caller::Internal,
    ] {
        let mut prompt = CommandInvocation::new(
            format!("agents.{provider_kind}.prompt"),
            vec!["Literal prompt".to_owned()],
            caller,
        );
        prompt.target = Some(target.clone());
        let prompted = submit_command_from_caller(&mut state, &wakes, caller, prompt, started);
        assert!(
            matches!(prompted, CommandOutcome::Success { .. }),
            "{prompted:?}"
        );
    }
    let mut stop =
        CommandInvocation::from_action(&format!("agents.{provider_kind}.stop"), Caller::Socket);
    stop.target = Some(target);
    stop.confirmation = Some(stop.confirmation());
    let stopped = submit_command_from_caller(&mut state, &wakes, Caller::Socket, stop, started);
    assert!(
        matches!(stopped, CommandOutcome::Success { .. }),
        "{stopped:?}"
    );
}

#[rstest]
#[case(bootty_config::config::NotificationPolicy::Never, true, 0)]
#[case(bootty_config::config::NotificationPolicy::Unfocused, true, 0)]
#[case(bootty_config::config::NotificationPolicy::Unfocused, false, 1)]
#[case(bootty_config::config::NotificationPolicy::Always, true, 1)]
fn agent_attention_projects_live_targets_and_notifies_once(
    #[case] policy: bootty_config::config::NotificationPolicy,
    #[case] focused: bool,
    #[case] notifications: usize,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.agent_notifications = policy;
    let (wake, wakes) = mpsc::channel();
    let (events, receiver) = bootty_control::event_queue();
    drop(receiver); // Publication failure must not erase the provider's observed state.
    let mut state = AppState::new_for_window_with_agents(
        config,
        "main".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )
    .unwrap();
    let now = Instant::now();
    open_native_session(&mut state, directory.path(), now);
    let pane = state.focused_pane().unwrap();
    let service = state.agent_service().unwrap();
    for event in ["agent_start", "agent_settled"] {
        let outcome = service.ingest(
            bootty_agents::AgentKind::Pi,
            Some(&pane),
            serde_json::json!({"type": event, "sessionId":"session-a"}),
            Instant::now()
                .checked_add(Duration::from_secs(1))
                .expect("test timestamp fits"),
            &CommandCancellation::new(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    let entries = state.agent_overview();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert!(entry.unread && entry.can_resume);
    assert_eq!(entry.status, "complete");
    let mut frame = frames::idle_frame(now);
    frame.input.window_focused = focused;
    let effects = state.update_frame(frame.clone());
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, bootty_ui::AppEffect::DesktopNotification { .. }))
            .count(),
        notifications
    );
    assert!(
        !state
            .update_frame(frame)
            .iter()
            .any(|effect| matches!(effect, bootty_ui::AppEffect::DesktopNotification { .. }))
    );
    let mut focus = CommandInvocation::from_action("agents.focus", Caller::Socket);
    focus.target = Some(entry.target.clone());
    let outcome =
        submit_command_from_caller(&mut state, &wakes, Caller::Socket, focus.clone(), now);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(state.terminal_focused());
    let target = focus.target.as_mut().unwrap();
    target.generation = target
        .generation
        .checked_add(1)
        .expect("next generation fits");
    assert!(matches!(
        submit_command_from_caller(&mut state, &wakes, Caller::Socket, focus, now),
        CommandOutcome::StaleTarget { .. }
    ));
}

#[rstest]
fn doctor_reports_the_live_binding_without_creating_processes() {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = native_state(directory.path());
    let result = submit_action(&mut state, "doctor", Caller::Socket, Instant::now());
    let CommandOutcome::Success { value, .. } = result else {
        panic!("{result:?}")
    };
    assert_eq!(value["bindings"].as_array().unwrap().len(), 1);
    let binding = &value["bindings"][0];
    assert_eq!(binding["sessions"], 0);
    assert_eq!(binding["backend"], "native");
    assert_eq!(binding["host"], "Local");
    assert!(
        binding["capabilities"]["operations"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("split_pane"))
    );
    assert_eq!(value["reported_agents"], 0);
}

#[cfg(unix)]
#[rstest]
#[case(Caller::Socket)]
#[case(Caller::CommandPalette)]
#[case(Caller::Internal)]
fn jobs_use_the_shared_mailbox_and_preserve_process_results(#[case] caller: Caller) {
    let directory = assert_fs::TempDir::new().unwrap();
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .unwrap();
    let now = Instant::now();
    let spec = bootty_host::jobs::JobSpec {
        program: "/bin/sh".to_owned(),
        args: vec!["-c".to_owned(), "printf job-proof; exit 7".to_owned()],
        cwd: directory.path().to_string_lossy().into_owned(),
        timeout_seconds: 30,
    };
    let result = submit_command_from_caller(
        &mut state,
        &wakes,
        caller,
        CommandInvocation::new(
            "jobs.start",
            vec![serde_json::to_string(&spec).unwrap()],
            caller,
        ),
        now,
    );
    let CommandOutcome::Success { value, .. } = result else {
        panic!("{result:?}")
    };
    let job: bootty_host::jobs::JobSummary = serde_json::from_value(value).unwrap();
    let mut cursor = 0;
    let mut output = Vec::new();
    loop {
        let result = submit_command_from_caller(
            &mut state,
            &wakes,
            caller,
            CommandInvocation::new(
                "jobs.read",
                vec![job.id.clone(), cursor.to_string(), "100".to_owned()],
                caller,
            ),
            now,
        );
        let CommandOutcome::Success { value, .. } = result else {
            panic!("{result:?}")
        };
        let batch: bootty_host::jobs::JobRead = serde_json::from_value(value).unwrap();
        cursor = batch.cursor;
        for chunk in batch.chunks {
            use base64::Engine as _;
            output.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(chunk.data)
                    .unwrap(),
            );
        }
        if batch.job.status.finished() && cursor == batch.job.next_cursor {
            assert_eq!(
                batch.job.status,
                bootty_host::jobs::JobStatus::Exited {
                    code: Some(7),
                    signal: None
                }
            );
            break;
        }
    }
    assert_eq!(output, b"job-proof");
    assert!(matches!(
        submit_command_from_caller(
            &mut state,
            &wakes,
            caller,
            CommandInvocation::new("jobs.forget", vec![job.id], caller),
            now
        ),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(
        state.job_overview(),
        Vec::<bootty_host::jobs::JobSummary>::new()
    );
}

#[cfg(unix)]
#[rstest]
fn job_events_are_owned_by_the_live_window_registry() {
    let directory = assert_fs::TempDir::new().unwrap();
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let (wake, wakes) = mpsc::channel();
    let (events, receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "main".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )
    .unwrap();
    let source = state.command_catalog().control_catalog();
    assert!(source.source().topics().contains("jobs.changed"));
    let spec = bootty_host::jobs::JobSpec {
        program: "/bin/sh".to_owned(),
        args: vec!["-c".to_owned(), "exit 0".to_owned()],
        cwd: "/".to_owned(),
        timeout_seconds: 30,
    };
    let result = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "jobs.start",
            vec![serde_json::to_string(&spec).unwrap()],
            Caller::Socket,
        ),
        Instant::now(),
    );
    assert!(matches!(result, CommandOutcome::Success { .. }));
    let event = loop {
        if let Ok(event) = receiver.try_recv() {
            break event;
        }
        wakes.recv_timeout(Duration::from_secs(2)).unwrap();
    };
    assert_eq!(event.topic, "jobs.changed");
    let mut published = false;
    source
        .source()
        .with_active_topic(&event.identity, event.generation, &event.topic, &mut || {
            published = true;
        })
        .unwrap();
    assert!(published);
    event.response.send(Ok(())).unwrap();
    drop(state);
    assert!(
        source
            .source()
            .with_active_topic(
                &event.identity,
                event.generation,
                &event.topic,
                &mut || panic!("retired owner published")
            )
            .is_err()
    );
}

#[rstest]
#[case(Caller::Socket)]
#[case(Caller::CommandPalette)]
#[case(Caller::Internal)]
fn shell_history_uses_shared_commands_and_keeps_the_shell_file(
    #[case] caller: Caller,
    #[values(false, true)] explicit_local: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("history");
    fs::write(&path, "git status\necho hello\n").unwrap();
    let (wake, wakes) = mpsc::channel();
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let mut state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .unwrap();
    let request = bootty_host::shell_history::HistoryRequest {
        shell: "bash".into(),
        path: path.to_string_lossy().into_owned(),
        query: "gts".into(),
        cwd: directory.path().to_string_lossy().into_owned(),
        recent: Vec::new(),
    };
    let mut arguments = vec![serde_json::to_string(&request).unwrap()];
    if explicit_local {
        arguments.push("local".into());
    }
    let outcome = submit_command_from_caller(
        &mut state,
        &wakes,
        caller,
        CommandInvocation::new("history.search", arguments, caller),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(value["entries"][0]["command"], "git status");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "git status\necho hello\n"
    );
}

#[rstest]
#[case(None, true)]
#[case(Some("local"), true)]
#[case(Some("semantic"), true)]
#[case(Some("typo"), false)]
fn history_search_modes_are_validated_at_the_shared_entry(
    #[values("history.search", "shell.history")] command: &str,
    #[case] mode: Option<&str>,
    #[case] accepted: bool,
) {
    let mut arguments = vec!["query or spec".into()];
    arguments.extend(mode.map(str::to_owned));
    let result =
        CommandCatalog::default().resolve(CommandInvocation::new(command, arguments, Caller::Cli));
    assert_eq!(result.is_ok(), accepted);
}

#[rstest]
fn file_transfers_use_the_captured_binding_and_report_completion() {
    let directory = assert_fs::TempDir::new().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    fs::write(&source, b"transfer through command mailbox").unwrap();
    let (wake, wakes) = mpsc::channel();
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let mut state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .unwrap();
    let started = Instant::now();
    let spec = bootty_host::jobs::TransferSpec {
        direction: bootty_host::jobs::TransferDirection::Upload,
        local_path: source.to_string_lossy().into_owned(),
        host_path: destination.to_string_lossy().into_owned(),
        timeout_seconds: 30,
    };
    let result = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "transfers.start",
            vec![serde_json::to_string(&spec).unwrap()],
            Caller::Socket,
        ),
        started,
    );
    let CommandOutcome::Success { value, .. } = result else {
        panic!("{result:?}");
    };
    let id = value["id"].as_str().unwrap();
    loop {
        let result = submit_command_from_caller(
            &mut state,
            &wakes,
            Caller::Socket,
            CommandInvocation::new(
                "jobs.read",
                vec![id.to_owned(), "0".to_owned(), "4000".to_owned()],
                Caller::Socket,
            ),
            started,
        );
        let CommandOutcome::Success { value, .. } = result else {
            panic!("{result:?}");
        };
        let read: bootty_host::jobs::JobRead = serde_json::from_value(value).unwrap();
        if read.job.status.finished() {
            assert_eq!(
                read.job.status,
                bootty_host::jobs::JobStatus::Exited {
                    code: Some(0),
                    signal: None
                }
            );
            assert_eq!(read.job.transfer.unwrap().phase, "complete");
            break;
        }
    }
    assert_eq!(
        fs::read(destination).unwrap(),
        b"transfer through command mailbox"
    );
}

#[rstest]
#[case("toggle_sessions_panel")]
#[case("toggle_files_panel")]
#[case("toggle_changes_panel")]
#[case("toggle_diff_panel")]
#[case("toggle_coordination_panel")]
#[case("toggle_browser_panel")]
#[case("toggle_left_dock")]
#[case("toggle_right_dock")]
#[case("show_codexbar")]
#[case("show_spaces")]
#[case("show_sidebar")]
#[case("show_files")]
#[case("show_changes")]
#[case("show_coordination")]
#[case("browser.show")]
#[case("show_diff")]
fn dock_commands_share_palette_bindings_and_window_completion(
    #[case] command: &str,
    #[values(
        Caller::CommandPalette,
        Caller::Keybinding,
        Caller::Cli,
        Caller::Socket
    )]
    caller: Caller,
) {
    let catalog = CommandCatalog::default();
    assert!(catalog.describe(command).unwrap().palette);
    let mut bindings =
        bootty_ui::app_actions::AppKeyBindings::from_keybinds(&[format!("ctrl+shift+d={command}")])
            .expect("dock action is bindable");
    let bound = bindings
        .invocation_for_input(bootty_terminal::terminal::KeyInput {
            key: bootty_terminal::terminal::TerminalKey::D,
            mods: bootty_terminal::terminal::KeyMods {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
            repeat: false,
            utf8: None,
            unshifted: Some('d'),
        })
        .expect("keybinding produces invocation");
    assert_eq!(bound.command, command);

    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = native_state(directory.path());
    let now = Instant::now();
    let response = state
        .app_command_sender(caller)
        .submit(
            CommandInvocation::from_action(command, caller),
            now.checked_add(Duration::from_secs(5))
                .expect("test timestamp fits"),
            CommandCancellation::new(),
        )
        .unwrap();
    let effects = state.update_frame(frames::idle_frame(now));
    assert!(
        matches!(response.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "must not acknowledge a dock mutation before the window handles it"
    );
    let request = effects
        .into_iter()
        .find_map(|effect| match effect {
            bootty_ui::AppEffect::Dock(request) => Some(request),
            _ => None,
        })
        .expect("window receives the registered action");
    assert_eq!(request.action.command().action(), command);
    let rejected = CommandOutcome::StaleTarget {
        message: "group removed".into(),
    };
    request.complete(rejected.clone());
    state.update_frame(frames::idle_frame(now));
    assert_eq!(response.try_recv().unwrap(), rejected);
}

proptest::proptest! {
    #[test]
    fn panel_commands_preserve_valid_group_ids(group in 1u64..=u64::try_from(i64::MAX).expect("positive bound fits")) {
        let catalog = CommandCatalog::default();
        for action in bootty_ui::commands::DockAction::PANELS {
            let resolved = catalog.resolve(CommandInvocation::new(
                action.command().action(), vec![group.to_string()], Caller::Socket,
            )).unwrap();
            assert!(matches!(resolved.executor,
                CommandExecutor::Core(bootty_ui::commands::CoreCommandExecutor::Dock(actual, Some(id)))
                    if actual == action && id == group));
        }
    }
}

#[rstest]
#[case("0")]
#[case("-1")]
#[case("1.5")]
#[case("unknown")]
#[case("9223372036854775808")]
fn panel_commands_reject_invalid_group_ids(#[case] group: &str) {
    let catalog = CommandCatalog::default();
    assert!(matches!(catalog.resolve(CommandInvocation::new(
        "show_files", vec![group.into()], Caller::Socket,
    )), Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"));
}

#[rstest]
#[case(
    "toggle_sidebar_visibility",
    bootty_ui::commands::DockAction::TogglePanel(bootty_config::config::PanelKind::Sessions)
)]
#[case("toggle_sidebar_focus", bootty_ui::commands::DockAction::Sidebar)]
fn legacy_sidebar_commands_request_dock_mutations(
    #[case] command: &str,
    #[case] expected: bootty_ui::commands::DockAction,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = native_state(directory.path());
    let initial = state.config().chrome.sidebar;
    let now = Instant::now();
    let _response = state
        .app_command_sender(Caller::Keybinding)
        .submit(
            CommandInvocation::from_action(command, Caller::Keybinding),
            now.checked_add(Duration::from_secs(5))
                .expect("test timestamp fits"),
            CommandCancellation::new(),
        )
        .unwrap();
    let effects = state.update_frame(frames::idle_frame(now));
    assert!(effects.iter().any(|effect| matches!(effect,
        bootty_ui::AppEffect::Dock(request) if request.action == expected)));
    assert_eq!(state.config().chrome.sidebar, initial);
}

#[rstest]
fn setting_links_use_the_shared_command_path() {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = native_state(directory.path());
    let now = Instant::now();
    let response = state
        .app_command_sender(Caller::Cli)
        .submit(
            CommandInvocation::new("open_setting", vec!["font.size".to_owned()], Caller::Cli),
            now.checked_add(Duration::from_secs(5))
                .expect("test timestamp fits"),
            CommandCancellation::new(),
        )
        .unwrap();
    let effects = state.update_frame(frames::idle_frame(now));
    assert!(effects.contains(&bootty_ui::AppEffect::OpenSetting("font.size".to_owned())));
    assert!(matches!(
        response.try_recv().unwrap(),
        CommandOutcome::Success { .. }
    ));
}

#[rstest]
#[case("focus_terminal")]
#[case("ui.sidebar.focus_terminal")]
#[case("toggle_sidebar_focus")]
fn returning_from_sidebar_requests_native_terminal_focus(#[case] command: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = native_state(directory.path());
    let now = Instant::now();
    submit_action(&mut state, "toggle_sidebar_focus", Caller::Keybinding, now);
    assert!(state.sidebar_focused());
    let _response = state
        .app_command_sender(Caller::Keybinding)
        .submit(
            CommandInvocation::from_action(command, Caller::Keybinding),
            now.checked_add(Duration::from_secs(5))
                .expect("test timestamp fits"),
            CommandCancellation::new(),
        )
        .unwrap();
    let effects = state.update_frame(frames::idle_frame(now));
    assert!(state.terminal_focused());
    assert!(effects.contains(&bootty_ui::AppEffect::FocusTerminal));
}

fn owned(arguments: &[&str]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect()
}

fn json_argv(argv: &[String]) -> String {
    serde_json::to_string(argv).expect("encode argv")
}

/// The outcome's failure class: its code when it failed, else its variant.
fn failure_kind(outcome: &CommandOutcome) -> &str {
    match outcome {
        CommandOutcome::Failed { code, .. } => code,
        CommandOutcome::Unsupported { .. } => "unsupported",
        CommandOutcome::Unavailable { .. } => "unavailable",
        CommandOutcome::StaleTarget { .. } => "stale_target",
        CommandOutcome::Denied { .. } => "denied",
        CommandOutcome::ConfirmationRequired { .. } => "confirmation_required",
        CommandOutcome::Success { .. } => "success",
    }
}

#[rstest]
#[case::missing_cwd(owned(&["agent"]), "invalid_arguments")]
#[case::empty_name(owned(&["", "/tmp"]), "invalid_arguments")]
#[case::name_tmux_rewrites(owned(&["crash:12", "/tmp"]), "invalid_arguments")]
#[case::name_with_dot(owned(&["v1.2", "/tmp"]), "invalid_arguments")]
#[case::name_like_a_flag(owned(&["-w", "/tmp"]), "invalid_arguments")]
#[case::name_like_a_session_id(owned(&["$1", "/tmp"]), "invalid_arguments")]
#[case::name_with_control_character(owned(&["crash\t12", "/tmp"]), "invalid_arguments")]
#[case::name_with_format(owned(&["#{host}", "/tmp"]), "invalid_arguments")]
#[case::name_too_long(vec!["n".repeat(SESSION_NAME_MAX_BYTES + 1), "/tmp".to_owned()], "invalid_arguments")]
#[case::relative_cwd(owned(&["agent", "src/arc"]), "invalid_arguments")]
#[case::argv_not_json(owned(&["agent", "/tmp", "claude -w agent"]), "invalid_arguments")]
#[case::argv_not_strings(owned(&["agent", "/tmp", "[\"claude\", 1]"]), "invalid_arguments")]
#[case::argv_with_nul(owned(&["agent", "/tmp", "[\"a\\u0000b\"]"]), "invalid_arguments")]
#[case::argv_empty_program(owned(&["agent", "/tmp", "[\"\"]"]), "invalid_arguments")]
#[case::argv_too_many(
    owned(&["agent", "/tmp", &json_argv(&vec!["a".to_owned(); SESSION_ARGV_MAX_ELEMENTS + 1])]),
    "invalid_arguments"
)]
#[case::argv_too_large(
    owned(&["agent", "/tmp", &json_argv(&["a".repeat(SESSION_ARGV_MAX_BYTES)])]),
    "invalid_arguments"
)]
fn session_create_refuses_a_bad_request_before_the_backend(
    #[case] arguments: Vec<String>,
    #[case] expected: &str,
) {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let mut state = native_state(directory.path());
    let outcome = submit_command(
        &mut state,
        CommandInvocation::new("session.create", arguments, Caller::Socket),
        Instant::now(),
    );
    assert_eq!(failure_kind(&outcome), expected, "{outcome:?}");
    assert!(
        spaces_listing(&mut state)
            .iter()
            .all(|space| space["sessions"] == serde_json::json!([])),
        "a refused request creates nothing"
    );
    assert_eq!(
        state.last_error(),
        None,
        "a socket failure stays with its caller"
    );
}

fn selection(state: &AppState) -> (Option<String>, Option<String>) {
    (
        state.mux().selected_session().map(str::to_owned),
        state.mux().selected_window().map(str::to_owned),
    )
}

fn spaces_listing(state: &mut AppState) -> Vec<serde_json::Value> {
    let outcome = submit_command(
        state,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("spaces.list failed: {outcome:?}");
    };
    value.as_array().expect("a list of Spaces").clone()
}

fn listed_space(state: &mut AppState, name: &str) -> serde_json::Value {
    spaces_listing(state)
        .into_iter()
        .find(|space| space["name"] == name)
        .unwrap_or_else(|| panic!("Space {name} is listed"))
}

fn listed_session_names(space: &serde_json::Value) -> Vec<&str> {
    space["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .map(|session| session["name"].as_str().expect("session name"))
        .collect()
}

fn session_request(
    command: &str,
    arguments: Vec<String>,
    target: serde_json::Value,
) -> CommandInvocation {
    CommandInvocation {
        target: Some(serde_json::from_value(target).expect("command target")),
        ..CommandInvocation::new(command, arguments, Caller::Socket)
    }
}

/// Scripts create and close sessions in the active Space and in another one; neither moves the
/// selection, the focused window or the active Space, and membership persists before publication.
#[rstest]
fn session_requests_leave_selection_and_the_active_space_alone() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    WorkspaceRepository::open(&config_path)
        .expect("workspace")
        .0
        .create_space(
            "Scripts",
            "2",
            [1, 2, 3],
            false,
            SpaceMuxOverride::default(),
            false,
        )
        .expect("create Space")
        .expect("valid Space");
    let mut state = native_state(directory.path());
    let started = Instant::now();
    let project = directory.path().join("project");
    fs::create_dir(&project).expect("project directory");
    open_native_session(&mut state, &project, started);
    let before = selection(&state);
    assert_eq!(before.0.as_deref(), Some("project"));
    let home = spaces_listing(&mut state)
        .into_iter()
        .find(|space| space["active"] == true)
        .expect("active Space");
    let scripts = listed_space(&mut state, "Scripts");
    assert_eq!(scripts["active"], false);
    let unchanged = |state: &mut AppState| {
        for tick in 500..505 {
            state.update_frame(frames::idle_frame(
                started
                    .checked_add(Duration::from_millis(tick))
                    .expect("test timestamp fits"),
            ));
        }
        assert_eq!(selection(state), before);
        assert_eq!(listed_space(state, "Scripts")["active"], false);
    };

    let cwd = project.to_string_lossy().into_owned();
    let mut created = Vec::new();
    for (name, space) in [("here", &home), ("there", &scripts)] {
        let outcome = submit_command(
            &mut state,
            session_request(
                "session.create",
                owned(&[name, &cwd]),
                space["target"].clone(),
            ),
            started,
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            panic!("session.create {name} failed: {outcome:?}");
        };
        assert_eq!(value.get("focused"), None, "{value}");
        let terminal: CommandTarget =
            serde_json::from_value(value["terminal"].clone()).expect("terminal target");
        assert_eq!(terminal.kind, ResourceKind::Terminal);
        created.push(value["created"].clone());
        unchanged(&mut state);
    }
    assert_eq!(
        listed_session_names(&listed_space(&mut state, "Scripts")),
        ["there"]
    );
    let (_, persisted) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    let persisted_names = persisted
        .spaces()
        .iter()
        .map(|space| space.binding().sessions().backend_names())
        .collect::<Vec<_>>();
    assert!(
        persisted_names.contains(&vec!["there".to_owned()]),
        "{persisted_names:?}"
    );

    let taken = submit_command(
        &mut state,
        session_request(
            "session.create",
            owned(&["here", &cwd]),
            home["target"].clone(),
        ),
        started,
    );
    assert_eq!(failure_kind(&taken), "session_exists", "{taken:?}");

    for target in created {
        let mut close = session_request("session.close", Vec::new(), target);
        let unconfirmed = submit_command(&mut state, close.clone(), started);
        assert_eq!(failure_kind(&unconfirmed), "confirmation_required");
        close.confirmation = Some(close.confirmation());
        let closed = submit_command(&mut state, close, started);
        assert_eq!(failure_kind(&closed), "success", "{closed:?}");
        unchanged(&mut state);
    }
    assert_eq!(
        listed_session_names(&listed_space(&mut state, "Scripts")),
        Vec::<&str>::new()
    );
    let home_name = home["name"].as_str().expect("Space name").to_owned();
    assert_eq!(
        listed_session_names(&listed_space(&mut state, &home_name)),
        ["project"]
    );
}

#[rstest]
#[case("new_tab", "target", 2, 2)]
#[case("split_right", "pane_target", 1, 2)]
#[case("split_down", "pane_target", 1, 2)]
fn issued_topology_targets_modify_the_inactive_space_without_retargeting_selection(
    #[case] command: &str,
    #[case] field: &str,
    #[case] expected_windows: usize,
    #[case] expected_panes: usize,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let config_path = directory.path().join("config.toml");
    let other = WorkspaceRepository::open(&config_path)
        .unwrap()
        .0
        .create_space(
            "Phone",
            "2",
            [1, 2, 3],
            false,
            SpaceMuxOverride::default(),
            false,
        )
        .unwrap()
        .unwrap()
        .id();
    let mut state = native_state(directory.path());
    let now = Instant::now();
    open_native_session(&mut state, directory.path(), now);
    let original_space = state.active_space_id();
    let original_selection = selection(&state);
    let phone = listed_space(&mut state, "Phone");
    let created = submit_command(
        &mut state,
        session_request(
            "session.create",
            owned(&["phone", directory.path().to_str().unwrap()]),
            phone["target"].clone(),
        ),
        now,
    );
    assert!(
        matches!(created, CommandOutcome::Success { .. }),
        "{created:?}"
    );
    let phone = listed_space(&mut state, "Phone");
    let session = &phone["sessions"][0];
    assert_eq!(session["topology_supported"], true);
    let target = session[field].clone();
    let mut stale = session_request(command, Vec::new(), target.clone());
    let generation = &mut stale.target.as_mut().unwrap().generation;
    *generation = generation.saturating_add(1);
    let rejected = submit_command(&mut state, stale, now);
    assert!(
        matches!(rejected, CommandOutcome::StaleTarget { .. }),
        "{rejected:?}"
    );
    let outcome = submit_command(
        &mut state,
        session_request(command, Vec::new(), target),
        now,
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let phone = listed_space(&mut state, "Phone");
    let windows = phone["sessions"][0]["windows"].as_array().unwrap();
    assert_eq!(windows.len(), expected_windows);
    assert_eq!(
        windows
            .iter()
            .map(|window| window["panes"].as_array().unwrap().len())
            .sum::<usize>(),
        expected_panes
    );
    for window in windows {
        let issued: CommandTarget = serde_json::from_value(window["target"].clone()).unwrap();
        assert_eq!(issued.kind, ResourceKind::MuxWindow);
        for pane in window["panes"].as_array().unwrap() {
            let issued: CommandTarget = serde_json::from_value(pane["target"].clone()).unwrap();
            let terminal: CommandTarget =
                serde_json::from_value(pane["terminal_target"].clone()).unwrap();
            assert_eq!(issued.kind, ResourceKind::Pane);
            assert_eq!(terminal.kind, ResourceKind::Terminal);
        }
    }
    assert_eq!(state.active_space_id(), original_space);
    assert_eq!(selection(&state), original_selection);
    assert!(state.activate_space_from_ui(other));
    if command != "new_tab" {
        let rects = state.pane_rects(SurfaceRect::from_min_size(0.0, 0.0, 200.0, 100.0), 4.0);
        assert_eq!(rects.len(), 2);
        for (_, rect) in rects {
            if command == "split_right" {
                assert!(rect.width() < 100.0);
                assert!((rect.height() - 100.0).abs() < f32::EPSILON);
            } else {
                assert!((rect.width() - 200.0).abs() < f32::EPSILON);
                assert!(rect.height() < 50.0);
            }
        }
    }
}
