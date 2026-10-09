//! The public `OpenAI` Realtime protocol; microphone and speaker media stay in the native audio host.
use super::{AudioHost, account::Auth, publish};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_host::speech::SpeechEvent;
use serde_json::{Value, json};
use std::{
    io::Read as _,
    net::TcpStream,
    sync::mpsc,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, client::IntoClientRequest as _, stream::MaybeTlsStream};

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

pub(super) struct Session {
    audio: AudioHost,
    auth: Auth,
    socket: Option<Socket>,
    voice: bool,
    closed: bool,
    audio_bytes: usize,
    deadline: Option<Instant>,
    delegation: Option<String>,
}

impl Session {
    pub fn start(auth: Auth, voice: bool) -> Result<Self, String> {
        let socket = if voice {
            None
        } else {
            Some(dictation_socket(&auth)?)
        };
        let mut audio = AudioHost::start()?;
        audio.send(&json!({"type":if voice {"start_v3"} else {"start_dictation"}}))?;
        Ok(Self {
            audio,
            auth,
            socket,
            voice,
            closed: false,
            audio_bytes: 0,
            deadline: None,
            delegation: None,
        })
    }

    pub const fn voice(&self) -> bool {
        self.voice
    }
    pub const fn closed(&self) -> bool {
        self.closed
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.audio.send(&json!({"type":"stop"}))?;
        if self.voice {
            self.closed = true;
            return publish(&SpeechEvent::Stopped);
        }
        if self.audio_bytes < 4800 {
            return Err(
                "No microphone audio was received. Check the selected input device.".into(),
            );
        }
        if let Some(socket) = &mut self.socket {
            socket
                .send(Message::Text(
                    json!({"type":"input_audio_buffer.commit"})
                        .to_string()
                        .into(),
                ))
                .map_err(|_| "OpenAI dictation disconnected")?;
        }
        self.deadline = Instant::now().checked_add(Duration::from_secs(10));
        Ok(())
    }

    pub fn speak(&mut self, text: &str) -> Result<(), String> {
        if !self.voice || text.len() > 32_768 {
            return Err("Invalid voice reply".into());
        }
        let id = self
            .delegation
            .take()
            .ok_or("There is no voice request waiting for a reply")?;
        self.audio.send(&json!({"type":"send_data","message":{
            "type":"conversation.item.create","item":{"type":"function_call_output","call_id":id,"output":text}
        }}))?;
        self.audio
            .send(&json!({"type":"send_data","message":{"type":"response.create"}}))
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
        let Some(socket) = &mut self.socket else {
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
                            .trim();
                        if text.is_empty() {
                            return Err("No speech was detected. Try dictating again.".into());
                        }
                        if text.len() > 32_768 {
                            return Err("OpenAI returned an oversized transcript".into());
                        }
                        publish(&SpeechEvent::Final { text: text.into() })?;
                        publish(&SpeechEvent::Stopped)?;
                        self.closed = true;
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
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err("OpenAI dictation transcription timed out".into());
        }
        Ok(())
    }

    fn media(&mut self, event: &Value) -> Result<(), String> {
        match event.get("type").and_then(Value::as_str) {
            Some("ready") if event.get("version").and_then(Value::as_u64) != Some(6) => {
                Err("The audio host protocol is incompatible".into())
            }
            Some("offer") => {
                let offer = event
                    .get("sdp")
                    .and_then(Value::as_str)
                    .ok_or("Invalid audio host offer")?;
                let answer = voice_answer(&self.auth, offer)?;
                self.audio
                    .send(&json!({"type":"apply_answer","sdp":answer}))
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
                if let Some(socket) = &mut self.socket {
                    socket
                        .send(Message::Text(
                            json!({"type":"input_audio_buffer.append","audio":audio})
                                .to_string()
                                .into(),
                        ))
                        .map_err(|_| "OpenAI dictation disconnected")?;
                }
                Ok(())
            }
            Some("state") => {
                if event
                    .get("state")
                    .and_then(Value::as_str)
                    .is_some_and(|state| matches!(state, "connected" | "recording" | "listening"))
                {
                    publish(&SpeechEvent::Listening)?;
                }
                Ok(())
            }
            Some("data") => self.voice_event(event.get("message").ok_or("Invalid voice event")?),
            Some("error") => Err(
                "The microphone or speaker is unavailable. Check microphone access and retry."
                    .into(),
            ),
            _ => Ok(()),
        }
    }

