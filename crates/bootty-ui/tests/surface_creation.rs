#![cfg(test)]

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};
use bootty_ui::{AppEffect, AppState};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

use crate::support::idle_frames as frames;

use crate::support::config as test_config;

#[fixture]
fn empty_workspace() -> (assert_fs::TempDir, AppState) {
    let directory = assert_fs::TempDir::new().expect("private workspace");
    let mut config = test_config::config(directory.path().join("config.toml"));
    config.session.working_directory = Some(directory.path().to_owned());
    let state = AppState::new(config, Arc::new(|| {}), None, None).expect("workspace");
    (directory, state)
}

/// Submit a creation command and drive frames until it completes, collecting every effect.
fn create(
    state: &mut AppState,
    command: &str,
    caller: Caller,
    cancellation: &CommandCancellation,
) -> (CommandOutcome, Vec<AppEffect>) {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("deadline");
    let response = state
        .app_command_sender(caller)
        .submit(
            CommandInvocation::new(command, Vec::new(), caller),
            deadline,
            cancellation.clone(),
        )
        .expect("submit");
    let mut effects = Vec::new();
    loop {
        let now = Instant::now();
        assert!(now < deadline, "{command} completes");
        effects.extend(state.update_frame(frames::idle_frame(now)));
        if let Ok(outcome) = response.try_recv() {
            return (outcome, effects);
        }
        std::thread::yield_now();
    }
}

#[rstest]
fn queued_new_tab_cancellation_starts_no_process(empty_workspace: (assert_fs::TempDir, AppState)) {
    let (_directory, mut state) = empty_workspace;
    let cancellation = CommandCancellation::new();
    assert!(cancellation.cancel());
    let (outcome, effects) = create(&mut state, "new_tab", Caller::Socket, &cancellation);
    assert!(matches!(outcome, CommandOutcome::Failed { code, .. } if code == "cancelled"));
    assert_eq!(state.pending_new_surface(), None);
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

#[cfg(unix)]
#[rstest]
fn opening_a_new_tab_replaces_the_global_creation_surface(
    empty_workspace: (assert_fs::TempDir, AppState),
) {
    let (_directory, mut state) = empty_workspace;
    create(
        &mut state,
        "new_mux_session",
        Caller::Socket,
        &CommandCancellation::new(),
    );
    assert!(matches!(
        state.modal_dialog(),
        Some(bootty_ui::ModalDialog::NewSession(_))
    ));
    let (outcome, effects) = create(
        &mut state,
        "new_tab",
        Caller::Socket,
        &CommandCancellation::new(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(state.modal_dialog().is_none());
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::AttachNewSurface { .. }))
    );
}

#[cfg(unix)]
#[rstest]
#[case(Caller::Socket)]
#[case(Caller::Internal)]
#[case(Caller::BuiltinKeybinding)]
fn new_tab_in_an_empty_space_persists_its_first_terminal_before_attachment(
    empty_workspace: (assert_fs::TempDir, AppState),
    #[case] caller: Caller,
) {
    use bootty_ui::surface_creation::{SurfaceParent, SurfacePlacement};
    let (directory, mut state) = empty_workspace;
    let (outcome, effects) = create(&mut state, "new_tab", caller, &CommandCancellation::new());
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let attached = effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::AttachNewSurface { request, .. } => Some(request),
            _ => None,
        })
        .expect("the first terminal attaches");
    assert_eq!(attached.placement, SurfacePlacement::Tab);
    assert!(
        matches!(&attached.parent, SurfaceParent::Binding(target) if *target == attached.binding)
    );
    assert_eq!(attached.cwd, directory.path().to_string_lossy());
    assert_eq!(attached.task_identity, "");
    assert_eq!(state.pending_new_surface(), None);
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
