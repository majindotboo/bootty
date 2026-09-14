#![cfg(test)]
#![cfg(unix)]

use bootty_ui::gpui as bootty_gpui;

use pretty_assertions::assert_eq;

use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use assert_fs::prelude::*;
use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig, load_config_from_path};
use bootty_control::{
    AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    controller::SpaceId,
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider,
        MuxBackendRegistry, MuxCommandDispatch, PaneBehavior, PaneTopology, PersistedSessionPolicy,
        SelectionPublicationPolicy, TerminalProgressPolicy, TerminalResidency,
    },
    terminal::BackendPanePolicy,
};
use bootty_terminal::geometry::{CellMetrics, SurfaceRect, TerminalSurface, ViewTransform};
use bootty_terminal::terminal_input::DirectKeyInput;
use bootty_terminal::terminal_input_model::{KeyInput, KeyMods, TerminalKey};
use bootty_ui::gpui::{InputEvent, Key, Modifiers, Point, PointerButton};
use bootty_ui::product_dialogs::terminal_find::{
    FindDirection, TerminalFindModel, TerminalFindOutput,
};
use bootty_ui::{
    AppEffect, AppState, CursorIcon, ModalDialog, presentation::dialogs::NewSessionPickerEvent,
};

#[path = "support/frames.rs"]
mod frames;
mod support;

/// Bounds a real pane-process hang without treating scheduler jitter as failure.
const PANE_BUDGET: Duration = Duration::from_secs(30);

fn native_state() -> (assert_fs::TempDir, AppState) {
    native_state_with_direct_input(None)
}

fn native_state_with_direct_input(
    direct_input_rx: Option<mpsc::Receiver<bootty_terminal::terminal_input::DirectKeyInput>>,
) -> (assert_fs::TempDir, AppState) {
    native_state_with_script(
        direct_input_rx,
        "#!/bin/sh\nprintf '%s\\n' ready\nwhile IFS= read -r line; do\n  printf 'seen:%s\\n' \"$line\"\ndone\n",
    )
}

fn native_state_with_script(
    direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
    source: &str,
) -> (assert_fs::TempDir, AppState) {
    state_with_script_on_backend(direct_input_rx, source, MultiplexerBackendConfig::Native)
}

/// Mirrors rmux's app-facing pane policy while using native PTYs for a test-local terminal.
/// The production rmux provider starts an identity-scoped daemon before it is usable; an app
/// integration test must not bootstrap or attach to that user-owned daemon.
struct RmuxHostPolicyProvider;

impl MuxBackendProvider for RmuxHostPolicyProvider {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Rmux
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::WorkerThread
    }

    fn build_backend(
        &self,
        _config: &MuxBindingConfig,
        workspace: Option<&std::path::Path>,
    ) -> Box<dyn MuxBackend> {
        Box::new(workspace.map_or_else(
            bootty_mux::native::NativeBackend::new,
            bootty_mux::native::NativeBackend::for_workspace,
        ))
    }
}

impl MuxAppBackendProvider for RmuxHostPolicyProvider {
    fn build_pane_policy(&self, _config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(bootty_mux::native::NativePanePolicy)
    }

    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::BackendReconciled,
                cache_terminals: true,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::TerminalOsc,
            persisted_sessions: PersistedSessionPolicy::AfterEmptyInitialSnapshot,
            generated_session_names: GeneratedSessionNamePolicy::PreserveBackend,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::PersistBeforePublish,
        }
    }

    fn capabilities(&self, scope: SpaceId) -> bootty_mux::capability::BindingCapabilityDescriptor {
        bootty_mux::native::native_capabilities(scope)
    }
}

fn rmux_host_policy_backends() -> Arc<MuxBackendRegistry> {
    Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(RmuxHostPolicyProvider)],
            [MuxBackendKind::Rmux],
        )
        .expect("rmux-shaped test backend registry"),
    )
}

