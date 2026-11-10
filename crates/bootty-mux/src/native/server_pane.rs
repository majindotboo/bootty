use super::{
    protocol::{Frame, Response},
    subscriber::Subscriber,
};
use crate::command::PaneInput;
use anyhow::{Context as _, Result};
use bootty_terminal::{
    SessionLaunchConfig,
    geometry::TerminalGeometry,
    terminal_capture::{CaptureOptions, TerminalCapture},
    terminal_engine::TerminalEngine,
    terminal_input::TerminalInputCommand,
    terminal_input_model::{KeyInput, KeyMods, MouseButton, MouseInput, TerminalKey},
    terminal_transport::{TerminalControl, TerminalSize, TerminalTransport},
};
use std::{
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
};
use tokio::sync::{mpsc, oneshot};

pub type PaneLauncher =
    Arc<dyn Fn(TerminalGeometry, &SessionLaunchConfig) -> Result<TerminalTransport> + Send + Sync>;
#[must_use]
pub fn local_launcher() -> PaneLauncher {
    Arc::new(|size, launch| TerminalTransport::launch(physical_size(size), launch))
}
fn physical_size(size: TerminalGeometry) -> TerminalSize {
    TerminalSize {
        cols: size.cols,
        rows: size.rows,
        pixel_width: size.pixel_width(),
        pixel_height: size.pixel_height(),
    }
}
type Reply<T> = oneshot::Sender<std::result::Result<T, String>>;
pub(super) enum PaneCommand {
    Output {
        epoch: u64,
        bytes: Vec<u8>,
    },
    End(u64),
    Stop,
    Respawn {
        launch: Box<SessionLaunchConfig>,
        history: Vec<u8>,
        reply: Reply<()>,
    },
    Command(
        super::protocol::PaneCommand,
        Subscriber,
        Reply<std::result::Result<super::protocol::PaneResult, super::protocol::PaneError>>,
    ),
    LatestClient(Subscriber),
    ReconcilePolicy,
    Input(PaneInput, Reply<()>),
    Resize {
        size: TerminalGeometry,
        reply: Reply<()>,
    },
    CurrentDirectory(Reply<Option<String>>),
    Capture(CaptureOptions, Reply<TerminalCapture>),
    CaptureCheckpoint {
        pane: String,
        options: CaptureOptions,
        reply: Reply<crate::session_snapshot::SessionPaneCapture>,
    },
}
pub(super) struct PaneHost {
    pub clock: Arc<dyn Fn() -> std::time::Instant + Send + Sync>,
    pub changed: Arc<dyn Fn() + Send + Sync>,
}
#[derive(Clone)]
pub(super) struct Pane {
    pub tx: mpsc::Sender<PaneCommand>,
    pub generation: u64,
    pub frames:
        tokio::sync::watch::Receiver<Option<Arc<bootty_terminal::terminal_frame::RenderFrame>>>,
    stopping: Arc<AtomicBool>,
    title: Arc<Mutex<Option<String>>>,
}
// Close a launched transport until a worker takes ownership, including unwinding.
struct StagedControl(Option<Arc<dyn TerminalControl>>);
impl StagedControl {
    fn disarm(&mut self) {
        self.0 = None;
    }
}
impl Drop for StagedControl {
    fn drop(&mut self) {
        if let Some(control) = &self.0 {
            control.close();
        }
    }
}
impl Pane {
    pub fn spawn(
        size: TerminalGeometry,
        launch: &SessionLaunchConfig,
        launcher: &PaneLauncher,
        history: Vec<u8>,
        desired_policy: tokio::sync::watch::Receiver<super::protocol::VtPolicy>,
        exited: impl FnOnce(u64) + Send + 'static,
        host: PaneHost,
    ) -> Result<Self> {
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        let PaneHost { clock, changed } = host;
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        let TerminalTransport {
            reader,
            writer,
            control,
            ..
        } = launcher(size, launch)?;
        let mut staged = StagedControl(Some(Arc::clone(&control)));
        let (tx, rx) = mpsc::channel(128);
        let (frames_tx, frames) = tokio::sync::watch::channel(None);
        let stored_cwd = launch.working_directory.clone();
        let worker_output = tx.clone();
        let launcher = Arc::clone(launcher);
        let title = Arc::new(Mutex::new(None));
        let published_title = Arc::clone(&title);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        thread::spawn(move || {
            let policy = desired_policy.borrow().clone();
            let live = policy.live();
            let engine = TerminalEngine::new_with_terminal_options(
                size,
                live.colors,
                live.cursor,
                live.features,
                policy.max_scrollback,
                bootty_terminal::terminal_input_model::MacosOptionAsAlt::default(),
            )
            .map_err(|e| format!("{e:#}"));
            let mut engine = match engine {
                Ok(engine) => engine,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            engine.import_history(&history);
            drop(history);
            let worker = PaneWorker {
                engine,
                clock,
                stored_cwd,
                frames: frames_tx,
                latest_client: None,
                progress: None,
                shell_prompt: bootty_terminal::shell_prompt::ShellPrompt::default(),
                size,
                writer: start_writer(writer),
                control,
                stopping: stop,
                policy,
                desired_policy,
                launcher,
                output: worker_output,
                epoch: 1,
                title: published_title,
                changed,
            };
            staged.disarm();
            let _ = ready_tx.send(Ok(()));
            if worker.run(rx) {
                exited(generation);
            }
        });
        ready_rx
            .recv()
            .context("native pane worker stopped during startup")?
            .map_err(anyhow::Error::msg)?;
        let output = tx.clone();
        thread::spawn(move || read_output(reader, &output, 1));
        Ok(Self {
            tx,
            generation,
            frames,
            stopping,
            title,
        })
    }
    pub fn title(&self) -> Option<String> {
        self.title
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.tx.try_send(PaneCommand::Stop);
    }
    pub async fn call<T>(&self, command: impl FnOnce(Reply<T>) -> PaneCommand) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(command(tx))
            .await
            .context("native pane worker stopped")?;
        rx.await
            .context("native pane worker stopped")?
            .map_err(anyhow::Error::msg)
    }
}
fn read_output(
    mut reader: bootty_terminal::terminal_output::TerminalReader,
    output: &mpsc::Sender<PaneCommand>,
    epoch: u64,
) {
    while let Ok(Some(bootty_terminal::terminal_output::TerminalOutput::Bytes { bytes, .. })) =
        reader.next_output()
    {
        if output
            .blocking_send(PaneCommand::Output { epoch, bytes })
            .is_err()
        {
            return;
        }
    }
    let _ = output.blocking_send(PaneCommand::End(epoch));
}
#[derive(Debug, thiserror::Error)]
#[error("native pane input queue full; bytes were not accepted")]
struct InputQueueFull;

