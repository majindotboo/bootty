use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use bootty_surface::geometry::TerminalGeometry;
use bootty_terminal::{
    terminal_engine::{TerminalColorConfig, TerminalEngine, TerminalSideEffect},
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MacosOptionAsAlt, MouseInput},
};

use bootty_runtime::{DrainStats, render_source::TerminalRenderSource};

use crate::{
    config::MuxBackendKind,
    tmux_protocol::{TmuxControlNotification, TmuxControlParser},
};

use super::{
    pane::{MuxPaneTarget, TMUX_CLIENT_FEATURES, TerminalRuntime},
    tmux_codec::{
        TmuxPassthroughDecoder, decode_tmux_control_output, send_tmux_hex_input,
        target_input_selector,
    },
};

const TMUX_CONTROL_READY_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const TMUX_CONTROL_BACKLOG_FRAME_INTERVAL: Duration = Duration::from_millis(64);
const TMUX_CONTROL_SETTLED_FRAME_DELAY: Duration = Duration::from_millis(16);
const TMUX_CONTROL_SYNC_OUTPUT_MAX_SUPPRESS: Duration = Duration::from_secs(1);

pub(super) struct TmuxControlTerminal {
    geometry: TerminalGeometry,
    command_tx: mpsc::Sender<TmuxControlCommand>,
    child: Child,
    latest_frame: Arc<PublishedMuxFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_output_len: Arc<AtomicUsize>,
}

impl TmuxControlTerminal {
    pub(super) fn new(
        backend: MuxBackendKind,
        target: MuxPaneTarget,
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
        macos_option_as_alt: MacosOptionAsAlt,
        side_effect_tx: Option<mpsc::Sender<TerminalSideEffect>>,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Result<Self> {
        let program = native_control_program(backend)?;
        let mut command = Command::new(program);
        configure_native_control_command(&mut command, &target);
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("native mux control stdout missing"))?;
        let stdin =
            Arc::new(Mutex::new(child.stdin.take().ok_or_else(|| {
                anyhow::anyhow!("native mux control stdin missing")
            })?));
        let (line_tx, line_rx) = mpsc::channel();
        let pending_output_len = Arc::new(AtomicUsize::new(0));
        let reader_pending_output_len = Arc::clone(&pending_output_len);
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(std::result::Result::ok) {
                reader_pending_output_len.fetch_add(line.len(), Ordering::Relaxed);
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });

        let (command_tx, command_rx) = mpsc::channel();
        let latest_frame = Arc::new(PublishedMuxFrame::new());
        let latest_drain = Arc::new(Mutex::new(DrainStats::default()));

        let terminal = Self {
            geometry,
            command_tx,
            child,
            latest_frame: Arc::clone(&latest_frame),
            latest_drain: Arc::clone(&latest_drain),
            pending_output_len: Arc::clone(&pending_output_len),
        };
        spawn_tmux_control_worker(TmuxControlWorkerConfig {
            target,
            geometry,
            colors,
            macos_option_as_alt,
            stdin,
            line_rx,
            repaint_wakeup,
            command_rx,
            latest_frame,
            latest_drain,
            pending_output_len,
            side_effect_tx,
        })?;
        Ok(terminal)
    }
}

impl TerminalRenderSource for TmuxControlTerminal {
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if self.geometry == geometry {
            return Ok(());
        }
        self.geometry = geometry;
        self.command_tx
            .send(TmuxControlCommand::Resize(geometry))
            .map_err(|_| anyhow::anyhow!("native mux control worker stopped"))
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.latest_frame.load()
    }
}

impl TerminalRuntime for TmuxControlTerminal {
    fn drain_pty(&mut self) -> DrainStats {
        let Ok(mut stats) = self.latest_drain.lock() else {
            return DrainStats::default();
        };
        let drained = *stats;
        *stats = DrainStats::default();
        drained
    }

    fn pending_pty_len(&self) -> usize {
        self.pending_output_len.load(Ordering::Relaxed)
    }

    fn child_exited(&mut self) -> Result<bool> {
        self.child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(Into::into)
    }

    fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        self.command_tx
            .send(TmuxControlCommand::Colors(colors))
            .map_err(|_| anyhow::anyhow!("native mux control worker stopped"))
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.command_tx
            .send(TmuxControlCommand::RawInput(bytes.to_vec()))
            .map_err(|_| anyhow::anyhow!("native mux control worker stopped"))
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        self.command_tx
            .send(TmuxControlCommand::Paste(text.to_owned()))
            .map_err(|_| anyhow::anyhow!("native mux control worker stopped"))
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.command_tx
            .send(TmuxControlCommand::Key(input))
            .map_err(|_| anyhow::anyhow!("native mux control worker stopped"))
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.command_tx
            .send(TmuxControlCommand::Focus(gained))
            .map_err(|_| anyhow::anyhow!("native mux control worker stopped"))
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.command_tx
            .send(TmuxControlCommand::Mouse(input))
            .map_err(|_| anyhow::anyhow!("native mux control worker stopped"))
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.command_tx
            .send(TmuxControlCommand::MouseWheel {
                input,
                scroll_delta,
            })
            .map_err(|_| anyhow::anyhow!("native mux control worker stopped"))
    }
}

enum TmuxControlCommand {
    Resize(TerminalGeometry),
    Colors(TerminalColorConfig),
    Key(KeyInput),
    Focus(bool),
    Mouse(MouseInput),
    MouseWheel {
        input: MouseInput,
        scroll_delta: isize,
    },
    Paste(String),
    RawInput(Vec<u8>),
}

struct PublishedMuxFrame {
    latest: Mutex<Arc<RenderFrame>>,
}

impl PublishedMuxFrame {
    fn new() -> Self {
        Self {
            latest: Mutex::new(Arc::new(RenderFrame::default())),
        }
    }

    fn load(&self) -> Result<Arc<RenderFrame>> {
        self.latest
            .lock()
            .map(|frame| Arc::clone(&frame))
            .map_err(|_| anyhow::anyhow!("native mux render frame lock poisoned"))
    }

    fn publish(&self, frame: &RenderFrame) -> Result<()> {
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux render frame lock poisoned"))?;
        *latest = Arc::new(frame.clone());
        Ok(())
    }
}

struct TmuxControlWorker {
    target: MuxPaneTarget,
    pane_number: Option<usize>,
    geometry: TerminalGeometry,
    engine: TerminalEngine,
    stdin: Arc<Mutex<ChildStdin>>,
    line_rx: mpsc::Receiver<String>,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    command_rx: mpsc::Receiver<TmuxControlCommand>,
    latest_frame: Arc<PublishedMuxFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_output_len: Arc<AtomicUsize>,
    sync_output_since: Option<Instant>,
    control_parser: TmuxControlParser,
    passthrough_decoder: TmuxPassthroughDecoder,
    side_effect_tx: Option<mpsc::Sender<TerminalSideEffect>>,
}

struct TmuxControlWorkerConfig {
    target: MuxPaneTarget,
    geometry: TerminalGeometry,
    colors: TerminalColorConfig,
    macos_option_as_alt: MacosOptionAsAlt,
    stdin: Arc<Mutex<ChildStdin>>,
    line_rx: mpsc::Receiver<String>,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    command_rx: mpsc::Receiver<TmuxControlCommand>,
    latest_frame: Arc<PublishedMuxFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_output_len: Arc<AtomicUsize>,
    side_effect_tx: Option<mpsc::Sender<TerminalSideEffect>>,
}

fn spawn_tmux_control_worker(config: TmuxControlWorkerConfig) -> Result<()> {
    thread::Builder::new()
        .name("bootty-tmux-control-worker".to_owned())
        .spawn(move || {
            if let Ok(mut worker) = TmuxControlWorker::new(config) {
                worker.run();
            }
        })
        .map(|_| ())
        .map_err(Into::into)
}

