#![cfg(test)]

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use bootty_config::config::MultiplexerBackendConfig;
use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};
use bootty_ui::{
    AppEffect, AppState,
    surface_creation::{SurfaceParent, SurfacePlacement},
};
use pretty_assertions::{assert_eq, assert_ne};
use rstest::{fixture, rstest};

#[path = "support/idle_frames.rs"]
mod frames;
mod support;
#[path = "support/config.rs"]
mod test_config;

#[fixture]
fn empty_workspace() -> (assert_fs::TempDir, AppState) {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.working_directory = Some(directory.path().to_owned());
    let state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).expect("workspace");
    (directory, state)
}

fn command(
    state: &mut AppState,
    command: &str,
    arguments: Vec<String>,
    caller: Caller,
) -> (CommandOutcome, Vec<AppEffect>) {
    let now = Instant::now();
    let result = state
        .app_command_sender(caller)
        .submit(
            CommandInvocation::new(command, arguments, caller),
            now.checked_add(Duration::from_secs(1)).expect("deadline"),
            CommandCancellation::new(),
        )
        .expect("submit");
    let effects = state.update_frame(frames::idle_frame(now));
    (
        result.try_recv().expect("synchronous chooser outcome"),
        effects,
    )
}

#[rstest]
#[case(Caller::Socket)]
#[case(Caller::Internal)]
#[case(Caller::BuiltinKeybinding)]
fn new_tab_captures_an_empty_space_without_starting_a_terminal(
    empty_workspace: (assert_fs::TempDir, AppState),
    #[case] caller: Caller,
) {
    let (directory, mut state) = empty_workspace;
    let (outcome, effects) = command(&mut state, "new_tab", Vec::new(), caller);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let request = state.pending_new_surface().expect("retained chooser");
    assert_eq!(request.placement, SurfacePlacement::Tab);
    assert!(
        matches!(&request.parent, SurfaceParent::Binding(target) if *target == request.binding)
    );
    assert_eq!(request.cwd, directory.path().to_string_lossy());
    assert_eq!(request.task_identity, "");
    assert!(effects.iter().any(
        |effect| matches!(effect, AppEffect::OpenSurfaceChooser(opened) if opened == request)
    ));
    assert!(
        state
            .binding_session_groups()
            .iter()
            .all(|group| group.sessions.is_empty())
    );
}

#[rstest]
fn cancelled_and_replaced_requests_cannot_create_surfaces(
    empty_workspace: (assert_fs::TempDir, AppState),
) {
    let (_directory, mut state) = empty_workspace;
    command(&mut state, "new_tab", Vec::new(), Caller::Socket);
    let first = state.pending_new_surface().expect("first request").id;
    let (_, effects) = command(&mut state, "new_tab", Vec::new(), Caller::Socket);
    let second = state.pending_new_surface().expect("replacement request").id;
    assert!(second > first);
    assert!(effects.contains(&AppEffect::CloseSurfaceChooser(first)));
    let (stale, _) = command(
        &mut state,
        "surface.choose",
        vec![first.to_string(), "terminal".to_owned()],
        Caller::Socket,
    );
    assert!(matches!(stale, CommandOutcome::StaleTarget { .. }));
    let (cancelled, effects) = command(
        &mut state,
        "surface.cancel",
        vec![second.to_string()],
        Caller::Socket,
    );
    assert!(matches!(cancelled, CommandOutcome::Success { .. }));
    assert!(effects.contains(&AppEffect::CloseSurfaceChooser(second)));
    assert!(state.pending_new_surface().is_none());
    let (stale, _) = command(
        &mut state,
        "surface.choose",
        vec![second.to_string(), "agent".to_owned()],
        Caller::Socket,
    );
    assert!(matches!(stale, CommandOutcome::StaleTarget { .. }));
    assert!(
        state
            .binding_session_groups()
            .iter()
            .all(|group| group.sessions.is_empty())
    );
}

