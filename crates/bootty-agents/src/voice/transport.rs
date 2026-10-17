//! Dictation over `OpenAI` Realtime transcription or on device, and voice mode's WebRTC media for
//! a call the conversation's Codex session makes. Microphone and speaker stay in the native audio
//! host.
use super::{AudioHost, account::Auth, publish};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_host::speech::SpeechEvent;
use serde_json::{Value, json};
use std::{
    net::TcpStream,
    sync::mpsc,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, client::IntoClientRequest as _, stream::MaybeTlsStream};

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Where the microphone's speech goes.
enum Sink {
    /// A voice call; media flows over WebRTC to the call Codex made.
    Call,
    OpenAi(Box<Socket>),
    #[cfg(target_os = "macos")]
    OnDevice(super::on_device::Recognition),
}

pub(super) struct Session {
    audio: AudioHost,
    sink: Sink,
    closed: bool,
    audio_bytes: usize,
    loudness: Loudness,
    deadline: Option<Instant>,
}

/// The microphone's loudness over successive 80 ms windows.
#[derive(Default)]
struct Loudness {
    squares: u64,
    samples: u64,
}

impl Loudness {
    /// 80 ms of 24 kHz mono audio.
    const WINDOW: u64 = 1_920;

    /// Adds a frame of little-endian 16-bit samples, and gives the window's RMS once it fills.
    fn add(&mut self, frame: &[u8]) -> Option<u16> {
        for &sample in frame.as_chunks::<2>().0 {
            let value = u64::from(i16::from_le_bytes(sample).unsigned_abs());
            self.squares = self.squares.saturating_add(value.saturating_mul(value));
            self.samples = self.samples.saturating_add(1);
        }
        if self.samples < Self::WINDOW {
            return None;
        }
        let rms = self.squares.checked_div(self.samples).unwrap_or(0).isqrt();
        *self = Self::default();
        Some(u16::try_from(rms).unwrap_or(u16::MAX))
    }
}

impl Session {
    pub fn dictation(auth: &Auth) -> Result<Self, String> {
        let socket = dictation_socket(auth)?;
        Self::start(Sink::OpenAi(Box::new(socket)), "start_dictation")
    }

    #[cfg(target_os = "macos")]
    pub fn on_device() -> Result<Self, String> {
        let recognition = super::on_device::Recognition::start()?;
        Self::start(Sink::OnDevice(recognition), "start_dictation")
    }

    /// Codex makes the voice call and answers this offer; Codex then listens, delegates to the
    /// conversation's agent and speaks its replies.
    pub fn voice_call() -> Result<Self, String> {
        Self::start(Sink::Call, "start_v3")
    }

    fn start(sink: Sink, mode: &str) -> Result<Self, String> {
        let mut audio = AudioHost::start()?;
        audio.send(&json!({"type":mode}))?;
        Ok(Self {
            audio,
            sink,
            closed: false,
            audio_bytes: 0,
            loudness: Loudness::default(),
            deadline: None,
        })
    }

    pub const fn voice(&self) -> bool {
        matches!(self.sink, Sink::Call)
    }
    pub const fn closed(&self) -> bool {
        self.closed
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.audio.send(&json!({"type":"stop"}))?;
        if self.voice() {
            self.closed = true;
            return publish(&SpeechEvent::Stopped);
        }
        if self.audio_bytes < 4800 {
            return Err(
                "No microphone audio was received. Check the selected input device.".into(),
            );
        }
        match &mut self.sink {
            Sink::Call => {}
            Sink::OpenAi(socket) => socket
                .send(Message::Text(
                    json!({"type":"input_audio_buffer.commit"})
                        .to_string()
                        .into(),
                ))
                .map_err(|_| "OpenAI dictation disconnected")?,
            #[cfg(target_os = "macos")]
            Sink::OnDevice(recognition) => recognition.finish(),
        }
        self.deadline = Instant::now().checked_add(Duration::from_secs(10));
        Ok(())
    }

    pub fn answer(&mut self, sdp: &str) -> Result<(), String> {
        if !self.voice() {
            return Err("There is no voice call to answer".into());
        }
        self.audio.send(&json!({"type":"apply_answer","sdp":sdp}))
    }

