//! An empty background pane for fixtures whose live process is in another tab.
use anyhow::Result;
use bootty_mux::terminal::TerminalRuntime;
use bootty_terminal::{
    DrainStats,
    frame_source::TerminalFrameSource,
    geometry::{CellMetrics, TerminalGeometry},
    terminal_capture::{CaptureOptions, TerminalCapture},
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
    terminal_session::{PendingWorkerResponse, worker_request},
};
use std::sync::Arc;

pub struct PlaceholderTerminal {
    frame: Arc<RenderFrame>,
    cwd: Option<String>,
}

impl PlaceholderTerminal {
    pub fn new(geometry: TerminalGeometry, cwd: Option<String>) -> Self {
        Self {
            frame: Arc::new(RenderFrame {
                cols: geometry.cols,
                rows: geometry.rows,
                ..Default::default()
            }),
            cwd,
        }
    }
}

impl TerminalFrameSource for PlaceholderTerminal {
    fn set_display_scale(&mut self, _display_scale: f32) -> Result<()> {
        Ok(())
    }

    fn set_render_cell_metrics(&mut self, _cell: CellMetrics) -> Result<()> {
        Ok(())
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        let frame = Arc::make_mut(&mut self.frame);
        frame.cols = geometry.cols;
        frame.rows = geometry.rows;
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::clone(&self.frame))
    }
}

impl TerminalRuntime for PlaceholderTerminal {
    fn drain_pty(&mut self) -> DrainStats {
        DrainStats::default()
    }

    fn pending_pty_len(&self) -> usize {
        0
    }

    fn child_exited(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn tty_name(&self) -> Option<&str> {
        None
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        Ok(())
    }

    fn force_resize(&mut self) -> Result<()> {
        Ok(())
    }

    fn format_selection(&mut self, _format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(self.cwd.clone())
    }

    fn restore_history(&mut self, text: &str) -> Result<()> {
        anyhow::ensure!(
            text.trim().is_empty(),
            "placeholder cannot restore live terminal output"
        );
        Ok(())
    }

    fn capture(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        options.validate().map_err(anyhow::Error::msg)?;
        let (request, response) = worker_request();
        anyhow::ensure!(request.try_claim(), "placeholder capture was cancelled");
        request.send(Ok(TerminalCapture {
            cols: self.frame.cols,
            rows: self.frame.rows,
            scope: options.scope,
            format: options.format,
            alternate_screen: false,
            captured_lines: 0,
            omitted_lines: 0,
            text: String::new(),
        }));
        Ok(response)
    }

    // Idle slots have no terminal state. The live update is intentionally a no-op.
    fn apply_live_config(&mut self, _config: TerminalLiveConfig) -> Result<()> {
        Ok(())
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn scroll_viewport_delta(&mut self, _delta: isize) -> Result<()> {
        Ok(())
    }

    fn scroll_viewport_to(&mut self, _offset: usize) -> Result<()> {
        Ok(())
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        Ok(())
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn handle_copy_mode_action(
        &mut self,
        _action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        Ok(TerminalCopyModeOutcome::default())
    }

    fn search_viewport(
        &mut self,
        _query: &str,
        _direction: TerminalSearchDirection,
    ) -> Result<bool> {
        Ok(false)
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

    fn write_input(&mut self, _bytes: &[u8]) -> Result<()> {
        anyhow::bail!("placeholder has no process input")
    }

    fn write_paste(&mut self, _text: &str) -> Result<()> {
        anyhow::bail!("placeholder has no process input")
    }
    fn encode_key(&mut self, _input: KeyInput) -> Result<()> {
        anyhow::bail!("placeholder has no process input")
    }

    fn encode_focus(&mut self, _gained: bool) -> Result<()> {
        Ok(())
    }

    fn encode_mouse(&mut self, _input: MouseInput) -> Result<()> {
        Ok(())
    }

    fn handle_mouse_wheel(&mut self, _input: MouseInput, _scroll_delta: isize) -> Result<()> {
        Ok(())
    }
}
