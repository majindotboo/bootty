# egui branch behavior inventory

This inventory treats the local `egui` branch as the application behavior
oracle for the WGPU rewrite. Paths below are read with `git show egui:<path>`.
The Bootty surface column records the rewrite surface that owns the observed
behavior or why it belongs outside the default app.

| Behavior | egui source | Bootty surface |
| --- | --- | --- |
| Default application is an eframe app named `BoottyApp` with a single live terminal session and a WGPU terminal widget | `crates/bootty-app/src/app.rs` defines `BoottyApp`, owns `TerminalSession`, and constructs `TerminalWidget` | Default `bootty` binary as `BoottyApp`, using shared terminal/session/render seams |
| Terminal pixels are rendered through a WGPU callback rather than egui text painting | `crates/bootty-app/src/renderer.rs` builds `TerminalWidget`, `TerminalRenderFrame`, and `terminal_render_callback` | Shared renderer used by the main app and examples; egui hosts chrome and callback plumbing |
| App status bar reports render command count, tmux session count, frame timing, grid size, pending PTY bytes, drain timing, and errors | `crates/bootty-app/src/app.rs` samples `StatusMetrics` and renders `show_status_bar` | Default `bootty` app status bar |
| Tmux sidebar lists sessions, marks current session, refreshes, and attaches or switches clients | `crates/bootty-app/src/app.rs` calls `list_sessions`, `tmux_attach_plan`, `switch_client`, and renders session rows; `crates/bootty-app/src/tmux.rs` owns tmux command parsing | Default `bootty` app tmux sidebar |
| Direct/native keyboard input suppresses duplicate egui events and routes terminal commands to the session | `crates/bootty-app/src/app.rs` implements `raw_input_hook`, `handle_direct_input`, and `handle_egui_input`; `crates/bootty-winit/src/direct_input.rs` owns native event capture | Host-neutral input conversion shared by app and examples |
| Repaint scheduling reacts to drained bytes, dirty rows, pending PTY data, and idle frame cadence | `crates/bootty-app/src/app.rs` calls `RepaintScheduler`; `crates/bootty-runtime/src/scheduler.rs` owns recommendations | Session/runtime scheduler and app repaint integration |
| Bare WGPU host opens a native winit window, owns its own terminal session, handles input/resize, and renders with the WGPU renderer | `crates/bootty-winit/src/bare_host.rs` creates a winit `Window`, `TerminalSession`, `TerminalRenderFrame`, and `TerminalWgpuRenderer` | `cargo run -p bootty-app --example bare` |
| Kitty image state and terminal feature matrix are represented by Bootty-owned fixtures/tests | `crates/bootty-app/tests/*` | Renderer/terminal contract fixtures; inherited upstream gaps remain explicit in fixture metadata |
| Stability trace writes CSV samples when enabled | `crates/bootty-app/src/app.rs` reads `BOOTTY_STABILITY_TRACE` and writes drain/render/session samples | `BOOTTY_STABILITY_TRACE`; legacy env names are not runtime API |
| Egui tabs host does not exist on the oracle branch | No `egui-tabs` example on `egui` | `cargo run -p bootty-app --example egui-tabs`, a simplified tabs host that opens terminal tabs through the shared renderer |
