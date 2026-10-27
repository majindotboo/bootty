#![cfg(test)]

use bootty_agents::{
    AgentKind, AgentLaunch, TerminalAgentService, TerminalAgentStatus, TerminalProviderTransport,
    TerminalQueryOutput,
};
use bootty_control::{CommandTarget, ResourceKind};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const SESSION: &str = "00000000-0000-4000-8000-000000000001";
const DEADLINE: Duration = Duration::from_secs(30);

#[rstest]
fn active_claude_uses_the_injected_query_and_wakes_when_the_service_clock_advances() {
    let directory = assert_fs::TempDir::new().unwrap();
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock_now = now.clone();
    let queries = Arc::new(AtomicUsize::new(0));
    let query_count = queries.clone();
    let transport = TerminalProviderTransport {
        output: Arc::new(move |command, cancelled| {
            assert!(!cancelled());
            assert_eq!(command.get_program(), "/fixture/claude");
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                ["agents", "--json", "--all"]
            );
            assert_eq!(
                command
                    .get_envs()
                    .find(|(name, _)| *name == "CLAUDE_CONFIG_DIR")
                    .and_then(|(_, value)| value)
                    .and_then(|value| value.to_str()),
                Some("/fixture/account")
            );
            let status = if query_count.fetch_add(1, Ordering::Relaxed) == 0 {
                "busy"
            } else {
                "done"
            };
            Ok(TerminalQueryOutput {
                stdout: serde_json::json!([{"sessionId":SESSION,"state":status}])
                    .to_string()
                    .into_bytes(),
                stderr: vec![],
                exit_code: Some(0),
                successful: true,
            })
        }),
        ..TerminalProviderTransport::default()
    };
    let service = Arc::new(
        TerminalAgentService::open_with_clock(
            directory.path().join("catalog.json"),
            Arc::new(move || *clock_now.lock().unwrap()),
        )
        .unwrap()
        .with_provider_transport(transport),
    );
    let (changes, changed) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = changes.send(());
    }));
    let prepared = service
        .prepare(
            AgentKind::Claude,
            AgentLaunch {
                program: "/fixture/claude".to_owned(),
                cwd: None,
                arguments: vec!["--session-id".to_owned(), SESSION.to_owned()],
                ephemeral: false,
                account_directory: Some("/fixture/account".to_owned()),
            },
        )
        .unwrap();
    let target = CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "issued-terminal".to_owned(),
        generation: 1,
    };
    service
        .register(prepared, target.clone(), "binding".to_owned())
        .unwrap();
    for status in [TerminalAgentStatus::Working, TerminalAgentStatus::Finished] {
        while service.record(&target).unwrap().observation.status != status {
            changed.recv_timeout(DEADLINE).unwrap();
        }
        if status == TerminalAgentStatus::Working {
            let mut clock = now.lock().unwrap();
            *clock = clock.checked_add(Duration::from_millis(750)).unwrap();
            drop(clock);
            service.clock_advanced();
        }
    }
    service.shutdown_and_wait().unwrap();
    assert_eq!(queries.load(Ordering::Relaxed), 2);
}

#[cfg(unix)]
#[rstest]
fn stopping_the_real_claude_query_cancels_and_reaps_its_process() {
    use std::{fs::OpenOptions, io::Read as _, process::Command, thread};
    let directory = assert_fs::TempDir::new().unwrap();
    let started = directory.path().join("started");
    let release = directory.path().join("release");
    assert!(
        Command::new("mkfifo")
            .args([&started, &release])
            .status()
            .unwrap()
            .success()
    );
    let mut ready = OpenOptions::new()
        .read(true)
        .write(true)
        .open(started)
        .unwrap();
    let _release = OpenOptions::new()
        .read(true)
        .write(true)
        .open(release)
        .unwrap();
    let program = directory.path().join("provider");
    bootty_test_support::write_executable(
        &program,
        r#"#!/bin/sh
[ "$*" = 'agents --json --all' ] || exit 9
printf '%s\n' "$$" > "$CLAUDE_CONFIG_DIR/provider.pid"
printf R > "$CLAUDE_CONFIG_DIR/started"
IFS= read -r release < "$CLAUDE_CONFIG_DIR/release"
"#,
    )
    .unwrap();
    let (started, arrival) = mpsc::channel();
    let reader = thread::spawn(move || {
        let _ = started.send(ready.read_exact(&mut [0]));
    });
    let (observations, observed) = mpsc::channel();
    let launch = AgentLaunch {
        program: program.to_str().unwrap().to_owned(),
        cwd: None,
        arguments: vec!["--session-id".to_owned(), SESSION.to_owned()],
        ephemeral: false,
        account_directory: Some(directory.path().to_str().unwrap().to_owned()),
    };
    let mut observer = bootty_agents::ClaudeTerminalObserver::prepare(
        &launch,
        Arc::new(move |value| {
            let _ = observations.send(value);
        }),
    )
    .unwrap();
    arrival.recv_timeout(DEADLINE).unwrap().unwrap();
    reader.join().unwrap();
    observer.stop_and_wait().unwrap();
    let observation = observed.recv_timeout(DEADLINE).unwrap();
    assert_eq!(observation.status, TerminalAgentStatus::Unavailable);
    let detail = observation.detail.expect("cancellation diagnostic");
    assert!(detail.contains("cancelled"), "{detail}");
    let pid = std::fs::read_to_string(directory.path().join("provider.pid")).unwrap();
    assert_eq!(
        rmux_os::process::ProcessInspector
            .is_live(pid.trim().parse().unwrap())
            .unwrap(),
        Some(false),
    );
}
