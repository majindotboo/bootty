//! Opt-in phase timings for latency work shared across terminal consumers.
//!
//! Set `BOOTTY_TRACE_LATENCY` to an absolute path to append timings to that file; any other value
//! writes them to stderr. A windowed app is launched without a terminal attached, so the path form
//! is the one that survives. After the first environment lookup, a disabled call returns after the
//! sink's atomic initialization check without reading the clock.

use std::{io::Write, path::PathBuf, time::Instant};

enum LatencySink {
    File(PathBuf),
    Stderr,
}

fn latency_sink() -> Option<&'static LatencySink> {
    static SINK: std::sync::OnceLock<Option<LatencySink>> = std::sync::OnceLock::new();
    SINK.get_or_init(|| {
        let value = std::env::var_os("BOOTTY_TRACE_LATENCY")?;
        let path = PathBuf::from(&value);
        Some(if path.is_absolute() {
            LatencySink::File(path)
        } else {
            LatencySink::Stderr
        })
    })
    .as_ref()
}

#[derive(Clone, Copy)]
pub struct LatencyStart {
    started: Instant,
    sink: &'static LatencySink,
}

/// Capture a phase start only when tracing is enabled.
#[must_use]
pub fn start() -> Option<LatencyStart> {
    latency_sink().map(|sink| LatencyStart {
        started: Instant::now(),
        sink,
    })
}

/// Report how long `name` took. Use for phases worth seeing whatever they cost.
pub fn trace_phase(name: &str, start: Option<LatencyStart>) {
    let Some(start) = start else { return };
    record_latency(start.sink, name, millis(start.started));
}

/// Report `name` only when it took at least `threshold_ms`, for things called every frame.
pub fn trace_slow(name: &str, start: Option<LatencyStart>, threshold_ms: f64) {
    let Some(start) = start else { return };
    let elapsed = millis(start.started);
    if elapsed >= threshold_ms {
        record_latency(start.sink, name, elapsed);
    }
}

fn record_latency(sink: &LatencySink, name: &str, elapsed_ms: f64) {
    let line = format!("[latency] {name} {elapsed_ms:.1}ms\n");
    match sink {
        LatencySink::File(path) => {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = file.write_all(line.as_bytes());
            }
        }
        LatencySink::Stderr => eprint!("{line}"),
    }
}

fn millis(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
