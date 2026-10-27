//! Dictation signed in with the Codex account already on this machine, or on device without one,
//! and the media for voice calls a Codex conversation makes, using Pi's bundled native audio host.
mod account;
#[cfg(target_os = "macos")]
mod on_device;
mod transport;

use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    process::{Child, Command, Stdio},
    sync::mpsc,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use bootty_host::speech::{SpeechCommand, SpeechEvent};
use serde_json::{Value, json};

const FRAME_LIMIT: u64 = 320 * 1024;

/// What speech this machine offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpeechAvailability {
    /// The Codex sign-in can dictate. Conversation calls authenticate through Codex separately.
    pub codex: bool,
    /// This Mac can dictate on device.
    pub on_device: bool,
}

impl SpeechAvailability {
    #[must_use]
    pub const fn dictation(self) -> bool {
        self.codex || self.on_device
    }
}

/// What the Codex sign-in in `directory` (or the default Codex home) and this machine offer.
/// It never prompts for permission.
#[must_use]
pub fn availability(directory: Option<&std::path::Path>) -> SpeechAvailability {
    SpeechAvailability {
        codex: account::signed_in(directory),
        on_device: on_device_available(),
    }
}

#[cfg(target_os = "macos")]
fn on_device_available() -> bool {
    on_device::available()
}

/// Deliberate limit: on-device dictation uses Apple's recognizer. Add a local model here when
/// Windows and Linux need dictation without Codex.
#[cfg(not(target_os = "macos"))]
const fn on_device_available() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn on_device_dictation(context: &VoiceContext) -> Result<transport::Session, String> {
    transport::Session::on_device(context)
}

#[cfg(not(target_os = "macos"))]
fn on_device_dictation(_context: &VoiceContext) -> Result<transport::Session, String> {
    Err("On-device dictation needs macOS".into())
}

/// Owns the bounded framed input and command output of an audio host.
pub struct AudioHost {
    child: Option<Child>,
    input: Box<dyn std::io::Write + Send>,
    events: mpsc::Receiver<Result<Value, String>>,
}

impl AudioHost {
    fn start(command: &Mutex<Command>) -> Result<Self, String> {
        let mut child = command
            .lock()
            .map_err(|_| "The audio host is unavailable")?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "The bundled audio host could not start")?;
        let Some((input, output)) = child.stdin.take().zip(child.stdout.take()) else {
            _ = child.kill();
            _ = child.wait();
            return Err("The audio host transport is unavailable".into());
        };
        let mut host = Self::from_io(BufReader::new(output), input);
        host.child = Some(child);
        Ok(host)
    }

    /// Attaches a framed audio transport on the thread that owns its speech session.
    /// Input frames wake that owner; output commands retain the audio host protocol.
    #[must_use]
    pub fn from_io(
        input: impl std::io::BufRead + Send + 'static,
        output: impl std::io::Write + Send + 'static,
    ) -> Self {
        let (sender, events) = mpsc::sync_channel(64);
        let wake = thread::current();
        thread::spawn(move || read_frames(input, sender, &wake));
        Self {
            child: None,
            input: Box::new(output),
            events,
        }
    }

    fn send(&mut self, value: &Value) -> Result<(), String> {
        serde_json::to_writer(&mut self.input, value)
            .and_then(|()| self.input.write_all(b"\n").map_err(serde_json::Error::io))
            .and_then(|()| self.input.flush().map_err(serde_json::Error::io))
            .map_err(|_| "The audio host disconnected".into())
    }
}

impl Drop for AudioHost {
    fn drop(&mut self) {
        _ = self.send(&json!({"type":"shutdown"}));
        if let Some(child) = &mut self.child {
            _ = child.kill();
            _ = child.wait();
        }
    }
}

fn read_frames(
    mut reader: impl std::io::BufRead,
    sender: mpsc::SyncSender<Result<Value, String>>,
    wake: &thread::Thread,
) {
    loop {
        let mut frame = Vec::new();
        let result = match (&mut reader)
            .take(FRAME_LIMIT.saturating_add(1))
            .read_until(b'\n', &mut frame)
        {
            Ok(0) => break,
            Ok(size) if u64::try_from(size).unwrap_or(u64::MAX) <= FRAME_LIMIT => {
                serde_json::from_slice(&frame).map_err(|_| "Invalid voice frame".into())
            }
            _ => Err("Oversized voice frame".into()),
        };
        let failed = result.is_err();
        let disconnected = sender.send(result).is_err();
        wake.unpark();
        if disconnected || failed {
            break;
        }
    }
    drop(sender);
    wake.unpark();
}