// User input budget: raise when supported paste workloads require more buffered input.
const INPUT_BYTE_LIMIT: usize = 4 * 1024 * 1024;
// Query reply budget: raise when legitimate terminal protocols need larger pending replies.
const REPLY_BYTE_LIMIT: usize = 64 * 1024;

struct WriteRequest {
    bytes: Vec<u8>,
    reply: Reply<()>,
    queued: Arc<AtomicUsize>,
}
struct InputWriter {
    tx: std::sync::mpsc::Sender<WriteRequest>,
    input: Arc<AtomicUsize>,
    responses: Arc<AtomicUsize>,
}
impl InputWriter {
    fn check_input(&self, capacity: usize) -> Result<()> {
        anyhow::ensure!(
            capacity <= INPUT_BYTE_LIMIT.saturating_sub(self.input.load(Ordering::Acquire)),
            InputQueueFull
        );
        Ok(())
    }
    fn send(&self, bytes: Vec<u8>, reply: Reply<()>, response: bool) -> Result<()> {
        let queued = if response {
            &self.responses
        } else {
            &self.input
        };
        if response && bytes.len() > REPLY_BYTE_LIMIT.saturating_sub(queued.load(Ordering::Acquire))
        {
            return Ok(());
        }
        let len = bytes.len();
        queued.fetch_add(len, Ordering::AcqRel);
        if let Err(error) = self.tx.send(WriteRequest {
            bytes,
            reply,
            queued: Arc::clone(queued),
        }) {
            queued.fetch_sub(len, Ordering::AcqRel);
            let _ = error.0.reply.send(Err("native pane writer stopped".into()));
            anyhow::bail!("native pane writer stopped");
        }
        Ok(())
    }
}
fn start_writer(mut writer: Box<dyn Write + Send>) -> InputWriter {
    let (tx, rx) = std::sync::mpsc::channel::<WriteRequest>();
    std::thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            let result = writer
                .write_all(&request.bytes)
                .and_then(|()| writer.flush())
                .map_err(|e| format!("write native pane input: {e}"));
            request
                .queued
                .fetch_sub(request.bytes.len(), Ordering::AcqRel);
            let _ = request.reply.send(result);
        }
    });
    InputWriter {
        tx,
        input: Arc::default(),
        responses: Arc::default(),
    }
}
struct PaneWorker {
    clock: Arc<dyn Fn() -> std::time::Instant + Send + Sync>,
    stored_cwd: Option<std::path::PathBuf>,
    frames: tokio::sync::watch::Sender<Option<Arc<bootty_terminal::terminal_frame::RenderFrame>>>,
    latest_client: Option<Subscriber>,
    progress: Option<(String, Option<u8>)>,
    shell_prompt: bootty_terminal::shell_prompt::ShellPrompt,
    engine: TerminalEngine,
    size: TerminalGeometry,
    writer: InputWriter,
    control: Arc<dyn TerminalControl>,
    stopping: Arc<AtomicBool>,
    policy: super::protocol::VtPolicy,
    desired_policy: tokio::sync::watch::Receiver<super::protocol::VtPolicy>,
    launcher: PaneLauncher,
    output: mpsc::Sender<PaneCommand>,
    epoch: u64,
    title: Arc<Mutex<Option<String>>>,
    changed: Arc<dyn Fn() + Send + Sync>,
}
impl Drop for PaneWorker {
    fn drop(&mut self) {
        self.control.close();
    }
}
impl PaneWorker {
    fn run(mut self, mut commands: mpsc::Receiver<PaneCommand>) -> bool {
        use bootty_terminal::terminal_session::{
            PublishHold, output_settling, should_publish_frame_after_work,
            sync_output_suppresses_publish,
        };
        use std::time::Duration;
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        else {
            return true;
        };
        let mut pending = false;
        let mut last_change = (self.clock)();
        let mut last_publish = last_change;
        let mut sync_since = None;
        let _ = self.publish_frame();
        // This actor is the pane lock. Snapshot formatting and subscription are one turn.
        loop {
            let mut command = if pending {
                runtime.block_on(async {
                    tokio::time::timeout(Duration::from_millis(4), commands.recv())
                        .await
                        .ok()
                        .flatten()
                })
            } else {
                commands.blocking_recv()
            };
            if self.reconcile_policy().is_err() {
                return true;
            }
            let output = matches!(command, Some(PaneCommand::Output { .. }));
            if output {
                last_change = (self.clock)();
                pending = true;
            }
            if let Some(command) = command.take() {
                if self.stopping.load(Ordering::Acquire) || matches!(command, PaneCommand::Stop) {
                    return false;
                }
                if let PaneCommand::End(epoch) = command {
                    if epoch != self.epoch {
                        continue;
                    }
                    let _ = self.publish_frame();
                    return true;
                }
                if self.apply(command).is_err() {
                    return true;
                }
            } else if commands.is_closed() {
                return false;
            }
            drop(command);
            if pending {
                let now = (self.clock)();
                let active = self.engine.is_synchronized_output().unwrap_or(false);
                let elapsed = if active {
                    now.duration_since(*sync_since.get_or_insert(now))
                } else {
                    sync_since = None;
                    Duration::ZERO
                };
                let hold = if sync_output_suppresses_publish(
                    active,
                    self.engine.take_synchronized_output_observed(),
                    elapsed,
                ) {
                    PublishHold::SyncOutput
                } else if output_settling(
                    now.duration_since(last_change),
                    now.duration_since(last_publish),
                ) {
                    PublishHold::Settling
                } else {
                    PublishHold::None
                };
                if should_publish_frame_after_work(
                    pending,
                    false,
                    hold,
                    commands.len(),
                    now.duration_since(last_change),
                    now.duration_since(last_publish),
                ) {
                    if self.publish_frame().is_err() {
                        return true;
                    }
                    pending = false;
                    last_publish = now;
                }
            }
        }
    }
    fn reconcile_policy(&mut self) -> Result<()> {
        let desired = self.desired_policy.borrow().clone();
        if desired != self.policy {
            self.engine.apply_live_config(desired.live())?;
            self.policy = desired;
            self.publish_frame()?;
        }
        Ok(())
    }
    fn apply(&mut self, command: PaneCommand) -> Result<()> {
        let changes_presentation = !matches!(
            &command,
            PaneCommand::LatestClient(_)
                | PaneCommand::Input(..)
                | PaneCommand::CurrentDirectory(_)
                | PaneCommand::Capture(..)
                | PaneCommand::CaptureCheckpoint { .. }
                | PaneCommand::Command(
                    super::protocol::PaneCommand::CurrentDirectory
                        | super::protocol::PaneCommand::Capture { .. }
                        | super::protocol::PaneCommand::FormatSelection(_)
                        | super::protocol::PaneCommand::Prompt(None),
                    ..
                )
        );
        match command {
            PaneCommand::Command(command, client, reply) => {
                self.latest_client = Some(client);
                let result = self.apply_terminal_command(command).map_err(|e| {
                    if e.is::<InputQueueFull>() {
                        super::protocol::PaneError::Overloaded
                    } else {
                        super::protocol::PaneError::CommandFailed(format!("{e:#}"))
                    }
                });
                let _ = reply.send(Ok(result));
            }
            PaneCommand::LatestClient(client) => self.latest_client = Some(client),
            PaneCommand::Respawn {
                launch,
                history,
                reply,
            } => {
                let result = self
                    .respawn(&launch, &history)
                    .map_err(|e| format!("{e:#}"));
                let _ = reply.send(result);
            }
            PaneCommand::Output { epoch, bytes } => {
                if epoch != self.epoch {
                    return Ok(());
                }
                self.output_bytes(&bytes)?;
                return Ok(());
            }
            PaneCommand::Input(input, reply) => {
                let capacity = match &input {
                    PaneInput::Write(bytes) => bytes.len(),
                    PaneInput::Paste(text) => text.len().saturating_add(64),
                    PaneInput::Submit => 256,
                };
                match self
                    .writer
                    .check_input(capacity)
                    .and_then(|()| self.encode_input(input))
                {
                    Ok(bytes) => {
                        let _ = self.writer.send(bytes, reply, false);
                    }
                    Err(error) => {
                        let _ = reply.send(Err(format!("{error:#}")));
                    }
                }
            }

            PaneCommand::Resize { size, reply } => {
                if self.size == size {
                    let _ = reply.send(Ok(()));
                    return Ok(());
                }
                let result = self.resize(size).map_err(|e| format!("{e:#}"));
                let _ = reply.send(result);
            }
            PaneCommand::CaptureCheckpoint {
                pane,
                options,
                reply,
            } => {
                let result = self.capture_checkpoint(pane, options);
                let _ = reply.send(result);
            }
            PaneCommand::CurrentDirectory(reply) => {
                let _ = reply.send(Ok(self.current_directory()));
            }
            PaneCommand::Capture(options, reply) => {
                let _ = reply.send(self.engine.capture(options).map_err(|e| format!("{e:#}")));
            }
            PaneCommand::ReconcilePolicy | PaneCommand::End(_) | PaneCommand::Stop => {}
        }
        if changes_presentation {
            self.publish_frame()?;
        }
        Ok(())
    }
    fn capture_checkpoint(
        &self,
        pane: String,
        options: CaptureOptions,
    ) -> std::result::Result<crate::session_snapshot::SessionPaneCapture, String> {
        self.engine
            .capture_checkpoint(options)
            .map(|capture| crate::session_snapshot::SessionPaneCapture {
                pane_id: pane,
                cwd: crate::session_snapshot::checkpoint_cwd(
                    self.engine.current_working_directory(),
                ),
                cols: capture.cols,
                rows: capture.rows,
                text: capture.text,
                omitted_lines: capture.omitted_lines,
            })
            .map_err(|e| format!("{e:#}"))
    }
    fn publish_frame(&mut self) -> Result<()> {
        // Look the directory up on every publish: a cached lookup goes stale when the shell
        // changes directory right after output, and no later frame corrects it. Publishes are
        // paced, and the lookup is one syscall.
        let cwd = self
            .process_directory()
            .or_else(|| {
                crate::session_snapshot::checkpoint_cwd(self.engine.current_working_directory())
            })
            .or_else(|| {
                self.stored_cwd
                    .as_ref()
                    .map(|cwd| cwd.to_string_lossy().into_owned())
            });
        let mut frame = self.engine.extract_frame()?.clone();
        frame.current_working_directory = cwd;
        frame.progress.clone_from(&self.progress);
        if self
            .frames
            .borrow()
            .as_ref()
            .is_none_or(|old| !old.same_presentation(&frame))
        {
            self.frames.send_replace(Some(Arc::new(frame)));
        }
        Ok(())
    }
    fn prompt_command(
        &mut self,
        text: Option<(u64, String, bool)>,
        bytes: &mut Vec<u8>,
    ) -> Result<bootty_terminal::shell_prompt::PromptSnapshot> {
        let allows = self.engine.allows_prompt_editing();
        if let Some((revision, text, submit)) = text {
            anyhow::ensure!(
                !text.is_empty()
                    && text.len() <= 16384
                    && !text
                        .chars()
                        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t')),
                "invalid prompt text"
            );
            self.shell_prompt
                .claim(revision, allows)
                .map_err(anyhow::Error::msg)?;
            self.engine.encode_paste_to_vec(&text, bytes)?;
            if submit {
                bytes.push(b'\r');
            }
        }
        Ok(self
            .shell_prompt
            .snapshot(self.engine.current_working_directory().to_owned(), allows))
    }
    fn check_command_input(&self, command: &super::protocol::PaneCommand) -> Result<()> {
        use super::protocol::PaneCommand as C;
        // Reserve a conservative encoding bound before any prompt or encoder mutation.
        let capacity = match command {
            C::Paste(text) | C::Prompt(Some((_, text, _))) => Some(text.len().saturating_add(64)),
            C::Key(input) => Some(
                input
                    .utf8
                    .map_or(256, |text| text.len().saturating_add(256)),
            ),
            C::Mouse(_) | C::Focus(_) => Some(256),
            C::MouseWheel { delta, .. } => Some(delta.saturating_mul(256).max(256)),
            _ => None,
        };
        if let Some(capacity) = capacity {
            self.writer.check_input(capacity)?;
        }
        Ok(())
    }
    fn apply_terminal_command(
        &mut self,
        command: super::protocol::PaneCommand,
    ) -> Result<super::protocol::PaneResult> {
        use super::protocol::{PaneCommand as C, PaneResult as R};
        self.check_command_input(&command)?;
        let mut bytes = Vec::new();
        match &command {
            C::Key(input) => self
                .shell_prompt
                .input(matches!(input.key, TerminalKey::Enter)),
            C::Paste(_) => self.shell_prompt.input(false),
            _ => {}
        }
        let result = match command {
            C::LiveConfig(config) => {
                self.engine.apply_live_config(config)?;
                R::Ok
            }
            C::Prompt(text) => R::Prompt(self.prompt_command(text, &mut bytes)?),
            C::Capture {
                options,
                checkpoint,
            } => R::Captured(if checkpoint {
                self.engine.capture_checkpoint(options)?
            } else {
                self.engine.capture(options)?
            }),
            C::CurrentDirectory => R::Directory(self.current_directory()),
            C::RestoreHistory(text) => {
                self.engine.import_history(
                    &bootty_terminal::terminal_history::styled_history_bytes(&text)?,
                );
                R::Ok
            }
            C::Key(input) => {
                self.engine.encode_key_to_vec(input, &mut bytes)?;
                R::Ok
            }
            C::Mouse(input) => {
                self.engine.encode_mouse_to_vec(input, &mut bytes)?;
                R::Ok
            }
            C::Paste(text) => {
                self.engine.encode_paste_to_vec(&text, &mut bytes)?;
                R::Ok
            }
            C::Focus(gained) => {
                self.engine.encode_focus_to_vec(gained, &mut bytes)?;
                R::Ok
            }
            C::MouseWheel { input, delta } => {
                mouse_wheel_input(input, delta).apply(&mut self.engine, &mut bytes)?;
                R::Ok
            }
            C::ScrollDelta(delta) => {
                self.engine.scroll_viewport_delta(delta);
                R::Ok
            }
            C::ScrollTo(offset) => {
                self.engine.scroll_viewport_to(offset);
                R::Ok
            }
            C::SelectionBegin(event) => {
                self.engine.begin_selection(event)?;
                R::Ok
            }
            C::SelectionUpdate(event) => {
                self.engine.update_selection(event)?;
                R::Ok
            }
            C::SelectionEnd(event) => {
                self.engine.end_selection(event)?;
                R::Ok
            }
            C::EnterCopyMode => {
                self.engine.enter_copy_mode()?;
                R::Ok
            }
            C::CopyMode(action) => R::CopyMode(self.copy_mode(action)?),
            C::Search {
                query,
                direction,
                options,
            } => R::Search(
                self.engine
                    .search_viewport_with_options(&query, direction, options)?,
            ),
            C::FormatSelection(format) => R::Selection(self.engine.format_selection(format)?),
        };
        if !bytes.is_empty() {
            // Encoded commands acknowledge FIFO acceptance; the actor must keep draining output.
            let (reply, _) = oneshot::channel();
            self.writer.send(bytes, reply, false)?;
        }
        Ok(result)
    }
    fn copy_mode(
        &mut self,
        action: bootty_terminal::terminal_engine::TerminalCopyModeAction,
    ) -> Result<bootty_terminal::terminal_engine::TerminalCopyModeOutcome> {
        let mut outcome = self.engine.handle_copy_mode_action(action)?;
        if let Some(bytes) = outcome.copied.take()
            && let Some(client) = &self.latest_client
        {
            client.push(Frame::json(&super::protocol::Response::PaneEffect(
                bootty_terminal::terminal_side_effect::TerminalSideEffect::ClipboardWrite(
                    String::from_utf8_lossy(&bytes).into_owned(),
                ),
            ))?);
        }
        Ok(outcome)
    }
    fn process_directory(&self) -> Option<String> {
        self.control.foreground_process_id().and_then(|pid| {
            let pid = sysinfo::Pid::from_u32(pid);
            let mut system = sysinfo::System::new().ok()?;
            system.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                sysinfo::ProcessRefreshKind::nothing().with_cwd(sysinfo::UpdateKind::Always),
            );
            system
                .process(pid)?
                .cwd()
                .map(|cwd| cwd.to_string_lossy().into_owned())
        })
    }
    fn current_directory(&self) -> Option<String> {
        self.process_directory()
            .or_else(|| {
                crate::session_snapshot::checkpoint_cwd(self.engine.current_working_directory())
            })
            .or_else(|| {
                self.stored_cwd
                    .as_ref()
                    .map(|cwd| cwd.to_string_lossy().into_owned())
            })
    }
    fn output_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let responses = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let pending = std::rc::Rc::clone(&responses);
        self.engine.on_pty_write(move |_, bytes: &[u8]| {
            pending.borrow_mut().extend_from_slice(bytes);
        })?;
        self.engine.write_vt(bytes);
        let effects = self.engine.drain_side_effects();
        for effect in effects {
            match &effect {
                bootty_terminal::terminal_side_effect::TerminalSideEffect::ConEmuProgress {
                    state,
                    value,
                } => {
                    self.progress = Some((state.clone(), *value));
                    continue;
                }
                bootty_terminal::terminal_side_effect::TerminalSideEffect::WindowTitle(title) => {
                    let mut published = self
                        .title
                        .lock()
                        .map_err(|_| anyhow::anyhow!("pane title lock poisoned"))?;
                    if published.as_ref() != Some(title) {
                        *published = Some(title.clone());
                        drop(published);
                        (self.changed)();
                    }
                }
                bootty_terminal::terminal_side_effect::TerminalSideEffect::ShellLifecycle(
                    event,
                ) => self.shell_prompt.lifecycle(*event),
                bootty_terminal::terminal_side_effect::TerminalSideEffect::ShellPrompt(report) => {
                    self.shell_prompt.report(
                        report,
                        self.engine.current_working_directory(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    );
                }
                _ => {}
            }
            if let Some(client) = &self.latest_client {
                client.push(Frame::json(&Response::PaneEffect(effect))?);
            }
        }
        let bytes = std::mem::take(&mut *responses.borrow_mut());
        if !bytes.is_empty() {
            let (reply, _) = oneshot::channel();
            self.writer.send(bytes, reply, true)?;
        }
        Ok(())
    }
    fn respawn(&mut self, launch: &SessionLaunchConfig, history: &[u8]) -> Result<()> {
        let policy = self.desired_policy.borrow().clone();
        let transport = (self.launcher)(self.size, launch)?;
        let mut staged = StagedControl(Some(Arc::clone(&transport.control)));
        let live = policy.live();
        let engine = TerminalEngine::new_with_terminal_options(
            self.size,
            live.colors,
            live.cursor,
            live.features,
            policy.max_scrollback,
            bootty_terminal::terminal_input_model::MacosOptionAsAlt::default(),
        );
        let mut engine = engine?;
        let epoch = self
            .epoch
            .checked_add(1)
            .context("native pane stream generations exhausted")?;
        engine.import_history(history);
        self.control.close();
        self.control = transport.control;
        staged.disarm();
        self.stored_cwd.clone_from(&launch.working_directory);
        self.writer = start_writer(transport.writer);
        self.engine = engine;
        self.progress = None;
        self.shell_prompt = bootty_terminal::shell_prompt::ShellPrompt::default();
        self.policy = policy;
        self.epoch = epoch;
        *self
            .title
            .lock()
            .map_err(|_| anyhow::anyhow!("pane title lock poisoned"))? = None;
        (self.changed)();
        let output = self.output.clone();
        std::thread::spawn(move || read_output(transport.reader, &output, epoch));
        Ok(())
    }

    fn encode_input(&mut self, input: PaneInput) -> Result<Vec<u8>> {
        match &input {
            PaneInput::Paste(_) => self.shell_prompt.input(false),
            PaneInput::Submit => self.shell_prompt.input(true),
            PaneInput::Write(_) => {}
        }
        let mut bytes = Vec::new();
        match input {
            PaneInput::Write(raw) => {
                for byte in &raw {
                    self.shell_prompt.input(matches!(byte, b'\r' | b'\n'));
                }
                bytes = raw;
            }
            PaneInput::Paste(text) => self.engine.encode_paste_to_vec(&text, &mut bytes)?,
            PaneInput::Submit => self.engine.encode_key_to_vec(
                KeyInput {
                    key: TerminalKey::Enter,
                    mods: KeyMods::default(),
                    repeat: false,
                    utf8: None,
                    unshifted: None,
                },
                &mut bytes,
            )?,
        }
        Ok(bytes)
    }
    fn resize(&mut self, size: TerminalGeometry) -> Result<()> {
        if self.size == size {
            return Ok(());
        }
        self.control.resize(physical_size(size))?;
        if let Err(error) = self.engine.resize(size) {
            self.control.resize(physical_size(self.size))?;
            return Err(error);
        }
        self.size = size;
        Ok(())
    }
}

fn mouse_wheel_input(input: MouseInput, delta: usize) -> TerminalInputCommand {
    let delta = isize::try_from(delta).unwrap_or(isize::MAX);
    let scroll_delta = match input.button {
        Some(MouseButton::Four) => delta.saturating_neg(),
        _ => delta,
    };
    TerminalInputCommand::MouseWheel {
        input,
        scroll_delta,
    }
}