impl TmuxControlWorker {
    fn new(config: TmuxControlWorkerConfig) -> Result<Self> {
        let mut engine = TerminalEngine::new_with_options(
            config.geometry,
            config.colors,
            bootty_terminal::terminal_engine::DEFAULT_MAX_SCROLLBACK,
            config.macos_option_as_alt,
        )?;
        let input_stdin = Arc::clone(&config.stdin);
        let input_target = target_input_selector(&config.target).to_owned();
        engine.on_pty_write(move |_terminal, bytes| {
            let _ = send_tmux_hex_input(&input_stdin, &input_target, bytes);
        })?;

        let mut worker = Self {
            pane_number: config.target.tmux_pane_number(),
            target: config.target,
            geometry: config.geometry,
            engine,
            stdin: config.stdin,
            line_rx: config.line_rx,
            repaint_wakeup: config.repaint_wakeup,
            command_rx: config.command_rx,
            latest_frame: config.latest_frame,
            latest_drain: config.latest_drain,
            pending_output_len: config.pending_output_len,
            sync_output_since: None,
            side_effect_tx: config.side_effect_tx,
            control_parser: TmuxControlParser::default(),
            passthrough_decoder: TmuxPassthroughDecoder::default(),
        };
        worker.write_control_command(&format!(
            "refresh-client -C {}x{}",
            worker.geometry.cols, worker.geometry.rows
        ))?;
        Ok(worker)
    }

    fn run(&mut self) {
        let mut unpublished_frame = true;
        let mut last_publish = Instant::now() - TMUX_CONTROL_READY_FRAME_INTERVAL;
        let mut last_terminal_change = Instant::now();
        loop {
            let (command_work, commands_disconnected) = self.process_commands();
            if commands_disconnected {
                break;
            }
            let drain = self.drain_output();
            if drain.bytes > 0 {
                self.publish_drain(drain);
                unpublished_frame = true;
                last_terminal_change = Instant::now();
            }
            if command_work {
                unpublished_frame = true;
                last_terminal_change = Instant::now();
            }
            if self.should_publish_frame(
                unpublished_frame,
                last_terminal_change.elapsed(),
                last_publish.elapsed(),
            ) {
                if let Ok(frame) = self.engine.extract_frame()
                    && self.latest_frame.publish(frame).is_ok()
                {
                    (self.repaint_wakeup)();
                }
                last_publish = Instant::now();
                unpublished_frame = false;
            }
            if !command_work && drain.bytes == 0 {
                thread::sleep(Duration::from_millis(4));
            }
        }
    }

    fn should_publish_frame(
        &mut self,
        unpublished_frame: bool,
        elapsed_since_last_terminal_change: Duration,
        elapsed_since_last_publish: Duration,
    ) -> bool {
        should_publish_tmux_control_frame(
            unpublished_frame,
            self.sync_output_suppressed(),
            self.pending_output_len.load(Ordering::Relaxed),
            elapsed_since_last_terminal_change,
            elapsed_since_last_publish,
        )
    }

    fn sync_output_suppressed(&mut self) -> bool {
        if !self.engine.is_synchronized_output().unwrap_or(false) {
            self.sync_output_since = None;
            return false;
        }
        let since = *self.sync_output_since.get_or_insert_with(Instant::now);
        since.elapsed() < TMUX_CONTROL_SYNC_OUTPUT_MAX_SUPPRESS
    }

    fn process_commands(&mut self) -> (bool, bool) {
        let mut did_work = false;
        loop {
            match self.command_rx.try_recv() {
                Ok(command) => {
                    did_work |= self.process_command(command).unwrap_or(true);
                }
                Err(mpsc::TryRecvError::Empty) => return (did_work, false),
                Err(mpsc::TryRecvError::Disconnected) => return (did_work, true),
            }
        }
    }