type Publisher = Arc<dyn Fn(&SpeechEvent) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
struct VoiceContext {
    audio_host: Arc<dyn Fn() -> Result<AudioHost, String> + Send + Sync>,
    publish: Publisher,
}

/// Own a bounded stdio session. Credentials never enter the GUI or helper arguments.
/// # Errors
/// Returns transport failures after releasing recording and playback.
pub fn run() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let audio_host = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("The audio host is unavailable"))?
        .join(if cfg!(windows) {
            "voice-host.exe"
        } else {
            "voice-host"
        });
    run_with_io(
        BufReader::new(std::io::stdin()),
        std::io::stdout(),
        Command::new(audio_host),
    )
}

/// Own a bounded speech protocol using the supplied input, output, and bundled audio host.
/// # Errors
/// Returns transport failures after releasing recording and playback.
pub fn run_with_io(
    input: impl std::io::BufRead + Send + 'static,
    output: impl std::io::Write + Send + 'static,
    audio_host: Command,
) -> anyhow::Result<()> {
    let command = Mutex::new(audio_host);
    run_with_audio(input, output, move || AudioHost::start(&command))
}

/// Own a speech protocol with an audio transport factory. Each recording or call owns its host.
/// # Errors
/// Returns transport failures after releasing recording and playback.
pub fn run_with_audio(
    input: impl std::io::BufRead + Send + 'static,
    output: impl std::io::Write + Send + 'static,
    audio_host: impl Fn() -> Result<AudioHost, String> + Send + Sync + 'static,
) -> anyhow::Result<()> {
    let output = Arc::new(Mutex::new(output));
    let publish: Publisher = Arc::new(move |event| {
        let mut output = output.lock().map_err(|_| "The composer disconnected")?;
        serde_json::to_writer(&mut *output, event)
            .and_then(|()| output.write_all(b"\n").map_err(serde_json::Error::io))
            .and_then(|()| output.flush().map_err(serde_json::Error::io))
            .map_err(|_| "The composer disconnected".into())
    });
    let context = VoiceContext {
        audio_host: Arc::new(audio_host),
        publish: Arc::clone(&publish),
    };
    _ = rustls::crypto::ring::default_provider().install_default();
    let (sender, requests) = mpsc::sync_channel(8);
    let wake = thread::current();
    thread::spawn(move || read_frames(input, sender, &wake));
    publish(&SpeechEvent::Ready).map_err(anyhow::Error::msg)?;
    let mut account = None;
    let mut active: Option<transport::Session> = None;
    loop {
        let command = match requests.try_recv() {
            Ok(value) => Some(value.and_then(|value| {
                serde_json::from_value::<SpeechCommand>(value)
                    .map_err(|_| "Invalid speech command".into())
            })),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => break,
        };
        let received = command.is_some();
        let result = (|| {
            if let Some(command) = command {
                match command? {
                    SpeechCommand::Account {
                        provider: _,
                        program: _,
                        directory,
                    } => {
                        account = Some(account::Account { directory });
                    }
                    SpeechCommand::Listen { voice } => {
                        // A voice call stays connected between delegations and replies.
                        if active.as_ref().is_none_or(|session| !session.voice()) {
                            active = None;
                            active = Some(if voice {
                                // The conversation's Codex session authenticates the call.
                                transport::Session::voice_call(&context)?
                            } else {
                                let account = account.as_ref().ok_or(account::SIGN_IN)?;
                                transport::Session::dictation(&account.authenticate()?, &context)?
                            });
                        }
                    }
                    // The composer chose this after finding no Codex sign-in.
                    SpeechCommand::ListenOnDevice => {
                        active = None;
                        active = Some(on_device_dictation(&context)?);
                    }
                    SpeechCommand::Stop => {
                        if let Some(session) = active.as_mut() {
                            session.finish()?;
                        }
                    }
                    SpeechCommand::Answer { sdp } => {
                        active
                            .as_mut()
                            .ok_or("There is no voice call to answer")?
                            .answer(&sdp)?;
                    }
                }
            }
            if let Some(session) = active.as_mut() {
                session.poll()?;
                if session.closed() {
                    active = None;
                }
            }
            Ok::<_, String>(())
        })();
        if let Err(message) = result {
            active = None;
            publish(&SpeechEvent::Error { message }).map_err(anyhow::Error::msg)?;
            publish(&SpeechEvent::Stopped).map_err(anyhow::Error::msg)?;
        }
        if received {
            continue;
        }
        // Calls and idle sessions advance only on input or media. Dictation also polls
        // its network socket and platform recognizer, which do not share this wakeup.
        if active.as_ref().is_some_and(|session| !session.voice()) {
            thread::park_timeout(Duration::from_millis(10));
        } else {
            thread::park();
        }
    }
    Ok(())
}
