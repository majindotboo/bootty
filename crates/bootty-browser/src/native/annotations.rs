use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use async_channel::{Receiver, Sender};
use serde::Deserialize;

use super::{BrowserView, NativeBrowserError};
use crate::{AnnotationCaptureContext, AnnotationError};

pub(super) type CaptureResponses =
    Rc<RefCell<BTreeMap<u64, Sender<Result<AnnotationCaptureContext, AnnotationError>>>>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureReply {
    intent: String,
    context: Option<AnnotationCaptureContext>,
}

pub(super) fn receive_capture(message: &str, responses: &CaptureResponses) -> bool {
    let Some(message) = message.strip_prefix("browser-annotation-capture:") else {
        return false;
    };
    if message.len() <= 16 * 1024
        && let Ok(reply) = serde_json::from_str::<CaptureReply>(message)
        && let Ok(intent) = reply.intent.parse::<u64>()
        && let Some(sender) = responses.borrow_mut().remove(&intent)
    {
        let result = reply
            .context
            .ok_or(AnnotationError::Invalid)
            .and_then(|context| {
                context.validate()?;
                Ok(context)
            });
        _ = sender.try_send(result);
    }
    true
}

impl BrowserView {
    /// Hide only this editor and await a rendered frame containing its selection marks.
    /// # Errors
    /// Rejects overlapping captures and native dispatch failures. Page metadata stays untrusted.
    pub fn prepare_annotation_capture(
        &self,
        annotation: u64,
        intent: u64,
    ) -> Result<Receiver<Result<AnnotationCaptureContext, AnnotationError>>, NativeBrowserError>
    {
        if annotation == 0 || intent == 0 || !self.annotation_captures.borrow().is_empty() {
            return Err(NativeBrowserError::Platform(
                "Annotation capture is unavailable.".into(),
            ));
        }
        let (sender, receiver) = async_channel::bounded(1);
        _ = self.annotation_captures.borrow_mut().insert(intent, sender);
        let payload = serde_json::json!([annotation.to_string(), intent.to_string()]);
        if let Err(error) = self.view.evaluate_script(&format!(
            "window.__boottyAnnotations?.prepareCapture(...{payload})"
        )) {
            _ = self.annotation_captures.borrow_mut().remove(&intent);
            return Err(error.into());
        }
        Ok(receiver)
    }

    /// Reobserve the exact document and page geometry before admitting image publication.
    /// # Errors
    /// Reports native dispatch failure; a changed editor returns invalid context.
    pub fn annotation_capture_context(
        &self,
        annotation: u64,
        intent: u64,
    ) -> Result<Receiver<Result<AnnotationCaptureContext, AnnotationError>>, NativeBrowserError>
    {
        let payload = serde_json::json!([annotation.to_string(), intent.to_string()]);
        let script = format!(
            "(() => {{ try {{ return window.__boottyAnnotations?.captureContext(...{payload}) ?? null; }} catch {{ return null; }} }})()"
        );
        let (sender, receiver) = async_channel::bounded(1);
        self.view
            .evaluate_script_with_callback(&script, move |result| {
                let context = if result.len() <= 16 * 1024 {
                    serde_json::from_str::<AnnotationCaptureContext>(&result).ok()
                } else {
                    None
                };
                _ = sender.try_send(context.ok_or(AnnotationError::Invalid).and_then(|context| {
                    context.validate()?;
                    Ok(context)
                }));
            })?;
        Ok(receiver)
    }

    /// Restore only the editor that owns this capture lease; never restore focus to another editor.
    /// # Errors
    /// Reports native script dispatch failure.
    pub fn finish_annotation_capture(
        &self,
        annotation: u64,
        intent: u64,
    ) -> Result<(), NativeBrowserError> {
        _ = self.annotation_captures.borrow_mut().remove(&intent);
        let payload = serde_json::json!([annotation.to_string(), intent.to_string()]);
        self.view.evaluate_script(&format!(
            "window.__boottyAnnotations?.finishCapture(...{payload})"
        ))?;
        Ok(())
    }
}
