#![cfg(test)]

use bootty_config::config::{BoottyConfig, NotificationPolicy};
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    capability::BindingCapabilityDescriptor,
    controller::SpaceId,
    native::NativeProvider,
    provider::{
        MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider, MuxBackendRegistry,
        MuxCommandDispatch,
    },
    repository::{SpaceMuxOverride, WorkspaceRepository},
    terminal::{
        BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest, ScopedMuxPaneTarget,
        TerminalRuntime,
    },
};
use bootty_terminal::{
    shell_lifecycle::ShellEvent,
    terminal_side_effect::{TerminalSideEffect, TerminalSideEffectEvent},
};
use bootty_ui::{AppEffect, AppState, presentation::dialogs::NewSessionPickerEvent};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    path::Path,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
#[path = "support/idle_frames.rs"]
mod frames;

type Sources = Arc<Mutex<Vec<(String, mpsc::Sender<TerminalSideEffectEvent>)>>>;
struct NotificationProvider(Sources);
struct NotificationPane(Sources);
impl MuxBackendProvider for NotificationProvider {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Native
    }
    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::CallerThread
    }
    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        NativeProvider.build_backend(config, workspace)
    }
}
impl MuxAppBackendProvider for NotificationProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        NativeProvider.app_policy()
    }
    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        NativeProvider.capabilities(scope)
    }
    fn build_pane_policy(&self, _: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(NotificationPane(Arc::clone(&self.0)))
    }
}
impl BackendPanePolicy for NotificationPane {
    fn remote_target(&self) -> Option<bootty_mux::RemoteTarget> {
        None
    }
    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> anyhow::Result<Option<Box<dyn TerminalRuntime>>> {
        self.0.lock().unwrap().push((
            request.target.side_effect_pane_id().unwrap(),
            request.terminal_config.side_effect_tx.clone().unwrap(),
        ));
        Ok(None)
    }
    fn sync_target(&mut self, _: Option<&ScopedMuxPaneTarget>, _: bool) {}
    fn set_layout_window(&mut self, _: Option<&str>) {}
    fn resize_layout_window(&mut self, _: PaneLayoutResizeRequest<'_>) -> anyhow::Result<bool> {
        Ok(false)
    }
    fn deactivate(&mut self) {}
}

#[rstest]
#[case(NotificationPolicy::Always, true, 10, false, true)]
#[case(NotificationPolicy::Always, false, 9, false, false)]
#[case(NotificationPolicy::Never, false, 12, false, false)]
#[case(NotificationPolicy::Unfocused, true, 12, false, false)]
#[case(NotificationPolicy::Unfocused, false, 12, false, true)]
#[case(NotificationPolicy::Unfocused, true, 12, true, true)]
fn notifications_use_observed_time_focus_and_original_space(
    #[case] policy: NotificationPolicy,
    #[case] focused: bool,
    #[case] seconds: u64,
    #[case] background_space: bool,
    #[case] notify: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.session.command_notifications = policy;
    config.session.command_notification_min_seconds = 10;
    let (mut repository, _) = WorkspaceRepository::open(&config.config_path).unwrap();
    let other = repository
        .create_space(
            "Other",
            "z",
            [1, 2, 3],
            false,
            SpaceMuxOverride::default(),
            false,
        )
        .unwrap()
        .unwrap()
        .id();
    drop(repository);
    let sources = Sources::default();
    let provider = Arc::new(NotificationProvider(Arc::clone(&sources)));
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers([provider], [MuxBackendKind::Native]).unwrap(),
    );
    let mut state = AppState::new(config, backends, Arc::new(|| {}), None, None).unwrap();
    assert!(state.open_session_picker_dialog_from_ui());
    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: directory.path().to_string_lossy().into_owned(),
    });
    let now = Instant::now();
    for _ in 0..4 {
        state.update_frame(frames::idle_frame(now));
    }
    let (pane, sender) = sources
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("pane event source");
    let mut start = TerminalSideEffectEvent::new(
        Some(pane.clone()),
        TerminalSideEffect::ShellLifecycle(ShellEvent::CommandStart),
    );
    start.observed_at = Some(now);
    sender.send(start).unwrap();
    // Switch before draining: navigation must not throw away the start marker.
    if background_space {
        assert!(state.activate_space_from_ui(other));
    }
    let mut finish = TerminalSideEffectEvent::new(
        Some(pane),
        TerminalSideEffect::ShellLifecycle(ShellEvent::CommandFinish { exit_code: Some(7) }),
    );
    finish.observed_at = Some(
        now.checked_add(Duration::from_secs(seconds))
            .expect("test timestamp fits"),
    );
    sender.send(finish.clone()).unwrap();
    sender.send(finish).unwrap();
    let mut frame = frames::idle_frame(
        now.checked_add(Duration::from_secs(100))
            .expect("test timestamp fits"),
    );
    frame.input.window_focused = focused;
    let effects = state.update_frame(frame);
    let notifications = effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::DesktopNotification { title, .. } => Some(title.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(notifications.len(), usize::from(notify));
    if notify {
        assert!(notifications[0].contains('7'));
    }
}
