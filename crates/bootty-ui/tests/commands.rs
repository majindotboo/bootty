#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

use std::{
    fs,
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, MutationClass,
    ResourceKind, ValueType,
};
use bootty_mux::repository::{SpaceMuxOverride, WorkspaceRepository};
use bootty_mux::workspace::{SESSION_ARGV_MAX_ELEMENTS, SESSION_NAME_MAX_BYTES};
use bootty_terminal::geometry::SurfaceRect;
use bootty_ui::commands::CommandCatalog;
use bootty_ui::{AppState, ModalDialog, presentation::dialogs::NewSessionPickerEvent};
use rusqlite::Connection;

/// Bounds waits on real child processes and command replies. Passing tests advance on events;
/// only a broken fixture reaches this, so it is generous enough for a loaded machine.
const FIXTURE_DEADLINE: Duration = Duration::from_secs(30);

use crate::support::idle_frames as frames;

use crate::support::config as test_config;

fn event_repaint(repaint: impl Fn() + Send + Sync + 'static) -> bootty_mux::RepaintHandle {
    let host = std::thread::current();
    Arc::new(move || {
        repaint();
        host.unpark();
    })
}

fn metadata_state(directory: &Path) -> AppState {
    state_with_backends(directory)
}

fn focus_state(directory: &Path) -> AppState {
    state_with_backends(directory)
}

fn terminal_state(directory: &Path) -> AppState {
    state_with_backends(directory)
}

fn state_with_backends(directory: &Path) -> AppState {
    let mut config = test_config::config(directory.join("config.toml"));
    config.session.working_directory = Some(directory.to_owned());
    AppState::new(config, event_repaint(|| {}), None, None).expect("app state")
}

fn wait_for_saved_attachment(state: &mut AppState, identity: &str) {
    let deadline = Instant::now().checked_add(FIXTURE_DEADLINE).unwrap();
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if state
            .mux()
            .all_sessions()
            .iter()
            .any(|session| session.tag.identity.as_deref() == Some(identity))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "saved session did not reattach: {:?}",
            state.last_error()
        );
        std::thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

fn open_native_session(state: &mut AppState, cwd: &Path, started: Instant) {
    let outcome = submit_action(state, "new_mux_session", Caller::Socket, started);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(
        matches!(state.modal_dialog(), Some(ModalDialog::NewSession(_))),
        "new session form is open"
    );
    // Directory-picker compatibility keeps the fixture's backend session names.
    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: cwd.to_string_lossy().into_owned(),
    });
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline fits");
    loop {
        // Dialog projection observes command results, as it does during rendering.
        let _ = state.dialog_projection();
        if state.modal_dialog().is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "session start did not complete: {:?}",
            state.dialog_projection()
        );
        let outcome = submit_command(
            state,
            CommandInvocation::new("resource.current", owned(&["binding"]), Caller::Socket),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        state.update_frame(frames::idle_frame(Instant::now()));
    }
    state.update_frame(frames::idle_frame(Instant::now()));
}

