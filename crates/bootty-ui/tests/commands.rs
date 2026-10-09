#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

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
use bootty_ui::commands::{BrowserAction, CommandCatalog, CommandExecutor, CoreCommandExecutor};
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
    let mut config = test_config::config(
        directory.join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.working_directory = Some(directory.to_owned());
    AppState::new(config, support::backends(), Arc::new(|| {}), None, None).expect("app state")
}

fn open_native_session(state: &mut AppState, cwd: &Path, started: Instant) {
    let outcome = submit_action(state, "new_mux_session", Caller::Socket, started);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let id = state
        .modal_dialog()
        .and_then(|dialog| match dialog {
            ModalDialog::NewSession(dialog) => Some(dialog.spec().id),
            _ => None,
        })
        .expect("new session form is open");
    state.apply_dialog_intent(
        &bootty_gpui::DialogIntent::FieldChanged {
            dialog: id,
            field: "mode".to_owned(),
            value: "Terminal".to_owned(),
        },
        &mut Vec::new(),
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
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.working_directory = Some(directory.path().to_owned());
    let (wake, wakes) = mpsc::channel();
    let state = AppState::new(
        config,
        support::backends(),
        Arc::new(move || {
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
    state.apply_dialog_intent(
        &bootty_gpui::DialogIntent::FieldChanged {
            dialog: id.clone(),
            field: "mode".to_owned(),
            value: "Terminal".to_owned(),
        },
        &mut Vec::new(),
    );
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
        assert_eq!(spec.text_label.as_deref(), Some("Terminal"));
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
    let mut state = native_state(directory.path());
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
    "_project_v1 notes".to_owned(),
    "_project_v1 notes-2".to_owned()
)]
#[case::bounded_utf8(
    format!(".{}é", "x".repeat(252)),
    format!("_{}é", "x".repeat(252)),
    format!("_{}-2", "x".repeat(252))
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
    let mut state = native_state(directory.path());
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
#[case(
    "new_mux_session",
    MutationClass::Write,
    Some(ResourceKind::ApplicationWindow)
)]
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
fn browser_commands_resolve_typed_page_targets() {
    let catalog = CommandCatalog::default();
    let open = catalog
        .resolve(CommandInvocation::new(
            "browser.open",
            vec!["http://localhost:48159".to_owned(), "19".to_owned()],
            Caller::Socket,
        ))
        .expect("resolve browser open");
    assert!(matches!(
        open.executor,
        CommandExecutor::Core(CoreCommandExecutor::Browser(
            BrowserAction::Open(address),
            Some(19)
        )) if address == "http://localhost:48159"
    ));

    let close = catalog
        .resolve(CommandInvocation::new(
            "browser.close_tab",
            vec!["19".to_owned()],
            Caller::Socket,
        ))
        .expect("resolve browser close");
    assert!(matches!(
        close.executor,
        CommandExecutor::Core(CoreCommandExecutor::Browser(
            BrowserAction::CloseTab(19),
            Some(19)
        ))
    ));
    assert!(matches!(
        catalog.resolve(CommandInvocation::new(
            "browser.address",
            vec!["0".to_owned()],
            Caller::Socket,
        )),
        Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments"
    ));

    let descriptor = catalog
        .describe("browser.address")
        .expect("browser address descriptor");
    let [page_id] = descriptor.arguments.arguments.as_slice() else {
        panic!("browser page argument schema: {:?}", descriptor.arguments);
    };
    assert_eq!(page_id.name, "page-id");
    assert_eq!(page_id.value_type, ValueType::Integer);
    assert!(!page_id.required);
    assert_eq!(page_id.minimum, Some(1));
}

#[rstest]
fn browser_request_honors_command_cancellation() {
    let directory = assert_fs::TempDir::new().expect("isolated config");
    let mut state = native_state(directory.path());
    let started = Instant::now();
    let cancellation = CommandCancellation::new();
    let outcomes = state
        .app_command_sender(Caller::Socket)
        .submit(
            CommandInvocation::from_action("browser.reload", Caller::Socket),
            started
                .checked_add(Duration::from_secs(5))
                .expect("test deadline fits"),
            cancellation.clone(),
        )
        .expect("submit browser command");
    let effects = state.update_frame(frames::idle_frame(started));
    let request = effects
        .into_iter()
        .find_map(|effect| match effect {
            bootty_ui::AppEffect::Browser(request) => Some(request),
            _ => None,
        })
        .expect("window receives the browser request");
    assert!(matches!(
        outcomes.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    assert!(cancellation.cancel());
    state.update_frame(frames::idle_frame(
        started
            .checked_add(Duration::from_millis(1))
            .expect("test timestamp fits"),
    ));
    assert!(matches!(
        outcomes.try_recv(),
        Ok(CommandOutcome::Failed { code, .. }) if code == "cancelled"
    ));
    request.complete(CommandOutcome::success());
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
        Some(ResourceKind::Session)
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

#[cfg(unix)]
fn wait_for_native_agent_input(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    target: &CommandTarget,
) -> Result<(), Box<dyn std::error::Error>> {
    let service = state
        .terminal_agent_service()
        .ok_or("native agent service is unavailable")?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or("observation deadline does not fit")?;
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if service.live_records().iter().any(|record| {
            record.target == *target
                && matches!(
                    record.observation.status,
                    bootty_agents::TerminalAgentStatus::Idle
                        | bootty_agents::TerminalAgentStatus::Working
                        | bootty_agents::TerminalAgentStatus::Waiting
                )
        }) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "native observation did not become ready for {target:?}: {:?}",
                service
                    .live_records()
                    .into_iter()
                    .map(|record| (record.target, record.observation))
                    .collect::<Vec<_>>()
            )
            .into());
        }
        match wakes.recv_timeout(remaining) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(unix)]
#[rstest]
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
    let program = claude_terminal_fixture(directory.path()).unwrap();
    let session_id = "00000000-0000-4000-8000-000000000007";
    // This test exercises caller routing, so make the provider query ready before launch.
    fs::write(
        directory.path().join("query-session"),
        serde_json::to_vec(&serde_json::json!({"session_id":session_id})).unwrap(),
    )
    .unwrap();
    let launched = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "agents.claude.start",
            vec![
                directory.path().to_string_lossy().into_owned(),
                program.to_string_lossy().into_owned(),
                serde_json::to_string(&["--session-id", session_id]).unwrap(),
            ],
            Caller::Socket,
        ),
        started,
    );
    let CommandOutcome::Success { value, .. } = launched else {
        panic!("native agent launch failed: {launched:?}");
    };
    let target: CommandTarget = serde_json::from_value(value["terminal"].clone()).unwrap();
    wait_for_native_agent_input(&mut state, &wakes, &target).unwrap();
    let selected = state.mux().selected_session().map(str::to_owned);

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
        let mut invocation = CommandInvocation::new(
            "agents.claude.prompt",
            vec![format!("caller-{index}")],
            caller,
        );
        invocation.target = Some(target.clone());
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
        assert_eq!(state.mux().selected_session(), selected.as_deref());
    }
    let messages = directory.path().join("agent-input");
    loop {
        state.update_frame(frames::idle_frame(started));
        if let Ok(text) = fs::read_to_string(&messages) {
            let received: Vec<String> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            if received.len() == callers.len() {
                assert_eq!(
                    received,
                    (0..callers.len())
                        .map(|index| format!("caller-{index}"))
                        .collect::<Vec<_>>()
                );
                break;
            }
        }
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("provider terminal receives submitted prompts");
    }
    let mut stale = target;
    stale.generation = stale.generation.checked_add(1).unwrap();
    let mut invocation = CommandInvocation::new(
        "agents.claude.prompt",
        vec!["must not arrive".to_owned()],
        Caller::Socket,
    );
    invocation.target = Some(stale);
    assert!(matches!(
        submit_command_from_caller(&mut state, &wakes, Caller::Socket, invocation, started),
        CommandOutcome::StaleTarget { .. }
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

    let outcome = submit_terminal_choice(
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

    let direct = submit_terminal_choice(
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
fn ditch_session_keeps_saved_identity_after_authoritative_command() {
    let directory = assert_fs::TempDir::new().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let started = Instant::now();
    let mut state = native_state(directory.path());
    let created = submit_terminal_choice(&mut state, "new_tab", Caller::Socket, started);
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
        reopened.spaces()[0]
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
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new(
        test_config::config(
            directory.path().join("config.toml"),
            MultiplexerBackendConfig::Native,
        ),
        support::backends(),
        Arc::new(move || {
            _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("app state");
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

    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline fits");
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if !state
            .binding_session_groups()
            .iter()
            .flat_map(|group| group.sessions.iter())
            .any(|session| session.id == session_id)
        {
            break;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("partial cleanup must still submit Ditch and wake the app");
    }
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
    let outcome = submit_terminal_choice(
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
    config.session.working_directory = Some(directory.path().to_owned());
    config.when_closing_with_no_tabs = bootty_config::config::WhenClosingWithNoTabs::CloseWindow;
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).unwrap();
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
            receiver.recv_timeout(Duration::from_millis(5)).ok()
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

#[rstest]
fn keeping_the_window_open_still_closes_its_last_backend_pane(
    #[values(Caller::Keybinding, Caller::Socket)] caller: Caller,
) {
    let directory = assert_fs::TempDir::new().expect("isolated workspace");
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.working_directory = Some(directory.path().to_owned());
    config.when_closing_with_no_tabs = bootty_config::config::WhenClosingWithNoTabs::KeepWindowOpen;
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).unwrap();
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
            started.checked_add(Duration::from_secs(5)).unwrap(),
            CommandCancellation::new(),
        )
        .unwrap();
    let outcome = loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if let Ok(outcome) = outcomes.recv_timeout(Duration::from_millis(5)) {
            break outcome;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "close command completed"
        );
    };
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.mux().sessions(), []);
}

fn submit_terminal_choice(
    state: &mut AppState,
    action: &str,
    caller: Caller,
    started: Instant,
) -> CommandOutcome {
    let before = (
        state
            .mux()
            .all_sessions()
            .iter()
            .map(|session| session.id.clone())
            .collect::<Vec<_>>(),
        state
            .mux()
            .all_sessions()
            .iter()
            .flat_map(|session| session.windows.iter().map(|window| window.id.clone()))
            .collect::<Vec<_>>(),
        pane_count(state),
    );
    let opened = submit_action(state, action, caller, started);
    assert!(
        matches!(opened, CommandOutcome::Success { .. }),
        "{opened:?}"
    );
    assert_eq!(
        (
            state
                .mux()
                .all_sessions()
                .iter()
                .map(|session| session.id.clone())
                .collect::<Vec<_>>(),
            state
                .mux()
                .all_sessions()
                .iter()
                .flat_map(|session| session.windows.iter().map(|window| window.id.clone()))
                .collect::<Vec<_>>(),
            pane_count(state)
        ),
        before,
        "Opening a chooser must not create a backend terminal"
    );
    let request = state.pending_new_surface().expect("retained chooser").id;
    let created = submit_command(
        state,
        CommandInvocation::new(
            "surface.choose",
            vec![request.to_string(), "terminal".to_owned()],
            caller,
        ),
        started,
    );
    if let CommandOutcome::Success { value, .. } = &created {
        let target = value
            .get("terminal")
            .or_else(|| value.get("created"))
            .expect("actual created terminal target");
        let mut focus = CommandInvocation::new("agents.focus", Vec::new(), caller);
        focus.target = Some(serde_json::from_value(target.clone()).expect("issued target"));
        let focused = submit_command(state, focus, started);
        assert!(
            matches!(focused, CommandOutcome::Success { .. }),
            "{focused:?}"
        );
    }
    created
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
            creation_receipt: None,
            invocation,
            deadline: Instant::now()
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
        "files.source",
        vec![
            name.clone(),
            directory.path().to_string_lossy().into_owned(),
        ],
    ) else {
        panic!("read bounded source")
    };
    let FileResponse::Source(source) = serde_json::from_value(value).unwrap() else {
        panic!("file source descriptor")
    };
    assert_eq!(source.name, "document.rs");
    assert_eq!(source.len, 8);
    let outside = directory.path().join("restricted");
    fs::create_dir(&outside).unwrap();
    assert!(
        matches!(run("files.source", vec![name.clone(), outside.to_string_lossy().into_owned()]),
        CommandOutcome::Failed { code, .. } if code == "file_failed")
    );
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
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    // OSC titles are independent of layout mutations; keep this fixture's shell title stable.
    config.session.shell = cfg!(unix).then(|| "/bin/sh".to_owned());
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).expect("app state");
    open_native_session(&mut state, directory.path(), started);
    let first_window = state.mux().selected_window().expect("window").to_owned();
    let first = state.focused_pane().expect("first pane");
    assert!(matches!(
        submit_terminal_choice(&mut state, "split_right", Caller::Socket, started),
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
        submit_terminal_choice(&mut state, "new_tab", Caller::Socket, started),
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
#[case("light", bootty_config::config::AppearanceVariant::Light)]
#[case("dark", bootty_config::config::AppearanceVariant::Dark)]
fn authored_theme_preview_restore_save_and_apply_share_command_path(
    #[case] appearance: &str,
    #[case] variant: bootty_config::config::AppearanceVariant,
) {
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
        "[multiplexer]\nbackend='native'\n[font]\nsize=22\n[chrome]\nsidebar-width=333\n[appearance]\nmode='{mode}'\n[appearance.light.colors]\nbackground='#abcdef'\n[appearance.dark.colors]\nbackground='#fedcba'\n"
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

#[cfg(unix)]
fn claude_terminal_fixture(directory: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;

    let program = directory.join("claude-fixture.py");
    fs::write(
        &program,
        r"#!/usr/bin/env python3
import json, os, pathlib, sys
directory = pathlib.Path(__file__).parent
if sys.argv[1:3] == ['agents', '--json']:
    source = directory / 'query-session'
    if not source.exists():
        source = directory / 'agent-output'
    if source.exists():
        observed = json.loads(source.read_text())
        print(json.dumps([{'sessionId': observed['session_id'], 'status': 'busy',
                           'waitingFor': observed.get('detail')}]))
    else:
        print('[]')
    sys.exit(0)
arguments = sys.argv[1:]
session_id = arguments[1]
arguments = arguments[2:]
tool_configs = []
while '--mcp-config' in arguments:
    index = arguments.index('--mcp-config')
    tool_configs.append(arguments[index + 1])
    del arguments[index:index + 2]
path = directory / 'agent-output'
temporary = directory / 'agent-output.tmp'
temporary.write_text(json.dumps({'argv': arguments, 'session_id': session_id,
                                 'cwd': os.getcwd(), 'tty': os.isatty(0),
                                 'account_directory': os.environ.get('CLAUDE_CONFIG_DIR'),
                                 'tool_configs': tool_configs}))
os.replace(temporary, path)
print('agent ready', flush=True)
with open(directory / 'agent-input', 'a') as received:
    for line in sys.stdin:
        received.write(json.dumps(line.rstrip('\n')) + '\n')
        received.flush()
        print('received prompt', flush=True)
",
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    Ok(program)
}

#[cfg(unix)]
#[rstest]
#[case("start", Vec::new())]
#[case("tab", Vec::new())]
#[case("pane", vec!["right".to_owned()])]
#[case("resume", vec!["existing-conversation".to_owned()])]
#[case("fork", vec!["existing-conversation".to_owned()])]
fn disabled_providers_do_not_launch_or_mutate_terminals(
    #[case] operation: &str,
    #[case] arguments: Vec<String>,
    #[values("codex", "claude", "pi")] provider: &str,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    for preferences in [
        &mut config.agents.codex,
        &mut config.agents.claude,
        &mut config.agents.pi,
    ] {
        preferences.enabled = false;
        preferences.program = directory
            .path()
            .join("not-installed")
            .to_string_lossy()
            .into_owned();
    }
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
    let now = Instant::now();
    open_native_session(&mut state, directory.path(), now);
    // Shell startup can change a window title while the denied command is polled.
    // Compare backend topology and focus, rather than asynchronous process facts.
    let topology = |state: &AppState| {
        state
            .mux()
            .all_sessions()
            .iter()
            .map(|session| {
                (
                    session.id.clone(),
                    session.name.clone(),
                    session
                        .windows
                        .iter()
                        .map(|window| {
                            (
                                window.id.clone(),
                                window.layout.clone(),
                                window
                                    .panes
                                    .iter()
                                    .map(|pane| pane.pane_id.clone())
                                    .collect::<Vec<_>>(),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let before = topology(&state);
    let focus = selection(&state);
    let outcome = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            format!("agents.{provider}.{operation}"),
            arguments,
            Caller::Socket,
        ),
        now,
    );
    assert!(
        matches!(outcome, CommandOutcome::Denied { .. }),
        "{outcome:?}"
    );
    assert_eq!(topology(&state), before);
    assert_eq!(selection(&state), focus);
    assert!(state.terminal_agent_service().unwrap().records().is_empty());
}

#[cfg(unix)]
#[rstest]
#[case::tab("new_tab")]
#[case::pane("split_right")]
fn chooser_profile_keeps_the_accepted_terminal_when_its_checkpoint_write_fails(
    #[case] action: &str,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.agents.claude.program = claude_terminal_fixture(directory.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    config.agents.claude.selected = "work".to_owned();
    config.agents.claude.profiles.insert(
        "work".to_owned(),
        bootty_config::config::AgentProfileConfig {
            name: "Work".to_owned(),
            directory: Some(
                directory
                    .path()
                    .join("account")
                    .to_string_lossy()
                    .into_owned(),
            ),
            arguments: Vec::new(),
        },
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
    let database_path = directory.path().join("session-order.sqlite3");
    let (identity, prior): (String, String) = {
        let database = Connection::open(&database_path).unwrap();
        let saved = database
            .query_row(
                "SELECT identity, terminal_snapshot FROM workspace_sessions LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        database.execute_batch("CREATE TRIGGER reject_created_checkpoint BEFORE UPDATE OF terminal_snapshot ON workspace_sessions BEGIN SELECT RAISE(ABORT, 'injected checkpoint write failure'); END;").unwrap();
        saved
    };
    let opened = submit_action(&mut state, action, Caller::Socket, started);
    assert!(
        matches!(opened, CommandOutcome::Success { .. }),
        "{opened:?}"
    );
    let pending = state.pending_new_surface().unwrap();
    let request = pending.id;
    let bootty_ui::surface_creation::SurfaceParent::Terminal(parent) = &pending.parent else {
        panic!("profile creation has an exact terminal parent");
    };
    let parent = parent.clone();
    let response = state
        .app_command_sender(Caller::Socket)
        .submit(
            CommandInvocation::new(
                "surface.choose",
                vec![
                    request.to_string(),
                    "profile".to_owned(),
                    "claude".to_owned(),
                ],
                Caller::Socket,
            ),
            Instant::now().checked_add(Duration::from_secs(5)).unwrap(),
            CommandCancellation::new(),
        )
        .unwrap();
    let mut effects = Vec::new();
    let outcome = loop {
        effects.extend(state.update_frame(frames::idle_frame(started)));
        if let Ok(outcome) = response.try_recv() {
            break outcome;
        }
        wakes.recv_timeout(Duration::from_secs(5)).unwrap();
    };
    assert!(
        matches!(&outcome, CommandOutcome::Failed { code, message }
        if code == "session_checkpoint_failed" && message.contains("injected checkpoint write failure")),
        "{outcome:?}"
    );
    let target = effects
        .iter()
        .find_map(|effect| match effect {
            bootty_ui::AppEffect::AttachNewSurface { request_id, target }
                if *request_id == request =>
            {
                Some(target.clone())
            }
            _ => None,
        })
        .expect("the accepted child stays visible despite failed persistence");
    assert_eq!(target.kind, ResourceKind::Terminal);
    assert_ne!(target, parent, "the receipt attaches the admitted child");
    assert!(state.pending_new_surface().is_none());
    let sessions = state.mux().all_sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].windows.len(),
        if action == "new_tab" { 2 } else { 1 },
        "tabs add a window while splits add a pane to the original window"
    );
    assert_eq!(
        sessions[0]
            .windows
            .iter()
            .map(|window| window.panes.len())
            .sum::<usize>(),
        2,
        "the backend retains both real terminal panes"
    );
    let mut capture = CommandInvocation::new("terminal.capture", Vec::new(), Caller::Socket);
    capture.target = Some(target);
    let captured = submit_command(&mut state, capture, started);
    assert!(
        matches!(captured, CommandOutcome::Success { .. }),
        "{captured:?}"
    );
    let mut capture = CommandInvocation::new("terminal.capture", Vec::new(), Caller::Socket);
    capture.target = Some(parent);
    let captured = submit_command(&mut state, capture, started);
    assert!(
        matches!(captured, CommandOutcome::Success { .. }),
        "the original terminal remains live: {captured:?}"
    );
    let after: String = {
        let database = Connection::open(&database_path).unwrap();
        database
            .query_row(
                "SELECT terminal_snapshot FROM workspace_sessions WHERE identity = ?1",
                [&identity],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        after, prior,
        "failure preserves the prior committed checkpoint"
    );
}

#[cfg(unix)]
#[rstest]
#[case::session("start", None)]
#[case::tab("tab", None)]
#[case::right_pane("pane", Some("right"))]
#[case::down_pane("pane", Some("down"))]
fn agent_start_delivers_literal_arguments_to_a_new_native_pty(
    #[values(true, false)] explicit_cwd: bool,
    #[case] operation: &str,
    #[case] direction: Option<&str>,
    #[values(Caller::CommandPalette, Caller::Cli, Caller::Socket)] caller: Caller,
    #[values(false, true)] configured_profile: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let configured_cwd = directory.path().join("configured-directory");
    fs::create_dir(&configured_cwd).unwrap();
    config.session.working_directory = Some(configured_cwd);
    let program = claude_terminal_fixture(directory.path()).unwrap();
    let output = directory.path().join("agent-output");
    let literal = "quoted ' value; $HOME `uname`\nsecond line\tend";
    let account = directory.path().join("account ' ; $HOME");
    if configured_profile {
        config.agents.claude.program = program.to_string_lossy().into_owned();
        config.agents.claude.selected = if direction.is_some() {
            String::new()
        } else {
            "work".to_owned()
        };
        config.agents.claude.profiles.insert(
            "work".to_owned(),
            bootty_config::config::AgentProfileConfig {
                name: "Work".to_owned(),
                directory: Some(account.to_string_lossy().into_owned()),
                arguments: vec![literal.to_owned(), output.to_string_lossy().into_owned()],
            },
        );
    }
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
    let argv = serde_json::to_string(&[literal, output.to_str().unwrap()]).unwrap();
    let mut arguments = vec![
        if explicit_cwd {
            directory.path().to_string_lossy().into_owned()
        } else {
            String::new()
        },
        if configured_profile {
            String::new()
        } else {
            program.to_string_lossy().into_owned()
        },
        if configured_profile {
            String::new()
        } else {
            argv
        },
    ];
    if let Some(direction) = direction {
        arguments.insert(0, direction.to_owned());
        arguments.push(if configured_profile {
            "work".to_owned()
        } else {
            String::new()
        });
    }
    let mut invocation =
        CommandInvocation::new(format!("agents.claude.{operation}"), arguments, caller);
    if direction.is_some() {
        let current = submit_command(
            &mut state,
            CommandInvocation::new("resource.current", owned(&["terminal"]), caller),
            started,
        );
        let CommandOutcome::Success { value, .. } = current else {
            panic!("{current:?}");
        };
        invocation.target = Some(serde_json::from_value(value["target"].clone()).unwrap());
    }
    let outcome = submit_command_from_caller(&mut state, &wakes, caller, invocation, started);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    loop {
        state.update_frame(frames::idle_frame(started));
        if let Ok(text) = fs::read_to_string(&output) {
            let observed: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(observed["argv"][0], literal);
            assert_eq!(observed["argv"][1], output.to_str().unwrap());
            assert_eq!(observed["session_id"].as_str().unwrap().len(), 36);
            assert_eq!(
                Path::new(observed["cwd"].as_str().unwrap())
                    .canonicalize()
                    .unwrap(),
                directory.path().canonicalize().unwrap()
            );
            assert_eq!(observed["tty"], true);
            let captured_account = state
                .terminal_agent_service()
                .unwrap()
                .records()
                .last()
                .unwrap()
                .launch
                .account_directory
                .clone()
                .expect("native launch freezes its effective account directory");
            assert_eq!(
                observed["account_directory"].as_str(),
                Some(if configured_profile {
                    account.to_str().unwrap()
                } else {
                    captured_account.as_str()
                })
            );
            assert_eq!(
                state.mux().all_sessions().len(),
                1,
                "Agent launch adds a tab or pane, never a session"
            );
            let CommandOutcome::Success { value, .. } = &outcome else {
                panic!("native launch failed: {outcome:?}");
            };
            let target: CommandTarget = serde_json::from_value(value["created"].clone()).unwrap();
            let record = state
                .terminal_agent_service()
                .unwrap()
                .record(&target)
                .unwrap();
            assert_eq!(record.provider, bootty_agents::AgentKind::Claude);
            assert_eq!(record.launch.program, program.to_string_lossy());
            assert_eq!(record.launch.arguments, Vec::<String>::new());
            if direction.is_some() {
                assert_eq!(state.mux().sessions()[0].windows.len(), 1);
                assert_eq!(pane_count(&state), 2);
            }
            break;
        }
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("native agent output");
    }
}

#[cfg(unix)]
#[rstest]
#[case::invalid_direction("direction")]
#[case::stale_generation("stale")]
#[case::retired_parent("retired")]
#[case::disabled_provider("disabled")]
fn provider_splits_reject_invalid_or_retired_destinations_before_launch(#[case] boundary: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.agents.claude.enabled = boundary != "disabled";
    config.agents.claude.program = directory
        .path()
        .join("must-not-launch")
        .to_string_lossy()
        .into_owned();
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
    let now = Instant::now();
    open_native_session(&mut state, directory.path(), now);
    let current = submit_command(
        &mut state,
        CommandInvocation::new("resource.current", owned(&["terminal"]), Caller::Socket),
        now,
    );
    let CommandOutcome::Success { value, .. } = current else {
        panic!("{current:?}");
    };
    let mut parent: CommandTarget = serde_json::from_value(value["target"].clone()).unwrap();
    if boundary == "stale" {
        parent.generation = parent.generation.checked_add(1).unwrap();
    }
    if boundary == "retired" {
        let mut close = CommandInvocation::new("pane.close", Vec::new(), Caller::Socket);
        close.target = Some(parent.clone());
        close.confirmation = Some(close.confirmation());
        let closed = submit_command_from_caller(&mut state, &wakes, Caller::Socket, close, now);
        assert!(
            matches!(closed, CommandOutcome::Success { .. }),
            "{closed:?}"
        );
    }
    let before = state.mux().all_sessions().to_vec();
    let mut launch = CommandInvocation::new(
        "agents.claude.pane",
        owned(&[if boundary == "direction" {
            "left"
        } else {
            "right"
        }]),
        Caller::Socket,
    );
    launch.target = Some(parent);
    let outcome = submit_command_from_caller(&mut state, &wakes, Caller::Socket, launch, now);
    match boundary {
        "direction" => assert!(
            matches!(outcome, CommandOutcome::Failed { .. }),
            "{outcome:?}"
        ),
        "disabled" => assert!(
            matches!(outcome, CommandOutcome::Denied { .. }),
            "{outcome:?}"
        ),
        _ => assert!(
            matches!(outcome, CommandOutcome::StaleTarget { .. }),
            "{outcome:?}"
        ),
    }
    assert_eq!(state.mux().all_sessions(), before);
    assert!(state.terminal_agent_service().unwrap().records().is_empty());
}

#[cfg(unix)]
#[rstest]
fn native_state_is_bounded_and_saved_provider_history_keeps_selected_account_and_space() {
    let directory = assert_fs::TempDir::new().unwrap();
    let elsewhere = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect("workspace")
        .0
        .create_space(
            "Elsewhere",
            "2",
            [1, 2, 3],
            false,
            SpaceMuxOverride::default(),
            false,
        )
        .expect("create Space")
        .expect("valid Space")
        .id();
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let account = directory.path().join("selected-history-account");
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.agents.claude.selected = "work".to_owned();
    config.agents.claude.profiles.insert(
        "work".to_owned(),
        bootty_config::config::AgentProfileConfig {
            name: "Work".to_owned(),
            directory: Some(account.to_string_lossy().into_owned()),
            arguments: Vec::new(),
        },
    );
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
    let program = claude_terminal_fixture(directory.path()).unwrap();
    let session = "8ea5a4d1-9c09-4e2a-93e2-c4d2d9658b60";
    let detail = "é".repeat(8 * 1024);
    fs::write(
        directory.path().join("query-session"),
        serde_json::to_vec(&serde_json::json!({"session_id":session,"detail":detail})).unwrap(),
    )
    .unwrap();
    let argv = serde_json::to_string(&[
        "--session-id",
        session,
        "--model",
        "configured-model",
        "private prompt",
    ])
    .unwrap();
    let outcome = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "agents.claude.start",
            vec![
                directory.path().to_string_lossy().into_owned(),
                program.to_string_lossy().into_owned(),
                argv,
            ],
            Caller::Socket,
        ),
        now,
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("native terminal launch failed: {outcome:?}");
    };
    let target: CommandTarget = serde_json::from_value(value["terminal"].clone()).unwrap();
    let service = state.terminal_agent_service().unwrap();
    loop {
        if service.activity(&target).unwrap().session_id.as_deref() == Some(session) {
            break;
        }
        state.update_frame(frames::idle_frame(now));
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("native identity published");
    }
    let session_count = state.mux().all_sessions().len();
    let window_count = state.mux().selected_session_windows().len();
    let resumed = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "agents.claude.resume",
            vec![
                session.to_owned(),
                directory.path().to_string_lossy().into_owned(),
                program.to_string_lossy().into_owned(),
            ],
            Caller::Socket,
        ),
        now,
    );
    assert!(
        matches!(resumed, CommandOutcome::Success { .. }),
        "{resumed:?}"
    );
    assert_eq!(state.mux().all_sessions().len(), session_count);
    assert_eq!(
        state.mux().selected_session_windows().len(),
        window_count,
        "Resuming a live identity selects its existing tab"
    );
    assert_eq!(service.records().len(), 1);
    assert!(state.activate_space_from_ui(elsewhere));
    // Give the other Space a settled, distinct selection; its initial snapshot arrives on a frame.
    let elsewhere_cwd = directory.path().join("elsewhere");
    fs::create_dir(&elsewhere_cwd).unwrap();
    open_native_session(&mut state, &elsewhere_cwd, now);
    let selected = state.mux().selected_session().map(str::to_owned);
    assert_eq!(selected.as_deref(), Some("elsewhere"));
    let mut read = CommandInvocation::new("agents.claude.state", Vec::new(), Caller::Socket);
    read.target = Some(target.clone());
    let outcome = submit_command_from_caller(&mut state, &wakes, Caller::Socket, read, now);
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("exact native state failed: {outcome:?}");
    };
    assert_eq!(value["target"], serde_json::to_value(&target).unwrap());
    assert_eq!(value["observation"]["status"], "working");
    assert_eq!(value["observation"]["session_id"], session);
    let bounded = value["observation"]["detail"].as_str().unwrap();
    assert_eq!(bounded.len(), 4096);
    assert!(detail.starts_with(bounded));
    assert_eq!(
        value["launch"]["arguments"],
        serde_json::json!(["--model", "configured-model"])
    );
    let saved_root = account.join("projects/provider-project");
    fs::create_dir_all(&saved_root).unwrap();
    for (id, cwd) in [
        ("saved-original", directory.path()),
        ("saved-elsewhere", elsewhere_cwd.as_path()),
    ] {
        let records = [
            serde_json::json!({"type":"user", "sessionId":id, "cwd":cwd, "timestamp":"2026-01-01T00:00:00Z", "message":{"content":"private saved prompt"}}),
            serde_json::json!({"type":"custom-title", "sessionId":id, "customTitle":"Saved purpose"}),
        ];
        fs::write(
            saved_root.join(format!("{id}.jsonl")),
            records
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
    }
    let history = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new("agents.claude.history", Vec::new(), Caller::Socket),
        now,
    );
    let CommandOutcome::Success { value: history, .. } = history else {
        panic!("saved provider history failed: {history:?}");
    };
    assert_eq!(history["entries"].as_array().unwrap().len(), 1);
    assert_eq!(history["entries"][0]["session_id"], "saved-elsewhere");
    assert_eq!(history["entries"][0]["title"], "Saved purpose");
    assert_eq!(
        history["entries"][0]["account_directory"],
        account.to_string_lossy().as_ref()
    );
    assert!(
        !serde_json::to_string(&history)
            .unwrap()
            .contains("private saved prompt")
    );
    let all = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "agents.claude.history",
            vec![String::new(), "work".to_owned()],
            Caller::Socket,
        ),
        now,
    );
    let CommandOutcome::Success { value: all, .. } = all else {
        panic!("all selected-account history: {all:?}")
    };
    assert_eq!(all["entries"].as_array().unwrap().len(), 2);
    let crossed = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "agents.claude.history",
            vec![
                String::new(),
                "work".to_owned(),
                directory
                    .path()
                    .join("foreign-account")
                    .to_string_lossy()
                    .into_owned(),
            ],
            Caller::Socket,
        ),
        now,
    );
    assert!(
        matches!(crossed, CommandOutcome::Failed { .. }),
        "{crossed:?}"
    );
    let unknown = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "agents.claude.history",
            vec![String::new(), "missing-profile".to_owned()],
            Caller::Socket,
        ),
        now,
    );
    assert!(
        matches!(unknown, CommandOutcome::Failed { .. }),
        "{unknown:?}"
    );
    assert_eq!(state.active_space_id(), elsewhere);
    assert_eq!(state.mux().selected_session(), selected.as_deref());
    let mut stale = target;
    stale.generation = stale.generation.checked_add(1).unwrap();
    let mut read = CommandInvocation::new("agents.claude.state", Vec::new(), Caller::Socket);
    read.target = Some(stale);
    assert!(matches!(
        submit_command_from_caller(&mut state, &wakes, Caller::Socket, read, now),
        CommandOutcome::StaleTarget { .. }
    ));
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
#[case("toggle_sessions_panel", true)]
#[case("toggle_files_panel", true)]
#[case("toggle_changes_panel", true)]
#[case("toggle_diff_panel", true)]
#[case("toggle_agents_panel", false)]
#[case("toggle_left_dock", true)]
#[case("toggle_right_dock", true)]
#[case("toggle_tab_bar", false)]
#[case("toggle_hidden_tabs", false)]
#[case("show_codexbar", true)]
#[case("show_spaces", true)]
#[case("show_sidebar", true)]
#[case("show_files", true)]
#[case("show_changes", true)]
#[case("show_agents", false)]
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

#[cfg(unix)]
#[rstest]
#[case::literal(vec!["/bin/sh", "-c", "printf '%s' \"$1\" > \"$2.tmp\"; mv \"$2.tmp\" \"$2\"; printf 'ready\\n'; exec cat", "tab", "quoted ' value; $HOME `uname`\nnext line"], false)]
#[case::missing_program(vec!["/does/not/exist"], true)]
fn explicit_tab_starts_literal_argv_and_failure_preserves_the_session(
    #[case] argv: Vec<&str>,
    #[case] fails: bool,
) {
    let directory = assert_fs::TempDir::new().expect("private workspace");
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
    let mut state = native_state(directory.path());
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
    let mut state = native_state(directory.path());
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
    let mut state = native_state(directory.path());
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
    let mut state = native_state(restarted.path());
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
fn closed_session_keeps_its_saved_identity_and_does_not_adopt_a_name_collision() {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let project = directory.path().join("project");
    fs::create_dir(&project).expect("project directory");
    let mut state = native_state(directory.path());
    open_native_session(&mut state, &project, Instant::now());
    let space = spaces_listing(&mut state).remove(0);
    let original = saved_sessions(&mut state, &space).remove(0);
    let identity = original["identity"].as_str().expect("identity");
    let mut close = session_request(
        "session.close",
        Vec::new(),
        space["sessions"][0]["target"].clone(),
    );
    close.confirmation = Some(close.confirmation());
    let closed = submit_command(&mut state, close, Instant::now());
    assert_eq!(failure_kind(&closed), "success", "{closed:?}");
    assert_eq!(state.mux().all_sessions(), []);
    assert_eq!(
        saved_sessions(&mut state, &space)[0]["identity"],
        original["identity"]
    );
    let created = submit_command(
        &mut state,
        session_request(
            "session.create",
            owned(&["project", &project.to_string_lossy()]),
            space["target"].clone(),
        ),
        Instant::now(),
    );
    assert_eq!(failure_kind(&created), "success", "{created:?}");
    let reopened = submit_command(
        &mut state,
        session_request(
            "session.reopen",
            owned(&[identity]),
            space["target"].clone(),
        ),
        Instant::now(),
    );
    assert_eq!(failure_kind(&reopened), "success", "{reopened:?}");
    assert_eq!(state.mux().all_sessions().len(), 2);
    let saved = saved_sessions(&mut state, &space);
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0]["identity"], original["identity"]);
    assert_ne!(saved[0]["identity"], saved[1]["identity"]);
    assert_ne!(saved[0]["attachment"], saved[1]["attachment"]);
}

#[rstest]
#[case::empty("")]
#[case::control("bad\ntitle")]
#[case::oversize(&"a".repeat(257))]
fn invalid_saved_session_title_leaves_existing_metadata_unchanged(#[case] title: &str) {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let mut state = native_state(directory.path());
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
fn failed_saved_title_commit_keeps_live_and_restarted_metadata_unchanged() {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let mut state = native_state(directory.path());
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
    let mut state = native_state(restarted.path());
    let space = spaces_listing(&mut state).remove(0);
    let after = saved_sessions(&mut state, &space);
    assert_eq!(after[0]["identity"], before[0]["identity"]);
    assert_eq!(after[0]["title"], before[0]["title"]);
    assert_eq!(after[0]["cwd"], before[0]["cwd"]);
    assert_eq!(state.mux().all_sessions().len(), 1);
    assert_eq!(after[0]["attachment"], before[0]["attachment"]);
}

#[cfg(unix)]
#[rstest]
#[case::default_account("", false, false)]
#[case::selected_account("work", false, false)]
#[case::captured_destination("", true, false)]
#[case::saved_task("", false, true)]
fn named_agent_start_creates_exactly_one_task_directly_on_its_issued_binding(
    #[case] profile: &str,
    #[case] switch_space: bool,
    #[case] saved_task: bool,
    #[values(false, true)] supply_destination: bool,
) {
    let directory = assert_fs::TempDir::new().expect("isolated task");
    let program = claude_terminal_fixture(directory.path()).expect("native provider fixture");
    let account = directory.path().join("selected-account");
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.agents.claude.selected = "work".to_owned();
    config.agents.claude.profiles.insert(
        "work".to_owned(),
        bootty_config::config::AgentProfileConfig {
            name: "Work".to_owned(),
            directory: Some(account.to_string_lossy().into_owned()),
            arguments: Vec::new(),
        },
    );
    let elsewhere = switch_space.then(|| {
        WorkspaceRepository::open(&directory.path().join("config.toml"))
            .expect("repository")
            .0
            .create_space(
                "Elsewhere",
                "2",
                [1, 2, 3],
                false,
                SpaceMuxOverride::default(),
                false,
            )
            .expect("create other Space")
            .expect("other Space")
            .id()
    });
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "named-task".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )
    .expect("native state");
    let now = Instant::now();
    assert_eq!(state.mux().all_sessions().len(), 0);
    let binding = submit_command(
        &mut state,
        CommandInvocation::new(
            "resource.current",
            vec!["binding".to_owned()],
            Caller::Socket,
        ),
        now,
    );
    let CommandOutcome::Success { value, .. } = binding else {
        panic!("issued binding: {binding:?}")
    };
    let destination: CommandTarget =
        serde_json::from_value(value["target"].clone()).expect("issued binding target");
    let captured_scope = state.mux_scope();
    if let Some(elsewhere) = elsewhere {
        assert!(state.activate_space_from_ui(elsewhere));
    }
    let literal = "Fix 'quotes'; $HOME `uname`\nand preserve this line";
    let mut invocation = CommandInvocation::new(
        "agents.claude.start",
        vec![
            directory.path().to_string_lossy().into_owned(),
            program.to_string_lossy().into_owned(),
            serde_json::to_string(&["--", literal]).expect("literal prompt argv"),
            "useful-task".to_owned(),
            profile.to_owned(),
        ],
        Caller::Internal,
    );
    let identity = bootty_mux::snapshot::new_session_identity();
    let title = "Fix literal prompt handling";
    if saved_task {
        invocation
            .arguments
            .extend([identity.clone(), title.to_owned()]);
    }
    if supply_destination {
        invocation.target = Some(destination);
    }
    let outcome = submit_command_from_caller(&mut state, &wakes, Caller::Internal, invocation, now);
    if !supply_destination {
        assert!(
            matches!(outcome, CommandOutcome::Denied { .. }),
            "{outcome:?}"
        );
        assert!(
            state.mux().all_sessions().is_empty(),
            "no destination never launches"
        );
        assert!(
            state
                .terminal_agent_service()
                .expect("agent owner")
                .records()
                .is_empty()
        );
        return;
    }
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("named task creation: {outcome:?}")
    };
    assert_eq!(
        state.mux().all_sessions().len(),
        1,
        "one task, no throwaway shell"
    );
    assert_eq!(state.mux().all_sessions()[0].name, "useful-task");
    assert_eq!(state.mux().all_sessions()[0].windows.len(), 1);
    if saved_task {
        assert_eq!(
            state.mux().all_sessions()[0].tag.identity.as_deref(),
            Some(identity.as_str())
        );
        let current = submit_command(
            &mut state,
            CommandInvocation::new(
                "resource.current",
                vec!["binding".to_owned()],
                Caller::Socket,
            ),
            now,
        );
        let CommandOutcome::Success { value: space, .. } = current else {
            panic!("current Space: {current:?}")
        };
        let saved = saved_sessions(&mut state, &space);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["identity"], identity);
        assert_eq!(saved[0]["title"], title);
    }
    let terminal: CommandTarget =
        serde_json::from_value(value["terminal"].clone()).expect("observed terminal");
    let record = state
        .terminal_agent_service()
        .expect("native owner")
        .record(&terminal)
        .expect("observer belongs to the created terminal");
    assert_eq!(record.target, terminal);
    assert_eq!(
        record
            .location
            .as_ref()
            .map(|location| location.task_identity.as_str()),
        state.mux().all_sessions()[0].tag.identity.as_deref(),
        "captured destination registration retains its saved task association"
    );
    assert!(record.launch.account_directory.is_some());
    if !profile.is_empty() {
        assert_eq!(record.launch.account_directory.as_deref(), account.to_str());
    }
    assert_eq!(
        record.binding_id,
        captured_scope.persistence_value().to_string()
    );
    assert_eq!(
        state.mux_scope(),
        captured_scope,
        "the issued destination survives a Space switch"
    );
    loop {
        state.update_frame(frames::idle_frame(now));
        if let Ok(source) = fs::read_to_string(directory.path().join("agent-output")) {
            let observed: serde_json::Value =
                serde_json::from_str(&source).expect("provider output");
            assert_eq!(observed["argv"], serde_json::json!(["--", literal]));
            assert_eq!(observed["tool_configs"].as_array().map(Vec::len), Some(1));
            assert_eq!(observed["tty"], true);
            assert_eq!(
                observed["account_directory"].as_str(),
                record.launch.account_directory.as_deref()
            );
            break;
        }
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("native provider publishes output");
    }
}