    pub fn poll(&mut self) -> Result<(), String> {
        // Drain captured media before waiting on network input; both queues are bounded.
        loop {
            match self.audio.events.try_recv() {
                Ok(event) => self.media(&event?)?,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("The audio host disconnected".into());
                }
            }
        }
        match &mut self.sink {
            Sink::Call => return Ok(()),
            Sink::OpenAi(_) => self.read_openai()?,
            #[cfg(target_os = "macos")]
            Sink::OnDevice(_) => self.read_on_device()?,
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err("Dictation transcription timed out".into());
        }
        Ok(())
    }

    /// Publish the recognizer's transcripts; the final one ends the dictation.
    #[cfg(target_os = "macos")]
    fn read_on_device(&mut self) -> Result<(), String> {
        use super::on_device::Heard;
        let Sink::OnDevice(recognition) = &self.sink else {
            return Ok(());
        };
        while let Some(heard) = recognition.heard() {
            match heard {
                Heard::Partial(text) => publish(&SpeechEvent::Partial { text })?,
                Heard::Final(text) => return self.transcribed(&text),
                Heard::Failed(message) => return Err(message),
            }
        }
        Ok(())
    }

    fn transcribed(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("No speech was detected. Try dictating again.".into());
        }
        if text.len() > 32_768 {
            return Err("The transcript exceeds its size limit".into());
        }
        publish(&SpeechEvent::Final { text: text.into() })?;
        publish(&SpeechEvent::Stopped)?;
        self.closed = true;
        Ok(())
    }

    fn read_openai(&mut self) -> Result<(), String> {
        let Sink::OpenAi(socket) = &mut self.sink else {
            return Ok(());
        };
        match socket.read() {
            Ok(Message::Text(text)) => {
                if text.len() > 72 * 1024 {
                    return Err("OpenAI returned an oversized dictation event".into());
                }
                let event: Value =
                    serde_json::from_str(&text).map_err(|_| "Invalid OpenAI dictation event")?;
                match event.get("type").and_then(Value::as_str) {
                    Some("error") => {
                        return Err("OpenAI dictation failed. Retry the recording.".into());
                    }
                    Some("conversation.item.input_audio_transcription.delta") => {
                        if let Some(text) = event.get("delta").and_then(Value::as_str) {
                            publish(&SpeechEvent::Partial { text: text.into() })?;
                        }
                    }
                    Some(
                        "conversation.item.input_audio_transcription.completed"
                        | "input_audio_transcription.completed",
                    ) => {
                        let text = event
                            .get("transcript")
                            .and_then(Value::as_str)
                            .ok_or("Invalid OpenAI transcript")?
                            .to_owned();
                        self.transcribed(&text)?;
                    }
                    _ => {}
                }
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Ok(Message::Close(_)) | Err(_) => return Err("OpenAI dictation disconnected".into()),
            _ => {}
        }
        Ok(())
    }

    fn media(&mut self, event: &Value) -> Result<(), String> {
        match event.get("type").and_then(Value::as_str) {
            Some("ready") if event.get("version").and_then(Value::as_u64) != Some(6) => {
                Err("The audio host protocol is incompatible".into())
            }
            Some("offer") => {
                let sdp = event
                    .get("sdp")
                    .and_then(Value::as_str)
                    .filter(|sdp| sdp.len() <= 32_768)
                    .ok_or("Invalid audio host offer")?;
                publish(&SpeechEvent::Offer { sdp: sdp.into() })
            }
            Some("pcm") if self.deadline.is_none() => {
                if event.get("sample_rate").and_then(Value::as_u64) != Some(24_000)
                    || event.get("num_channels").and_then(Value::as_u64) != Some(1)
                {
                    return Err("Invalid microphone audio format".into());
                }
                let audio = event
                    .get("audio")
                    .and_then(Value::as_str)
                    .ok_or("Invalid microphone audio")?;
                if audio.len() > 90_000 {
                    return Err("Oversized microphone audio frame".into());
                }
                let bytes = STANDARD
                    .decode(audio)
                    .map_err(|_| "Invalid microphone audio frame")?;
                if bytes.len() > 65_536 {
                    return Err("Oversized microphone audio frame".into());
                }
                self.audio_bytes = self.audio_bytes.saturating_add(bytes.len());
                if let Some(rms) = self.loudness.add(&bytes) {
                    publish(&SpeechEvent::Level { rms })?;
                }
                match &mut self.sink {
                    Sink::Call => Ok(()),
                    Sink::OpenAi(socket) => socket
                        .send(Message::Text(
                            json!({"type":"input_audio_buffer.append","audio":audio})
                                .to_string()
                                .into(),
                        ))
                        .map_err(|_| "OpenAI dictation disconnected".into()),
                    #[cfg(target_os = "macos")]
                    Sink::OnDevice(recognition) => recognition.append(&bytes),
                }
            }
            Some("state") => self.media_state(event.get("state").and_then(Value::as_str)),
            // Protocol 6 encodes recoverable CPAL Xruns as errors. Use a typed severity when
            // the audio host exposes one; an output glitch must not tear down a healthy call.
            Some("error")
                if self.voice()
                    && event.get("message").and_then(Value::as_str)
                        == Some("speaker stream error: A buffer underrun or overrun occurred.") =>
            {
                eprintln!("Voice speaker buffer underrun; continuing the audio stream.");
                Ok(())
            }
            Some("error") => Err(format!(
                "Voice audio host: {}",
                media_error(
                    event.get("message").and_then(Value::as_str),
                    "The microphone or speaker is unavailable"
                )
            )),
            Some("data")
                if event.pointer("/message/type").and_then(Value::as_str) == Some("error") =>
            {
                Err(format!(
                    "Voice provider: {}",
                    media_error(
                        event
                            .pointer("/message/error/message")
                            .and_then(Value::as_str),
                        "The voice service rejected the call"
                    )
                ))
            }
            _ => Ok(()),
        }
    }

    fn media_state(&mut self, state: Option<&str>) -> Result<(), String> {
        match state {
            Some("connected" | "recording" | "listening") => publish(&SpeechEvent::Listening),
            Some("failed") if self.voice() => {
                Err("Voice connection failed. Check your network and try again.".into())
            }
            Some("disconnected") if self.voice() => {
                Err("Voice connection was lost. Check your network and try again.".into())
            }
            Some("closed") if self.voice() => {
                self.closed = true;
                publish(&SpeechEvent::Closed)
            }
            _ => Ok(()),
        }
    }
}