    fn process_command(&mut self, command: TmuxControlCommand) -> Result<bool> {
        match command {
            TmuxControlCommand::Resize(geometry) => {
                if self.geometry != geometry {
                    self.geometry = geometry;
                    self.engine.resize(geometry)?;
                    self.write_control_command(&format!(
                        "refresh-client -C {}x{}",
                        geometry.cols, geometry.rows
                    ))?;
                }
            }
            TmuxControlCommand::Colors(colors) => self.engine.set_colors(colors)?,
            TmuxControlCommand::Key(input) => {
                let mut bytes = Vec::new();
                self.engine.encode_key_to_vec(input, &mut bytes)?;
                self.send_input(&bytes)?;
            }
            TmuxControlCommand::Focus(gained) => {
                let mut bytes = Vec::new();
                self.engine.encode_focus_to_vec(gained, &mut bytes)?;
                self.send_input(&bytes)?;
            }
            TmuxControlCommand::Mouse(input) => {
                let mut bytes = Vec::new();
                self.engine.encode_mouse_to_vec(input, &mut bytes)?;
                self.send_input(&bytes)?;
            }
            TmuxControlCommand::MouseWheel {
                input,
                scroll_delta,
            } => match tmux_control_mouse_wheel_action(self.engine.is_mouse_tracking()?) {
                TmuxControlMouseWheelAction::EncodeMouse => {
                    let mut bytes = Vec::new();
                    self.engine.encode_mouse_to_vec(input, &mut bytes)?;
                    self.send_input(&bytes)?;
                }
                TmuxControlMouseWheelAction::ScrollViewport if scroll_delta != 0 => {
                    self.engine.scroll_viewport_delta(scroll_delta);
                }
                TmuxControlMouseWheelAction::ScrollViewport => return Ok(false),
            },
            TmuxControlCommand::Paste(text) => {
                let mut bytes = Vec::new();
                self.engine.encode_paste_to_vec(&text, &mut bytes)?;
                self.send_input(&bytes)?;
            }
            TmuxControlCommand::RawInput(bytes) => self.send_input(&bytes)?,
        }
        Ok(true)
    }

    fn drain_output(&mut self) -> DrainStats {
        let start = Instant::now();
        let mut stats = DrainStats::default();
        while stats.chunks < 512 && start.elapsed().as_micros() < 20_000 {
            let line = match self.line_rx.try_recv() {
                Ok(line) => line,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            };
            self.pending_output_len
                .fetch_sub(line.len(), Ordering::Relaxed);
            stats.bytes += line.len();
            stats.chunks += 1;
            if let Ok(notifications) = self.control_parser.put_str(&(line + "\n")) {
                for notification in notifications {
                    if let TmuxControlNotification::Output(output) = notification
                        && self.output_matches_target(output.pane_id)
                    {
                        let bytes = decode_tmux_control_output(&output.data);
                        let bytes = self.passthrough_decoder.push(&bytes);
                        if !bytes.is_empty() {
                            self.engine.write_vt(&bytes);
                            self.forward_side_effects();
                        }
                    }
                }
            }
        }
        stats.elapsed_us = start.elapsed().as_micros() as u64;
        stats
    }

    fn forward_side_effects(&mut self) {
        let Some(tx) = self.side_effect_tx.as_ref() else {
            return;
        };
        let mut disconnected = false;
        for effect in self.engine.drain_side_effects() {
            if tx.send(effect).is_err() {
                disconnected = true;
                break;
            }
        }
        if disconnected {
            self.side_effect_tx = None;
        }
    }

    fn publish_drain(&self, stats: DrainStats) {
        if let Ok(mut latest) = self.latest_drain.lock() {
            latest.chunks = latest.chunks.saturating_add(stats.chunks);
            latest.bytes = latest.bytes.saturating_add(stats.bytes);
            latest.elapsed_us = latest.elapsed_us.saturating_add(stats.elapsed_us);
        }
    }

    fn send_input(&self, bytes: &[u8]) -> Result<()> {
        send_tmux_hex_input(&self.stdin, target_input_selector(&self.target), bytes)
    }

    fn write_control_command(&mut self, command: &str) -> Result<()> {
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux control stdin lock poisoned"))?;
        writeln!(stdin, "{command}")?;
        stdin.flush()?;
        Ok(())
    }