#[cfg(unix)]
#[rstest]
fn native_agent_retry_reuses_saved_identity_and_user_title_after_launch_failure() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = assert_fs::TempDir::new().unwrap();
    let program = claude_terminal_fixture(directory.path()).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o600)).unwrap();
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "retry-agent".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )
    .unwrap();
    let space = spaces_listing(&mut state).remove(0);
    let identity = bootty_mux::snapshot::new_session_identity();
    let mut invocation = CommandInvocation::new(
        "agents.claude.start",
        vec![
            directory.path().to_string_lossy().into_owned(),
            program.to_string_lossy().into_owned(),
            serde_json::to_string(&["--model", "selected", "--", "private retry prompt"]).unwrap(),
            "retry-agent".to_owned(),
            String::new(),
            identity.clone(),
            "Initial purpose".to_owned(),
        ],
        Caller::Socket,
    );
    invocation.target = Some(serde_json::from_value(space["target"].clone()).unwrap());
    for attempt in 0..2 {
        let outcome = submit_command_from_caller(
            &mut state,
            &wakes,
            Caller::Socket,
            invocation.clone(),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Failed { .. }),
            "{outcome:?}"
        );
        assert_eq!(state.mux().all_sessions().len(), 0);
        assert_eq!(state.terminal_agent_service().unwrap().records().len(), 0);
        let saved = saved_sessions(&mut state, &space);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["identity"], identity);
        if attempt == 0 {
            let edited = submit_command(
                &mut state,
                session_request(
                    "session.set_title",
                    owned(&[identity.as_str(), "User title survives retry"]),
                    space["target"].clone(),
                ),
                Instant::now(),
            );
            assert!(
                matches!(edited, CommandOutcome::Success { .. }),
                "{edited:?}"
            );
        } else {
            assert_eq!(saved[0]["title"], "User title survives retry");
        }
    }
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let outcome = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        invocation,
        Instant::now(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.mux().all_sessions().len(), 1);
    let saved = saved_sessions(&mut state, &space);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["identity"], identity);
    assert_eq!(saved[0]["title"], "User title survives retry");
    assert_eq!(state.terminal_agent_service().unwrap().records().len(), 1);
}