fn state_with_script_on_backend(
    direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
    source: &str,
    backend: MultiplexerBackendConfig,
) -> (assert_fs::TempDir, AppState) {
    let directory = assert_fs::TempDir::new().expect("temporary app directory");
    let script = directory.child("terminal-interaction-shell");
    script.write_str(source).expect("write terminal program");
    std::fs::set_permissions(script.path(), std::fs::Permissions::from_mode(0o755))
        .expect("make terminal program executable");
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        multiplexer: bootty_config::config::MultiplexerConfig {
            backend,
            ..bootty_config::config::MultiplexerConfig::default()
        },
        ..BoottyConfig::default()
    };
    config.session.shell = Some(script.path().to_string_lossy().into_owned());
    let backends = if backend == MultiplexerBackendConfig::Rmux {
        rmux_host_policy_backends()
    } else {
        support::backends()
    };
    let state = AppState::new(config, backends, Arc::new(|| {}), direct_input_rx, None)
        .expect("native app state");
    (directory, state)
}

fn submit(state: &mut AppState, action: &str) -> Option<CommandOutcome> {
    let started = Instant::now();
    let (response, outcomes) = mpsc::channel();
    state
        .app_command_sender(Caller::Socket)
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action(action, Caller::Socket),
            deadline: started
                .checked_add(PANE_BUDGET)
                .expect("pane deadline fits"),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");
    let deadline = started
        .checked_add(PANE_BUDGET)
        .expect("pane deadline fits");
    while Instant::now() < deadline {
        state.update_frame(frames::frame(Instant::now(), Vec::new()));
        if let Ok(outcome) = outcomes.try_recv() {
            return Some(outcome);
        }
        thread::sleep(Duration::from_millis(5));
    }
    None
}

fn start_two_panes(state: &mut AppState) -> (String, String) {
    assert!(matches!(
        submit(state, "new_mux_session"),
        Some(CommandOutcome::Success { .. })
    ));
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::NewSession(_))
    ));
    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
    });
    let deadline = Instant::now()
        .checked_add(PANE_BUDGET)
        .expect("pane deadline fits");
    while Instant::now() < deadline && state.focused_pane().is_none() {
        state.update_frame(frames::frame(Instant::now(), Vec::new()));
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        state.focused_pane().is_some(),
        "new session has no pane target"
    );
    let split = submit(state, "split_right");
    assert!(
        matches!(split, Some(CommandOutcome::Success { .. })),
        "split right failed: {split:?}; last error: {:?}",
        state.last_error()
    );
    let focused = state.focused_pane().expect("focused native pane");
    let other = state
        .pane_rects(SurfaceRect::from_min_size(0.0, 0.0, 200.0, 100.0), 4.0)
        .into_iter()
        .map(|(pane_id, _)| pane_id)
        .find(|pane_id| pane_id != &focused)
        .expect("other native pane");
    (other, focused)
}

