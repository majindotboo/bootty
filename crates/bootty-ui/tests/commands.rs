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
use bootty_mux::repository::WorkspaceRepository;
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

#[test]
fn native_agent_commands_are_static_and_explicitly_unsupported_until_composed() {
    let catalog = CommandCatalog::default();
    let native = catalog
        .list()
        .into_iter()
        .filter(|descriptor| descriptor.id.starts_with("agents."))
        .collect::<Vec<_>>();
    assert_eq!(
        native
            .iter()
            .map(|command| &command.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        native.len()
    );
    assert_eq!(
        catalog
            .describe("agents.pi.start")
            .expect("Pi start descriptor")
            .target,
        Some(ResourceKind::Terminal)
    );
    assert!(matches!(
        catalog
            .resolve(CommandInvocation::from_action(
                "agents.pi.state",
                Caller::Socket
            ))
            .expect("resolve static native command")
            .executor,
        CommandExecutor::UncomposedAgent
    ));
}

#[test]
fn composed_native_agent_prompt_uses_the_same_mailbox_for_every_caller() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let (wake, wakes) = mpsc::channel();
    let (agent_events, _agent_event_receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "main".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(agent_events),
    )
    .expect("composed app state");
    let started = Instant::now();
    open_native_session(&mut state, directory.path(), started);
    assert!(state.agent_service().is_some());

    let callers = [
        Caller::CommandPalette,
        Caller::Keybinding,
        Caller::BuiltinKeybinding,
        Caller::Cli,
        Caller::Socket,
        Caller::Luau,
        Caller::Internal,
    ];
    for (index, caller) in callers.into_iter().enumerate() {
        let invocation =
            CommandInvocation::new("agents.pi.prompt", vec![format!("caller-{index}")], caller);
        let outcome = submit_command_from_caller(
            &mut state,
            &wakes,
            caller,
            invocation,
            started
                .checked_add(Duration::from_millis(
                    20_u64
                        .checked_add(u64::try_from(index).expect("caller index fits"))
                        .expect("test tick fits"),
                ))
                .expect("test timestamp fits"),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
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
    assert_eq!(state.mux().selected_window(), Some(first_window.as_str()));
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
            outcomes.try_recv().ok()
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
    use bootty_host::files::{FileResponse, encode_document};
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("document.txt");
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
    let arguments = vec![name, snapshot.digest, encode_document("saved").unwrap()];
    assert!(matches!(
        run("files.save", arguments.clone()),
        CommandOutcome::Success { .. }
    ));
    fs::write(&path, "external").unwrap();
    assert!(
        matches!(run("files.save",arguments),CommandOutcome::Failed{code,..} if code=="file_failed")
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "external");
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
fn agent_start_delivers_literal_arguments_to_a_new_native_pty() {
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
    let started = Instant::now();
    open_native_session(&mut state, directory.path(), started);
    let output = directory.path().join("agent-output");
    let literal = "quoted ' value; $HOME `uname`";
    let script = "printf '%s\\n%s\\n' \"$1\" \"$BOOTTY_AGENT_LAUNCH_CONTEXT\" > \"$2\"; printf 'agent ready\\n'; exec cat";
    let argv =
        serde_json::to_string(&["-c", script, "agent", literal, output.to_str().unwrap()]).unwrap();
    let outcome = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "agents.pi.start",
            vec![
                directory.path().to_string_lossy().into_owned(),
                "/bin/sh".to_owned(),
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
    loop {
        state.update_frame(frames::idle_frame(started));
        if let Ok(text) = fs::read_to_string(&output) {
            let mut lines = text.lines();
            assert_eq!(lines.next(), Some(literal));
            let launch: bootty_agents::AgentLaunch =
                serde_json::from_str(lines.next().unwrap()).unwrap();
            assert_eq!(launch.program, "/bin/sh");
            assert_eq!(launch.arguments, Vec::<std::string::String>::new());
            break;
        }
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("native agent output");
    }
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
fn shell_history_uses_shared_commands_and_keeps_the_shell_file(#[case] caller: Caller) {
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
    let outcome = submit_command_from_caller(
        &mut state,
        &wakes,
        caller,
        CommandInvocation::new(
            "history.search",
            vec![serde_json::to_string(&request).unwrap()],
            caller,
        ),
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
#[case("toggle_agents_panel")]
#[case("toggle_left_dock")]
#[case("toggle_right_dock")]
#[case("toggle_tab_bar")]
#[case("toggle_hidden_tabs")]
#[case("show_codexbar")]
#[case("show_spaces")]
#[case("show_sidebar")]
#[case("show_files")]
#[case("show_changes")]
#[case("show_agents")]
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