#[rstest]
fn an_empty_space_can_choose_a_first_native_agent_without_creating_a_shell(
    empty_workspace: (assert_fs::TempDir, AppState),
) {
    let (_directory, mut state) = empty_workspace;
    command(&mut state, "new_tab", Vec::new(), Caller::Socket);
    let request = state.pending_new_surface().expect("request").clone();
    let (outcome, effects) = command(
        &mut state,
        "surface.choose",
        vec![request.id.to_string(), "agent".to_owned()],
        Caller::Socket,
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(effects.contains(&AppEffect::OpenSurfaceAgentForm(request.clone())));
    state.open_surface_agent_form(&request);
    let Some(bootty_ui::ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
        panic!("first native agent form");
    };
    let draft = dialog.draft().expect("captured form draft");
    assert_eq!(draft.cwd, request.cwd);
    assert_ne!(draft.identity, "");
    assert!(!draft.isolated);
    assert_eq!(
        draft.mode,
        bootty_ui::presentation::new_session_form::NewSessionMode::Agent
    );
    assert!(
        state
            .binding_session_groups()
            .iter()
            .all(|group| group.sessions.is_empty())
    );
}

enum FormDismissal {
    Dialog,
    Surface,
    Replacement,
}

#[rstest]
#[case(FormDismissal::Dialog)]
#[case(FormDismissal::Surface)]
#[case(FormDismissal::Replacement)]
fn dismissing_or_replacing_an_agent_form_closes_its_captured_surface(
    empty_workspace: (assert_fs::TempDir, AppState),
    #[case] dismissal: FormDismissal,
) {
    let (_directory, mut state) = empty_workspace;
    command(&mut state, "new_tab", Vec::new(), Caller::Internal);
    let request = state
        .pending_new_surface()
        .expect("chooser request")
        .clone();
    command(
        &mut state,
        "surface.choose",
        vec![request.id.to_string(), "agent".to_owned()],
        Caller::Internal,
    );
    state.open_surface_agent_form(&request);
    let mut effects = Vec::new();
    match dismissal {
        FormDismissal::Dialog => {
            state.apply_dialog_intent(
                &bootty_ui::gpui::DialogIntent::Dismiss {
                    dialog: bootty_ui::gpui::DialogId::new(
                        bootty_ui::presentation::dialogs::NEW_SESSION_ID,
                    ),
                },
                &mut effects,
            );
            assert!(state.modal_dialog().is_none());
        }
        FormDismissal::Surface => {
            let (outcome, closed) = command(
                &mut state,
                "surface.cancel",
                vec![request.id.to_string()],
                Caller::Internal,
            );
            assert!(matches!(outcome, CommandOutcome::Success { .. }));
            assert!(state.modal_dialog().is_none());
            effects.extend(closed);
        }
        FormDismissal::Replacement => {}
    }
    let (outcome, opened) = command(
        &mut state,
        "new_mux_session",
        Vec::new(),
        Caller::BuiltinKeybinding,
    );
    assert!(matches!(outcome, CommandOutcome::Success { .. }));
    effects.extend(opened);
    effects.extend(state.update_frame(frames::idle_frame(Instant::now())));
    assert!(effects.contains(&AppEffect::CloseSurfaceChooser(request.id)));
    assert!(state.pending_new_surface().is_none());
    let Some(bootty_ui::ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
        panic!("global new session form remains open");
    };
    assert!(
        dialog
            .spec()
            .fields
            .iter()
            .any(|field| field.id == "provider")
    );
    let (stale, _) = command(
        &mut state,
        "surface.choose",
        vec![request.id.to_string(), "terminal".to_owned()],
        Caller::Internal,
    );
    assert!(matches!(stale, CommandOutcome::StaleTarget { .. }));
    assert!(
        state
            .binding_session_groups()
            .iter()
            .all(|group| group.sessions.is_empty())
    );
}

#[rstest]
fn queued_creation_cancellation_keeps_the_request_and_starts_no_process(
    empty_workspace: (assert_fs::TempDir, AppState),
) {
    let (_directory, mut state) = empty_workspace;
    command(&mut state, "new_tab", Vec::new(), Caller::Socket);
    let request = state.pending_new_surface().expect("request").clone();
    let cancellation = CommandCancellation::new();
    let now = Instant::now();
    let response = state
        .app_command_sender(Caller::Socket)
        .submit(
            CommandInvocation::new(
                "surface.choose",
                vec![request.id.to_string(), "terminal".to_owned()],
                Caller::Socket,
            ),
            now.checked_add(Duration::from_secs(1)).expect("deadline"),
            cancellation.clone(),
        )
        .expect("queue real command");
    assert!(cancellation.cancel());
    let effects = state.update_frame(frames::idle_frame(now));
    let outcome = response.try_recv().expect("cancelled command completes");
    assert!(matches!(outcome, CommandOutcome::Failed { code, .. } if code == "cancelled"));
    assert_eq!(state.pending_new_surface(), Some(&request));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::AttachNewSurface { .. }))
    );
    assert!(
        state
            .binding_session_groups()
            .iter()
            .all(|group| group.sessions.is_empty())
    );
}

