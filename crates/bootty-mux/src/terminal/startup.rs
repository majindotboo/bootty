use bootty_terminal::terminal_search::TerminalSearchOptions;
use bootty_terminal::{
    terminal_capture::{CaptureOptions, TerminalCapture},
    terminal_session::PendingWorkerResponse,
};
use std::{
    collections::VecDeque,
    sync::{Arc, mpsc},
    thread,
};

use anyhow::Result;
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{TerminalSessionConfig, frame_source::TerminalFrameSource};
use bootty_terminal::{
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
};

use super::native_terminal::{NativeTerminal, NativeTerminalError};
use super::pane::{PaneStartRequest, TerminalRuntime};
use std::path::Path;

#[derive(Clone)]
enum QueuedStartupCommand {
    Geometry {
        geometry: TerminalGeometry,
        display_scale: f32,
        render_cell: CellMetrics,
    },
    RawInput(Vec<u8>),
    RestoreHistory(String),
    Paste(String),
    Key(KeyInput),
    Focus(bool),
    Mouse(MouseInput),
    MouseWheel {
        input: MouseInput,
        scroll_delta: isize,
    },
    ScrollViewport(isize),
    ScrollViewportTo(usize),
    EnterCopyMode,
    Search {
        query: String,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    },
    SelectionBegin(TerminalSelectionEvent),
    SelectionUpdate(TerminalSelectionEvent),
    SelectionEnd(Option<TerminalSelectionEvent>),
}

pub struct StartingNativeTerminal {
    rx: mpsc::Receiver<std::result::Result<NativeTerminal, String>>,
    terminal: Option<NativeTerminal>,
    geometry: TerminalGeometry,
    placeholder_frame: Arc<RenderFrame>,
    display_scale: f32,
    render_cell: CellMetrics,
    pending_live_config: Option<TerminalLiveConfig>,
    pending_commands: VecDeque<QueuedStartupCommand>,
    startup_error: Option<String>,
}

