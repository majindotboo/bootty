//! A bounded local speech helper. The worker owns the process; views own only its control channel.
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead as _, BufReader, Write as _},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

const FRAME_LIMIT: u64 = 65_536;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SpeechEvent {
    Ready,
    Listening,
    Stopped,
    Closed,
    Partial {
        text: String,
    },
    Final {
        text: String,
    },
    /// How loud the microphone was over the last 80 ms: the RMS of its 16-bit samples.
    Level {
        rms: u16,
    },
    /// Voice mode's WebRTC offer, for the conversation to take to its provider.
    Offer {
        sdp: String,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SpeechCommand {
    Account {
        provider: String,
        program: String,
        directory: Option<String>,
    },
    Listen {
        voice: bool,
    },
    /// Dictate with this machine's own recognizer, for a composer without a Codex sign-in.
    ListenOnDevice,
    Stop,
    /// The provider's answer to the voice mode offer.
    Answer {
        sdp: String,
    },
}

/// The helper's stdio streams and worker-owned teardown.
pub struct SpeechPeer {
    input: Box<dyn std::io::Write + Send>,
    output: Box<dyn std::io::Read + Send>,
    close: CloseOnDrop,
}

impl SpeechPeer {
    /// Capture streams and their teardown, which runs once when the peer is released.
    #[must_use]
    pub fn new(
        input: impl std::io::Write + Send + 'static,
        output: impl std::io::Read + Send + 'static,
        close: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            input: Box::new(input),
            output: Box::new(output),
            close: CloseOnDrop(Some(Box::new(close))),
        }
    }

    /// Start the selected local process with private stdio pipes.
    /// # Errors
    /// Returns spawn or missing-pipe errors after reaping any started process.
    pub fn spawn(program: PathBuf) -> anyhow::Result<Self> {
        let mut child = Command::new(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let Some((input, output)) = child.stdin.take().zip(child.stdout.take()) else {
            _ = child.kill();
            _ = child.wait();
            anyhow::bail!("The speech helper transport is unavailable");
        };
        Ok(Self::new(input, output, move || {
            _ = child.kill();
            _ = child.wait();
        }))
    }
}

struct CloseOnDrop(Option<Box<dyn FnOnce() + Send>>);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Some(close) = self.0.take() {
            close();
        }
    }
}

pub struct SpeechSession {
    commands: mpsc::SyncSender<SpeechCommand>,
    closing: Arc<AtomicBool>,
}

impl SpeechSession {
    /// Start an explicitly selected local helper without blocking the calling thread.
    #[must_use]
    pub fn start(program: PathBuf) -> (Self, tokio::sync::mpsc::Receiver<SpeechEvent>) {
        Self::start_with(program, SpeechPeer::spawn)
    }

    /// Start the helper using the selected stdio peer constructor.
    #[must_use]
    pub fn start_with(
        program: PathBuf,
        launcher: impl FnOnce(PathBuf) -> anyhow::Result<SpeechPeer> + Send + 'static,
    ) -> (Self, tokio::sync::mpsc::Receiver<SpeechEvent>) {
        let (commands, requests) = mpsc::sync_channel::<SpeechCommand>(8);
        let (events, receiver) = tokio::sync::mpsc::channel(32);
        let closing = Arc::new(AtomicBool::new(false));
        let worker_closing = Arc::clone(&closing);
        thread::spawn(move || {
            if let Err(error) =
                launcher(program).map(|peer| run(peer, requests, &events, worker_closing))
            {
                _ = events.blocking_send(SpeechEvent::Error {
                    message: error.to_string(),
                });
                _ = events.blocking_send(SpeechEvent::Closed);
            }
        });
        (Self { commands, closing }, receiver)
    }

    /// # Errors
    /// Rejects an oversized voice answer, stopped helpers, or a full command queue.
    pub fn send(&self, command: SpeechCommand) -> Result<(), String> {
        // A helper frame is at most 64 KiB; an answer stays well under it.
        if matches!(&command, SpeechCommand::Answer { sdp } if sdp.len() > 32_768) {
            return Err("The voice answer exceeds 32 KiB".into());
        }
        self.commands
            .try_send(command)
            .map_err(|_| "The speech helper is unavailable".into())
    }
}

impl Drop for SpeechSession {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
    }
}

fn run(
    peer: SpeechPeer,
    requests: mpsc::Receiver<SpeechCommand>,
    events: &tokio::sync::mpsc::Sender<SpeechEvent>,
    closing: Arc<AtomicBool>,
) {
    let SpeechPeer {
        mut input,
        output,
        close: _close,
    } = peer;
    let published = events.clone();
    thread::spawn(move || {
        let mut output = BufReader::new(output);
        let mut reported_error = false;
        loop {
            let mut frame = Vec::new();
            let size = std::io::Read::take(&mut output, FRAME_LIMIT.saturating_add(1))
                .read_until(b'\n', &mut frame);
            let event = match size {
                Ok(0) => break,
                Ok(size) if u64::try_from(size).unwrap_or(u64::MAX) <= FRAME_LIMIT => {
                    serde_json::from_slice(&frame)
                }
                _ => {
                    reported_error = true;
                    _ = published.blocking_send(SpeechEvent::Error {
                        message: "The speech helper returned an invalid frame".into(),
                    });
                    break;
                }
            };
            if let Ok(event) = event {
                if published.blocking_send(event).is_err() {
                    break;
                }
            } else {
                reported_error = true;
                _ = published.blocking_send(SpeechEvent::Error {
                    message: "The speech helper returned an invalid event".into(),
                });
                break;
            }
        }
        if !reported_error && !closing.load(Ordering::Acquire) {
            _ = published.blocking_send(SpeechEvent::Error {
                message: "The local speech helper stopped unexpectedly".into(),
            });
        }
        _ = published.blocking_send(SpeechEvent::Closed);
    });
    for request in requests {
        if serde_json::to_writer(&mut input, &request)
            .and_then(|()| input.write_all(b"\n").map_err(serde_json::Error::io))
            .and_then(|()| input.flush().map_err(serde_json::Error::io))
            .is_err()
        {
            break;
        }
    }
    // Last control handle dropped: close recording and playback, then reap off the UI thread.
    drop(input);
}
