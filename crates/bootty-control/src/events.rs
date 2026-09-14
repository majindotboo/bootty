use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::CommandCancellation;

pub const EVENT_QUEUE_LIMIT: usize = 64;

pub struct ControlEventRequest {
    pub identity: String,
    pub generation: u64,
    pub topic: String,
    pub payload: Value,
    pub deadline: Instant,
    pub cancellation: CommandCancellation,
    pub response: mpsc::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct ControlEventSender {
    sender: mpsc::SyncSender<ControlEventRequest>,
    ready: Arc<tokio::sync::Notify>,
}

pub struct ControlEventReceiver {
    receiver: mpsc::Receiver<ControlEventRequest>,
    ready: Arc<tokio::sync::Notify>,
}

#[must_use]
pub fn event_queue() -> (ControlEventSender, ControlEventReceiver) {
    let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE_LIMIT);
    let ready = Arc::new(tokio::sync::Notify::new());
    (
        ControlEventSender {
            sender,
            ready: Arc::clone(&ready),
        },
        ControlEventReceiver { receiver, ready },
    )
}

impl ControlEventSender {
    /// # Errors
    /// Returns cancellation, queue, deadline, or publication errors from the event owner.
    pub fn publish(
        &self,
        identity: String,
        generation: u64,
        topic: String,
        payload: Value,
        deadline: Instant,
        cancellation: &CommandCancellation,
    ) -> Result<(), String> {
        if cancellation.is_cancelled() {
            return Err("control event was cancelled".to_owned());
        }
        let (response, receiver) = mpsc::channel();
        let request = ControlEventRequest {
            identity,
            generation,
            topic,
            payload,
            deadline,
            cancellation: cancellation.clone(),
            response,
        };
        match self.sender.try_send(request) {
            Ok(()) => self.ready.notify_one(),
            Err(mpsc::TrySendError::Full(request)) => {
                let _ = request
                    .response
                    .send(Err("control event queue is full".to_owned()));
                return Err("control event queue is full".to_owned());
            }
            Err(mpsc::TrySendError::Disconnected(request)) => {
                let _ = request
                    .response
                    .send(Err("control event queue is shut down".to_owned()));
                return Err("control event queue is shut down".to_owned());
            }
        }
        loop {
            if cancellation.is_cancelled() {
                return Err("control event was cancelled".to_owned());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("control event deadline expired".to_owned());
            }
            match receiver.recv_timeout(remaining.min(Duration::from_millis(5))) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("control event response closed".to_owned());
                }
            }
        }
    }
}

impl ControlEventReceiver {
    pub(crate) fn ready(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.ready)
    }

    /// # Errors
    /// Returns `Empty` if no event is ready or `Disconnected` if all senders have closed.
    pub fn try_recv(&self) -> Result<ControlEventRequest, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for ControlEventReceiver {
    fn drop(&mut self) {
        while let Ok(request) = self.receiver.try_recv() {
            let _ = request
                .response
                .send(Err("control event queue shut down".to_owned()));
        }
    }
}
