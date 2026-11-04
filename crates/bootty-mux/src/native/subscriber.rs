use super::protocol::Frame;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};

pub const SUBSCRIBER_BYTES: usize = 1024 * 1024;
pub(super) struct QueuedFrame {
    pub frame: Frame,
    pub _budget: OwnedSemaphorePermit,
}
#[derive(Clone)]
pub(super) struct Subscriber {
    tx: mpsc::Sender<QueuedFrame>,
    budget: Arc<Semaphore>,
    pub dead: Arc<AtomicBool>,
    pub wake: Arc<Notify>,
}
impl Subscriber {
    pub fn new() -> (Self, mpsc::Receiver<QueuedFrame>) {
        let (tx, rx) = mpsc::channel(256);
        (
            Self {
                tx,
                budget: Arc::new(Semaphore::new(SUBSCRIBER_BYTES)),
                dead: Arc::new(AtomicBool::new(false)),
                wake: Arc::new(Notify::new()),
            },
            rx,
        )
    }
    pub fn push(&self, frame: Frame) -> bool {
        if self.dead.load(Ordering::Acquire) {
            return false;
        }
        let sent = u32::try_from(frame.bytes.len().saturating_add(5))
            .ok()
            .and_then(|n| Arc::clone(&self.budget).try_acquire_many_owned(n).ok())
            .is_some_and(|permit| {
                self.tx
                    .try_send(QueuedFrame {
                        frame,
                        _budget: permit,
                    })
                    .is_ok()
            });
        if !sent {
            self.close();
        }
        sent
    }
    pub fn close(&self) {
        self.dead.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}