#[rstest]
fn another_caller_cannot_adopt_a_captured_chooser(empty_workspace: (assert_fs::TempDir, AppState)) {
    let (_directory, mut state) = empty_workspace;
    command(&mut state, "new_tab", Vec::new(), Caller::BuiltinKeybinding);
    let request = state.pending_new_surface().expect("request").clone();
    let (outcome, _) = command(
        &mut state,
        "surface.choose",
        vec![request.id.to_string(), "terminal".to_owned()],
        Caller::Socket,
    );
    assert!(matches!(outcome, CommandOutcome::Denied { .. }));
    assert_eq!(state.pending_new_surface(), Some(&request));
}

#[rstest]
fn chooser_navigation_retains_the_current_request_and_rejects_a_closed_one(
    empty_workspace: (assert_fs::TempDir, AppState),
) {
    let (_directory, mut state) = empty_workspace;
    let (outcome, _) = command(
        &mut state,
        "ui.surface.next",
        Vec::new(),
        Caller::Keybinding,
    );
    assert!(matches!(outcome, CommandOutcome::Unavailable { .. }));
    command(&mut state, "new_tab", Vec::new(), Caller::Socket);
    let request = state.pending_new_surface().unwrap().clone();
    for action in bootty_ui::commands::SurfaceChooserAction::ALL {
        let (outcome, effects) =
            command(&mut state, action.command(), Vec::new(), Caller::Keybinding);
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert!(effects.iter().any(|effect| matches!(effect, AppEffect::NavigateSurfaceChooser { id, action: observed } if *id == request.id && *observed == action)));
        assert_eq!(state.pending_new_surface(), Some(&request));
    }
    command(
        &mut state,
        "surface.cancel",
        vec![request.id.to_string()],
        Caller::Socket,
    );
    let (outcome, _) = command(
        &mut state,
        "ui.surface.confirm",
        Vec::new(),
        Caller::Keybinding,
    );
    assert!(matches!(outcome, CommandOutcome::Unavailable { .. }));
}

#[rstest]
fn opening_a_new_tab_replaces_the_global_creation_surface(
    empty_workspace: (assert_fs::TempDir, AppState),
) {
    let (_, mut state) = empty_workspace;
    command(&mut state, "new_mux_session", Vec::new(), Caller::Socket);
    assert!(matches!(
        state.modal_dialog(),
        Some(bootty_ui::ModalDialog::NewSession(_))
    ));
    let (outcome, effects) = command(&mut state, "new_tab", Vec::new(), Caller::Socket);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(state.modal_dialog().is_none());
    let request = state.pending_new_surface().expect("new tab chooser");
    assert!(effects.contains(&AppEffect::OpenSurfaceChooser(request.clone())));
}

#[cfg(unix)]
#[rstest]
#[case(Caller::Socket)]
#[case(Caller::Internal)]
#[case(Caller::BuiltinKeybinding)]
fn first_terminal_surface_persists_its_selection_before_attachment(
    empty_workspace: (assert_fs::TempDir, AppState),
    #[case] caller: Caller,
) {
    let (directory, mut state) = empty_workspace;
    command(&mut state, "new_tab", Vec::new(), caller);
    let request = state.pending_new_surface().unwrap().clone();
    let now = Instant::now();
    let deadline = now.checked_add(Duration::from_secs(5)).unwrap();
    let response = state
        .app_command_sender(caller)
        .submit(
            CommandInvocation::new(
                "surface.choose",
                vec![
                    request.id.to_string(),
                    "terminal".to_owned(),
                    serde_json::json!(["/bin/cat"]).to_string(),
                ],
                caller,
            ),
            deadline,
            CommandCancellation::new(),
        )
        .unwrap();
    let mut attached = false;
    let outcome = loop {
        let now = Instant::now();
        assert!(now < deadline, "first terminal completes");
        attached |= state.update_frame(frames::idle_frame(now)).iter().any(
            |effect| matches!(effect, AppEffect::AttachNewSurface { request_id, .. } if *request_id == request.id),
        );
        if let Ok(outcome) = response.try_recv() {
            break outcome;
        }
        std::thread::yield_now();
    };
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(attached);
    let (_, persisted) =
        bootty_mux::repository::WorkspaceRepository::open(&directory.path().join("config.toml"))
            .unwrap();
    let binding = persisted.spaces().first().unwrap().binding();
    let identity = binding
        .selected_session_identity()
        .expect("foreground creation saves its logical identity");
    let saved = binding.sessions().get(identity).unwrap();
    assert_eq!(
        binding.selection().unwrap().session_id(),
        saved.backend_name
    );
    assert!(binding.selection().unwrap().window_id().is_some());
}
