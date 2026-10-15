//! Dictation with Apple's on-device speech recognizer, for a Mac without a Codex sign-in. Audio
//! and text stay on this Mac.
#![allow(
    unsafe_code,
    reason = "Speech and AVFAudio calls are confined to this adapter."
)]
use std::{ptr::NonNull, sync::mpsc, time::Duration};

use block2::RcBlock;
use objc2::{AnyThread as _, rc::Retained};
use objc2_avf_audio::{AVAudioFormat, AVAudioPCMBuffer};
use objc2_foundation::{NSError, NSOperationQueue};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTask,
    SFSpeechRecognizer, SFSpeechRecognizerAuthorizationStatus,
};

/// The audio host's microphone rate.
const SAMPLE_RATE: f64 = 24_000.0;
/// The user answers the speech recognition prompt within this time, or dictation stops.
const AUTHORIZATION_WAIT: Duration = Duration::from_secs(60);
/// The recognizer's error code for audio without speech.
const NO_SPEECH: isize = 1110;
const NOT_ALLOWED: &str = "Speech recognition is turned off for Bootty. Allow it in System Settings → Privacy & Security → Speech Recognition.";

/// What the recognizer heard.
pub(super) enum Heard {
    /// The whole transcript so far; later results revise it.
    Partial(String),
    Final(String),
    Failed(String),
}

/// Whether this Mac can dictate on device: a local recognizer for the user's language, and speech
/// recognition not refused. It never prompts.
pub fn available() -> bool {
    let status = unsafe { SFSpeechRecognizer::authorizationStatus() };
    status != SFSpeechRecognizerAuthorizationStatus::Denied
        && status != SFSpeechRecognizerAuthorizationStatus::Restricted
        && recognizer().is_some()
}

/// A recognizer for the user's language that runs on this Mac.
fn recognizer() -> Option<Retained<SFSpeechRecognizer>> {
    let recognizer = unsafe { SFSpeechRecognizer::init(SFSpeechRecognizer::alloc()) }?;
    unsafe { recognizer.supportsOnDeviceRecognition() }.then_some(recognizer)
}

/// Ask once; later dictations see the answer.
fn authorize() -> Result<(), String> {
    match unsafe { SFSpeechRecognizer::authorizationStatus() } {
        SFSpeechRecognizerAuthorizationStatus::Authorized => return Ok(()),
        SFSpeechRecognizerAuthorizationStatus::NotDetermined => {}
        _ => return Err(NOT_ALLOWED.into()),
    }
    let (sender, answer) = mpsc::sync_channel(1);
    let answered = RcBlock::new(move |status: SFSpeechRecognizerAuthorizationStatus| {
        _ = sender.try_send(status);
    });
    unsafe { SFSpeechRecognizer::requestAuthorization(&answered) };
    match answer.recv_timeout(AUTHORIZATION_WAIT) {
        Ok(SFSpeechRecognizerAuthorizationStatus::Authorized) => Ok(()),
        _ => Err(NOT_ALLOWED.into()),
    }
}

/// One dictation: microphone audio in, transcripts out through [`Recognition::heard`].
pub(super) struct Recognition {
    request: Retained<SFSpeechAudioBufferRecognitionRequest>,
    task: Retained<SFSpeechRecognitionTask>,
    format: Retained<AVAudioFormat>,
    heard: mpsc::Receiver<Heard>,
    /// A task needs its recognizer alive.
    _recognizer: Retained<SFSpeechRecognizer>,
}

impl Recognition {
    pub fn start() -> Result<Self, String> {
        authorize()?;
        let recognizer =
            recognizer().ok_or("On-device dictation does not support your language")?;
        // Results arrive on this queue; the helper has no main run loop to deliver them.
        unsafe { recognizer.setQueue(&NSOperationQueue::new()) };
        let request = unsafe { SFSpeechAudioBufferRecognitionRequest::new() };
        unsafe {
            request.setRequiresOnDeviceRecognition(true);
            request.setShouldReportPartialResults(true);
            request.setAddsPunctuation(true);
        }
        let (sender, heard) = mpsc::channel();
        let handler = RcBlock::new(
            move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
                // The recognizer passes a result, an error, or both after a failure.
                let heard = if let Some(result) = unsafe { result.as_ref() } {
                    let text = unsafe { result.bestTranscription().formattedString() }.to_string();
                    if unsafe { result.isFinal() } {
                        Heard::Final(text)
                    } else {
                        Heard::Partial(text)
                    }
                } else if let Some(error) = unsafe { error.as_ref() } {
                    Heard::Failed(if error.code() == NO_SPEECH {
                        "No speech was detected. Try dictating again.".to_owned()
                    } else {
                        format!(
                            "On-device dictation failed. {}",
                            error.localizedDescription()
                        )
                    })
                } else {
                    return;
                };
                _ = sender.send(heard);
            },
        );
        let task =
            unsafe { recognizer.recognitionTaskWithRequest_resultHandler(&request, &handler) };
        let format = unsafe {
            AVAudioFormat::initStandardFormatWithSampleRate_channels(
                AVAudioFormat::alloc(),
                SAMPLE_RATE,
                1,
            )
        }
        .ok_or("Could not prepare on-device dictation")?;
        Ok(Self {
            request,
            task,
            format,
            heard,
            _recognizer: recognizer,
        })
    }

    /// Recognize mono 16-bit little-endian samples at the audio host's rate.
    pub fn append(&self, pcm: &[u8]) -> Result<(), String> {
        let (samples, _) = pcm.as_chunks::<2>();
        let Ok(frames) = u32::try_from(samples.len()) else {
            return Err("Oversized microphone audio frame".into());
        };
        if frames == 0 {
            return Ok(());
        }
        let buffer = unsafe {
            AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                AVAudioPCMBuffer::alloc(),
                &self.format,
                frames,
            )
        }
        .ok_or("Could not buffer microphone audio")?;
        unsafe { buffer.setFrameLength(frames) };
        let channel = NonNull::new(unsafe { buffer.floatChannelData() })
            .ok_or("Could not buffer microphone audio")?;
        // The standard format holds one non-interleaved float channel of `frames` samples.
        let channel =
            unsafe { std::slice::from_raw_parts_mut(channel.read().as_ptr(), samples.len()) };
        for (sample, bytes) in channel.iter_mut().zip(samples) {
            *sample = f32::from(i16::from_le_bytes(*bytes)) / 32_768.0;
        }
        unsafe { self.request.appendAudioPCMBuffer(&buffer) };
        Ok(())
    }

    /// No more audio; the final transcript follows.
    pub fn finish(&self) {
        unsafe { self.request.endAudio() };
    }

    pub fn heard(&self) -> Option<Heard> {
        self.heard.try_recv().ok()
    }
}

impl Drop for Recognition {
    fn drop(&mut self) {
        unsafe { self.task.cancel() };
    }
}
