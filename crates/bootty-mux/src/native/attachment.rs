use std::{
    sync::Arc,
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct ReconnectSchedule {
    pub now: Arc<dyn Fn() -> Instant + Send + Sync>,
    pub wait: Arc<dyn Fn(Duration) + Send + Sync>,
    pub deadline: Duration,
}
impl Default for ReconnectSchedule {
    fn default() -> Self {
        Self {
            now: Arc::new(Instant::now),
            wait: Arc::new(std::thread::park_timeout),
            deadline: Duration::from_secs(5),
        }
    }
}
