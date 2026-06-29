use std::sync::Arc;

use anyhow::Result;
use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{terminal_engine::TerminalSelectionEvent, terminal_frame::RenderFrame};

use crate::terminal_session::TerminalSession;

pub trait TerminalRenderSource {
    fn set_display_scale(&mut self, _display_scale: f32) -> Result<()> {
        Ok(())
    }

    fn set_render_cell_metrics(&mut self, _cell: CellMetrics) -> Result<()> {
        Ok(())
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()>;
    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>>;
    fn is_mouse_tracking(&mut self) -> Result<bool> {
        Ok(false)
    }
    fn scroll_viewport_delta(&mut self, _delta: isize) -> Result<()> {
        Ok(())
    }
    fn begin_selection(&mut self, _event: TerminalSelectionEvent) -> Result<()> {
        Ok(())
    }
    fn update_selection(&mut self, _event: TerminalSelectionEvent) -> Result<()> {
        Ok(())
    }
    fn end_selection(&mut self, _event: Option<TerminalSelectionEvent>) -> Result<()> {
        Ok(())
    }
}

impl TerminalRenderSource for TerminalSession {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        Self::set_display_scale(self, display_scale)
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        Self::set_render_cell_metrics(self, cell)
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        Self::resize(self, geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Self::extract_frame(self)
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        Self::is_mouse_tracking(self)
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        Self::scroll_viewport_delta(self, delta)
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        Self::begin_selection(self, event)
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        Self::update_selection(self, event)
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        Self::end_selection(self, event)
    }
}
