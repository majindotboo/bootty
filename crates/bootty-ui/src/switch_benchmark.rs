//! Opt-in terminal paint evidence for the live switching benchmark.

use std::{
    fs::{File, OpenOptions},
    io::Write as _,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

fn sink() -> Option<&'static Mutex<File>> {
    static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("BOOTTY_SWITCH_BENCH_TRACE")?;
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => Some(Mutex::new(file)),
            Err(error) => {
                eprintln!("Cannot open switching benchmark trace: {error}");
                None
            }
        }
    })
    .as_ref()
}

pub fn enabled() -> bool {
    sink().is_some()
}

pub fn requested(command: &str) {
    let Some(sink) = sink() else { return };
    let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    if let Ok(mut file) = sink.lock() {
        let record = serde_json::json!({
            "schema_version": 1,
            "event": "command_received",
            "unix_ns": elapsed.as_nanos(),
            "pid": std::process::id(),
            "command": command,
        });
        let _ = writeln!(file, "{record}");
    }
}

/// Called after terminal scene painting and input-handler installation, not at frame publication.
/// This is CPU paint completion, not platform submission or display scanout.
pub fn painted(target: Option<&str>, focused: bool, has_text: bool) {
    let Some(sink) = sink() else { return };
    let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    let record = serde_json::json!({
        "event": "terminal_painted",
        "schema_version": 1,
        "unix_ns": elapsed.as_nanos(),
        "pid": std::process::id(),
        "target": target,
        "focused": focused,
        "has_text": has_text,
    });
    if let Ok(mut file) = sink.lock() {
        let _ = writeln!(file, "{record}");
    }
}