fn wait_for_pane_text(state: &mut AppState, pane_id: &str, expected: &str) {
    let deadline = Instant::now()
        .checked_add(PANE_BUDGET)
        .expect("pane deadline fits");
    let mut last_rows = Vec::new();
    while Instant::now() < deadline {
        state.update_frame(frames::frame(Instant::now(), Vec::new()));
        if let Some(runtime) = state.terminal_mut().focused_terminal_runtime(pane_id) {
            if let Ok(frame) = runtime.extract_frame() {
                last_rows = frame.text_rows();
                if last_rows.iter().any(|row| row.contains(expected)) {
                    return;
                }
            }
            assert!(
                !runtime.child_exited().unwrap_or(false),
                "pane {pane_id} shell exited before it rendered {expected:?}"
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("pane {pane_id} did not render {expected:?}; last rows: {last_rows:?}");
}

fn wait_for_pane_hex(state: &mut AppState, pane_id: &str, expected: &[&str]) {
    let deadline = Instant::now()
        .checked_add(PANE_BUDGET)
        .expect("pane deadline fits");
    let mut last_rows = Vec::new();
    while Instant::now() < deadline {
        state.update_frame(frames::frame(Instant::now(), Vec::new()));
        if let Some(runtime) = state.terminal_mut().focused_terminal_runtime(pane_id) {
            if let Ok(frame) = runtime.extract_frame() {
                last_rows = frame.text_rows();
                if last_rows
                    .iter()
                    .any(|row| row.split_whitespace().eq(expected.iter().copied()))
                {
                    return;
                }
            }
            assert!(
                !runtime.child_exited().unwrap_or(false),
                "pane {pane_id} shell exited before it rendered {expected:?}"
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("pane {pane_id} did not render {expected:?}; last rows: {last_rows:?}");
}

fn wait_for_pane_hex_prefix(state: &mut AppState, pane_id: &str, expected: &[&str]) {
    let deadline = Instant::now()
        .checked_add(PANE_BUDGET)
        .expect("pane deadline fits");
    let mut last_rows = Vec::new();
    while Instant::now() < deadline {
        state.update_frame(frames::frame(Instant::now(), Vec::new()));
        if let Some(runtime) = state.terminal_mut().focused_terminal_runtime(pane_id) {
            if let Ok(frame) = runtime.extract_frame() {
                last_rows = frame.text_rows();
                if last_rows.iter().any(|row| {
                    row.split_whitespace()
                        .take(expected.len())
                        .eq(expected.iter().copied())
                }) {
                    return;
                }
            }
            assert!(
                !runtime.child_exited().unwrap_or(false),
                "pane {pane_id} shell exited before it rendered {expected:?}"
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("pane {pane_id} did not render hex prefix {expected:?}; last rows: {last_rows:?}");
}

fn mouse_tracking_script(mode: &str) -> String {
    format!(
        "#!/bin/sh\nprintf '\\033[?1003h{mode}ready\\r\\n'\nwhile IFS= read -r line; do\n  printf '%s' \"$line\" | od -An -tx1\ndone\n"
    )
}

#[derive(Clone, Copy)]
struct MouseProtocolCase {
    mode: &'static str,
    expected: &'static [&'static str],
}

fn send_unfocused_pane_mouse_press(state: &mut AppState, pane_id: &str, surface: TerminalSurface) {
    send_unfocused_pane_mouse_button(state, pane_id, surface, PointerButton::Left);
}

fn send_unfocused_pane_mouse_button(
    state: &mut AppState,
    pane_id: &str,
    surface: TerminalSurface,
    button: PointerButton,
) {
    let position = Point {
        x: surface.rect.min_x + 10.0,
        y: surface.rect.min_y + 25.0,
    };
    // This is the same ordering as the GPUI callback: capture the hit pane's presented
    // geometry, then focus/sync its runtime, then decode the queued pointer event.
    state.record_mouse_input_target_for_pane(
        Some(pane_id.to_owned()),
        surface,
        ViewTransform::IDENTITY,
        Some(position),
    );
    state.focus_pane(pane_id);
    state.update_frame(frames::frame(
        Instant::now(),
        vec![InputEvent::PointerButton {
            position,
            button,
            pressed: true,
            click_count: 1,
            modifiers: Modifiers::default(),
        }],
    ));
}

fn send_pane_mouse_wheel(state: &mut AppState, pane_id: &str, surface: TerminalSurface) {
    let position = Point {
        x: surface.rect.min_x + 10.0,
        y: surface.rect.min_y + 25.0,
    };
    state.record_mouse_input_target_for_pane(
        Some(pane_id.to_owned()),
        surface,
        ViewTransform::IDENTITY,
        Some(position),
    );
    state.record_mouse_input_target_for_pane(
        Some(pane_id.to_owned()),
        surface,
        ViewTransform::IDENTITY,
        Some(position),
    );
    let mut frame = frames::frame(
        Instant::now(),
        vec![
            InputEvent::PointerMoved(position),
            InputEvent::MouseWheel {
                unit: bootty_gpui::WheelUnit::Lines,
                delta: Point { x: 0.0, y: 1.0 },
                phase: bootty_gpui::ScrollPhase::Moved,
                modifiers: Modifiers::default(),
            },
        ],
    );
    frame.input.hover_position = Some(position);
    state.update_frame(frame);
}

fn click_terminal(
    state: &mut AppState,
    surface: TerminalSurface,
    position: Point,
    click_count: usize,
) {
    state.record_surface(surface);
    state.update_frame(frames::frame(
        Instant::now(),
        vec![
            InputEvent::PointerButton {
                position,
                button: PointerButton::Left,
                pressed: true,
                click_count,
                modifiers: Modifiers::default(),
            },
            InputEvent::PointerButton {
                position,
                button: PointerButton::Left,
                pressed: false,
                click_count,
                modifiers: Modifiers::default(),
            },
        ],
    ));
}

fn wait_for_selection(
    state: &mut AppState,
    pane_id: &str,
    end_col: u16,
) -> Vec<bootty_terminal::terminal_frame::FrameSelection> {
    let deadline = Instant::now()
        .checked_add(PANE_BUDGET)
        .expect("pane deadline fits");
    while Instant::now() < deadline {
        state.update_frame(frames::frame(Instant::now(), Vec::new()));
        if let Some(runtime) = state.terminal_mut().focused_terminal_runtime(pane_id)
            && let Ok(frame) = runtime.extract_frame()
            && frame
                .selections
                .first()
                .is_some_and(|selection| selection.end_col == end_col)
        {
            return frame.selections.clone();
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("pane {pane_id} did not publish a selection");
}

fn click_surface() -> TerminalSurface {
    TerminalSurface::for_logical_size(
        180.0,
        100.0,
        CellMetrics::new(10.0, 20.0),
        bootty_terminal::geometry::TerminalPadding::default(),
    )
}

#[test]
fn rmux_shaped_terminal_double_and_triple_clicks_select_without_shift() {
    let (_directory, mut state) = state_with_script_on_backend(
        None,
        "#!/bin/sh\nprintf 'abc def\n'\nwhile IFS= read -r line; do\n  printf '%s' \"$line\" | od -An -tx1\ndone\n",
        MultiplexerBackendConfig::Rmux,
    );
    let (_other, pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &pane, "abc def");

    let surface = click_surface();
    let position = Point { x: 15.0, y: 10.0 };
    click_terminal(&mut state, surface, position, 2);
    let word = wait_for_selection(&mut state, &pane, 2);
    assert_eq!(word[0].start_col, 0);
    assert_eq!(word[0].end_col, 2);

    click_terminal(&mut state, surface, position, 3);
    let line = wait_for_selection(&mut state, &pane, 6);
    assert_eq!(line[0].start_col, 0);
    assert_eq!(line[0].end_col, 6);
}

#[test]
fn multi_clicks_reach_applications_that_report_the_mouse() {
    let (_directory, mut state) = state_with_script_on_backend(
        None,
        "#!/bin/sh\nprintf '\\033[?1003h\\033[?1006habc def\n'\nwhile IFS= read -r line; do\n  printf '%s' \"$line\" | od -An -tx1\ndone\n",
        MultiplexerBackendConfig::Rmux,
    );
    let (_other, pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &pane, "abc def");

    let surface = click_surface();
    let position = Point { x: 15.0, y: 10.0 };
    click_terminal(&mut state, surface, position, 2);
    state
        .terminal_mut()
        .write_input(b"\n")
        .expect("terminate double click mouse report line");
    wait_for_pane_hex_prefix(
        &mut state,
        &pane,
        &["1b", "5b", "3c", "30", "3b", "32", "3b", "31", "4d"],
    );
    let frame = state
        .terminal_mut()
        .focused_terminal_runtime(&pane)
        .expect("focused pane runtime")
        .extract_frame()
        .expect("frame");
    assert!(
        frame.selections.is_empty(),
        "double click must not select while the app reports the mouse"
    );
}

#[test]
fn unfocused_pane_mouse_uses_the_hit_surface_after_focus_handoff() {
    let cases = [
        MouseProtocolCase {
            mode: "\\033[?1006h",
            expected: &["1b", "5b", "3c", "30", "3b", "32", "3b", "32", "4d"],
        },
        MouseProtocolCase {
            mode: "\\033[?1016h",
            expected: &[
                "1b", "5b", "3c", "30", "3b", "31", "30", "3b", "32", "35", "4d",
            ],
        },
    ];

    for backend in [
        MultiplexerBackendConfig::Native,
        MultiplexerBackendConfig::Rmux,
    ] {
        for case in cases {
            let source = mouse_tracking_script(case.mode);
            let (_directory, mut state) = state_with_script_on_backend(None, &source, backend);
            let (unfocused_pane, _focused_pane) = start_two_panes(&mut state);
            wait_for_pane_text(&mut state, &unfocused_pane, "ready");

            let area = SurfaceRect::from_min_size(0.0, 0.0, 200.0, 100.0);
            let rect = state
                .pane_rects(area, 4.0)
                .into_iter()
                .find(|(pane_id, _)| pane_id == &unfocused_pane)
                .map(|(_, rect)| rect)
                .expect("unfocused pane rect");
            let surface = TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 20.0));

            send_unfocused_pane_mouse_press(&mut state, &unfocused_pane, surface);
            state
                .terminal_mut()
                .write_input(b"\n")
                .expect("terminate mouse report line");
            wait_for_pane_hex(&mut state, &unfocused_pane, case.expected);
        }
    }
}

#[test]
fn native_split_middle_and_right_presses_focus_the_pointer_pane() {
    let source = mouse_tracking_script("\\033[?1003h\\033[?1006h");
    let (_directory, mut state) = native_state_with_script(None, &source);
    let (unfocused_pane, focused_pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &unfocused_pane, "ready");
    wait_for_pane_text(&mut state, &focused_pane, "ready");

    let area = SurfaceRect::from_min_size(0.0, 0.0, 200.0, 100.0);
    let rects = state.pane_rects(area, 4.0);
    let unfocused_surface = TerminalSurface::for_rect(
        rects
            .iter()
            .find(|(pane_id, _)| pane_id == &unfocused_pane)
            .map(|(_, rect)| *rect)
            .expect("unfocused pane rect"),
        CellMetrics::new(9.0, 20.0),
    );
    let focused_surface = TerminalSurface::for_rect(
        rects
            .iter()
            .find(|(pane_id, _)| pane_id == &focused_pane)
            .map(|(_, rect)| *rect)
            .expect("focused pane rect"),
        CellMetrics::new(9.0, 20.0),
    );

    send_unfocused_pane_mouse_button(
        &mut state,
        &unfocused_pane,
        unfocused_surface,
        PointerButton::Middle,
    );
    state
        .terminal_mut()
        .write_input(b"\n")
        .expect("terminate middle-button report line");
    wait_for_pane_hex_prefix(
        &mut state,
        &unfocused_pane,
        &["1b", "5b", "3c", "31", "3b", "32", "3b", "32", "4d"],
    );

    send_unfocused_pane_mouse_button(
        &mut state,
        &focused_pane,
        focused_surface,
        PointerButton::Right,
    );
    state
        .terminal_mut()
        .write_input(b"\n")
        .expect("terminate right-button report line");
    wait_for_pane_hex_prefix(
        &mut state,
        &focused_pane,
        &["1b", "5b", "3c", "32", "3b", "32", "3b", "32", "4d"],
    );
}

#[test]
fn native_split_wheel_uses_the_presented_surface_geometry() {
    let source = mouse_tracking_script("\\033[?1000h\\033[?1006h");
    let (_directory, mut state) = native_state_with_script(None, &source);
    let (unfocused_pane, focused_pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &unfocused_pane, "ready");

    let area = SurfaceRect::from_min_size(0.0, 0.0, 200.0, 100.0);
    let rect = state
        .pane_rects(area, 4.0)
        .into_iter()
        .find(|(pane_id, _)| pane_id == &unfocused_pane)
        .map(|(_, rect)| rect)
        .expect("unfocused pane rect");
    let surface = TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 20.0));
    send_pane_mouse_wheel(&mut state, &unfocused_pane, surface);
    assert_eq!(state.focused_pane().as_deref(), Some(focused_pane.as_str()));
    state
        .terminal_mut()
        .focused_terminal_runtime(&unfocused_pane)
        .expect("hovered pane runtime")
        .write_input(b"\n")
        .expect("terminate hovered shell input line");
    wait_for_pane_hex_prefix(
        &mut state,
        &unfocused_pane,
        &["1b", "5b", "3c", "36", "34", "3b", "32", "3b", "32", "4d"],
    );
}

#[test]
fn one_frame_routes_wheel_events_to_each_hit_pane() {
    let source = mouse_tracking_script("\\033[?1000h\\033[?1006h");
    let (_directory, mut state) = native_state_with_script(None, &source);
    let (first, second) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &first, "ready");
    wait_for_pane_text(&mut state, &second, "ready");
    let rects = state.pane_rects(SurfaceRect::from_min_size(0.0, 0.0, 200.0, 100.0), 4.0);
    let mut events = Vec::new();
    let mut hover = None;
    for pane in [&first, &second] {
        let rect = rects.iter().find(|(id, _)| id == pane).unwrap().1;
        let surface = TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 20.0));
        let position = Point {
            x: rect.min_x + 10.0,
            y: rect.min_y + 25.0,
        };
        state.record_mouse_input_target_for_pane(
            Some(pane.clone()),
            surface,
            ViewTransform::IDENTITY,
            Some(position),
        );
        events.push(InputEvent::MouseWheel {
            unit: bootty_gpui::WheelUnit::Lines,
            delta: Point { x: 0.0, y: 1.0 },
            phase: bootty_gpui::ScrollPhase::Moved,
            modifiers: Modifiers::default(),
        });
        hover = Some(position);
    }
    let mut frame = frames::frame(Instant::now(), events);
    frame.input.hover_position = hover;
    state.update_frame(frame);
    for pane in [&first, &second] {
        state
            .terminal_mut()
            .focused_terminal_runtime(pane)
            .unwrap()
            .write_input(b"\n")
            .unwrap();
        wait_for_pane_hex_prefix(
            &mut state,
            pane,
            &["1b", "5b", "3c", "36", "34", "3b", "32", "3b", "32", "4d"],
        );
    }
    assert_eq!(state.focused_pane().as_deref(), Some(second.as_str()));
}

#[rstest::rstest]
fn queued_pointer_input_is_discarded_when_its_terminal_window_changes() {
    let source = mouse_tracking_script("\\033[?1000h\\033[?1006h");
    let (_directory, mut state) = native_state_with_script(None, &source);
    let (first, second) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &first, "ready");
    wait_for_pane_text(&mut state, &second, "ready");
    assert!(matches!(
        submit(&mut state, "new_tab"),
        Some(CommandOutcome::Success { .. })
    ));
    let target = state.focused_pane().unwrap();
    wait_for_pane_text(&mut state, &target, "ready");
    let surface = TerminalSurface::for_rect(
        SurfaceRect::from_min_size(0.0, 0.0, 200.0, 100.0),
        CellMetrics::new(9.0, 20.0),
    );
    let position = Point { x: 10.0, y: 25.0 };
    state.record_mouse_input_target_for_pane(
        Some(target.clone()),
        surface,
        ViewTransform::IDENTITY,
        Some(position),
    );
    state.apply_command_palette_event(bootty_ui::presentation::dialogs::CommandPaletteEvent::Run(
        bootty_ui::action_catalog::Command::NextTab,
    ));
    // The queued palette command changes windows before this frame encodes the pointer event.
    state.update_frame(frames::frame(
        Instant::now(),
        vec![InputEvent::PointerButton {
            position,
            button: PointerButton::Left,
            pressed: true,
            click_count: 1,
            modifiers: Modifiers::default(),
        }],
    ));
    assert_ne!(state.focused_pane().as_deref(), Some(target.as_str()));
    assert!(matches!(
        submit(&mut state, "next_tab"),
        Some(CommandOutcome::Success { .. })
    ));
    assert_eq!(state.focused_pane().as_deref(), Some(target.as_str()));
    state
        .terminal_mut()
        .focused_terminal_runtime(&target)
        .unwrap()
        .write_input(b"safe\n")
        .unwrap();
    // The shell reads the complete ordered input line: a stale mouse report would add bytes.
    wait_for_pane_hex(&mut state, &target, &["73", "61", "66", "65"]);
}

#[test]
fn unbound_command_alt_key_reaches_terminal_input_path() {
    let (_directory, mut state) = native_state();
    let (_other, pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &pane, "ready");

    let effects = state.update_frame(frames::frame(
        Instant::now(),
        vec![InputEvent::Key {
            key: Key::Letter('a'),
            pressed: true,
            repeat: false,
            modifiers: Modifiers {
                alt: true,
                platform: true,
                ..Modifiers::default()
            },
        }],
    ));

    assert!(effects.contains(&AppEffect::SetTerminalCursorIcon(CursorIcon::None)));
}

#[test]
fn direct_command_alt_key_writes_exact_kitty_bytes() {
    let (direct_input_tx, direct_input_rx) = mpsc::channel();
    let (_directory, mut state) = native_state_with_script(
        Some(direct_input_rx),
        "#!/bin/sh\nprintf '\\033[>1uready\\r\\n'\nwhile IFS= read -r line; do\n  printf '%s' \"$line\" | od -An -tx1\ndone\n",
    );
    let (_other, pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &pane, "ready");

    direct_input_tx
        .send(DirectKeyInput {
            input: KeyInput {
                key: TerminalKey::A,
                mods: KeyMods {
                    alt: true,
                    command: true,
                    ..KeyMods::default()
                },
                repeat: false,
                utf8: Some("a"),
                unshifted: Some('a'),
            },
        })
        .expect("queue direct GPUI-equivalent input");

    state.drain_direct_input();
    assert_eq!(state.pending_direct_input().len(), 1);
    assert!(state.direct_input_suppresses_host_events());
    let effects = state.update_frame(frames::frame(Instant::now(), Vec::new()));
    assert!(effects.contains(&AppEffect::SetTerminalCursorIcon(CursorIcon::None)));
    state
        .terminal_mut()
        .focused_terminal_runtime(&pane)
        .expect("focused runtime")
        .write_input(b"\n")
        .expect("terminate encoded input line");

    wait_for_pane_hex(
        &mut state,
        &pane,
        &["1b", "5b", "39", "37", "3b", "31", "31", "75"],
    );
}

#[test]
fn terminal_tab_reaches_the_focused_shell_for_completion() {
    let (_directory, mut state) = native_state_with_script(
        None,
        "#!/bin/sh\nprintf '\\033[>1uready\\r\\n'\nwhile IFS= read -r line; do\n  printf '%s' \"$line\" | od -An -tx1\ndone\n",
    );
    let (_other, pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &pane, "ready");

    state.update_frame(frames::frame(
        Instant::now(),
        vec![InputEvent::Key {
            key: Key::Tab,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }],
    ));
    state
        .terminal_mut()
        .focused_terminal_runtime(&pane)
        .expect("focused runtime")
        .write_input(b"\n")
        .expect("terminate shell input line");

    wait_for_pane_hex(&mut state, &pane, &["09"]);
}

fn search_state(state: &mut AppState, pane_id: &str) -> (usize, bool) {
    let frame = state
        .terminal_mut()
        .focused_terminal_runtime(pane_id)
        .expect("pane runtime")
        .extract_frame()
        .expect("pane frame");
    (
        frame.search_match_count,
        frame.active_search_match.is_some(),
    )
}

#[test]
fn normal_search_targets_focused_pane_and_close_clears_search() {
    let (_directory, mut state) = native_state();
    let (unfocused_pane, focused_pane) = start_two_panes(&mut state);

    state
        .terminal_mut()
        .focused_terminal_runtime(&unfocused_pane)
        .expect("first pane")
        .write_input(b"unfocused-only\n")
        .expect("write first pane marker");
    state
        .terminal_mut()
        .focused_terminal_runtime(&focused_pane)
        .expect("second pane")
        .write_input(b"focused-only\n")
        .expect("write second pane marker");
    wait_for_pane_text(&mut state, &unfocused_pane, "seen:unfocused-only");
    wait_for_pane_text(&mut state, &focused_pane, "seen:focused-only");
    state.focus_pane(&focused_pane);

    let dialog = TerminalFindModel::new(String::new());
    state.apply_terminal_find_event(
        dialog,
        TerminalFindOutput::Search {
            query: "focused-only".to_owned(),
            direction: FindDirection::Current,
        },
    );
    assert!(search_state(&mut state, &focused_pane).0 > 0);
    assert_eq!(search_state(&mut state, &unfocused_pane).0, 0);

    let dialog = state.take_terminal_find_dialog().expect("find dialog");
    state.apply_terminal_find_event(dialog, TerminalFindOutput::Close);
    assert_eq!(search_state(&mut state, &focused_pane), (0, false));
}

#[test]
fn copy_mode_search_returns_focus_and_terminal_text_reaches_shell() {
    let (_directory, mut state) = native_state();
    let (_unfocused_pane, focused_pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &focused_pane, "ready");
    state.terminal_mut().enter_copy_mode().expect("copy mode");

    state.update_frame(frames::frame(
        Instant::now(),
        vec![InputEvent::Key {
            key: Key::Slash,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }],
    ));
    let dialog = state.take_terminal_find_dialog().expect("search dialog");
    state.apply_terminal_find_event(
        dialog,
        TerminalFindOutput::Search {
            query: "ready".to_owned(),
            direction: FindDirection::Next,
        },
    );
    assert!(state.terminal_focused());

    state.update_frame(frames::frame(
        Instant::now(),
        vec![
            InputEvent::Key {
                key: Key::Escape,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            },
            InputEvent::ImeCommit("after-search\n".to_owned()),
        ],
    ));
    assert!(
        !state
            .terminal_mut()
            .copy_mode_active()
            .expect("copy mode state")
    );

    wait_for_pane_text(&mut state, &focused_pane, "seen:after-search");
}

#[test]
fn opening_another_overlay_clears_terminal_search() {
    let (_directory, mut state) = native_state();
    let (_unfocused_pane, focused_pane) = start_two_panes(&mut state);
    wait_for_pane_text(&mut state, &focused_pane, "ready");

    assert!(matches!(
        submit(&mut state, "start_search"),
        Some(CommandOutcome::Success { .. })
    ));

    let dialog = state.take_terminal_find_dialog().expect("find dialog");
    state.apply_terminal_find_event(
        dialog,
        TerminalFindOutput::Search {
            query: "ready".to_owned(),
            direction: FindDirection::Current,
        },
    );
    assert!(search_state(&mut state, &focused_pane).0 > 0);

    assert!(matches!(
        submit(&mut state, "command_palette"),
        Some(CommandOutcome::Success { .. })
    ));

    assert_eq!(search_state(&mut state, &focused_pane), (0, false));
}

#[test]
fn configured_platform_shortcut_opens_the_command_palette() {
    let (_directory, mut state) = native_state();
    let now = Instant::now();

    state.update_frame(frames::frame(
        now,
        vec![InputEvent::Key {
            key: Key::Letter('p'),
            pressed: true,
            repeat: false,
            modifiers: Modifiers {
                shift: true,
                platform: cfg!(target_os = "macos"),
                control: !cfg!(target_os = "macos"),
                ..Modifiers::default()
            },
        }],
    ));

    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::CommandPalette(_))
    ));
}