#[fixture]
fn native_terminal_form() -> (assert_fs::TempDir, AppState, mpsc::Receiver<()>) {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let mut config = test_config::config(directory.path().join("config.toml"));
    config.session.working_directory = Some(directory.path().to_owned());
    let (wake, wakes) = mpsc::channel();
    let state = AppState::new(
        config,
        event_repaint(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("native workspace");
    (directory, state, wakes)
}

#[rstest]
#[case::current(false)]
#[case::stale(true)]
fn session_start_uses_the_captured_application_window(
    native_terminal_form: (assert_fs::TempDir, AppState, mpsc::Receiver<()>),
    #[case] stale: bool,
    #[values(Caller::Internal, Caller::Socket)] caller: Caller,
) {
    let (_directory, mut state, _wakes) = native_terminal_form;
    let current = submit_command(
        &mut state,
        CommandInvocation::new("resource.current", owned(&["application_window"]), caller),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("current application window: {current:?}");
    };
    let mut target: CommandTarget =
        serde_json::from_value(value["target"].clone()).expect("application window target");
    if stale {
        target.generation = target.generation.checked_add(1).expect("fresh generation");
    }
    let mut invocation = CommandInvocation::from_action("new_mux_session", caller);
    invocation.target = Some(target);
    let outcome = submit_command(&mut state, invocation, Instant::now());
    if stale {
        assert!(
            matches!(outcome, CommandOutcome::StaleTarget { .. }),
            "{outcome:?}"
        );
        assert!(state.modal_dialog().is_none());
    } else {
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert!(matches!(
            state.modal_dialog(),
            Some(ModalDialog::NewSession(_))
        ));
    }
    assert_eq!(
        state.mux().all_sessions().len(),
        0,
        "opening the form starts no process"
    );
}

#[rstest]
#[case::start(false)]
#[case::cancel_and_reopen(true)]
fn empty_terminal_form_starts_one_shell_only_after_explicit_start(
    native_terminal_form: (assert_fs::TempDir, AppState, mpsc::Receiver<()>),
    #[case] cancel: bool,
) {
    let (_directory, mut state, wakes) = native_terminal_form;
    let outcome = submit_action(
        &mut state,
        "new_mux_session",
        Caller::Socket,
        Instant::now(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let Some(ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
        panic!("new session form");
    };
    let id = dialog.spec().id;
    if cancel {
        state.apply_dialog_intent(
            &bootty_gpui::DialogIntent::Dismiss { dialog: id },
            &mut Vec::new(),
        );
        assert!(state.modal_dialog().is_none());
        let outcome = submit_action(
            &mut state,
            "new_mux_session",
            Caller::Socket,
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        let Some(ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
            panic!("new session form");
        };
        assert_eq!(dialog.spec().text_label, None);
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .expect("test deadline fits");
    let spec = loop {
        assert!(
            Instant::now() < deadline,
            "project discovery did not complete"
        );
        state.update_frame(frames::idle_frame(Instant::now()));
        let _ = state.dialog_projection();
        let Some(ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
            panic!("retained terminal form");
        };
        let spec = dialog.spec();
        assert_eq!(spec.text_label, None);
        assert_eq!(spec.text.as_deref(), Some(""));
        if spec.rows[0].enabled {
            break spec;
        }
        wakes
            .recv_timeout(Duration::from_secs(1))
            .expect("project discovery completion");
    };
    assert_eq!(state.mux().all_sessions().len(), 0);
    state.apply_dialog_intent(
        &bootty_gpui::DialogIntent::Activate {
            dialog: spec.id,
            row: spec.rows[0].id.clone(),
            action: bootty_gpui::ActionId::new("start-session"),
            payload: bootty_gpui::DialogPayload::None,
        },
        &mut Vec::new(),
    );
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .expect("test deadline fits");
    loop {
        assert!(Instant::now() < deadline, "shell start did not complete");
        state.update_frame(frames::idle_frame(Instant::now()));
        let _ = state.dialog_projection();
        if state.modal_dialog().is_none() {
            break;
        }
        wakes
            .recv_timeout(Duration::from_secs(1))
            .expect("observed shell start completion");
    }
    state.update_frame(frames::idle_frame(Instant::now()));
    assert_eq!(state.mux().all_sessions().len(), 1);
    assert!(state.terminal_focused());
    let outcome = submit_command(
        &mut state,
        CommandInvocation::new("resource.current", owned(&["session"]), Caller::Socket),
        Instant::now(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
}

#[rstest]
fn creating_a_session_selects_it_and_keeps_it_selected_after_refresh() {
    let directory = assert_fs::TempDir::new().expect("isolated config");
    let mut state = metadata_state(directory.path());
    for name in ["first", "second", "third"] {
        let cwd = directory.path().join(name);
        fs::create_dir(&cwd).expect("project directory");
        open_native_session(&mut state, &cwd, Instant::now());
        assert_eq!(state.mux().selected_session(), Some(name));
        assert!(state.terminal_focused());
    }
}

#[rstest]
#[case::dot_and_space(
    ".project.v1 notes".to_owned(),
    ".project.v1 notes".to_owned(),
    ".project.v1 notes-2".to_owned()
)]
#[case::bounded_utf8(
    format!(".{}é", "x".repeat(252)),
    format!(".{}é", "x".repeat(252)),
    format!(".{}-2", "x".repeat(252))
)]
fn directory_picker_generates_valid_unique_names_without_changing_cwd(
    #[case] directory_name: String,
    #[case] first: String,
    #[case] second: String,
) {
    let directory = assert_fs::TempDir::new().expect("isolated config");
    let cwd = directory.path().join(directory_name);
    fs::create_dir(&cwd).expect("project directory");
    let canonical_cwd = cwd.canonicalize().expect("canonical project directory");
    let mut config = test_config::config(directory.path().join("config.toml"));
    config.session.working_directory = Some(directory.path().to_owned());
    let mut state =
        AppState::new(config, event_repaint(|| {}), None, None).expect("session naming fixture");
    for expected in [first, second] {
        open_native_session(&mut state, &cwd, Instant::now());
        assert_eq!(state.mux().selected_session(), Some(expected.as_str()));
        assert!(expected.len() <= SESSION_NAME_MAX_BYTES);
        assert!(state.terminal_focused());
    }
    assert_eq!(state.mux().all_sessions().len(), 2);
    let space = spaces_listing(&mut state).remove(0);
    let saved = saved_sessions(&mut state, &space);
    assert_eq!(saved.len(), 2);
    for session in saved {
        assert_eq!(session["cwd"], canonical_cwd.to_string_lossy().as_ref());
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
    let mut state = terminal_state(directory.path());
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
#[case(
    "new_mux_session",
    MutationClass::Write,
    Some(ResourceKind::ApplicationWindow)
)]
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
    let mut state = terminal_state(directory.path());
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
    catalog
        .resolve(CommandInvocation::from_action(
            "terminal.read",
            Caller::Socket,
        ))
        .expect("resolve core command");
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
    let config = test_config::config(directory.path().join("config.toml"));
    let started = Instant::now();
    let mut first =
        AppState::new(config.clone(), event_repaint(|| {}), None, None).expect("first app state");
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

    let mut replacement =
        AppState::new(config, event_repaint(|| {}), None, None).expect("replacement app state");
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

#[cfg(unix)]
#[test]
fn palette_split_launches_in_shell_process_directory_without_osc7() {
    let directory = assert_fs::TempDir::new().unwrap();
    let changed = directory.path().join("changed");
    fs::create_dir(&changed).unwrap();
    let expected = changed.canonicalize().unwrap();
    let launches = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&launches);
    let target = expected.clone();
    let base = bootty_mux::native::local_launcher();
    bootty_test_support::native_mux::configure(
        &directory.path().join("config.toml"),
        Arc::new(move |size, config| {
            let mut launches = recorded.lock().unwrap();
            launches.push(config.working_directory.clone());
            let first = launches.len() == 1;
            drop(launches);
            if !first {
                return base(size, config);
            }
            let launch = bootty_terminal::SessionLaunchConfig {
                shell: Some("/bin/sh".into()),
                args: vec![
                    "-c".into(),
                    "cd \"$1\" && printf READY && read line".into(),
                    "sh".into(),
                    target.to_string_lossy().into_owned(),
                ],
                shell_integration: false,
                ..config.clone()
            };
            let mut transport = base(size, &launch)?;
            let mut ready = Vec::new();
            while !ready.ends_with(b"READY") {
                let Some(bootty_terminal::terminal_output::TerminalOutput::Bytes { bytes, .. }) =
                    transport.reader.next_output()?
                else {
                    anyhow::bail!("shell exited before cd");
                };
                ready.extend(bytes);
            }
            Ok(transport)
        }),
    )
    .unwrap();
    let started = Instant::now();
    let mut state = focus_state(directory.path());
    open_native_session(&mut state, directory.path(), started);
    let outcome = submit_action(&mut state, "split_right", Caller::CommandPalette, started);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(launches.lock().unwrap().last(), Some(&Some(expected)));
}

#[test]
fn native_split_command_publishes_the_binding_owned_layout() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let started = Instant::now();
    let mut state = focus_state(directory.path());
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
}

#[test]
fn native_window_actions_use_the_binding_owned_plan() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let started = Instant::now();
    let mut state = terminal_state(directory.path());
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
    let mut state = metadata_state(directory.path());
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
    let mut config = test_config::config(directory.path().join("config.toml"));
    config.session.working_directory = Some(directory.path().to_owned());
    config.when_closing_with_no_tabs = bootty_config::config::WhenClosingWithNoTabs::CloseWindow;
    let mut state = AppState::new(config, event_repaint(|| {}), None, None).unwrap();
    for index in 0..session_count {
        let cwd = directory.path().join(format!("session-{index}"));
        fs::create_dir(&cwd).unwrap();
        open_native_session(&mut state, &cwd, Instant::now());
    }
    assert_eq!(state.mux().sessions().len(), session_count);
    let started = Instant::now();
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
                .checked_add(FIXTURE_DEADLINE)
                .expect("test timestamp fits"),
            CommandCancellation::new(),
        )
        .unwrap();
    let mut closed = false;
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("close deadline");
    let outcome = loop {
        closed |= state
            .update_frame(frames::idle_frame(Instant::now()))
            .iter()
            .any(|effect| matches!(effect, bootty_ui::AppEffect::CloseWindow));
        if let Ok(outcome) = receiver.try_recv() {
            break outcome;
        }
        assert!(Instant::now() < deadline, "close command did not answer");
        std::thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    };
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

#[rstest]
fn keeping_the_window_open_still_closes_its_last_backend_pane(
    #[values(Caller::Keybinding, Caller::Socket)] caller: Caller,
) {
    let directory = assert_fs::TempDir::new().expect("isolated workspace");
    let mut config = test_config::config(directory.path().join("config.toml"));
    config.session.working_directory = Some(directory.path().to_owned());
    config.when_closing_with_no_tabs = bootty_config::config::WhenClosingWithNoTabs::KeepWindowOpen;
    let mut state = AppState::new(config, event_repaint(|| {}), None, None).unwrap();
    open_native_session(&mut state, directory.path(), Instant::now());
    let mut invocation = CommandInvocation::from_action("close_surface", caller);
    if caller == Caller::Socket {
        let CommandOutcome::ConfirmationRequired { confirmation } =
            submit_command(&mut state, invocation.clone(), Instant::now())
        else {
            panic!("socket close requires its exact confirmation");
        };
        invocation.command.clone_from(&confirmation.command);
        invocation.arguments.clone_from(&confirmation.arguments);
        invocation.target.clone_from(&confirmation.target);
        invocation.confirmation = Some(*confirmation);
    }
    let started = Instant::now();
    let outcomes = state
        .app_command_sender(caller)
        .submit(
            invocation,
            started.checked_add(FIXTURE_DEADLINE).unwrap(),
            CommandCancellation::new(),
        )
        .unwrap();
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("close deadline");
    let outcome = loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if let Ok(outcome) = outcomes.try_recv() {
            break outcome;
        }
        assert!(Instant::now() < deadline, "close command did not answer");
        std::thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    };
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.mux().sessions(), []);
}

