//! Application frame assembly around the host-neutral GPUI input accumulator.

use std::time::Instant;

use crate::gpui::{FrameInputSnapshot, InputAccumulator};
use bootty_terminal::geometry::ViewTransform;
use bootty_terminal::terminal_input::DirectKeyInput;
use gpui_kit::KeyDownEvent;

use crate::{FrameInputs, ViewportSnapshot, frame_facts::RendererMetrics};

#[derive(Clone, Copy, Debug)]
pub struct GpuiFrameFacts {
    pub now: Instant,
    pub viewport: ViewportSnapshot,
    pub display_id: Option<u32>,
    pub renderer_metrics: RendererMetrics,
    pub terminal_cell_width: f32,
    pub terminal_cell_height: f32,
    pub terminal_scale_factor: f32,
    pub terminal_view_transform: ViewTransform,
}

pub const fn frame_inputs(input: FrameInputSnapshot, facts: GpuiFrameFacts) -> FrameInputs {
    FrameInputs {
        now: facts.now,
        input,
        viewport: facts.viewport,
        display_id: facts.display_id,
        renderer_metrics: facts.renderer_metrics,
        terminal_cell_width: facts.terminal_cell_width,
        terminal_cell_height: facts.terminal_cell_height,
        terminal_scale_factor: facts.terminal_scale_factor,
        terminal_view_transform: facts.terminal_view_transform,
    }
}

pub fn drain_frame_inputs(input: &mut InputAccumulator, facts: GpuiFrameFacts) -> FrameInputs {
    frame_inputs(input.drain_frame(), facts)
}

/// Return whether the terminal should stop GPUI propagation for this key.
pub fn terminal_owns_key_down(event: &KeyDownEvent) -> bool {
    crate::gpui::direct_input::terminal_owns_key_down(event)
}

/// Convert an unbound GPUI Command/Super key to the terminal's direct input command.
pub fn direct_key_input(event: &KeyDownEvent) -> Option<DirectKeyInput> {
    crate::gpui::direct_input::direct_key_input_from_gpui_event(event)
}