#[cfg(unix)]
fn finished_claude_terminal_fixture(
    directory: &Path,
) -> Result<(PathBuf, fs::File), Box<dyn std::error::Error>> {
    let program = claude_terminal_fixture(directory)?;
    let gate = directory.join("query-gate");
    let status = Command::new("mkfifo").arg(&gate).status()?;
    if !status.success() {
        return Err("provider query FIFO was not created".into());
    }
    let gate = fs::OpenOptions::new().read(true).write(true).open(gate)?;
    // The first query waits for the real session ID, without waiting for an observer poll.
    let script = fs::read_to_string(&program)?
        .replace(
            "    if source.exists():\n        observed",
            "    if not source.exists():\n        with open(directory / 'query-gate', 'rb', buffering=0) as gate:\n            gate.read(1)\n    if source.exists():\n        observed",
        )
        .replace("'status': 'busy'", "'state': 'done'")
        .replace(
            "os.replace(temporary, path)\n",
            "os.replace(temporary, path)\nwith open(directory / 'query-gate', 'wb', buffering=0) as gate:\n    gate.write(b'x')\n",
        );
    fs::write(&program, script)?;
    Ok((program, gate))
}

#[cfg(unix)]
fn wait_for_finished_native_agent(
    fixture: &mut NativeSpawnFixture,
) -> Result<(), Box<dyn std::error::Error>> {
    let service = fixture
        .state
        .terminal_agent_service()
        .ok_or("agent service unavailable")?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or("deadline overflow")?;
    loop {
        fixture
            .state
            .update_frame(frames::idle_frame(Instant::now()));
        if service.live_records().iter().any(|record| {
            record.target == fixture.parent
                && record.observation.status == bootty_agents::TerminalAgentStatus::Finished
        }) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("provider did not report a completed turn".into());
        }
        fixture.wakes.recv_timeout(remaining)?;
    }
}

