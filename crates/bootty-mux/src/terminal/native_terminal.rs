use bootty_terminal::terminal_search::TerminalSearchOptions;
use bootty_terminal::{
    terminal_capture::{CaptureOptions, TerminalCapture},
    terminal_session::PendingWorkerResponse,
};
use std::{
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

use super::pane::TerminalRuntime;

use crate::native::{
    NativeBackend, ReconnectSchedule,
    protocol::{self, Frame, FrameReader, PaneCommand, PaneResult, Request, Response},
};
use bootty_terminal::{
    frame_wire::{FrameDecoder, decode_image_content, decode_update},
    terminal_session::{WorkerRequest, worker_request},
    terminal_side_effect::deliver_terminal_side_effects,
};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, thiserror::Error)]
pub enum NativeTerminalError {
    #[error("native terminal input queue overloaded")]
    Overloaded,
    #[error("native terminal disconnected")]
    Disconnected,
}
struct Published {
    frame: Arc<RenderFrame>,
    connected: bool,
    exited: bool,
}
struct Queued {
    request: Request,
    reply: Option<Reply>,
}
pub struct NativeTerminal {
    tx: Option<mpsc::SyncSender<Queued>>,
    shutdown: Arc<Mutex<Option<crate::native::StreamShutdown>>>,
    threads: Vec<thread::JoinHandle<()>>,
    published: Arc<Mutex<Published>>,
    pane: String,
    next_id: u64,
    geometry: TerminalGeometry,
    stopped: Arc<AtomicBool>,
    display_scale: f32,
    render_cell: CellMetrics,
    sent_geometry: Option<TerminalGeometry>,
}
impl NativeTerminal {
    /// # Errors
    /// Returns connection and attachment failures.
    pub fn connect(
        backend: NativeBackend,
        pane: String,
        geometry: TerminalGeometry,
        config: TerminalSessionConfig,
        wakeup: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self> {
        Self::connect_with_schedule(
            backend,
            pane,
            geometry,
            config,
            wakeup,
            ReconnectSchedule::default(),
        )
    }
    /// # Errors
    /// Returns connection failures.
    pub fn connect_with_schedule(
        backend: NativeBackend,
        pane: String,
        geometry: TerminalGeometry,
        config: TerminalSessionConfig,
        wakeup: Arc<dyn Fn() + Send + Sync>,
        retry: ReconnectSchedule,
    ) -> Result<Self> {
        let stream = backend.connect()?;
        let (tx, rx) = mpsc::sync_channel::<Queued>(256);
        let published = Arc::new(Mutex::new(Published {
            frame: super::startup::startup_placeholder_frame(geometry),
            connected: true,
            exited: false,
        }));
        let state = Arc::clone(&published);
        let target = pane.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let (reader, writer, close) = stream.split()?;
        let shutdown = Arc::new(Mutex::new(Some(close)));
        let shared = Arc::new(ConnectionState {
            writer: Mutex::new(writer),
            pending: Mutex::new(HashMap::new()),
            geometry: Mutex::new(Some(geometry)),
            shutdown: Arc::clone(&shutdown),
        });
        shared.attach(&pane)?;
        tx.try_send(Queued {
            request: Request::Resize {
                pane: pane.clone(),
                size: geometry,
            },
            reply: None,
        })?;
        let outgoing = Arc::clone(&shared);
        let status = Arc::clone(&published);
        let writer_thread = thread::spawn(move || write_commands(&rx, &outgoing, &status));
        let reader_thread = thread::spawn(move || {
            run(
                reader,
                ViewerConnection {
                    backend: &backend,
                    pane: &target,
                    shared: &shared,
                    published: &state,
                    config,
                    wakeup: &wakeup,
                    stopped: &stop,
                    retry: &retry,
                },
            );
        });
        Ok(Self {
            tx: Some(tx),
            shutdown,
            threads: vec![reader_thread, writer_thread],
            published,
            pane,
            next_id: 0,
            geometry,
            stopped,
            display_scale: 1.0,
            render_cell: geometry.cell_metrics(),
            sent_geometry: Some(geometry),
        })
    }
    fn enqueue(&self, request: Request, reply: Option<Reply>) -> Result<()> {
        if !self
            .published
            .lock()
            .map_err(|_| anyhow::anyhow!("native frame poisoned"))?
            .connected
        {
            return Err(NativeTerminalError::Disconnected.into());
        }
        self.tx
            .as_ref()
            .ok_or(NativeTerminalError::Disconnected)?
            .try_send(Queued { request, reply })
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => NativeTerminalError::Overloaded,
                mpsc::TrySendError::Disconnected(_) => NativeTerminalError::Disconnected,
            })?;
        Ok(())
    }
    fn command(&mut self, command: PaneCommand, reply: Option<Reply>) -> Result<()> {
        self.next_id = self.next_id.wrapping_add(1);
        self.enqueue(
            Request::PaneCommand {
                pane: self.pane.clone(),
                request_id: self.next_id,
                command,
            },
            reply,
        )
    }
    fn capture_request(
        &mut self,
        options: CaptureOptions,
        checkpoint: bool,
    ) -> Result<PendingWorkerResponse<Result<TerminalCapture, String>>> {
        options.validate().map_err(anyhow::Error::msg)?;
        let (done, response) = worker_request();
        self.command(
            PaneCommand::Capture {
                options,
                checkpoint,
            },
            Some(Reply::Capture(done)),
        )?;
        Ok(response)
    }
}
fn write_commands(
    rx: &mpsc::Receiver<Queued>,
    outgoing: &ConnectionState,
    status: &Mutex<Published>,
) {
    while let Ok(queued) = rx.recv() {
        if let Request::Resize { size, .. } = &queued.request {
            *outgoing
                .geometry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(*size);
        }
        let mut pending = outgoing
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let connected = status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .connected;
        if !connected {
            if let Some(reply) = queued.reply
                && reply.claim()
            {
                reply.send(Err("native terminal disconnected".into()));
            }
            continue;
        }
        if let Request::PaneCommand { request_id, .. } = &queued.request
            && let Some(reply) = queued.reply
        {
            if !reply.claim() {
                continue;
            }
            pending.insert(*request_id, reply);
        }
        drop(pending);
        let mut writer = outgoing
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let close = outgoing
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let result = Frame::json(&queued.request)
            .and_then(|frame| protocol::write_blocking(&mut *writer, &frame));
        drop(writer);
        if result.is_err()
            && let Some(close) = close
        {
            close();
        }
    }
}
struct ViewerConnection<'a> {
    backend: &'a NativeBackend,
    pane: &'a str,
    shared: &'a ConnectionState,
    published: &'a Mutex<Published>,
    config: TerminalSessionConfig,
    wakeup: &'a Arc<dyn Fn() + Send + Sync>,
    stopped: &'a AtomicBool,
    retry: &'a ReconnectSchedule,
}
enum Reply {
    Selection(WorkerRequest<Result<Option<Vec<u8>>, String>>),
    Capture(WorkerRequest<Result<TerminalCapture, String>>),
    Prompt(WorkerRequest<Result<bootty_terminal::shell_prompt::PromptSnapshot, String>>),
}
impl Reply {
    fn claim(&self) -> bool {
        match self {
            Self::Selection(r) => r.try_claim(),
            Self::Capture(r) => r.try_claim(),
            Self::Prompt(r) => r.try_claim(),
        }
    }
    fn send(self, result: Result<PaneResult, String>) {
        match self {
            Self::Selection(r) => r.send(result.and_then(|v| match v {
                PaneResult::Selection(v) => Ok(v),
                _ => Err("invalid selection reply".into()),
            })),
            Self::Capture(r) => r.send(result.and_then(|v| match v {
                PaneResult::Captured(v) => Ok(v),
                _ => Err("invalid capture reply".into()),
            })),
            Self::Prompt(r) => r.send(result.and_then(|v| match v {
                PaneResult::Prompt(v) => Ok(v),
                _ => Err("invalid prompt reply".into()),
            })),
        }
    }
}
struct ConnectionState {
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    pending: Mutex<HashMap<u64, Reply>>,
    geometry: Mutex<Option<TerminalGeometry>>,
    shutdown: Arc<Mutex<Option<crate::native::StreamShutdown>>>,
}
impl ConnectionState {
    fn close(&self) {
        if let Some(close) = self
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            close();
        }
    }
    fn send(&self, request: &Request) -> Result<()> {
        protocol::write_blocking(
            &mut *self
                .writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &Frame::json(request)?,
        )
    }
    fn resize(&self, pane: &str) -> Result<()> {
        let geometry = *self
            .geometry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(size) = geometry {
            self.send(&Request::Resize {
                pane: pane.to_owned(),
                size,
            })?;
        }
        Ok(())
    }
    fn attach(&self, pane: &str) -> Result<()> {
        self.send(&Request::AttachFrames {
            pane: pane.to_owned(),
        })
    }
}
fn reconnect(
    backend: &NativeBackend,
    stopped: &AtomicBool,
    retry: &ReconnectSchedule,
    generation: Option<u64>,
) -> Option<Box<dyn crate::native::Duplex>> {
    while !stopped.load(Ordering::Acquire) {
        if let Ok(stream) = backend.connect() {
            if backend.peer_generation() == generation {
                return Some(stream);
            }
            return None;
        }
        (retry.wait)(retry.deadline);
    }
    None
}
fn decode_frame(
    shared: &ConnectionState,
    pane: &str,
    frame: &Frame,
    decoder: &mut FrameDecoder,
    published: &Mutex<Published>,
    wakeup: &Arc<dyn Fn() + Send + Sync>,
    retry: &ReconnectSchedule,
) -> Result<()> {
    let result = if frame.kind == protocol::IMAGE_CONTENT {
        decode_image_content(&frame.bytes)
            .and_then(|image| decoder.insert_image(image))
            .map(|()| None)
    } else {
        decode_update(&frame.bytes)
            .and_then(|update| decoder.apply(update))
            .map(Some)
    };
    match result {
        Ok(Some(frame)) => {
            let mut state = published
                .lock()
                .map_err(|_| anyhow::anyhow!("native frame poisoned"))?;
            let changed = !state.connected || !state.frame.same_presentation(&frame);
            state.frame = frame;
            state.connected = true;
            drop(state);
            if changed {
                wakeup();
            }
        }
        Ok(None) => {}
        Err(error @ bootty_terminal::frame_wire::WireError::ImageBudget) => {
            return Err(error.into());
        }
        Err(_) => {
            *decoder = FrameDecoder::default();
            (retry.wait)(retry.deadline);
            shared.attach(pane)?;
        }
    }
    Ok(())
}
fn disconnect(shared: &ConnectionState, published: &Mutex<Published>) {
    shared.close();
    if let Ok(mut state) = published.lock() {
        state.connected = false;
    }
    let pending = std::mem::take(
        &mut *shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for (_, reply) in pending {
        reply.send(Err("native terminal disconnected".to_owned()));
    }
}
fn pace_protocol_error(result: &Result<()>, delivered_frame: bool, retry: &ReconnectSchedule) {
    if !delivered_frame
        || result.as_ref().is_err_and(|error| {
            error
                .downcast_ref::<bootty_terminal::frame_wire::WireError>()
                .is_some()
        })
    {
        (retry.wait)(retry.deadline);
    }
}
#[expect(
    clippy::option_option,
    reason = "outer None means this connection has not reconciled host progress yet"
)]
fn publish_progress(
    published: &Mutex<Published>,
    config: &mut TerminalSessionConfig,
    progress: &mut Option<Option<(String, Option<u8>)>>,
    wakeup: &Arc<dyn Fn() + Send + Sync>,
) -> Result<bool> {
    let published = published
        .lock()
        .map_err(|_| anyhow::anyhow!("native frame poisoned"))?;
    let connected = published.connected;
    let next = published.frame.progress.clone();
    drop(published);
    if progress.as_ref() != Some(&next) {
        // Absent server progress clears whatever the host still shows for this pane.
        let (state, value) = next.clone().unwrap_or_else(|| ("remove".to_owned(), None));
        *progress = Some(next);
        deliver_terminal_side_effects(
            &mut config.side_effect_tx,
            &config.side_effect_pane_id,
            vec![
                bootty_terminal::terminal_side_effect::TerminalSideEffect::ConEmuProgress {
                    state,
                    value,
                },
            ],
        );
        wakeup();
    }
    Ok(connected)
}
fn run(mut stream: Box<dyn std::io::Read + Send>, connection: ViewerConnection<'_>) {
    let ViewerConnection {
        backend,
        pane,
        shared,
        published,
        mut config,
        wakeup,
        stopped,
        retry,
    } = connection;
    let generation = backend.peer_generation();
    let mut attached = true;
    loop {
        let mut delivered_frame = false;
        let mut progress = None;
        let result = (|| -> Result<()> {
            if !attached {
                shared.attach(pane)?;
                shared.resize(pane)?;
            }
            attached = false;
            let mut reader = FrameReader::default();
            let mut decoder = FrameDecoder::default();
            loop {
                if stopped.load(Ordering::Acquire) {
                    return Ok(());
                }
                let Some(frame) = reader.poll(&mut stream)? else {
                    continue;
                };
                match frame.kind {
                    protocol::FRAME_UPDATE | protocol::IMAGE_CONTENT => {
                        decode_frame(shared, pane, &frame, &mut decoder, published, wakeup, retry)?;
                        delivered_frame |=
                            publish_progress(published, &mut config, &mut progress, wakeup)?;
                    }
                    _ => match frame.decode::<Response>()? {
                        Response::PaneReply { request_id, result } => {
                            let reply = shared
                                .pending
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .remove(&request_id);
                            if let Some(reply) = reply {
                                reply.send(result.map_err(|e| format!("{e:?}")));
                                wakeup();
                            }
                        }
                        Response::PaneEffect(effect) => {
                            deliver_terminal_side_effects(
                                &mut config.side_effect_tx,
                                &config.side_effect_pane_id,
                                vec![effect],
                            );
                            wakeup();
                        }
                        Response::Exited => {
                            published
                                .lock()
                                .map_err(|_| anyhow::anyhow!("native frame poisoned"))?
                                .exited = true;
                            wakeup();
                            return Ok(());
                        }
                        Response::Error(error) => anyhow::bail!(error),
                        _ => {}
                    },
                }
            }
        })();
        disconnect(shared, published);
        if result.is_ok() || stopped.load(Ordering::Acquire) {
            return;
        }
        wakeup();
        pace_protocol_error(&result, delivered_frame, retry);
        let Some(next) = reconnect(backend, stopped, retry, generation) else {
            return;
        };
        let Ok((reader, writer, close)) = next.split() else {
            return;
        };
        let mut output = shared
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut shutdown = shared
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if stopped.load(Ordering::Acquire) {
            close();
            return;
        }
        *shutdown = Some(close);
        drop(shutdown);
        *output = writer;
        drop(output);
        stream = reader;
    }
}
impl Drop for NativeTerminal {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(close) = self
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            close();
        }
        self.tx.take();
        for thread in self.threads.drain(..) {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
impl TerminalFrameSource for NativeTerminal {
    fn set_display_scale(&mut self, scale: f32) -> Result<()> {
        if self.display_scale.to_bits() == scale.to_bits() {
            return Ok(());
        }
        let mut geometry = self.geometry;
        (geometry.cell_width, geometry.cell_height) = self.render_cell.physical_size(scale);
        self.resize(geometry)?;
        self.display_scale = scale;
        Ok(())
    }
    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        if self.render_cell == cell {
            return Ok(());
        }
        let mut geometry = self.geometry;
        (geometry.cell_width, geometry.cell_height) = cell.physical_size(self.display_scale);
        self.resize(geometry)?;
        self.render_cell = cell;
        Ok(())
    }
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if self.sent_geometry == Some(geometry) {
            return Ok(());
        }
        self.enqueue(
            Request::Resize {
                pane: self.pane.clone(),
                size: geometry,
            },
            None,
        )?;
        self.geometry = geometry;
        self.sent_geometry = Some(geometry);
        Ok(())
    }
    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::clone(
            &self
                .published
                .lock()
                .map_err(|_| anyhow::anyhow!("native frame poisoned"))?
                .frame,
        ))
    }
}
impl TerminalRuntime for NativeTerminal {
    fn child_exited(&mut self) -> Result<bool> {
        Ok(self
            .published
            .lock()
            .map_err(|_| anyhow::anyhow!("native frame poisoned"))?
            .exited)
    }
    fn restore_history(&mut self, text: &str) -> Result<()> {
        self.command(PaneCommand::RestoreHistory(text.to_owned()), None)
    }
    fn prompt(
        &mut self,
        text: Option<(u64, String, bool)>,
    ) -> Result<PendingWorkerResponse<Result<bootty_terminal::shell_prompt::PromptSnapshot, String>>>
    {
        let (done, response) = worker_request();
        self.command(PaneCommand::Prompt(text), Some(Reply::Prompt(done)))?;
        Ok(response)
    }
    fn capture(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<Result<TerminalCapture, String>>> {
        self.capture_request(options, false)
    }
    fn capture_checkpoint(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<Result<TerminalCapture, String>>> {
        self.capture_request(options, true)
    }
    fn current_working_directory(&mut self) -> Result<Option<String>> {
        let frame = self.extract_frame()?;
        let Some(cwd) = &frame.current_working_directory else {
            return Ok(None);
        };
        if let Ok(mut uri) = url::Url::parse(cwd)
            && uri.scheme() == "file"
        {
            uri.set_host(Some("localhost"))
                .map_err(|_| anyhow::anyhow!("invalid directory host"))?;
            return Ok(uri
                .to_file_path()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()));
        }
        Ok(Some(cwd.clone()))
    }
    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.command(PaneCommand::LiveConfig(config), None)
    }
    fn is_mouse_tracking(&mut self) -> Result<bool> {
        Ok(self.extract_frame()?.mouse_tracking)
    }
    fn copy_mode_active(&mut self) -> Result<bool> {
        Ok(self.extract_frame()?.copy_mode.is_some())
    }
    fn format_selection(
        &mut self,
        format: TerminalSelectionFormat,
    ) -> Result<PendingWorkerResponse<std::result::Result<Option<Vec<u8>>, String>>> {
        let (done, response) = worker_request();
        self.command(
            PaneCommand::FormatSelection(format),
            Some(Reply::Selection(done)),
        )?;
        Ok(response)
    }
    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.command(PaneCommand::CopyMode(action), None)?;
        Ok(TerminalCopyModeOutcome {
            active: self.extract_frame()?.copy_mode.is_some(),
            ..Default::default()
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
        self.command(
            PaneCommand::Search {
                query: query.to_owned(),
                direction,
                options,
            },
            None,
        )?;
        Ok(())
    }
    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.enqueue(
            Request::Input {
                pane: self.pane.clone(),
                input: crate::command::PaneInput::Write(bytes.to_vec()),
            },
            None,
        )
    }
    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.command(PaneCommand::Key(input), None)
    }
    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.command(PaneCommand::Mouse(input), None)
    }
    fn write_paste(&mut self, text: &str) -> Result<()> {
        self.command(PaneCommand::Paste(text.to_owned()), None)
    }
    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.command(PaneCommand::Focus(gained), None)
    }
    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.command(PaneCommand::ScrollDelta(delta), None)
    }
    fn scroll_viewport_to(&mut self, offset: usize) -> Result<()> {
        self.command(PaneCommand::ScrollTo(offset), None)
    }
    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.command(PaneCommand::SelectionBegin(event), None)
    }
    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.command(PaneCommand::SelectionUpdate(event), None)
    }
    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.command(PaneCommand::SelectionEnd(event), None)
    }
    fn enter_copy_mode(&mut self) -> Result<()> {
        self.command(PaneCommand::EnterCopyMode, None)
    }
    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.command(
            PaneCommand::MouseWheel {
                input,
                delta: scroll_delta.unsigned_abs(),
            },
            None,
        )
    }
}
