use bootty_ui::gpui as bootty_gpui;
pub fn frame(
    now: std::time::Instant,
    events: Vec<bootty_gpui::InputEvent>,
) -> bootty_ui::FrameInputs {
    bootty_ui::FrameInputs {
        now,
        input: bootty_gpui::FrameInputSnapshot {
            events,
            dropped_file_paths: Vec::new(),
            modifiers: bootty_gpui::Modifiers::default(),
            hover_position: None,
            pressed_mouse_button: None,
            window_focused: true,
        },
        viewport: bootty_ui::ViewportSnapshot::default(),
        display_id: None,
        renderer_metrics: bootty_ui::frame_facts::RendererMetrics::default(),
        terminal_cell_width: 9.0,
        terminal_cell_height: 20.0,
        terminal_scale_factor: 1.0,
        terminal_view_transform: bootty_terminal::geometry::ViewTransform::IDENTITY,
    }
}
