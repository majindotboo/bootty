//! Kit owns dictation controls and session state; the local helper owns audio and credentials.
use std::{cell::RefCell, rc::Rc};

use bootty_host::speech::{SpeechCommand, SpeechEvent, SpeechSession};
use gpui_kit::component::speech::{
    AudioFormat, AudioInput, AudioSink, RecognitionSession, SpeechError, SpeechRecognizer,
    SpeechSink,
};
use gpui_kit::{App, Subscription};

#[derive(Default)]
pub(super) struct Settings {
    pub account: Option<SpeechCommand>,
    pub availability: bootty_agents::voice::SpeechAvailability,
}

pub(super) struct Recognizer(pub Rc<RefCell<Settings>>);

impl SpeechRecognizer for Recognizer {
    fn is_available(&self, _: &App) -> bool {
        self.0.borrow().availability.dictation()
    }

    fn start(
        &self,
        sink: SpeechSink,
        cx: &mut App,
    ) -> Result<Box<dyn RecognitionSession>, SpeechError> {
        let program = super::helper_program().map_err(SpeechError::recognizer)?;
        let settings = self.0.borrow();
        let account = settings.account.clone().ok_or_else(|| {
            SpeechError::recognizer(anyhow::anyhow!("The Codex account is unavailable"))
        })?;
        let codex = settings.availability.codex;
        let (session, mut events) = SpeechSession::start(program);
        session
            .send(account)
            .map_err(|error| SpeechError::recognizer(anyhow::anyhow!(error)))?;
        session
            .send(if codex {
                SpeechCommand::Listen { voice: false }
            } else {
                SpeechCommand::ListenOnDevice
            })
            .map_err(|error| SpeechError::recognizer(anyhow::anyhow!(error)))?;
        let events_sink = sink.clone();
        let task = cx.spawn(async move |cx| {
            let sink = events_sink;
            let mut partial = String::new();
            while let Some(event) = events.recv().await {
                let finished = matches!(
                    event,
                    SpeechEvent::Final { .. } | SpeechEvent::Error { .. } | SpeechEvent::Closed
                );
                cx.update(|cx| match event {
                    SpeechEvent::Listening => sink.ready(cx),
                    SpeechEvent::Partial { text } => {
                        if codex {
                            partial.push_str(&text);
                        } else {
                            partial = text;
                        }
                        sink.hypothesis(partial.clone(), cx);
                    }
                    SpeechEvent::Final { text } => {
                        sink.hypothesis("", cx);
                        sink.phrase(text, cx);
                        sink.finish(cx);
                    }
                    SpeechEvent::Error { message } => {
                        sink.error(SpeechError::recognizer(anyhow::anyhow!(message)), cx);
                    }
                    SpeechEvent::Closed => sink.error(
                        SpeechError::recognizer(anyhow::anyhow!("The speech helper disconnected")),
                        cx,
                    ),
                    SpeechEvent::Ready | SpeechEvent::Stopped | SpeechEvent::Offer { .. } => {}
                });
                if finished {
                    break;
                }
            }
        });
        Ok(Box::new(Recognition {
            session,
            sink,
            _task: task,
        }))
    }
}

struct Recognition {
    session: SpeechSession,
    sink: SpeechSink,
    _task: gpui_kit::Task<()>,
}

impl RecognitionSession for Recognition {
    fn push_audio(&mut self, _: &[i16], _: &mut App) {}

    fn finish(&mut self, cx: &mut App) {
        if let Err(error) = self.session.send(SpeechCommand::Stop) {
            self.sink
                .error(SpeechError::recognizer(anyhow::anyhow!(error)), cx);
        }
    }
}

/// The helper captures and recognizes together, with the recognition session owning both.
/// No PCM crosses the UI. Use a PCM `AudioInput` when the helper exports levels for `SpeechWaveform`.
pub(super) struct HelperInput;

impl AudioInput for HelperInput {
    fn start(
        &self,
        _: AudioFormat,
        _: AudioSink,
        _: &mut App,
    ) -> Result<Subscription, SpeechError> {
        Ok(Subscription::new(|| {}))
    }
}
