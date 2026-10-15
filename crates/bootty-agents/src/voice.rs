//! Dictation signed in with the Codex account already on this machine, or on device without one,
//! and the media for voice calls a Codex conversation makes, using Pi's bundled native audio host.
mod account;
#[cfg(target_os = "macos")]
mod on_device;
mod transport;

use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
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
fn on_device_dictation() -> Result<transport::Session, String> {
    transport::Session::on_device()
}

#[cfg(not(target_os = "macos"))]
fn on_device_dictation() -> Result<transport::Session, String> {
    Err("On-device dictation needs macOS".into())
}

struct AudioHost {
    child: Child,
    input: ChildStdin,
    events: mpsc::Receiver<Result<Value, String>>,
}

impl AudioHost {
    fn start() -> Result<Self, String> {
        let executable = std::env::current_exe().map_err(|_| "The audio host is unavailable")?;
        let path = executable
            .parent()
            .ok_or("The audio host is unavailable")?
            .join(if cfg!(windows) {
                "voice-host.exe"
            } else {
                "voice-host"
            });
        let mut child = Command::new(path)
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
        let (sender, events) = mpsc::sync_channel(64);
        thread::spawn(move || read_frames(BufReader::new(output), &sender));
        Ok(Self {
            child,
            input,
            events,
        })
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
        _ = self.child.kill();
        _ = self.child.wait();
    }
}

fn read_frames(
    mut reader: impl std::io::BufRead,
    sender: &mpsc::SyncSender<Result<Value, String>>,
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
        if sender.send(result).is_err() || failed {
            break;
        }
    }
}

fn publish(event: &SpeechEvent) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, event)
        .and_then(|()| stdout.write_all(b"\n").map_err(serde_json::Error::io))
        .and_then(|()| stdout.flush().map_err(serde_json::Error::io))
        .map_err(|_| "The composer disconnected".into())
}

/// Own a bounded stdio session. Credentials never enter the GUI or helper arguments.
/// # Errors
/// Returns transport failures after releasing recording and playback.
pub fn run() -> anyhow::Result<()> {
    _ = rustls::crypto::ring::default_provider().install_default();
    let (sender, requests) = mpsc::sync_channel(8);
    thread::spawn(move || read_frames(std::io::stdin().lock(), &sender));
    publish(&SpeechEvent::Ready).map_err(anyhow::Error::msg)?;
    let mut account = None;
    let mut active: Option<transport::Session> = None;
    loop {
        let command = match requests.recv_timeout(Duration::from_millis(10)) {
            Ok(value) => Some(value.and_then(|value| {
                serde_json::from_value::<SpeechCommand>(value)
                    .map_err(|_| "Invalid speech command".into())
            })),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
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
                                transport::Session::voice_call()?
                            } else {
                                let account = account.as_ref().ok_or(account::SIGN_IN)?;
                                transport::Session::dictation(&account.authenticate()?)?
                            });
                        }
                    }
                    // The composer chose this after finding no Codex sign-in.
                    SpeechCommand::ListenOnDevice => {
                        active = None;
                        active = Some(on_device_dictation()?);
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
    }
    Ok(())
}