fn submit_command(
    state: &mut AppState,
    invocation: CommandInvocation,
    started: Instant,
) -> CommandOutcome {
    let deadline = Instant::now()
        .checked_add(FIXTURE_DEADLINE)
        .expect("command deadline");
    let outcomes = state
        .app_command_sender(Caller::Socket)
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
                .checked_add(FIXTURE_DEADLINE)
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
fn native_pane_arrangement_preserves_ids_and_publishes_only_completed_layouts() {
    let directory = assert_fs::TempDir::new().expect("isolated workspace");
    let started = Instant::now();
    let mut config = test_config::config(directory.path().join("config.toml"));
    // OSC titles are independent of layout mutations; keep this fixture's shell title stable.
    config.session.shell = cfg!(unix).then(|| "/bin/sh".to_owned());
    let mut state = AppState::new(config, event_repaint(|| {}), None, None).expect("app state");
    open_native_session(&mut state, directory.path(), started);
    let first_window = state.mux().selected_window().expect("window").to_owned();
    let first = state.focused_pane().expect("first pane");
    assert!(matches!(
        submit_action(&mut state, "split_right", Caller::Socket, started),
        CommandOutcome::Success { .. }
    ));
    let second = state.focused_pane().expect("second pane");
    let area = SurfaceRect::from_min_size(0., 0., 600., 400.);
    let layout = bootty_mux::snapshot::MuxPaneLayout::Split {
        direction: bootty_mux::snapshot::MuxPaneSplitDirection::Right,
        weights: vec![0.3, 0.7],
        children: vec![
            bootty_mux::snapshot::MuxPaneLayout::Pane(first.clone()),
            bootty_mux::snapshot::MuxPaneLayout::Pane(second.clone()),
        ],
    };
    assert!(matches!(
        submit_command(
            &mut state,
            CommandInvocation::new(
                "pane.resize",
                vec![
                    first_window.clone(),
                    serde_json::to_string(&layout).unwrap()
                ],
                Caller::Socket
            ),
            started
        ),
        CommandOutcome::Success { .. }
    ));
    assert!(matches!(
        submit_command(
            &mut state,
            CommandInvocation::new(
                "pane.focus",
                vec![first_window.clone(), first.clone()],
                Caller::Socket
            ),
            started
        ),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(state.focused_pane().as_deref(), Some(first.as_str()));
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
fn link_commands_open_the_resolved_file_and_honor_cancellation(
    #[case] cancel: bool,
    #[case] uri_cwd: bool,
) {
    use bootty_ui::AppEffect;
    let directory = assert_fs::TempDir::new().unwrap();
    fs::write(directory.path().join("target.rs"), "first\nsecond\n").unwrap();
    let (wake, wakes) = mpsc::channel();
    let config = test_config::config(directory.path().join("settings/config.toml"));
    let mut state = AppState::new(
        config,
        event_repaint(move || {
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
    let opened = effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::OpenWithSystem(path) => Some(path.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if cancel {
        assert!(
            !matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert_eq!(opened, Vec::<std::path::PathBuf>::new());
    } else {
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            opened,
            vec![fs::canonicalize(directory.path().join("target.rs")).unwrap()]
        );
    }
}

#[cfg(unix)]
#[rstest]
fn capture_and_export_use_the_attached_pane_and_never_replace_a_file() {
    use bootty_terminal::frame_source::TerminalFrameSource as _;

    let directory = assert_fs::TempDir::new().unwrap();
    let shell = directory.path().join("capture-shell");
    bootty_test_support::write_executable(&shell, "#!/bin/sh\nprintf 'first-capture-line\\r\\n'\ni=0; while [ $i -lt 100 ]; do printf 'line-%s\\r\\n' \"$i\"; i=$((i + 1)); done\nprintf 'capture-ready'\nread line\n").unwrap();
    let (wake, wakes) = mpsc::channel();
    let mut config = test_config::config(directory.path().join("config.toml"));
    config.session.shell = Some(shell.to_string_lossy().into_owned());
    let mut state = AppState::new(
        config,
        event_repaint(move || {
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
    assert_eq!(value["source"]["kind"], "pane_server_state");
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
#[case("light", bootty_config::config::AppearanceVariant::Light)]
#[case("dark", bootty_config::config::AppearanceVariant::Dark)]
fn authored_theme_preview_restore_save_and_apply_share_command_path(
    #[case] appearance: &str,
    #[case] variant: bootty_config::config::AppearanceVariant,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let (wake, wakes) = mpsc::channel();
    let config = test_config::config(directory.path().join("config.toml"));
    let mut state = AppState::new(
        config,
        event_repaint(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .unwrap();
    let original = state.config().clone();
    let original_variant = state.active_appearance_variant();
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
        if command == "theme.preview" {
            assert_eq!(state.active_appearance_variant(), variant);
            assert_eq!(
                state.config().appearance.mode.variant(original_variant),
                variant
            );
        }
        if command == "theme.restore" {
            assert_eq!(state.active_appearance_variant(), original_variant);
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

#[rstest]
#[case(bootty_config::config::AppearanceVariant::Light)]
#[case(bootty_config::config::AppearanceVariant::Dark)]
fn named_theme_restore_preserves_live_settings_and_current_appearance(
    native_terminal_form: (assert_fs::TempDir, AppState, mpsc::Receiver<()>),
    #[case] current_variant: bootty_config::config::AppearanceVariant,
) {
    let (_directory, mut state, wakes) = native_terminal_form;
    let original_appearance = state.config().appearance.clone();
    let started = Instant::now();
    let preview = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "theme.preview",
            owned(&[
                "[metadata]\nname='Temporary'\n[colors]\nbackground='#123456'\n",
                "light",
            ]),
            Caller::Socket,
        ),
        started,
    );
    assert!(
        matches!(preview, CommandOutcome::Success { .. }),
        "{preview:?}"
    );
    state.set_appearance_variant(current_variant);
    state.set_sidebar_width_live(333.0);
    let zoom = submit_command(
        &mut state,
        CommandInvocation::new("set_font_size", owned(&["23"]), Caller::Socket),
        started,
    );
    assert!(matches!(zoom, CommandOutcome::Success { .. }), "{zoom:?}");
    let current_font_size = state.config().font.size;
    let current_sidebar_width = state.config().chrome.sidebar_width;
    assert_eq!(current_font_size.to_bits(), 23.0_f32.to_bits());
    assert_eq!(current_sidebar_width.to_bits(), 333.0_f32.to_bits());
    let restore = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new("theme.restore", Vec::new(), Caller::Socket),
        started,
    );
    assert!(
        matches!(restore, CommandOutcome::Success { .. }),
        "{restore:?}"
    );
    assert_eq!(state.config().appearance, original_appearance);
    assert_eq!(
        state.active_appearance_variant(),
        original_appearance.mode.variant(current_variant)
    );
    assert_eq!(
        state.config().font.size.to_bits(),
        current_font_size.to_bits()
    );
    assert_eq!(
        state.config().chrome.sidebar_width.to_bits(),
        current_sidebar_width.to_bits()
    );
}

#[rstest]
#[case("system")]
#[case("light")]
#[case("dark")]
fn named_theme_restore_preserves_accepted_theme_and_unrelated_settings(
    native_terminal_form: (assert_fs::TempDir, AppState, mpsc::Receiver<()>),
    #[case] mode: &str,
) {
    let (directory, mut state, wakes) = native_terminal_form;
    let started = Instant::now();
    let preview = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "theme.preview",
            owned(&[
                "[metadata]\nname='Temporary'\n[colors]\nbackground='#123456'\n",
                "light",
            ]),
            Caller::Socket,
        ),
        started,
    );
    assert!(
        matches!(preview, CommandOutcome::Success { .. }),
        "{preview:?}"
    );
    let accepted_source = format!(
        "[multiplexer]\n[font]\nsize=22\n[chrome]\nsidebar-width=333\n[appearance]\nmode='{mode}'\n[appearance.light.colors]\nbackground='#abcdef'\n[appearance.dark.colors]\nbackground='#fedcba'\n"
    );
    let path = directory.path().join("config.toml");
    fs::write(&path, &accepted_source).unwrap();
    let reload = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new("reload_config", Vec::new(), Caller::Socket),
        started,
    );
    assert!(
        matches!(reload, CommandOutcome::Success { .. }),
        "{reload:?}"
    );
    let accepted = state.config().clone();
    assert_eq!(accepted.font.size.to_bits(), 22.0_f32.to_bits());
    assert_eq!(accepted.chrome.sidebar_width.to_bits(), 333.0_f32.to_bits());
    assert_eq!(
        accepted.appearance.light.colors.background,
        Some(bootty_config::color::Color::from_hex("#abcdef").unwrap())
    );
    let restore = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new("theme.restore", Vec::new(), Caller::Socket),
        started,
    );
    assert!(
        matches!(restore, CommandOutcome::Success { .. }),
        "{restore:?}"
    );
    assert_eq!(state.config().appearance, accepted.appearance);
    assert_eq!(
        state.config().font.size.to_bits(),
        accepted.font.size.to_bits()
    );
    assert_eq!(
        state.config().chrome.sidebar_width.to_bits(),
        accepted.chrome.sidebar_width.to_bits()
    );
    assert_eq!(fs::read_to_string(path).unwrap(), accepted_source);
}

#[rstest]
fn doctor_reports_the_live_binding_without_creating_processes() {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    let result = submit_action(&mut state, "doctor", Caller::Socket, Instant::now());
    let CommandOutcome::Success { value, .. } = result else {
        panic!("{result:?}")
    };
    assert_eq!(value["bindings"].as_array().unwrap().len(), 1);
    let binding = &value["bindings"][0];
    assert_eq!(binding["sessions"], 0);
    assert_eq!(binding["host"], "Local");
}

#[cfg(unix)]
#[rstest]
#[case(Caller::Socket)]
#[case(Caller::CommandPalette)]
#[case(Caller::Internal)]
fn jobs_use_the_shared_mailbox_and_preserve_process_results(#[case] caller: Caller) {
    let directory = assert_fs::TempDir::new().unwrap();
    let config = test_config::config(directory.path().join("config.toml"));
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new(
        config,
        event_repaint(move || {
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
    let mut output = String::new();
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
            output.push_str(&bootty_host::files::decode_document(&chunk.data).unwrap());
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
    assert_eq!(output, "job-proof");
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
    let config = test_config::config(directory.path().join("config.toml"));
    let (wake, wakes) = mpsc::channel();
    let (events, receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_events(
        config,
        "main".to_owned(),
        event_repaint(move || {
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
        wakes.recv_timeout(FIXTURE_DEADLINE).unwrap();
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
    let config = test_config::config(directory.path().join("config.toml"));
    let mut state = AppState::new(
        config,
        event_repaint(move || {
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
    let config = test_config::config(directory.path().join("config.toml"));
    let mut state = AppState::new(
        config,
        event_repaint(move || {
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
#[case("toggle_sessions_panel", true)]
#[case("toggle_left_dock", true)]
#[case("show_spaces", true)]
#[case("show_sidebar", true)]
fn dock_commands_share_palette_bindings_and_window_completion(
    #[case] command: &str,
    #[case] palette: bool,
    #[values(
        Caller::CommandPalette,
        Caller::Keybinding,
        Caller::Cli,
        Caller::Socket
    )]
    caller: Caller,
) {
    let catalog = CommandCatalog::default();
    assert_eq!(catalog.describe(command).unwrap().palette, palette);
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
    let mut state = metadata_state(directory.path());
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
        for action in [
            bootty_ui::commands::DockAction::ToggleSessions,
            bootty_ui::commands::DockAction::Sidebar,
            bootty_ui::commands::DockAction::Spaces,
        ] {
            let resolved = catalog.resolve(CommandInvocation::new(
                action.command().action(), vec![group.to_string()], Caller::Socket,
            )).unwrap();
            assert!(matches!(resolved.executor,
                bootty_ui::commands::CoreCommandExecutor::Dock(actual, Some(id))
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
        "show_sidebar", vec![group.into()], Caller::Socket,
    )), Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"));
}

#[rstest]
#[case(
    "toggle_sidebar_visibility",
    bootty_ui::commands::DockAction::ToggleSessions
)]
#[case("toggle_sidebar_focus", bootty_ui::commands::DockAction::Sidebar)]
fn legacy_sidebar_commands_request_dock_mutations(
    #[case] command: &str,
    #[case] expected: bootty_ui::commands::DockAction,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
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
    let mut state = metadata_state(directory.path());
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
    let mut state = focus_state(directory.path());
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
#[case::name_with_control_character(owned(&["crash\t12", "/tmp"]), "invalid_arguments")]
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
fn session_create_refuses_a_bad_request_before_the_backend(
    #[case] arguments: Vec<String>,
    #[case] expected: &str,
) {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let mut state = metadata_state(directory.path());
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

/// "New Session in Project" opens the form in that project's directory.
#[test]
fn new_sessions_can_start_in_a_sessions_project() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let mut state = metadata_state(directory.path());
    let project = directory
        .path()
        .join("project")
        .to_string_lossy()
        .into_owned();
    assert!(state.open_new_session_dialog_in(state.active_space_id(), project.clone()));
    let Some(ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
        panic!("new session form");
    };
    assert_eq!(dialog.draft().map(|draft| draft.cwd.clone()), Some(project));
}

/// Bootty's own notices keep their wording instead of collapsing into a generic sentence.
#[test]
fn closing_the_last_space_says_why() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let mut state = metadata_state(directory.path());
    assert!(!state.close_space_from_ui(state.active_space_id()));
    assert_eq!(
        state.last_error_notice(),
        Some(bootty_ui::error_catalog::ErrorNotice::LastSpaceCannotClose)
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
        )
        .expect("create Space")
        .expect("valid Space");
    let mut state = metadata_state(directory.path());
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

/// A project's script opens in a new tab of the session that asks, in its directory; an unknown
/// name says so.
#[cfg(unix)]
#[rstest]
fn project_scripts_open_in_new_tabs() {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let config = test_config::config(directory.path().join("config.toml"));
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new(
        config,
        event_repaint(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("app state");
    let started = Instant::now();
    open_native_session(&mut state, directory.path(), started);
    let cwd = directory.path().to_string_lossy().into_owned();
    let settings = serde_json::json!({"scripts": [
        {"name": "Mark", "command": "printf ran > marked; printf 'script-ready\n'; exec cat"},
    ]});
    let configured = submit_command(
        &mut state,
        CommandInvocation::new(
            "project.configure",
            vec![cwd.clone(), settings.to_string()],
            Caller::Socket,
        ),
        started,
    );
    assert_eq!(failure_kind(&configured), "success", "{configured:?}");
    let mut run = |command: &str, arguments: &[&str], file: &str| {
        let windows = state.mux().sessions()[0].windows.len();
        let outcome = submit_command(
            &mut state,
            CommandInvocation::new(
                command,
                arguments.iter().map(|arg| (*arg).to_owned()).collect(),
                Caller::Socket,
            ),
            started,
        );
        if !file.is_empty() {
            // The tab opens from the queue once the command has answered; its shell starts in
            // its own time.
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(10))
                .expect("test deadline fits");
            loop {
                state.update_frame(frames::idle_frame(started));
                if state.mux().sessions()[0].windows.len() > windows
                    && directory.path().join(file).exists()
                {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "{command} opened no tab that ran its script"
                );
                wakes
                    .recv_timeout(remaining)
                    .expect("script output wakes its host");
            }
        }
        outcome
    };
    let marked = run("project.script", &[&cwd, "Mark"], "marked");
    assert_eq!(failure_kind(&marked), "success", "{marked:?}");
    let unknown = run("project.script", &[&cwd, "Deploy"], "");
    assert!(
        format!("{unknown:?}").contains("The project has no script with that name"),
        "{unknown:?}"
    );
}

#[cfg(unix)]
#[rstest]
#[case::literal(vec!["/bin/sh", "-c", "printf '%s' \"$1\" > \"$2.tmp\"; mv \"$2.tmp\" \"$2\"; printf 'ready\\n'; exec cat", "tab", "quoted ' value; $HOME `uname`\nnext line"], false)]
#[case::missing_program(vec!["/does/not/exist"], true)]
fn explicit_tab_starts_literal_argv_and_failure_preserves_the_session(
    #[case] argv: Vec<&str>,
    #[case] fails: bool,
) {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let config = test_config::config(directory.path().join("config.toml"));
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new(
        config,
        event_repaint(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("app state");
    let started = Instant::now();
    open_native_session(&mut state, directory.path(), started);
    let original = state.mux().sessions()[0].clone();
    let output = directory.path().join("tab-output");
    let mut arguments = argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    if !fails {
        arguments.push(output.to_string_lossy().into_owned());
    }
    let outcome = submit_command(
        &mut state,
        CommandInvocation::new(
            "terminal.create_tab",
            vec![
                json_argv(&arguments),
                directory.path().to_string_lossy().into_owned(),
            ],
            Caller::Socket,
        ),
        started,
    );
    assert_eq!(
        state.mux().sessions().len(),
        1,
        "the parent session survives"
    );
    assert_eq!(state.mux().sessions()[0].id, original.id);
    if fails {
        assert_eq!(
            failure_kind(&outcome),
            "session_start_failed",
            "{outcome:?}"
        );
        assert_eq!(
            state.mux().sessions()[0].windows.len(),
            original.windows.len(),
            "only the failed new tab is closed"
        );
    } else {
        let CommandOutcome::Success { value, .. } = outcome else {
            panic!("{outcome:?}")
        };
        let target: CommandTarget =
            serde_json::from_value(value["created"].clone()).expect("issued terminal");
        assert_eq!(target.kind, ResourceKind::Terminal);
        assert_eq!(
            state.mux().sessions()[0].windows.len(),
            original
                .windows
                .len()
                .checked_add(1)
                .expect("one additional window")
        );
        loop {
            state.update_frame(frames::idle_frame(started));
            if let Ok(text) = fs::read_to_string(&output) {
                assert_eq!(text, argv[4], "argv bytes reach the new process unchanged");
                break;
            }
            wakes
                .recv_timeout(Duration::from_secs(5))
                .expect("native tab output");
        }
    }
}

#[rstest]
#[case(Caller::CommandPalette)]
#[case(Caller::Socket)]
#[case(Caller::Internal)]
fn session_grouping_command_persists_without_changing_sessions(#[case] caller: Caller) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    let now = Instant::now();
    open_native_session(&mut state, directory.path(), now);
    let sessions = state
        .mux()
        .sessions()
        .iter()
        .map(|session| session.id.clone())
        .collect::<Vec<_>>();
    let selected = state.mux().selected_session().map(str::to_owned);
    assert!(state.config().sidebar.group_by_project);
    assert!(matches!(
        submit_action(&mut state, "ui.sidebar.toggle_grouping", caller, now),
        CommandOutcome::Success { .. }
    ));
    assert!(!state.config().sidebar.group_by_project);
    let loaded =
        bootty_config::config::load_config_from_path(directory.path().join("config.toml")).unwrap();
    assert!(!loaded.sidebar.group_by_project);
    assert_eq!(
        state
            .mux()
            .sessions()
            .iter()
            .map(|session| session.id.clone())
            .collect::<Vec<_>>(),
        sessions
    );
    assert_eq!(state.mux().selected_session(), selected.as_deref());
    assert!(matches!(
        submit_action(&mut state, "ui.sidebar.toggle_grouping", caller, now),
        CommandOutcome::Success { .. }
    ));
    assert!(state.config().sidebar.group_by_project);
}

#[rstest]
fn rejected_session_view_save_keeps_the_accepted_view() {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    let path = directory.path().join("config.toml");
    if path.exists() {
        fs::remove_file(&path).unwrap();
    }
    fs::create_dir(&path).unwrap();
    let outcome = submit_action(
        &mut state,
        "ui.sidebar.toggle_grouping",
        Caller::Socket,
        Instant::now(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert!(state.config().sidebar.group_by_project);
}

fn saved_sessions(state: &mut AppState, space: &serde_json::Value) -> Vec<serde_json::Value> {
    let outcome = submit_command(
        state,
        session_request("session.saved", Vec::new(), space["target"].clone()),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("session.saved failed: {outcome:?}");
    };
    value.as_array().expect("saved sessions").clone()
}

#[rstest]
fn selected_saved_session_restores_on_start_and_reattaches_without_changing_identity_or_title() {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let project = directory.path().join("project");
    fs::create_dir(&project).expect("project directory");
    let mut state = terminal_state(directory.path());
    open_native_session(&mut state, &project, Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let original = saved_sessions(&mut state, &space).remove(0);
    let identity = original["identity"].as_str().expect("saved identity");
    let title = "Review #42: keyboard / input 🥟";
    let renamed = submit_command(
        &mut state,
        session_request(
            "session.set_title",
            owned(&[identity, title]),
            space["target"].clone(),
        ),
        Instant::now(),
    );
    assert_eq!(failure_kind(&renamed), "success", "{renamed:?}");
    assert_eq!(
        state.mux().sessions()[0].name,
        "project",
        "title is metadata"
    );
    let before = saved_sessions(&mut state, &space).remove(0);
    assert_eq!(before["title"], title);
    assert_eq!(before["identity"], original["identity"]);
    drop(state);

    // Native topology is shared across windows in one process. A fresh workspace key with
    // the same durable database represents a new process without forging backend internals.
    let restarted = assert_fs::TempDir::new().expect("restarted workspace");
    Connection::open(directory.path().join("session-order.sqlite3"))
        .expect("saved database")
        .execute(
            "VACUUM INTO ?1",
            [restarted
                .path()
                .join("session-order.sqlite3")
                .to_string_lossy()
                .as_ref()],
        )
        .expect("preserve committed database including WAL");
    let mut state = terminal_state(restarted.path());
    wait_for_saved_attachment(&mut state, identity);
    let space = spaces_listing(&mut state).remove(0);
    let restored = saved_sessions(&mut state, &space);
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0]["identity"], before["identity"]);
    assert_eq!(restored[0]["title"], before["title"]);
    assert_eq!(restored[0]["cwd"], before["cwd"]);
    assert_eq!(restored[0]["attachment"], "project");
    assert_eq!(state.mux().all_sessions().len(), 1);
    let restored_terminal = selection(&state);
    assert!(restored_terminal.1.is_some(), "restored pane is selected");
    let reopened = submit_command(
        &mut state,
        session_request(
            "session.reopen",
            owned(&[identity]),
            space["target"].clone(),
        ),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = reopened else {
        panic!("{reopened:?}");
    };
    assert_eq!(value["terminal"]["kind"], "terminal");
    let saved = saved_sessions(&mut state, &space);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["identity"], before["identity"]);
    assert_eq!(saved[0]["title"], before["title"]);
    assert_eq!(saved[0]["cwd"], before["cwd"]);
    assert!(saved[0]["attachment"].is_string());
    assert_eq!(selection(&state).0.as_deref(), Some("project"));
    let repeated = submit_command(
        &mut state,
        session_request(
            "session.reopen",
            owned(&[identity]),
            space["target"].clone(),
        ),
        Instant::now(),
    );
    assert_eq!(failure_kind(&repeated), "success", "{repeated:?}");
    assert_eq!(
        selection(&state),
        restored_terminal,
        "repeated reattachment uses the same restored process"
    );
    assert_eq!(state.mux().all_sessions().len(), 1);
}

#[rstest]
#[case::empty("")]
#[case::control("bad\ntitle")]
#[case::oversize(&"a".repeat(257))]
fn invalid_saved_session_title_leaves_existing_metadata_unchanged(#[case] title: &str) {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let mut state = metadata_state(directory.path());
    open_native_session(&mut state, directory.path(), Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let before = saved_sessions(&mut state, &space);
    let identity = before[0]["identity"].as_str().expect("identity");
    let outcome = submit_command(
        &mut state,
        session_request(
            "session.set_title",
            owned(&[identity, title]),
            space["target"].clone(),
        ),
        Instant::now(),
    );
    assert_eq!(
        failure_kind(&outcome),
        "session_title_failed",
        "{outcome:?}"
    );
    assert_eq!(saved_sessions(&mut state, &space), before);
}

#[rstest]
#[case::current(true)]
#[case::edited(false)]
fn generated_saved_title_preserves_a_newer_manual_edit(#[case] current: bool) {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let mut state = metadata_state(directory.path());
    open_native_session(&mut state, directory.path(), Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let original = saved_sessions(&mut state, &space).remove(0);
    let identity = original["identity"].as_str().unwrap();
    let expected = original["title"].as_str().unwrap();
    if !current {
        let outcome = submit_command(
            &mut state,
            session_request(
                "session.set_title",
                owned(&[identity, "Manual title"]),
                space["target"].clone(),
            ),
            Instant::now(),
        );
        assert_eq!(failure_kind(&outcome), "success");
    }
    let outcome = submit_command(
        &mut state,
        session_request(
            "session.set_title",
            owned(&[identity, "Generated title", expected]),
            space["target"].clone(),
        ),
        Instant::now(),
    );
    assert_eq!(failure_kind(&outcome), "success");
    assert_eq!(
        saved_sessions(&mut state, &space)[0]["title"],
        if current {
            "Generated title"
        } else {
            "Manual title"
        }
    );
}

#[rstest]
fn failed_saved_title_commit_keeps_live_and_restarted_metadata_unchanged() {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let mut state = metadata_state(directory.path());
    open_native_session(&mut state, directory.path(), Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let before = saved_sessions(&mut state, &space);
    let connection =
        Connection::open(directory.path().join("session-order.sqlite3")).expect("database");
    connection.execute_batch("CREATE TRIGGER reject_saved_title BEFORE DELETE ON workspace_sessions BEGIN SELECT RAISE(ABORT, 'injected title write failure'); END;").expect("failure boundary");
    let outcome = submit_command(
        &mut state,
        session_request(
            "session.set_title",
            owned(&[
                before[0]["identity"].as_str().expect("identity"),
                "Do not publish",
            ]),
            space["target"].clone(),
        ),
        Instant::now(),
    );
    assert_eq!(
        failure_kind(&outcome),
        "session_title_failed",
        "{outcome:?}"
    );
    assert_eq!(saved_sessions(&mut state, &space), before);
    connection
        .execute_batch("DROP TRIGGER reject_saved_title;")
        .expect("remove injected failure");
    drop(state);
    let restarted = assert_fs::TempDir::new().expect("restarted workspace");
    Connection::open(directory.path().join("session-order.sqlite3"))
        .expect("saved database")
        .execute(
            "VACUUM INTO ?1",
            [restarted
                .path()
                .join("session-order.sqlite3")
                .to_string_lossy()
                .as_ref()],
        )
        .expect("preserve committed database including WAL");
    let mut state = metadata_state(restarted.path());
    wait_for_saved_attachment(&mut state, before[0]["identity"].as_str().unwrap());
    let space = spaces_listing(&mut state).remove(0);
    let after = saved_sessions(&mut state, &space);
    assert_eq!(after[0]["identity"], before[0]["identity"]);
    assert_eq!(after[0]["title"], before[0]["title"]);
    assert_eq!(after[0]["cwd"], before[0]["cwd"]);
    assert_eq!(state.mux().all_sessions().len(), 1);
    assert_eq!(after[0]["attachment"], before[0]["attachment"]);
}

fn accepted_activity_receipt(
    state: &mut AppState,
    target: serde_json::Value,
    identity: &str,
    at: i64,
) -> CommandOutcome {
    let mut invocation = session_request(
        "session.activity",
        vec![identity.to_owned(), at.to_string()],
        target,
    );
    invocation.caller = Caller::Internal;
    let response = state
        .app_command_sender(Caller::Internal)
        .submit(
            invocation,
            Instant::now().checked_add(Duration::from_secs(1)).unwrap(),
            CommandCancellation::new(),
        )
        .unwrap();
    state.update_frame(frames::idle_frame(Instant::now()));
    response
        .try_recv()
        .expect("local metadata command completes in its frame")
}

#[rstest]
#[case(Caller::Socket)]
#[case(Caller::Cli)]
#[case(Caller::Luau)]
fn activity_requires_an_internal_accepted_input_receipt(#[case] caller: Caller) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    open_native_session(&mut state, directory.path(), Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let before = saved_sessions(&mut state, &space);
    let mut invocation = session_request(
        "session.activity",
        owned(&[before[0]["identity"].as_str().unwrap(), "42"]),
        space["target"].clone(),
    );
    invocation.caller = caller;
    let response = state
        .app_command_sender(caller)
        .submit(
            invocation,
            Instant::now().checked_add(Duration::from_secs(1)).unwrap(),
            CommandCancellation::new(),
        )
        .unwrap();
    state.update_frame(frames::idle_frame(Instant::now()));
    assert!(matches!(
        response.try_recv().unwrap(),
        CommandOutcome::Denied { .. }
    ));
    assert_eq!(saved_sessions(&mut state, &space), before);
}

#[rstest]
#[case("terminal.write", owned(&["accepted text"]))]
#[case("terminal.paste", owned(&["accepted paste"]))]
#[case("terminal.submit", Vec::new())]
fn accepted_terminal_input_records_durable_activity(
    #[case] command: &str,
    #[case] arguments: Vec<String>,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = terminal_state(directory.path());
    open_native_session(&mut state, directory.path(), Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let before = saved_sessions(&mut state, &space).remove(0);
    assert_eq!(before["state"]["last_activity_at"], serde_json::Value::Null);
    let current = submit_command(
        &mut state,
        CommandInvocation::new("resource.current", owned(&["terminal"]), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("{current:?}");
    };
    let input = session_request(command, arguments, value["target"].clone());
    let outcome = submit_command(&mut state, input, Instant::now());
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    state.update_frame(frames::idle_frame(Instant::now()));
    let after = saved_sessions(&mut state, &space).remove(0);
    assert!(
        after["state"]["last_activity_at"]
            .as_i64()
            .is_some_and(|at| at > 0)
    );
    let identity = after["identity"].as_str().unwrap();
    let repository = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .unwrap()
        .1;
    let saved = repository.spaces()[0]
        .binding()
        .sessions()
        .get(identity)
        .unwrap();
    assert_eq!(
        saved.state.last_activity_at,
        after["state"]["last_activity_at"].as_i64()
    );
}

#[rstest]
fn focus_empty_and_rejected_terminal_input_do_not_record_activity() {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = terminal_state(directory.path());
    open_native_session(&mut state, directory.path(), Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let before = saved_sessions(&mut state, &space);
    let mut frame = frames::idle_frame(Instant::now());
    frame.input.events = vec![
        bootty_gpui::InputEvent::WindowFocused(false),
        bootty_gpui::InputEvent::WindowFocused(true),
        bootty_gpui::InputEvent::ImeCommit(String::new()),
    ];
    state.update_frame(frame);
    let stale = CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "not a live terminal".to_owned(),
        generation: 1,
    };
    let mut input =
        CommandInvocation::new("terminal.write", owned(&["never sent"]), Caller::Socket);
    input.target = Some(stale);
    let outcome = submit_command(&mut state, input, Instant::now());
    assert!(!matches!(outcome, CommandOutcome::Success { .. }));
    state.update_frame(frames::idle_frame(Instant::now()));
    assert_eq!(saved_sessions(&mut state, &space), before);
}

#[rstest]
fn activity_is_max_only_and_failed_persistence_preserves_the_previous_timestamp() {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    open_native_session(&mut state, directory.path(), Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let saved = saved_sessions(&mut state, &space).remove(0);
    let identity = saved["identity"].as_str().unwrap();
    let initial = accepted_activity_receipt(&mut state, space["target"].clone(), identity, 42);
    assert!(matches!(initial, CommandOutcome::Success { .. }));
    let database = Connection::open(directory.path().join("session-order.sqlite3")).unwrap();
    database.execute_batch("CREATE TRIGGER reject_activity BEFORE DELETE ON workspace_sessions BEGIN SELECT RAISE(ABORT, 'injected activity failure'); END;").unwrap();
    for at in [42, 41] {
        let outcome = accepted_activity_receipt(&mut state, space["target"].clone(), identity, at);
        assert!(
            matches!(outcome, CommandOutcome::Success { ref value, .. } if value["changed"] == false),
            "{outcome:?}"
        );
    }
    let failed = accepted_activity_receipt(&mut state, space["target"].clone(), identity, 43);
    assert_eq!(failure_kind(&failed), "session_state_failed");
    assert_eq!(
        saved_sessions(&mut state, &space)[0]["state"]["last_activity_at"],
        42
    );
    let repository = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .unwrap()
        .1;
    assert_eq!(
        repository.spaces()[0]
            .binding()
            .sessions()
            .get(identity)
            .unwrap()
            .state
            .last_activity_at,
        Some(42)
    );
}

#[rstest]
#[case(-1)]
#[case(i64::MAX)]
fn invalid_activity_timestamp_is_refused(#[case] at: i64) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    open_native_session(&mut state, directory.path(), Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let before = saved_sessions(&mut state, &space);
    let outcome = accepted_activity_receipt(
        &mut state,
        space["target"].clone(),
        before[0]["identity"].as_str().unwrap(),
        at,
    );
    assert!(!matches!(outcome, CommandOutcome::Success { .. }));
    assert_eq!(saved_sessions(&mut state, &space), before);
}

#[rstest]
fn project_commands_register_empty_groups_without_creating_sessions_and_reject_old_bindings() {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    assert!(matches!(
        submit_action(
            &mut state,
            "new_mux_session",
            Caller::Socket,
            Instant::now()
        ),
        CommandOutcome::Success { .. }
    ));
    let Some(ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
        panic!("new session prompt");
    };
    let dialog_id = dialog.spec().id;
    let identity = dialog.draft().unwrap().identity.clone();
    let current = submit_command(
        &mut state,
        CommandInvocation::new("resource.current", owned(&["binding"]), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("binding: {current:?}");
    };
    let target: CommandTarget = serde_json::from_value(value["target"].clone()).unwrap();
    let invoke = |command: &str, args: Vec<String>| {
        let mut invocation = CommandInvocation::new(command, args, Caller::Socket);
        invocation.target = Some(target.clone());
        invocation
    };
    assert!(matches!(
        submit_command(
            &mut state,
            invoke("project.register", owned(&["/work/empty"])),
            Instant::now()
        ),
        CommandOutcome::Success { .. }
    ));
    state.dialog_projection();
    let Some(ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
        panic!("registration must keep the open form");
    };
    assert_eq!(dialog.spec().id, dialog_id);
    assert_eq!(dialog.draft().unwrap().identity, identity);
    assert_eq!(dialog.spec().projects[0].path, "/work/empty");
    let result = submit_command(
        &mut state,
        invoke("project.toggle_collapsed", owned(&["/work/empty"])),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = result else {
        panic!("collapse: {result:?}");
    };
    assert_eq!(value[0]["cwd"], "/work/empty");
    assert_eq!(value[0]["collapsed"], true);
    assert_eq!(state.mux().sessions().len(), 0);
    drop(state);
    let mut reopened = metadata_state(directory.path());
    assert!(matches!(
        submit_command(
            &mut reopened,
            invoke("project.register", owned(&["/work/stale"])),
            Instant::now()
        ),
        CommandOutcome::StaleTarget { .. }
    ));
    let (repository, _) = WorkspaceRepository::open(&directory.path().join("config.toml")).unwrap();
    assert_eq!(repository.registered_projects().unwrap().len(), 1);
    assert!(matches!(
        submit_command(
            &mut reopened,
            invoke("project.remove", owned(&["/work/empty"])),
            Instant::now()
        ),
        CommandOutcome::StaleTarget { .. }
    ));
    let current = submit_command(
        &mut reopened,
        CommandInvocation::new("resource.current", owned(&["binding"]), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("current binding")
    };
    let mut remove =
        CommandInvocation::new("project.remove", owned(&["/work/empty"]), Caller::Socket);
    remove.target = Some(serde_json::from_value(value["target"].clone()).unwrap());
    assert!(matches!(
        submit_command(&mut reopened, remove, Instant::now()),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(repository.registered_projects().unwrap(), Vec::new());
}

#[rstest]
#[case(Caller::Keybinding)]
#[case(Caller::CommandPalette)]
#[case(Caller::Socket)]
fn add_project_requests_a_system_folder_chooser_and_registers_any_location(#[case] caller: Caller) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    let outcomes = state
        .app_command_sender(caller)
        .submit(
            CommandInvocation::from_action("add_project", caller),
            Instant::now()
                .checked_add(Duration::from_secs(1))
                .expect("short deadline"),
            CommandCancellation::new(),
        )
        .unwrap();
    let effects = state.update_frame(frames::idle_frame(Instant::now()));
    let outcome = outcomes.try_recv().unwrap();
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let target = effects
        .iter()
        .find_map(|effect| match effect {
            bootty_ui::AppEffect::ChooseProjectDirectory(target) => Some(target.clone()),
            _ => None,
        })
        .expect("local Add project opens the system folder chooser");
    assert!(state.modal_dialog().is_none());
    let mut invocation = CommandInvocation::new(
        "project.register",
        vec![directory.path().to_str().unwrap().to_owned()],
        caller,
    );
    invocation.target = Some(target);
    let outcome = submit_command(&mut state, invocation, Instant::now());
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let (repository, _) = WorkspaceRepository::open(&directory.path().join("config.toml")).unwrap();
    assert_eq!(
        repository.registered_projects().unwrap()[0].cwd,
        directory.path().to_str().unwrap()
    );
    assert_eq!(state.mux().sessions(), []);
}

#[rstest]
fn sidebar_navigation_skips_sessions_in_collapsed_projects(
    #[values("ui.sidebar.next_session", "next_session", "previous_session")] action: &str,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let mut state = terminal_state(directory.path());
    open_native_session(&mut state, &first, Instant::now());
    let first_id = state.mux().selected_session().unwrap().to_owned();
    open_native_session(&mut state, &second, Instant::now());
    let current = submit_command(
        &mut state,
        CommandInvocation::new("resource.current", owned(&["binding"]), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("binding: {current:?}");
    };
    let mut collapse = CommandInvocation::new(
        "project.toggle_collapsed",
        vec![second.to_string_lossy().into_owned()],
        Caller::Socket,
    );
    collapse.target = Some(serde_json::from_value(value["target"].clone()).unwrap());
    assert!(matches!(
        submit_command(&mut state, collapse, Instant::now()),
        CommandOutcome::Success { .. }
    ));
    assert!(matches!(
        submit_action(&mut state, action, Caller::Socket, Instant::now()),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(
        state.sidebar_hovered_session().unwrap().session_id,
        first_id
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn close_surface_on_creation_preserves_the_underlying_workspace(
    #[case] has_session: bool,
    #[values(Caller::Keybinding, Caller::BuiltinKeybinding, Caller::CommandPalette)] caller: Caller,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    if has_session {
        open_native_session(&mut state, directory.path(), Instant::now());
    }
    let pane = state
        .mux()
        .selected_session_anchor()
        .and_then(|anchor| anchor.pane_id.clone());
    let count = state.mux().sessions().len();
    let opened = submit_action(
        &mut state,
        "new_mux_session",
        Caller::Keybinding,
        Instant::now(),
    );
    assert!(
        matches!(opened, CommandOutcome::Success { .. }),
        "{opened:?}"
    );
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::NewSession(_))
    ));
    let started = Instant::now();
    let outcomes = state
        .app_command_sender(caller)
        .submit(
            CommandInvocation::from_action("close_surface", caller),
            started.checked_add(Duration::from_secs(1)).unwrap(),
            CommandCancellation::new(),
        )
        .unwrap();
    state.update_frame(frames::idle_frame(started));
    let closed = outcomes.try_recv().expect("composer close is synchronous");
    assert!(
        matches!(closed, CommandOutcome::Success { .. }),
        "{closed:?}"
    );
    assert_eq!(state.mux().sessions().len(), count);
    assert_eq!(
        state
            .mux()
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone()),
        pane
    );
    assert_eq!(
        matches!(state.modal_dialog(), Some(ModalDialog::NewSession(_))),
        !has_session
    );
    assert_eq!(state.last_error(), None);
}

#[rstest]
fn session_close_ends_session_when_checkpoint_storage_fails() {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut state = metadata_state(directory.path());
    let started = Instant::now();
    open_native_session(&mut state, directory.path(), started);
    let space = spaces_listing(&mut state).remove(0);
    let connection = Connection::open(directory.path().join("session-order.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_checkpoint BEFORE UPDATE OF terminal_snapshot ON workspace_sessions BEGIN SELECT RAISE(ABORT, 'injected checkpoint failure'); END;").unwrap();
    let mut close = session_request(
        "session.close",
        Vec::new(),
        space["sessions"][0]["target"].clone(),
    );
    close.confirmation = Some(close.confirmation());
    let outcome = submit_command(&mut state, close, started);
    assert_eq!(failure_kind(&outcome), "success", "{outcome:?}");
    assert_eq!(state.mux().all_sessions(), []);
}