#[cfg(unix)]
struct NativeSpawnFixture {
    directory: assert_fs::TempDir,
    state: AppState,
    wakes: mpsc::Receiver<()>,
    parent: CommandTarget,
    other: bootty_mux::controller::SpaceId,
    _query_gate: Option<fs::File>,
}

#[cfg(unix)]
fn native_spawn_fixture(enabled: bool) -> Result<NativeSpawnFixture, Box<dyn std::error::Error>> {
    native_spawn_fixture_for_turn(enabled, false)
}

#[cfg(unix)]
fn native_spawn_fixture_for_turn(
    enabled: bool,
    finished: bool,
) -> Result<NativeSpawnFixture, Box<dyn std::error::Error>> {
    let directory = assert_fs::TempDir::new()?;
    let (program, query_gate) = if finished {
        let (program, gate) = finished_claude_terminal_fixture(directory.path())?;
        (program, Some(gate))
    } else {
        (claude_terminal_fixture(directory.path())?, None)
    };
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.agents.allow_spawn = enabled;
    config.agents.claude.selected = "work".to_owned();
    config.agents.claude.profiles.insert(
        "work".to_owned(),
        bootty_config::config::AgentProfileConfig {
            name: "Work".to_owned(),
            directory: Some(
                directory
                    .path()
                    .join("account")
                    .to_string_lossy()
                    .into_owned(),
            ),
            arguments: Vec::new(),
        },
    );
    let other = WorkspaceRepository::open(&config.config_path)?
        .0
        .create_space(
            "Other",
            "2",
            [1, 2, 3],
            false,
            SpaceMuxOverride::default(),
            false,
        )?
        .ok_or("Other Space was not created")?
        .id();
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "spawn-test".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )?;
    let current = submit_command(
        &mut state,
        CommandInvocation::new("resource.current", owned(&["binding"]), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = current else {
        return Err(format!("Binding: {current:?}").into());
    };
    let mut start = CommandInvocation::new(
        "agents.claude.start",
        vec![
            directory.path().to_string_lossy().into_owned(),
            program.to_string_lossy().into_owned(),
            serde_json::to_string(&["--model", "captured-model", "--", "parent prompt"])?,
            "parent-task".to_owned(),
            "work".to_owned(),
        ],
        Caller::Socket,
    );
    start.target = Some(serde_json::from_value(value["target"].clone())?);
    let outcome =
        submit_command_from_caller(&mut state, &wakes, Caller::Socket, start, Instant::now());
    let CommandOutcome::Success { value, .. } = outcome else {
        return Err(format!("Parent: {outcome:?}").into());
    };
    let parent = serde_json::from_value(value["terminal"].clone())?;
    Ok(NativeSpawnFixture {
        directory,
        state,
        wakes,
        parent,
        other,
        _query_gate: query_gate,
    })
}

#[cfg(unix)]
#[rstest]
#[case::shell(false)]
#[case::agent(true)]
fn opted_in_child_tasks_keep_parent_account_and_do_not_change_focus(
    #[case] agent: bool,
    #[values(false, true)] inactive_parent: bool,
) {
    let mut fixture = native_spawn_fixture(true).unwrap();
    let parent_scope = fixture.state.mux_scope();
    if inactive_parent {
        assert!(fixture.state.activate_space_from_ui(fixture.other));
        // Activation's first snapshot must settle before comparing a detached spawn's selection.
        let elsewhere_cwd = fixture.directory.path().join("elsewhere");
        fs::create_dir(&elsewhere_cwd).unwrap();
        open_native_session(&mut fixture.state, &elsewhere_cwd, Instant::now());
        assert_eq!(fixture.state.mux().selected_session(), Some("elsewhere"));
    }
    let before = selection(&fixture.state);
    let focus = (
        fixture.state.terminal_focused(),
        fixture.state.sidebar_focused(),
    );
    let active = fixture.state.mux_scope();
    let request = if agent {
        serde_json::json!({"kind":"agent","name":"child-task","title":"Child purpose","provider":"claude","profile":"work","prompt":"Child 'quotes'; $HOME `uname`\nnext line"})
    } else {
        serde_json::json!({"kind":"shell","name":"child-task","title":"Child purpose"})
    };
    let mut invocation =
        CommandInvocation::new("agents.spawn", vec![request.to_string()], Caller::Socket);
    invocation.target = Some(fixture.parent.clone());
    let outcome = submit_command_from_caller(
        &mut fixture.state,
        &fixture.wakes,
        Caller::Socket,
        invocation,
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("Detached child: {outcome:?}");
    };
    assert_eq!(fixture.state.mux_scope(), active);
    assert_eq!(selection(&fixture.state), before);
    assert_eq!(
        (
            fixture.state.terminal_focused(),
            fixture.state.sidebar_focused()
        ),
        focus
    );
    let parent_scope = parent_scope.persistence_value().to_string();
    let space = spaces_listing(&mut fixture.state)
        .into_iter()
        .find(|space| space["scope"].as_str() == Some(parent_scope.as_str()))
        .unwrap();
    let saved = saved_sessions(&mut fixture.state, &space);
    assert_eq!(fixture.state.mux_scope(), active);
    assert_eq!(selection(&fixture.state), before);
    assert_eq!(saved.len(), 2);
    let child = saved
        .iter()
        .find(|saved| saved["identity"] == value["task_id"])
        .unwrap();
    assert_eq!(child["title"], "Child purpose");
    assert_eq!(child["cwd"], saved[0]["cwd"]);
    let service = fixture.state.terminal_agent_service().unwrap();
    if agent {
        let terminal: CommandTarget = serde_json::from_value(value["terminal"].clone()).unwrap();
        let (child, lease) = service.spawn_parent(&terminal).unwrap();
        let parent = service.record(&fixture.parent).unwrap();
        assert_eq!(
            child
                .location
                .as_ref()
                .map(|location| location.task_identity.as_str()),
            value["task_id"].as_str(),
            "detached child registration retains its saved task association"
        );
        assert_eq!(
            parent.launch.account_directory,
            Some(
                fixture
                    .directory
                    .path()
                    .join("account")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        assert_eq!(
            child.launch.account_directory,
            parent.launch.account_directory
        );
        assert_eq!(child.launch.program, parent.launch.program);
        assert_eq!(
            child.launch.arguments,
            owned(&["--model", "captured-model"])
        );
        assert!(lease.enabled(None));
        assert_eq!(lease.caller(), Caller::Socket);
        assert!(!lease.spawn_enabled());
        assert!(!lease.enabled(Some(bootty_agents::ToolCapture::Browser)));
        service.revoke_terminal_tools(&fixture.parent);
        assert!(!lease.enabled(None));
    } else {
        assert_eq!(service.records().len(), 1);
    }
}

#[cfg(unix)]
#[rstest]
#[case::default_disabled(false, false)]
#[case::disabled_after_launch(true, true)]
#[case::changed_provider(true, false)]
fn child_spawning_denies_disabled_and_changed_provider_authority(
    #[case] enabled: bool,
    #[case] revoke: bool,
) {
    let mut fixture = native_spawn_fixture(enabled).unwrap();
    let service = fixture.state.terminal_agent_service().unwrap();
    if revoke {
        service.set_agent_spawning_enabled(false);
        service.set_agent_spawning_enabled(true);
    }
    let mut requests = vec![
        serde_json::json!({"kind":"agent","name":"blocked","provider":"pi","prompt":"task"}),
        serde_json::json!({"kind":"agent","name":"blocked","provider":"claude","profile":"foreign","prompt":"task"}),
    ];
    if !enabled || revoke {
        requests.push(serde_json::json!({"kind":"shell","name":"blocked"}));
    }
    for request in requests {
        let mut invocation =
            CommandInvocation::new("agents.spawn", vec![request.to_string()], Caller::Socket);
        invocation.target = Some(fixture.parent.clone());
        let outcome = submit_command_from_caller(
            &mut fixture.state,
            &fixture.wakes,
            Caller::Socket,
            invocation,
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Denied { .. }),
            "{outcome:?}"
        );
    }
    assert_eq!(fixture.state.mux().all_sessions().len(), 1);
    assert_eq!(service.records().len(), 1);
}

#[cfg(unix)]
#[rstest]
fn saved_provider_history_bounds_actual_json_without_cutting_complete_metadata() {
    let directory = assert_fs::TempDir::new().expect("private history");
    let account = directory.path().join("account");
    let store = account.join("projects/provider-project");
    fs::create_dir_all(&store).expect("provider store");
    let cwd = format!("/project/{}", "x".repeat(4000));
    let title = "\"".repeat(512);
    for index in 0..64 {
        let id = format!("complete-id-{index}");
        let records = [
            serde_json::json!({"type":"user", "sessionId":id, "cwd":cwd, "timestamp":"2026-01-01T00:00:00Z"}),
            serde_json::json!({"type":"custom-title", "sessionId":id, "customTitle":title}),
        ];
        fs::write(
            store.join(format!("{id}.jsonl")),
            records
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .expect("saved metadata");
    }
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.agents.claude.selected = "work".to_owned();
    config.agents.claude.profiles.insert(
        "work".to_owned(),
        bootty_config::config::AgentProfileConfig {
            name: "Work".to_owned(),
            directory: Some(account.to_string_lossy().into_owned()),
            arguments: Vec::new(),
        },
    );
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        "history-bounds".to_owned(),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )
    .expect("native history owner");
    let outcome = submit_command_from_caller(
        &mut state,
        &wakes,
        Caller::Socket,
        CommandInvocation::new(
            "agents.claude.history",
            vec![String::new(), "work".to_owned()],
            Caller::Socket,
        ),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("bounded history: {outcome:?}")
    };
    assert!(serde_json::to_vec(&value).expect("encoded envelope").len() <= 96 * 1024);
    assert_eq!(
        value["account_directory"],
        account.to_string_lossy().as_ref()
    );
    let entries = value["entries"].as_array().expect("complete entries");
    assert_ne!(entries.len(), 0);
    let omitted = value["omitted_entries"].as_u64().expect("omitted count");
    assert!(omitted > 0);
    assert_eq!(
        entries
            .len()
            .saturating_add(usize::try_from(omitted).expect("bounded count")),
        64
    );
    for entry in entries {
        assert_eq!(entry["cwd"], cwd);
        assert_eq!(entry["title"], title);
        assert_eq!(entry["resume_id"], entry["session_id"]);
        let id = entry["session_id"].as_str().expect("provider identity");
        assert!(
            store.join(format!("{id}.jsonl")).is_file(),
            "identity remains complete"
        );
    }
}

#[cfg(unix)]
fn close_native_spawn_fixture(
    fixture: &mut NativeSpawnFixture,
) -> Result<(), Box<dyn std::error::Error>> {
    fixture
        .state
        .terminal_agent_service()
        .ok_or("agent service unavailable")?
        .shutdown_and_wait()?;
    let mut close = CommandInvocation::new("pane.close", Vec::new(), Caller::Socket);
    close.target = Some(fixture.parent.clone());
    close.confirmation = Some(close.confirmation());
    let outcome = submit_command_from_caller(
        &mut fixture.state,
        &fixture.wakes,
        Caller::Socket,
        close,
        Instant::now(),
    );
    if matches!(outcome, CommandOutcome::Success { .. }) {
        Ok(())
    } else {
        Err(format!("fixture terminal close: {outcome:?}").into())
    }
}

#[cfg(unix)]
#[rstest]
#[case("prompt")]
#[case("follow_up")]
#[case("steer")]
fn completed_live_turn_accepts_next_prompt_on_exact_captured_terminal(
    #[case] operation: &str,
    #[values(false, true)] inactive: bool,
) {
    let mut fixture = native_spawn_fixture_for_turn(false, true).unwrap();
    wait_for_finished_native_agent(&mut fixture).unwrap();
    if inactive {
        assert!(fixture.state.activate_space_from_ui(fixture.other));
        let elsewhere = fixture.directory.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        open_native_session(&mut fixture.state, &elsewhere, Instant::now());
    }
    let before = selection(&fixture.state);
    for stopped_operation in ["abort", "interrupt"] {
        let mut invocation = CommandInvocation::new(
            format!("agents.claude.{stopped_operation}"),
            Vec::new(),
            Caller::Socket,
        );
        invocation.target = Some(fixture.parent.clone());
        invocation.confirmation = Some(invocation.confirmation());
        let outcome = submit_command_from_caller(
            &mut fixture.state,
            &fixture.wakes,
            Caller::Socket,
            invocation,
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Unavailable { .. }),
            "{outcome:?}"
        );
    }
    let message = format!("next turn via {operation}");
    let mut invocation = CommandInvocation::new(
        format!("agents.claude.{operation}"),
        vec![message.clone()],
        Caller::Socket,
    );
    invocation.target = Some(fixture.parent.clone());
    let outcome = submit_command_from_caller(
        &mut fixture.state,
        &fixture.wakes,
        Caller::Socket,
        invocation,
        Instant::now(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(selection(&fixture.state), before);
    let service = fixture.state.terminal_agent_service().unwrap();
    assert_eq!(
        service.record(&fixture.parent).unwrap().target,
        fixture.parent
    );
    loop {
        fixture
            .state
            .update_frame(frames::idle_frame(Instant::now()));
        if let Ok(text) = fs::read_to_string(fixture.directory.path().join("agent-input"))
            && !text.is_empty()
        {
            assert_eq!(
                text,
                format!("{}\n", serde_json::to_string(&message).unwrap())
            );
            break;
        }
        fixture
            .wakes
            .recv_timeout(Duration::from_secs(2))
            .expect("submitted next prompt arrives");
    }
    close_native_spawn_fixture(&mut fixture).unwrap();
}

#[cfg(unix)]
#[rstest]
fn retained_finished_provider_records_cannot_route_input_after_observation_shutdown() {
    let mut fixture = native_spawn_fixture_for_turn(false, true).unwrap();
    wait_for_finished_native_agent(&mut fixture).unwrap();
    let service = fixture.state.terminal_agent_service().unwrap();
    assert!(
        service
            .live_records()
            .iter()
            .any(|record| record.target == fixture.parent)
    );
    service.shutdown_and_wait().unwrap();
    assert!(service.live_records().is_empty());
    assert!(service.record(&fixture.parent).is_some());
    for operation in ["prompt", "steer", "follow_up", "abort", "interrupt"] {
        let arguments = if matches!(operation, "abort" | "interrupt") {
            Vec::new()
        } else {
            owned(&["must not reach the terminal"])
        };
        let mut invocation = CommandInvocation::new(
            format!("agents.claude.{operation}"),
            arguments,
            Caller::Socket,
        );
        invocation.target = Some(fixture.parent.clone());
        if matches!(operation, "abort" | "interrupt") {
            invocation.confirmation = Some(invocation.confirmation());
        }
        let outcome = submit_command_from_caller(
            &mut fixture.state,
            &fixture.wakes,
            Caller::Socket,
            invocation,
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Unavailable { .. }),
            "{operation}: {outcome:?}"
        );
    }
    let mut read = CommandInvocation::new("agents.claude.state", Vec::new(), Caller::Socket);
    read.target = Some(fixture.parent.clone());
    let outcome = submit_command_from_caller(
        &mut fixture.state,
        &fixture.wakes,
        Caller::Socket,
        read,
        Instant::now(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    // A subsequent raw submission proves rejected controls neither pasted nor interrupted the TUI.
    for (command, arguments) in [
        ("terminal.paste", owned(&["still interactive"])),
        ("terminal.submit", Vec::new()),
    ] {
        let mut invocation = CommandInvocation::new(command, arguments, Caller::Socket);
        invocation.target = Some(fixture.parent.clone());
        let outcome = submit_command_from_caller(
            &mut fixture.state,
            &fixture.wakes,
            Caller::Socket,
            invocation,
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    let received = fixture.directory.path().join("agent-input");
    loop {
        fixture
            .state
            .update_frame(frames::idle_frame(Instant::now()));
        if let Ok(text) = fs::read_to_string(&received)
            && !text.is_empty()
        {
            assert_eq!(text, "\"still interactive\"\n");
            break;
        }
        fixture
            .wakes
            .recv_timeout(Duration::from_secs(1))
            .expect("native terminal publishes the untouched provider's submission");
    }
    close_native_spawn_fixture(&mut fixture).unwrap();
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
    let mut state = native_state(directory.path());
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
    let mut state = native_state(directory.path());
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
    let mut state = native_state(directory.path());
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
    let mut state = native_state(directory.path());
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
    let mut state = native_state(directory.path());
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
    let mut state = native_state(directory.path());
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
    state.apply_dialog_intent(
        &bootty_gpui::DialogIntent::TextChanged {
            dialog: dialog_id.clone(),
            value: "Keep this draft".to_owned(),
        },
        &mut Vec::new(),
    );
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
        panic!("registration must preserve the prompt");
    };
    assert_eq!(dialog.spec().id, dialog_id);
    assert_eq!(dialog.draft().unwrap().prompt, "Keep this draft");
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
    let mut reopened = native_state(directory.path());
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
}

#[rstest]
fn sidebar_navigation_skips_sessions_in_collapsed_projects() {
    let directory = assert_fs::TempDir::new().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let mut state = native_state(directory.path());
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
        submit_action(
            &mut state,
            "ui.sidebar.next_session",
            Caller::Socket,
            Instant::now()
        ),
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
    let mut state = native_state(directory.path());
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