    fn output_matches_target(&self, pane_id: usize) -> bool {
        self.pane_number.is_none_or(|target| target == pane_id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TmuxControlMouseWheelAction {
    EncodeMouse,
    ScrollViewport,
}

fn tmux_control_mouse_wheel_action(mouse_tracking: bool) -> TmuxControlMouseWheelAction {
    if mouse_tracking {
        TmuxControlMouseWheelAction::EncodeMouse
    } else {
        TmuxControlMouseWheelAction::ScrollViewport
    }
}

fn should_publish_tmux_control_frame(
    unpublished_frame: bool,
    sync_output_suppressed: bool,
    pending_output_bytes: usize,
    elapsed_since_last_terminal_change: Duration,
    elapsed_since_last_publish: Duration,
) -> bool {
    if !unpublished_frame || sync_output_suppressed {
        return false;
    }
    if pending_output_bytes > 0 {
        return elapsed_since_last_publish >= TMUX_CONTROL_BACKLOG_FRAME_INTERVAL;
    }
    elapsed_since_last_terminal_change >= TMUX_CONTROL_SETTLED_FRAME_DELAY
}

fn native_control_program(backend: MuxBackendKind) -> Result<&'static str> {
    match backend {
        MuxBackendKind::Tmux => Ok("tmux"),
        MuxBackendKind::Rmux => bail!("rmux native rendering uses rmux-sdk, not control mode"),
        MuxBackendKind::Native => bail!("native rendering uses Bootty-owned terminals"),
        MuxBackendKind::Zellij => bail!("zellij native pane rendering is not implemented"),
    }
}

fn configure_native_control_command(command: &mut Command, target: &MuxPaneTarget) {
    command
        .args([
            "-C",
            "-T",
            TMUX_CLIENT_FEATURES,
            "attach-session",
            "-t",
            target.session_id(),
        ])
        .env_remove("TMUX")
        .env_remove("ZELLIJ")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_control_mouse_wheel_scrolls_viewport_when_mouse_tracking_is_off() {
        assert_eq!(
            tmux_control_mouse_wheel_action(false),
            TmuxControlMouseWheelAction::ScrollViewport
        );
    }

    #[test]
    fn tmux_control_mouse_wheel_encodes_mouse_when_mouse_tracking_is_on() {
        assert_eq!(
            tmux_control_mouse_wheel_action(true),
            TmuxControlMouseWheelAction::EncodeMouse
        );
    }

    #[test]
    fn native_control_command_removes_nested_mux_environment() {
        let target = MuxPaneTarget::Session {
            session_id: "bootty-session".to_owned(),
            cwd: None,
        };
        let mut command = Command::new("tmux");
        configure_native_control_command(&mut command, &target);

        let removals: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .collect();

        assert!(removals.iter().any(|(key, _)| *key == "TMUX"));
        assert!(removals.iter().any(|(key, _)| *key == "ZELLIJ"));
    }

    #[test]
    fn native_control_command_declares_tmux_client_features() {
        let target = MuxPaneTarget::Session {
            session_id: "bootty-session".to_owned(),
            cwd: None,
        };
        let mut command = Command::new("tmux");
        configure_native_control_command(&mut command, &target);

        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            args,
            vec![
                "-C",
                "-T",
                TMUX_CLIENT_FEATURES,
                "attach-session",
                "-t",
                "bootty-session",
            ]
        );
    }

    #[test]
    fn tmux_control_publish_policy_suppresses_synchronized_output() {
        assert!(!should_publish_tmux_control_frame(
            true,
            true,
            0,
            TMUX_CONTROL_SETTLED_FRAME_DELAY,
            TMUX_CONTROL_BACKLOG_FRAME_INTERVAL,
        ));
    }

    #[test]
    fn tmux_control_publish_policy_delays_backlogged_partial_frames() {
        assert!(!should_publish_tmux_control_frame(
            true,
            false,
            4096,
            Duration::ZERO,
            TMUX_CONTROL_READY_FRAME_INTERVAL,
        ));
        assert!(should_publish_tmux_control_frame(
            true,
            false,
            4096,
            Duration::ZERO,
            TMUX_CONTROL_BACKLOG_FRAME_INTERVAL,
        ));
    }

    #[test]
    fn tmux_control_publish_policy_waits_for_quiet_settle_without_backlog() {
        assert!(!should_publish_tmux_control_frame(
            true,
            false,
            0,
            Duration::ZERO,
            TMUX_CONTROL_READY_FRAME_INTERVAL,
        ));
        assert!(should_publish_tmux_control_frame(
            true,
            false,
            0,
            TMUX_CONTROL_READY_FRAME_INTERVAL,
            Duration::ZERO,
        ));
    }
}
