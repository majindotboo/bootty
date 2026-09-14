use std::sync::Arc;

use crate::geometry::{CellMetrics, TerminalGeometry};
use crate::terminal_frame::RenderFrame;
use anyhow::Result;

use crate::terminal_session::TerminalSession;

pub trait TerminalFrameSource {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()>;
    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()>;
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()>;
    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>>;
}

impl TerminalFrameSource for TerminalSession {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        Self::set_display_scale(self, display_scale)
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        Self::set_render_cell_metrics(self, cell)
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        // A frame source must not return a frame with the old grid after its surface changed.
        // Keep queue_resize for callers that can tolerate eventual publication.
        Self::resize(self, geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Self::extract_frame(self)
    }
}