impl StartingNativeTerminal {
    pub fn spawn(
        backend: crate::native::NativeBackend,
        pane: String,
        geometry: TerminalGeometry,
        display_scale: f32,
        render_cell: CellMetrics,
        config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = NativeTerminal::connect(
                backend,
                pane,
                geometry,
                config,
                Arc::clone(&repaint_wakeup),
            )
            .map_err(|error: anyhow::Error| format!("{error:#}"));
            let _ = tx.send(result);
            repaint_wakeup();
        });

        Self {
            rx,
            terminal: None,
            geometry,
            placeholder_frame: startup_placeholder_frame(geometry),
            display_scale,
            render_cell,
            pending_live_config: None,
            pending_commands: VecDeque::from([QueuedStartupCommand::Geometry {
                geometry,
                display_scale,
                render_cell,
            }]),
            startup_error: None,
        }
    }

    fn ready_terminal(&mut self) -> Result<Option<&mut NativeTerminal>> {
        self.poll_startup()?;
        // Ordered requests must not pass commands retained after a full writer queue.
        anyhow::ensure!(
            self.terminal.is_none()
                || (self.pending_commands.is_empty() && self.pending_live_config.is_none()),
            NativeTerminalError::Overloaded
        );
        Ok(self.terminal.as_mut())
    }

    fn poll_startup(&mut self) -> Result<()> {
        if self.terminal.is_some() {
            return self.replay_pending();
        }
        if let Some(error) = &self.startup_error {
            anyhow::bail!(error.clone());
        }

        let terminal = match self.rx.try_recv() {
            Ok(Ok(terminal)) => terminal,
            Ok(Err(error)) => {
                self.startup_error = Some(error.clone());
                anyhow::bail!(error);
            }
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => {
                let error = "native terminal startup worker stopped".to_owned();
                self.startup_error = Some(error.clone());
                anyhow::bail!(error);
            }
        };

        self.terminal = Some(terminal);
        self.replay_pending()
    }

    fn replay_pending(&mut self) -> Result<()> {
        let Some(terminal) = self.terminal.as_mut() else {
            return Ok(());
        };
        if let Some(config) = self.pending_live_config.clone() {
            match terminal.apply_live_config(config) {
                Ok(()) => self.pending_live_config = None,
                Err(error)
                    if matches!(error.downcast_ref(), Some(NativeTerminalError::Overloaded)) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        while let Some(command) = self.pending_commands.front() {
            match apply_queued_startup_command(terminal, command.clone()) {
                Ok(()) => {
                    self.pending_commands.pop_front();
                }
                Err(error)
                    if matches!(error.downcast_ref(), Some(NativeTerminalError::Overloaded)) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn queue_or_apply(&mut self, command: QueuedStartupCommand) -> Result<()> {
        self.poll_startup()?;
        if self.pending_commands.is_empty()
            && self.pending_live_config.is_none()
            && let Some(terminal) = self.terminal.as_mut()
        {
            return apply_queued_startup_command(terminal, command);
        }
        anyhow::ensure!(
            self.pending_commands
                .iter()
                .filter(|command| !matches!(command, QueuedStartupCommand::Geometry { .. }))
                .count()
                < 256,
            NativeTerminalError::Overloaded
        );
        self.pending_commands.push_back(command);
        Ok(())
    }

    fn queue_geometry(&mut self) -> Result<()> {
        let command = QueuedStartupCommand::Geometry {
            geometry: self.geometry,
            display_scale: self.display_scale,
            render_cell: self.render_cell,
        };
        // At most one geometry entry per run between inputs: 256 inputs permit
        // at most 257 geometry entries. Render state never fails the input cap.
        if let Some(tail @ QueuedStartupCommand::Geometry { .. }) = self.pending_commands.back_mut()
        {
            *tail = command;
        } else {
            self.pending_commands.push_back(command);
        }
        self.poll_startup()
    }

    fn with_published<T>(
        &mut self,
        pending: T,
        ready: impl FnOnce(&mut NativeTerminal) -> Result<T>,
    ) -> Result<T> {
        self.poll_startup()?;
        self.terminal.as_mut().map_or_else(|| Ok(pending), ready)
    }

    fn with_terminal<T>(
        &mut self,
        pending: T,
        ready: impl FnOnce(&mut NativeTerminal) -> Result<T>,
    ) -> Result<T> {
        self.ready_terminal()?.map_or_else(|| Ok(pending), ready)
    }
}

pub(super) fn startup_placeholder_frame(geometry: TerminalGeometry) -> Arc<RenderFrame> {
    let mut frame = RenderFrame {
        cols: geometry.cols,
        rows: geometry.rows,
        row_dirty: vec![true; usize::from(geometry.rows)],
        row_wraps: vec![false; usize::from(geometry.rows)],
        ..RenderFrame::default()
    };
    frame.stats.dirty_rows = usize::from(geometry.rows);
    Arc::new(frame)
}

fn apply_queued_startup_command(
    terminal: &mut NativeTerminal,
    command: QueuedStartupCommand,
) -> Result<()> {
    match command {
        QueuedStartupCommand::Geometry {
            geometry,
            display_scale,
            render_cell,
        } => {
            TerminalFrameSource::resize(terminal, geometry)?;
            terminal.set_display_scale(display_scale)?;
            terminal.set_render_cell_metrics(render_cell)
        }
        QueuedStartupCommand::RawInput(bytes) => terminal.write_input(&bytes),
        QueuedStartupCommand::RestoreHistory(text) => terminal.restore_history(&text),
        QueuedStartupCommand::Paste(text) => terminal.write_paste(&text),
        QueuedStartupCommand::Key(input) => terminal.encode_key(input),
        QueuedStartupCommand::Focus(gained) => terminal.encode_focus(gained),
        QueuedStartupCommand::Mouse(input) => terminal.encode_mouse(input),
        QueuedStartupCommand::MouseWheel {
            input,
            scroll_delta,
        } => terminal.handle_mouse_wheel(input, scroll_delta),
        QueuedStartupCommand::ScrollViewport(delta) => terminal.scroll_viewport_delta(delta),
        QueuedStartupCommand::ScrollViewportTo(offset) => terminal.scroll_viewport_to(offset),
        QueuedStartupCommand::EnterCopyMode => terminal.enter_copy_mode(),
        QueuedStartupCommand::Search {
            query,
            direction,
            options,
        } => terminal.search_viewport_with_options(&query, direction, options),
        QueuedStartupCommand::SelectionBegin(event) => terminal.begin_selection(event),
        QueuedStartupCommand::SelectionUpdate(event) => terminal.update_selection(event),
        QueuedStartupCommand::SelectionEnd(event) => terminal.end_selection(event),
    }
}

impl TerminalFrameSource for StartingNativeTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        if self.display_scale.to_bits() == display_scale.to_bits() {
            return self.poll_startup();
        }
        self.display_scale = display_scale;
        self.queue_geometry()
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        if self.render_cell == cell {
            return self.poll_startup();
        }
        self.render_cell = cell;
        self.queue_geometry()
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if self.geometry == geometry {
            return self.poll_startup();
        }
        if self.terminal.is_none() {
            self.placeholder_frame = startup_placeholder_frame(geometry);
        }
        self.geometry = geometry;
        self.queue_geometry()
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.with_published(
            Arc::clone(&self.placeholder_frame),
            NativeTerminal::extract_frame,
        )
    }
}

impl TerminalRuntime for StartingNativeTerminal {
    fn child_exited(&mut self) -> Result<bool> {
        self.with_published(false, NativeTerminal::child_exited)
    }

    fn started(&mut self) -> Result<bool> {
        self.poll_startup()?;
        Ok(self.terminal.is_some())
    }

    fn restore_history(&mut self, text: &str) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::RestoreHistory(text.to_owned()))
    }

    fn prompt(
        &mut self,
        text: Option<(u64, String, bool)>,
    ) -> Result<
        PendingWorkerResponse<
            std::result::Result<bootty_terminal::shell_prompt::PromptSnapshot, String>,
        >,
    > {
        self.ready_terminal()?
            .ok_or_else(|| anyhow::anyhow!("Terminal is still starting"))?
            .prompt(text)
    }
    fn capture(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        self.ready_terminal()?
            .ok_or_else(|| anyhow::anyhow!("Terminal is still starting"))?
            .capture(options)
    }

    fn format_selection(
        &mut self,
        format: TerminalSelectionFormat,
    ) -> Result<PendingWorkerResponse<std::result::Result<Option<Vec<u8>>, String>>> {
        self.ready_terminal()?
            .ok_or_else(|| anyhow::anyhow!("Terminal is still starting"))?
            .format_selection(format)
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        self.with_published(None, TerminalRuntime::current_working_directory)
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.poll_startup()?;
        if self.terminal.is_none() {
            // Keep the latest aggregate. Startup applies it once after the worker is ready.
            self.pending_live_config = Some(config);
            return Ok(());
        }
        self.with_terminal((), |terminal| terminal.apply_live_config(config))
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        self.with_published(false, NativeTerminal::is_mouse_tracking)
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::ScrollViewport(delta))
    }

    fn scroll_viewport_to(&mut self, offset: usize) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::ScrollViewportTo(offset))
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::EnterCopyMode)
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        self.with_published(false, NativeTerminal::copy_mode_active)
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.with_terminal(TerminalCopyModeOutcome::default(), |terminal| {
            terminal.handle_copy_mode_action(action)
        })
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<()> {
        self.search_viewport_with_options(query, direction, TerminalSearchOptions::default())
    }

    fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::Search {
            query: query.to_owned(),
            direction,
            options,
        })
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::SelectionBegin(event))
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::SelectionUpdate(event))
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::SelectionEnd(event))
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::RawInput(bytes.to_vec()))
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::Paste(text.to_owned()))
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::Key(input))
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::Focus(gained))
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::Mouse(input))
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::MouseWheel {
            input,
            scroll_delta,
        })
    }
}

/// Start a native pane without blocking the UI on PTY creation.
/// # Errors
/// Returns an error when the target has no native pane.
pub(super) fn start_native_terminal(request: PaneStartRequest<'_>) -> Box<dyn TerminalRuntime> {
    let mut config = request.terminal_config.clone();
    config.restored_history = None;
    config.launch.working_directory = request.target.cwd().map(Path::new).map(Path::to_path_buf);
    config.launch.pane_id = Some(request.target.pane_id().to_owned());
    config.side_effect_pane_id = Some(request.target.side_effect_pane_id());
    Box::new(StartingNativeTerminal::spawn(
        request.backend.clone(),
        request.target.pane_id().to_owned(),
        request.spawn_geometry,
        request.display_scale,
        request.render_cell,
        config,
        Arc::clone(request.repaint_wakeup),
    ))
}
