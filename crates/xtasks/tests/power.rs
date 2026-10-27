#![cfg(unix)]

use std::fs;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use assert_fs::TempDir;
use assert_fs::prelude::*;
use pretty_assertions::assert_eq;
use serde_json::Value;

#[test]
fn power_preserves_measured_command_failure_in_evidence() {
    let temp = TempDir::new().unwrap();
    let output = temp.child("power");

    let status = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "power-thermal"])
        .arg(output.path())
        .args(["--", "/bin/sh", "-c", "exit 7"])
        .env_clear()
        .env("PATH", temp.path())
        .status()
        .unwrap();

    // The top-level CLI maps the typed command failure back to its original code.
    assert_eq!(status.code(), Some(7));
    let records = fs::read_to_string(output.child("summary.jsonl").path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[1]["status"], "skipped");
    assert_eq!(records[2]["status"], "fail");
    assert_eq!(records[2]["exit_code"], 7);
}

#[test]
fn power_stops_continuous_nvidia_sampler_at_the_deadline() {
    let temp = TempDir::new().unwrap();
    let plan = xtasks::benchmark::power::sampler_plan(temp.path(), 0, 1, |program| {
        program == "nvidia-smi"
    })
    .unwrap();
    assert_eq!(plan.command.get_program(), "nvidia-smi");
    let log = plan.log.clone();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "continuous_sampler_helper", "--nocapture"])
        .env("BOOTTY_CONTINUOUS_SAMPLER_HELPER", "1");
    let base = Instant::now();
    let elapsed = Arc::new(AtomicU64::new(0));
    let time = Arc::clone(&elapsed);
    let mut sampler = xtasks::benchmark::power::RunningSampler::start(
        command,
        plan.tool,
        log.clone(),
        Duration::from_secs(1),
        Arc::new(move || base + Duration::from_secs(time.load(Ordering::SeqCst))),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut record = Vec::new();
    sampler.write_started(&mut record).unwrap();
    let record: Value = serde_json::from_slice(&record).unwrap();
    assert_eq!(record["tool"], "nvidia-smi");
    assert_eq!(record["status"], "started");
    while !fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("sampler-ready")
    {
        assert!(Instant::now() < deadline, "sampler did not start");
        thread::yield_now();
    }
    elapsed.store(1, Ordering::SeqCst);
    let stopped = sampler.finish().unwrap();
    assert!(
        !stopped.success(),
        "the deadline must terminate the continuous sampler"
    );
    assert!(fs::read_to_string(log).unwrap().contains("sampler-ready"));
}

#[test]
fn power_terminates_the_measured_process_group_on_sigterm() {
    let temp = TempDir::new().unwrap();
    let descendant_pid = temp.child("descendant.pid");
    let output = temp.child("power");
    let command = format!(
        "trap '' TERM; exec 3<&0; (trap '' TERM; read hold <&3) & echo $! > '{}'; wait",
        descendant_pid.path().display()
    );
    let mut xtasks = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "power-thermal"])
        .arg(output.path())
        .args(["--", "/bin/sh", "-c", &command])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let _input = xtasks.stdin.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if descendant_pid.path().is_file() {
            break;
        }
        thread::yield_now();
    }
    assert!(descendant_pid.path().is_file());
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &xtasks.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(xtasks.wait().unwrap().code(), Some(130));

    let pid = fs::read_to_string(descendant_pid.path()).unwrap();
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "descendant {pid} survived xtasks cancellation"
    );
}

#[test]
fn continuous_sampler_helper() {
    if std::env::var_os("BOOTTY_CONTINUOUS_SAMPLER_HELPER").is_some() {
        use std::io::Write as _;
        print!("sampler-ready");
        std::io::stdout().flush().unwrap();
        loop {
            thread::park();
        }
    }
}
