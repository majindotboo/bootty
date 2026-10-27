use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// One host's monotonic time. Rendering and receiving events do not advance it.
pub struct Clock(Arc<Mutex<Instant>>);

impl Default for Clock {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }
}

impl Clock {
    /// Capture the host's clock and provider transports.
    ///
    /// # Panics
    /// The captured clock panics if the fixture's clock mutex is poisoned.
    #[must_use]
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

    /// # Panics
    /// Panics if another fixture thread poisoned the clock lock.
    #[must_use]
    pub fn now(&self) -> Instant {
        *self.0.lock().expect("fixture clock")
    }

    /// Advance a product timer and wake the services that use this clock.
    /// # Panics
    /// Panics if the clock lock is poisoned or the duration exceeds the clock range.
    pub fn advance(&self, state: &bootty_ui::AppState, duration: Duration) {
        let mut now = self.0.lock().expect("fixture clock");
        *now = now.checked_add(duration).expect("fixture time fits");
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
    }
}
