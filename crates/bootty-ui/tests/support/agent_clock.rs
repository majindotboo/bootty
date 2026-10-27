use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// One host's monotonic time, advanced explicitly when the host drives another frame.
pub struct Clock(Arc<Mutex<Instant>>);

impl Default for Clock {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }
}

impl Clock {
    pub fn runtime(
        &self,
        events: bootty_control::ControlEventSender,
    ) -> bootty_ui::commands::AgentRuntimeConfig {
        let now = Arc::clone(&self.0);
        let launcher: bootty_agents::NativeLauncher = Arc::new(|launch| {
            Err(format!(
                "No native provider fixture for {}",
                launch.config.program
            ))
        });
        #[cfg(unix)]
        let terminal_transport = super::provider_queries::terminal_transport(None);
        #[cfg(not(unix))]
        let terminal_transport = bootty_agents::TerminalProviderTransport::default();
        bootty_ui::commands::AgentRuntimeConfig {
            events,
            clock: Arc::new(move || *now.lock().expect("fixture clock")),
            launcher,
            terminal_transport,
        }
    }

    pub fn tick(&self, state: &bootty_ui::AppState) -> Instant {
        let mut now = self.0.lock().expect("fixture clock");
        *now = now
            .checked_add(Duration::from_secs(1))
            .expect("fixture time fits");
        let frame_at = *now;
        drop(now);
        if let Some(service) = state.agent_service() {
            service.clock_advanced();
        }
        if let Some(service) = state.native_agent_service() {
            service.clock_advanced();
        }
        if let Some(service) = state.terminal_agent_service() {
            service.clock_advanced();
        }
        frame_at
    }
}
