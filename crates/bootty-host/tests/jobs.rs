#![cfg(unix)]
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_host::jobs::{JobRegistry, JobSpec, JobStatus, JobStream};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[rstest]
#[case(0)]
#[case(7)]
fn jobs_preserve_bytes_streams_and_observed_exit(#[case] code: i32) {
    let registry = JobRegistry::default();
    let job = registry
        .start(
            JobSpec {
                program: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    format!("printf '\\377out'; printf err >&2; exit {code}"),
                ],
                cwd: "/".to_owned(),
                timeout_seconds: 30,
            },
            None,
            Arc::new(|| {}),
        )
        .unwrap();
    let mut cursor = 0;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .expect("test deadline");
    loop {
        assert!(Instant::now() < deadline);
        let batch = registry.read(&job.id, cursor, 100).unwrap();
        assert!(!batch.gap);
        cursor = batch.cursor;
        for chunk in batch.chunks {
            let output = match chunk.stream {
                JobStream::Stdout => &mut stdout,
                JobStream::Stderr => &mut stderr,
            };
            output.extend(STANDARD.decode(chunk.data).unwrap());
        }
        if batch.job.status.finished() && cursor == batch.job.next_cursor {
            assert_eq!(
                batch.job.status,
                JobStatus::Exited {
                    code: Some(code),
                    signal: None
                }
            );
            break;
        }
    }
    assert_eq!(stdout, b"\xffout");
    assert_eq!(stderr, b"err");
    registry.forget(&job.id).unwrap();
    assert_eq!(registry.list(), Vec::<bootty_host::jobs::JobSummary>::new());
}

#[rstest]
fn cancellation_reaps_the_owned_process_tree() {
    let registry = JobRegistry::default();
    let job = registry
        .start(
            JobSpec {
                program: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "cat /dev/zero >/dev/null & echo $!; wait".to_owned(),
                ],
                cwd: "/".to_owned(),
                timeout_seconds: 30,
            },
            None,
            Arc::new(|| {}),
        )
        .unwrap();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .expect("test deadline");
    let mut cursor = 0;
    loop {
        assert!(Instant::now() < deadline);
        let batch = registry.read(&job.id, cursor, 100).unwrap();
        cursor = batch.cursor;
        if !batch.chunks.is_empty() {
            break;
        }
    }
    assert!(registry.forget(&job.id).is_err());
    registry.cancel(&job.id).unwrap();
    loop {
        assert!(Instant::now() < deadline);
        let batch = registry.read(&job.id, cursor, 100).unwrap();
        cursor = batch.cursor;
        if batch.job.status.finished() {
            assert!(batch.job.cancel_requested);
            assert!(matches!(
                batch.job.status,
                JobStatus::Exited {
                    signal: Some(_),
                    ..
                }
            ));
            break;
        }
    }
}

#[rstest]
fn relative_job_directories_never_inherit_the_daemon_directory() {
    let registry = JobRegistry::default();
    let job = registry
        .start(
            JobSpec {
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), "printf should-not-run".to_owned()],
                cwd: ".".to_owned(),
                timeout_seconds: 30,
            },
            None,
            Arc::new(|| {}),
        )
        .unwrap();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .expect("test deadline");
    loop {
        assert!(Instant::now() < deadline);
        let batch = registry.read(&job.id, 0, 100).unwrap();
        assert!(batch.chunks.is_empty());
        if batch.job.status.finished() {
            assert!(matches!(
                batch.job.status,
                JobStatus::Failed { message }
                    if message.contains("working directory must be absolute")
            ));
            break;
        }
    }
}

#[rstest]
fn output_retention_reports_a_gap_instead_of_fabricating_complete_output() {
    let (wake, wakes) = std::sync::mpsc::channel();
    let registry = JobRegistry::default();
    let job = registry
        .start(
            JobSpec {
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), "head -c 4000000 /dev/zero".to_owned()],
                cwd: "/".to_owned(),
                timeout_seconds: 30,
            },
            None,
            Arc::new(move || {
                let _ = wake.send(());
            }),
        )
        .unwrap();
    while !registry.list()[0].status.finished() {
        wakes.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    let batch = registry.read(&job.id, 0, 0).unwrap();
    assert!(batch.gap);
    assert!(batch.job.retained_from > 0);
    assert!(
        batch
            .chunks
            .iter()
            .map(|chunk| chunk.data.len())
            .sum::<usize>()
            <= 64 * 1024
    );
    assert!(
        registry
            .read(
                &job.id,
                batch.job.next_cursor.checked_add(1).expect("future cursor"),
                0
            )
            .is_err()
    );
}
