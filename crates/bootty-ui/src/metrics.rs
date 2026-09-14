//! Background sampling for the native system status cells.

use num_traits::ToPrimitive as _;

use starship_battery::{Manager as BatteryManager, State as BatteryState, units::time::second};
#[cfg(target_os = "macos")]
use std::sync::Mutex;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use sysinfo::{MemoryRefreshKind, System};

#[cfg(target_os = "macos")]
const MEMORY_PRESSURE_TTL: Duration = Duration::from_secs(5);

/// Cross-platform system metrics gathered natively (no per-OS shell-outs), for the native status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    /// Global CPU usage, 0-100.
    pub cpu: f32,
    /// 1-minute load average; 0 where the OS has no concept of it (e.g. Windows).
    pub load1: f64,
    /// Memory in use as a percentage. On macOS this is real memory pressure (what
    /// Activity Monitor's pressure reflects), not the cache-inflated "used" figure.
    pub mem_used_pct: f64,
    pub mem_total_bytes: u64,
    /// Battery charge 0-100, or `None` on a machine with no battery (desktop).
    pub battery_percent: Option<f32>,
    /// Plugged in / charging / full / no battery (not draining).
    pub on_ac: bool,
    /// Seconds until empty while discharging, or `None` when unavailable/not discharging.
    pub battery_time_to_empty_secs: Option<f32>,
    /// Seconds until full while charging, or `None` when unavailable/not charging.
    pub battery_time_to_full_secs: Option<f32>,
}

/// A window reads the last published sample and never waits for platform probes.
pub struct MetricsService {
    current: Metrics,
    requests: mpsc::SyncSender<()>,
    samples: mpsc::Receiver<Metrics>,
    pending: bool,
    last_sample: Option<Instant>,
}

impl Default for MetricsService {
    fn default() -> Self {
        let (requests, receiver) = mpsc::sync_channel(1);
        let (sender, samples) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut system = System::new();
            let battery = BatteryManager::new().ok();
            while receiver.recv().is_ok() {
                let sample = sample_metrics(&mut system, battery.as_ref());
                if sender.send(sample).is_err() {
                    break;
                }
            }
        });
        Self {
            current: Metrics::default(),
            requests,
            samples,
            pending: false,
            last_sample: None,
        }
    }
}

impl MetricsService {
    #[must_use]
    pub const fn current(&self) -> Metrics {
        self.current
    }

    pub fn refresh(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Ok(sample) = self.samples.try_recv() {
            changed = self.current != sample;
            self.current = sample;
            self.pending = false;
        }
        if !self.pending
            && self
                .last_sample
                .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(2))
        {
            self.pending = self.requests.try_send(()).is_ok();
            self.last_sample = Some(now);
        }
        changed
    }
}

fn sample_metrics(system: &mut System, battery: Option<&BatteryManager>) -> Metrics {
    system.refresh_cpu_usage();
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    let load = System::load_average();
    let (battery_percent, on_ac, battery_time_to_empty_secs, battery_time_to_full_secs) =
        battery_status(battery);
    Metrics {
        cpu: system.global_cpu_usage(),
        load1: load.one,
        mem_used_pct: memory_used_percent(system),
        mem_total_bytes: system.total_memory(),
        battery_percent,
        on_ac,
        battery_time_to_empty_secs,
        battery_time_to_full_secs,
    }
}

#[cfg(target_os = "macos")]
fn memory_used_percent(system: &System) -> f64 {
    macos_memory_pressure_used().unwrap_or_else(|| sysinfo_used_percent(system))
}

#[cfg(not(target_os = "macos"))]
fn memory_used_percent(system: &System) -> f64 {
    sysinfo_used_percent(system)
}

fn sysinfo_used_percent(system: &System) -> f64 {
    let total = system.total_memory();
    if total == 0 {
        return 0.0;
    }
    let available = system.available_memory().min(total);
    100.0 * total.saturating_sub(available).to_f64().unwrap_or(0.0) / total.to_f64().unwrap_or(1.0)
}
/// Parse `memory_pressure`'s "System-wide memory free percentage: NN%" and return
/// used = 100 - free, the figure Activity Monitor's memory-pressure graph reflects.
///
/// Shared by the native sampler and held for [`MEMORY_PRESSURE_TTL`], to bound the system command cadence.
#[cfg(target_os = "macos")]
fn macos_memory_pressure_used() -> Option<f64> {
    static CACHED: Mutex<Option<(Instant, f64)>> = Mutex::new(None);

    let mut cached = CACHED.lock().ok()?;
    if let Some((sampled_at, used)) = *cached
        && sampled_at.elapsed() < MEMORY_PRESSURE_TTL
    {
        return Some(used);
    }
    let used = macos_memory_pressure_sample()?;
    *cached = Some((Instant::now(), used));
    drop(cached);
    Some(used)
}

#[cfg(target_os = "macos")]
fn macos_memory_pressure_sample() -> Option<f64> {
    let output = std::process::Command::new("/usr/bin/memory_pressure")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let free: f64 = text
        .lines()
        .find_map(|line| line.split("free percentage:").nth(1))
        .and_then(|rest| rest.trim().trim_end_matches('%').trim().parse().ok())?;
    Some((100.0 - free).clamp(0.0, 100.0))
}

/// Charge percentage, AC state, and remaining battery time. A machine with no battery
/// (desktop, or a probe error) reports `(None, true, None, None)` so the bar shows an AC icon.
fn battery_status(
    manager: Option<&BatteryManager>,
) -> (Option<f32>, bool, Option<f32>, Option<f32>) {
    let Some(manager) = manager else {
        return (None, true, None, None);
    };
    let Ok(mut batteries) = manager.batteries() else {
        return (None, true, None, None);
    };
    match batteries.next() {
        Some(Ok(battery)) => {
            let percent = battery.state_of_charge().value * 100.0;
            let on_ac = matches!(battery.state(), BatteryState::Charging | BatteryState::Full);
            let time_to_empty = battery.time_to_empty().map(|time| time.get::<second>());
            let time_to_full = battery.time_to_full().map(|time| time.get::<second>());
            (Some(percent), on_ac, time_to_empty, time_to_full)
        }
        _ => (None, true, None, None),
    }
}
