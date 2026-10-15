//! `AppKit` status-item scenes can deadlock. Their main loop must not own a workspace window.
use super::Snapshot;
use anyhow::{Context as _, Result};
use std::{
    io::{self, BufReader, Write as _},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub(super) struct Backend {
    latest: Arc<Mutex<Snapshot>>,
    wake: mpsc::SyncSender<()>,
}

impl Backend {
    pub(super) fn new(snapshot: Snapshot) -> Result<Self, String> {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let latest = Arc::new(Mutex::new(snapshot));
        let state = latest.clone();
        let (wake, updates) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("agent-tray".to_owned())
            .spawn(move || {
                if let Err(error) = host(&executable, &state, &updates) {
                    eprintln!("Agent tray unavailable: {error:#}");
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { latest, wake })
    }

    pub(super) fn update(&self, snapshot: Snapshot) {
        *self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot;
        let _ = self.wake.try_send(());
    }
}

struct Helper(Child);
impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn host(
    executable: &std::path::Path,
    state: &Mutex<Snapshot>,
    updates: &mpsc::Receiver<()>,
) -> Result<()> {
    let mut helper = Helper(
        Command::new(executable)
            .arg("--agent-tray-stdio")
            .env_remove("CFProcessPath")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("start agent tray helper")?,
    );
    let mut input = helper.0.stdin.take().context("agent tray helper input")?;
    let output = helper.0.stdout.take().context("agent tray helper output")?;
    std::thread::Builder::new()
        .name("agent-tray-events".to_owned())
        .spawn(move || {
            let mut output = BufReader::new(output);
            while let Ok(Some(message)) =
                bootty_host::private_stdio::read_message(&mut output, MAX_MESSAGE_BYTES)
            {
                let Ok(id) = serde_json::from_slice::<String>(&message) else {
                    break;
                };
                super::dispatch(&id);
            }
        })?;
    loop {
        let snapshot = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut message = serde_json::to_vec(&snapshot)?;
        message.push(b'\n');
        anyhow::ensure!(
            message.len() <= MAX_MESSAGE_BYTES,
            "Agent tray snapshot is too large"
        );
        input.write_all(&message)?;
        input.flush()?;
        loop {
            match updates.recv_timeout(Duration::from_millis(250)) {
                Ok(()) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(status) = helper.0.try_wait()? {
                        anyhow::bail!("Agent tray helper exited: {status}");
                    }
                }
            }
        }
    }
}

/// Run the private macOS tray process without configuration, terminals or a control endpoint.
///
/// # Errors
/// Returns errors starting the bounded stdio readers and native tray event loop.
#[expect(
    clippy::exit,
    reason = "This stateless executable entry point must terminate on parent EOF even when AppKit is deadlocked."
)]
pub fn run_helper() -> Result<()> {
    let latest = Arc::new(Mutex::new(None));
    let state = latest.clone();
    let (wake, updates) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("agent-tray-input".to_owned())
        .spawn(move || {
            let result = read_snapshots(&state, &wake);
            if let Err(error) = &result {
                eprintln!("Agent tray input failed: {error:#}");
            }
            // EOF ends this stateless helper even if AppKit has blocked its main thread.
            std::process::exit(i32::from(result.is_err()));
        })?;
    let (events, clicks) = mpsc::sync_channel::<String>(128);
    std::thread::Builder::new()
        .name("agent-tray-output".to_owned())
        .spawn(move || {
            let stdout = io::stdout();
            let mut output = stdout.lock();
            for id in clicks {
                if serde_json::to_writer(&mut output, &id).is_err()
                    || output.write_all(b"\n").is_err()
                    || output.flush().is_err()
                {
                    break;
                }
            }
        })?;
    tray_icon::menu::MenuEvent::set_event_handler(Some(
        move |event: tray_icon::menu::MenuEvent| {
            let _ = events.try_send(event.id.0);
        },
    ));
    let application =
        gpui_kit::Application::with_platform(gpui_kit::platform::current_platform(false));
    application.run(move |cx| {
        if let Some(mtm) = objc2_foundation::MainThreadMarker::new() {
            objc2_app_kit::NSApplication::sharedApplication(mtm)
                .setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
        }
        cx.spawn(async move |_| {
            let mut tray = None;
            while updates.recv().await.is_ok() {
                let snapshot = latest
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                let Some(snapshot) = snapshot else { continue };
                if let Some(tray) = &mut tray {
                    super::native::Backend::update(tray, snapshot);
                } else {
                    match super::native::Backend::new(snapshot) {
                        Ok(backend) => tray = Some(backend),
                        Err(error) => {
                            eprintln!("Agent tray unavailable: {error}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        })
        .detach();
    });
    Ok(())
}

fn read_snapshots(state: &Mutex<Option<Snapshot>>, wake: &async_channel::Sender<()>) -> Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    while let Some(message) =
        bootty_host::private_stdio::read_message(&mut input, MAX_MESSAGE_BYTES)?
    {
        *state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(serde_json::from_slice(&message)?);
        let _ = wake.try_send(());
    }
    Ok(())
}