#[test]
fn native_terminal_progress_updates_active_binding_presentation() {
    let directory = assert_fs::TempDir::new().expect("temporary app directory");
    let script = directory.child("terminal-side-effects");
    script
        .write_str("#!/bin/sh\nprintf '\\033]9;4;42\\033\\\\'\nsleep 1\n")
        .expect("write terminal program");
    std::fs::set_permissions(script.path(), std::fs::Permissions::from_mode(0o755))
        .expect("make terminal program executable");

    let config_file = directory.child("config.toml");
    config_file
        .write_str("[multiplexer]\nbackend = \"native\"\n")
        .expect("write config");
    let mut config = load_config_from_path(config_file.path()).expect("load config");
    config.session.shell = Some(script.path().to_string_lossy().into_owned());
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");
    let pane = bootty_mux::snapshot::MuxPaneAnchor {
        session_id: "facts".to_owned(),
        pane_id: Some("%1".to_owned()),
        cwd: None,
        pane_pid: None,
        process: None,
    };
    state
        .terminal_mut()
        .sync_native_window(
            std::slice::from_ref(&pane),
            Some(&pane),
            Some("window"),
            MultiplexerBackendConfig::Native,
            false,
        )
        .expect("start terminal program");

    let deadline = Instant::now()
        .checked_add(PANE_BUDGET)
        .expect("pane deadline fits");
    let mut observed_progress_repaint = false;
    while Instant::now() < deadline && !observed_progress_repaint {
        for effect in state.update_frame(frames::frame(Instant::now(), Vec::new())) {
            observed_progress_repaint |= effect == AppEffect::RequestRepaint;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        observed_progress_repaint,
        "terminal progress must update binding presentation state"
    );
}
