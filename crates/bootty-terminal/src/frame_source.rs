use std::sync::Arc;

use crate::geometry::{CellMetrics, TerminalGeometry};
use crate::terminal_frame::RenderFrame;
use anyhow::Result;

use crate::terminal_session::TerminalSession;

/// Nonblocking host updates over immutable published snapshots. A queued resize
/// may leave the previous grid visible until the worker publishes its replacement.
pub trait TerminalFrameSource {
    ///
    /// # Errors
    /// Returns an error if the host cannot accept the scale update.
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()>;
    ///
    /// # Errors
    /// Returns an error if the host cannot accept the cell metrics.
    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()>;
    ///
    /// # Errors
    /// Returns an error for invalid geometry or a host that cannot accept the resize.
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()>;
    ///
    /// # Errors
    /// Returns an error if the published frame is unavailable or the terminal worker failed.
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
        Self::queue_resize(self, geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Self::extract_frame(self)
    }
}
