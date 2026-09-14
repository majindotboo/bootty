use std::{fs::File, io::Write, time::Instant};

use bootty_config::config::BoottyConfig;

use crate::strings::csv_field;

pub use bootty_terminal::latency::{start as latency_start, trace_phase, trace_slow};

pub struct StabilityTrace {
    pub started_at: Instant,
    file: File,
}

impl StabilityTrace {
    #[must_use]
    pub fn from_config(config: &BoottyConfig) -> Option<Self> {
        let path = config
            .diagnostics
            .stability_trace
            .clone()
            .or_else(|| std::env::var_os("BOOTTY_STABILITY_TRACE").map(Into::into))?;
        let mut file = File::create(path).ok()?;
        writeln!(
            file,
            "elapsed_ms,selected_session,cols,rows,pending_pty_bytes,drain_bytes,drain_elapsed_us,text_runs,last_error"
        )
        .ok()?;
        Some(Self {
            started_at: Instant::now(),
            file,
        })
    }

    pub fn record(&mut self, sample: StabilityTraceSample<'_>) {
        let _ = writeln!(
            self.file,
            "{},{},{},{},{},{},{},{},{}",
            self.started_at.elapsed().as_millis(),
            csv_field(sample.selected_session.unwrap_or("")),
            sample.cols,
            sample.rows,
            sample.pending_pty_bytes,
            sample.drain_bytes,
            sample.drain_elapsed_us,
            sample.text_runs,
            csv_field(sample.last_error.unwrap_or(""))
        );
    }
}

#[derive(Clone, Copy)]
pub struct StabilityTraceSample<'a> {
    pub selected_session: Option<&'a str>,
    pub cols: u16,
    pub rows: u16,
    pub pending_pty_bytes: usize,
    pub drain_bytes: usize,
    pub drain_elapsed_us: u64,
    pub text_runs: usize,
    pub last_error: Option<&'a str>,
}

impl crate::AppState {
    pub fn doctor(&self) -> serde_json::Value {
        let bindings = self.workspace.all_bindings().map(|binding| serde_json::json!({
            "scope":binding.scope().persistence_value().to_string(),
            "active":binding.scope() == self.workspace.active.binding.scope(),
            "backend":binding.multiplexer().backend,
            "host":binding.multiplexer().remote.as_ref().map_or_else(|| "Local".to_owned(), bootty_mux::RemoteTarget::label),
            "generation":binding.mux().binding_generation().to_string(),
            "available":binding.mux().unavailable_reason().is_none(),
            "availability_source":"last_backend_snapshot",
            "unavailable_reason":binding.mux().unavailable_reason(),
            "sessions":binding.mux().all_sessions().len(),
            "capabilities":binding.capabilities(),
        })).collect::<Vec<_>>();
        serde_json::json!({
            "healthy":bindings.iter().all(|binding| binding["available"] == true),
            "identity":bootty_config::ApplicationIdentity::current().cli_name(),
            "config_path":self.config().config_path,
            "config_revision":self.config_revision().to_string(),
            "remote_protocol":bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
            "bindings":bindings,
            "reported_agents":self.agent_overview().len(),
            "last_error":self.last_error(),
        })
    }
}