fn media_error(message: Option<&str>, fallback: &str) -> String {
    let message = message
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(fallback);
    // Deliberate limit: 8192 chars fit the 64 KiB speech frame after JSON escaping.
    // Grow both budgets together if the host needs larger diagnostics.
    let mut characters = message.chars();
    let mut text = characters.by_ref().take(8192).collect::<String>();
    if characters.next().is_some() {
        text.push_str("\n[Audio host diagnostic truncated]");
    }
    text
}

fn dictation_socket(auth: &Auth) -> Result<Socket, String> {
    let mut request = "wss://api.openai.com/v1/realtime?intent=transcription"
        .into_client_request()
        .map_err(|_| "Could not prepare OpenAI dictation")?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", auth.token)
            .parse()
            .map_err(|_| "The Codex sign-in is invalid")?,
    );
    // A subscription token is only valid together with its ChatGPT account.
    if let Some(account) = &auth.account_id {
        request.headers_mut().insert(
            "chatgpt-account-id",
            account
                .parse()
                .map_err(|_| "The Codex sign-in is invalid")?,
        );
    }
    let (mut socket, _) = tungstenite::client::connect_with_config(
        request,
        Some(
            tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(Some(72 * 1024))
                .max_frame_size(Some(72 * 1024)),
        ),
        0,
    )
    .map_err(|_| "Dictation could not connect. Check the Codex sign-in.")?;
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => stream.get_mut(),
        _ => return Err("OpenAI dictation requires TLS".into()),
    };
    stream
        .set_read_timeout(Some(Duration::from_millis(20)))
        .map_err(|_| "Could not configure OpenAI dictation")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| "Could not configure OpenAI dictation")?;
    socket
        .send(Message::Text(
            json!({"type":"session.update","session":{"type":"transcription","audio":{"input":{
                "format":{"type":"audio/pcm","rate":24000},"noise_reduction":{"type":"near_field"},
                "transcription":{"model":"gpt-4o-mini-transcribe"},"turn_detection":null
            }}}})
            .to_string()
            .into(),
        ))
        .map_err(|_| "OpenAI dictation disconnected")?;
    Ok(socket)
}