    fn voice_event(&mut self, event: &Value) -> Result<(), String> {
        match event.get("type").and_then(Value::as_str) {
            Some("response.done") => {
                if event
                    .get("response")
                    .and_then(|response| response.get("status"))
                    .and_then(Value::as_str)
                    != Some("completed")
                {
                    return Err(
                        "OpenAI voice could not finish the response. Retry voice mode.".into(),
                    );
                }
                let output = event
                    .get("response")
                    .and_then(|response| response.get("output"))
                    .and_then(Value::as_array);
                let call = output
                    .into_iter()
                    .flatten()
                    .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"));
                if let Some(call) = call {
                    if self.delegation.is_some()
                        || call.get("name").and_then(Value::as_str) != Some("ask_agent")
                    {
                        return Err("Invalid voice request".into());
                    }
                    let id = call
                        .get("call_id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty() && id.len() <= 256)
                        .ok_or("Invalid voice request")?;
                    let arguments = call
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|args| args.len() <= 65_536)
                        .ok_or("Invalid voice request")?;
                    let arguments: Value =
                        serde_json::from_str(arguments).map_err(|_| "Invalid voice request")?;
                    let text = arguments
                        .get("prompt")
                        .and_then(Value::as_str)
                        .filter(|text| !text.trim().is_empty() && text.len() <= 32_768)
                        .ok_or("Invalid voice request")?;
                    self.delegation = Some(id.into());
                    self.audio
                        .send(&json!({"type":"set_input_muted","muted":true}))?;
                    publish(&SpeechEvent::Final { text: text.into() })
                } else if self.delegation.is_none() {
                    self.audio
                        .send(&json!({"type":"set_input_muted","muted":false}))?;
                    publish(&SpeechEvent::Finished)
                } else {
                    Ok(())
                }
            }
            Some("response.output_audio_transcript.delta") => publish(&SpeechEvent::Speaking),
            Some("error") => Err("OpenAI voice failed. Check the API key and retry.".into()),
            _ => Ok(()),
        }
    }
}

fn dictation_socket(auth: &Auth) -> Result<Socket, String> {
    let mut request = "wss://api.openai.com/v1/realtime?intent=transcription"
        .into_client_request()
        .map_err(|_| "Could not prepare OpenAI dictation")?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", auth.token)
            .parse()
            .map_err(|_| "Invalid OpenAI API key")?,
    );
    let (mut socket, _) = tungstenite::client::connect_with_config(
        request,
        Some(
            tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(Some(72 * 1024))
                .max_frame_size(Some(72 * 1024)),
        ),
        0,
    )
    .map_err(|_| "OpenAI dictation could not connect. Check the OpenAI API key.")?;
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

fn voice_answer(auth: &Auth, sdp: &str) -> Result<String, String> {
    const BOUNDARY: &str = "bootty-realtime-session";
    if sdp.len() > 256 * 1024 {
        return Err("Oversized voice offer".into());
    }
    if sdp.contains(BOUNDARY) {
        return Err("Invalid voice offer".into());
    }
    let session = json!({
        "type":"realtime","model":"gpt-realtime-2.1",
        "instructions":"You are the voice interface for the current Bootty agent conversation. For every user request call ask_agent with a complete, faithful prompt. The agent owns conversation context and tools. Speak its returned result briefly and faithfully; never claim work completed before the agent replies.",
        "audio":{"input":{"transcription":{"model":"gpt-4o-mini-transcribe"}},"output":{"voice":"marin"}},
        "tools":[{"type":"function","name":"ask_agent","description":"Send the user's request to the current agent conversation.","parameters":{"type":"object","properties":{"prompt":{"type":"string"}},"required":["prompt"],"additionalProperties":false}}],
        "tool_choice":"auto"
    });
    let body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"sdp\"\r\n\r\n{sdp}\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"session\"\r\n\r\n{session}\r\n--{BOUNDARY}--\r\n"
    );
    let mut response = ureq::post("https://api.openai.com/v1/realtime/calls")
        .config()
        .timeout_global(Some(Duration::from_secs(15)))
        .max_redirects(0)
        .build()
        .header("Authorization", &format!("Bearer {}", auth.token))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .send(body.as_bytes())
        .map_err(|_| "OpenAI voice could not connect. Check the OpenAI API key.")?;
    if response.status() != 201 {
        return Err("OpenAI voice could not start".into());
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(262_145)
        .read_to_end(&mut bytes)
        .map_err(|_| "OpenAI voice returned an invalid answer")?;
    if bytes.len() > 262_144 {
        return Err("OpenAI voice returned an oversized answer".into());
    }
    String::from_utf8(bytes).map_err(|_| "OpenAI voice returned an invalid answer".into())
}
