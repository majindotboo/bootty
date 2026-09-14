use bootty_host::jobs::{JobRegistry, JobStatus, JobSummary, TransferDirection, TransferSpec};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
fn finish(registry: &JobRegistry, id: &str) -> anyhow::Result<JobSummary> {
    loop {
        let result = registry.read(id, 0, 4000)?;
        if result.job.status.finished() {
            return Ok(result.job);
        }
    }
}
#[rstest]
#[case(0)]
#[case(5*1024*1024+13)]
fn copies_files_larger_than_document_limit_without_replacing_destinations(#[case] size: usize) {
    let directory = assert_fs::TempDir::new().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    let bytes = (0..251_u8).cycle().take(size).collect::<Vec<_>>();
    std::fs::write(&source, &bytes).unwrap();
    let registry = JobRegistry::default();
    let spec = TransferSpec {
        direction: TransferDirection::Upload,
        local_path: source.to_string_lossy().into_owned(),
        host_path: destination.to_string_lossy().into_owned(),
        timeout_seconds: 30,
    };
    let started = registry
        .start_transfer(spec.clone(), None, Arc::new(|| {}))
        .unwrap();
    let done = finish(&registry, &started.id).expect("finished transfer");
    assert_eq!(
        done.status,
        JobStatus::Exited {
            code: Some(0),
            signal: None
        }
    );
    let progress = done.transfer.unwrap();
    assert_eq!(progress.bytes, u64::try_from(size).expect("transfer size"));
    assert_eq!(progress.sha256.unwrap().len(), 64);
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    let duplicate = registry
        .start_transfer(spec, None, Arc::new(|| {}))
        .unwrap();
    assert!(matches!(
        finish(&registry, &duplicate.id)
            .expect("finished transfer")
            .status,
        JobStatus::Failed { .. }
    ));
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
}
#[rstest]
fn cancellation_removes_staging_and_retry_keeps_the_original_paths() {
    let directory = assert_fs::TempDir::new().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    std::fs::write(&source, vec![37; 2 * 1024 * 1024]).unwrap();
    let registry = Arc::new(JobRegistry::default());
    let weak = Arc::downgrade(&registry);
    let (paused, pauses) = mpsc::channel();
    let (resume, resumes) = mpsc::channel();
    let resumes = Mutex::new(resumes);
    let stopped = AtomicBool::new(false);
    let wake = Arc::new(move || {
        if let Some(registry) = weak.upgrade()
            && registry.list().iter().any(|job| {
                job.transfer
                    .as_ref()
                    .is_some_and(|progress| progress.bytes >= 256 * 1024)
            })
            && !stopped.swap(true, Ordering::Relaxed)
        {
            paused.send(()).unwrap();
            resumes
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    });
    let spec = TransferSpec {
        direction: TransferDirection::Download,
        host_path: source.to_string_lossy().into_owned(),
        local_path: destination.to_string_lossy().into_owned(),
        timeout_seconds: 30,
    };
    let job = registry.start_transfer(spec, None, wake).unwrap();
    pauses.recv_timeout(Duration::from_secs(5)).unwrap();
    registry.cancel(&job.id).unwrap();
    resume.send(()).unwrap();
    let cancelled = finish(&registry, &job.id).expect("finished transfer");
    assert!(cancelled.cancel_requested);
    assert!(matches!(cancelled.status, JobStatus::Failed { .. }));
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    let retry = registry.retry_transfer(&job.id, Arc::new(|| {})).unwrap();
    assert_ne!(retry.id, job.id);
    assert_eq!(
        finish(&registry, &retry.id)
            .expect("finished transfer")
            .status,
        JobStatus::Exited {
            code: Some(0),
            signal: None
        }
    );
    assert_eq!(
        std::fs::read(destination).unwrap(),
        std::fs::read(source).unwrap()
    );
}
