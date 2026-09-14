//! Owner-scoped batch jobs with bounded output and platform-native descendant cleanup.
use crate::remote::RemoteHost;
use anyhow::{Context as _, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

mod process;
mod remote;
mod transfer;
pub use remote::serve;
pub use transfer::{TransferDirection, TransferProgress, TransferSpec, serve_transfer};

const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const CHUNK_LIMIT: usize = 512;
const READ_LIMIT: usize = 64 * 1024;
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
}
const fn default_timeout() -> u32 {
    3600
}
impl JobSpec {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.program.is_empty() && !self.cwd.is_empty(),
            "program and working directory are required"
        );
        ensure!(
            self.args.len() <= 256
                && self.args.iter().map(String::len).fold(
                    self.program.len().saturating_add(self.cwd.len()),
                    usize::saturating_add
                ) <= 64 * 1024,
            "job arguments exceed the limit"
        );
        ensure!(
            !self.program.contains('\0')
                && !self.cwd.contains('\0')
                && !self.args.iter().any(|arg| arg.contains('\0')),
            "job arguments cannot contain NUL"
        );
        ensure!(
            (1..=86400).contains(&self.timeout_seconds),
            "job timeout must be 1–86400 seconds"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= 64 * 1024,
            "encoded job request exceeds the limit"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobStatus {
    Starting,
    Transferring,
    Running {
        pid: u32,
    },
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Failed {
        message: String,
    },
}
impl JobStatus {
    #[must_use]
    pub const fn finished(&self) -> bool {
        matches!(self, Self::Exited { .. } | Self::Failed { .. })
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct JobSummary {
    pub id: String,
    pub host: String,
    pub program: String,
    pub cwd: String,
    pub status: JobStatus,
    pub cancel_requested: bool,
    pub timed_out: bool,
    pub next_cursor: u64,
    pub retained_from: u64,
    #[serde(default)]
    pub transfer: Option<TransferProgress>,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStream {
    Stdout,
    Stderr,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobChunk {
    pub sequence: u64,
    pub stream: JobStream,
    /// Base64 preserves arbitrary process bytes on the JSON wire.
    pub data: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobRead {
    pub job: JobSummary,
    pub chunks: Vec<JobChunk>,
    pub cursor: u64,
    pub gap: bool,
}
struct State {
    summary: JobSummary,
    chunks: VecDeque<JobChunk>,
    bytes: usize,
}
struct Job {
    transfer: Option<(TransferSpec, Option<RemoteHost>)>,
    state: Mutex<State>,
    changed: Condvar,
    cancel: AtomicBool,
    controller: Mutex<Option<rmux_os::process_tree::ProcessTreeController>>,
    deadline: Instant,
    wake: Wake,
    changes: Option<std::sync::mpsc::SyncSender<()>>,
}
impl Job {
    fn control(&self, controller: rmux_os::process_tree::ProcessTreeController) -> Result<()> {
        let mut current = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let controller = current.insert(controller);
        // Cancellation may have arrived while spawn was creating the transport.
        let result = if self.cancel.load(Ordering::Acquire) {
            controller.terminate().map_err(Into::into)
        } else {
            Ok(())
        };
        drop(current);
        result
    }
    fn notify(&self) {
        self.changed.notify_all();
        (self.wake)();
        if let Some(changes) = &self.changes {
            let _ = changes.try_send(());
        }
    }
    fn status(&self, status: JobStatus) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .summary
            .status = status;
        self.notify();
    }
    fn output(&self, stream: JobStream, bytes: &[u8]) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(next) = state.summary.next_cursor.checked_add(1) else {
            drop(state);
            self.cancel(false);
            self.status(JobStatus::Failed {
                message: "job output sequence exhausted".to_owned(),
            });
            return;
        };
        state.summary.next_cursor = next;
        let sequence = state.summary.next_cursor;
        let data = STANDARD.encode(bytes);
        state.bytes = state.bytes.saturating_add(data.len());
        state.chunks.push_back(JobChunk {
            sequence,
            stream,
            data,
        });
        while state.bytes > OUTPUT_LIMIT || state.chunks.len() > CHUNK_LIMIT {
            if let Some(old) = state.chunks.pop_front() {
                state.bytes = state.bytes.saturating_sub(old.data.len());
            }
        }
        state.summary.retained_from = state
            .chunks
            .front()
            .map_or(sequence, |chunk| chunk.sequence.saturating_sub(1));
        drop(state);
        self.notify();
    }
    fn cancel(&self, timed_out: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.summary.status.finished() {
            return;
        }
        state.summary.cancel_requested = true;
        state.summary.timed_out |= timed_out;
        self.cancel.store(true, Ordering::Release);
        drop(state);
        if let Some(controller) = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            let _ = controller.terminate();
        }
        self.notify();
    }
    fn read(&self, cursor: u64, wait: Duration) -> Result<JobRead> {
        let (state, _) = {
            let before_wait = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ensure!(
                cursor <= before_wait.summary.next_cursor,
                "job cursor is ahead of output"
            );
            self.changed
                .wait_timeout_while(before_wait, wait, |state| {
                    cursor == state.summary.next_cursor && !state.summary.status.finished()
                })
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        let mut size = 0_usize;
        let chunks = state
            .chunks
            .iter()
            .filter(|chunk| chunk.sequence > cursor)
            .take_while(|chunk| {
                size = size.saturating_add(chunk.data.len());
                size <= READ_LIMIT
            })
            .cloned()
            .collect::<Vec<_>>();
        let job = state.summary.clone();
        drop(state);
        Ok(JobRead {
            gap: cursor < job.retained_from,
            job,
            cursor: chunks.last().map_or(cursor, |chunk| chunk.sequence),
            chunks,
        })
    }
}
pub struct JobRegistry {
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
    changes: Option<std::sync::mpsc::SyncSender<()>>,
    active: AtomicBool,
    generation: u64,
}
impl Default for JobRegistry {
    fn default() -> Self {
        Self::new(None)
    }
}
impl Drop for JobRegistry {
    fn drop(&mut self) {
        for job in self
            .jobs
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            job.cancel(false);
        }
    }
}
impl JobRegistry {
    pub fn new(changes: Option<std::sync::mpsc::SyncSender<()>>) -> Self {
        Self {
            jobs: Mutex::new(BTreeMap::new()),
            changes,
            active: AtomicBool::new(true),
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    pub fn retire(&self) {
        self.active.store(false, Ordering::Release);
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for job in jobs {
            job.cancel(false);
        }
    }

    /// # Errors
    /// Returns invalid job, retired owner, capacity, identity, deadline, or worker startup errors.
    pub fn start(
        &self,
        spec: JobSpec,
        remote: Option<RemoteHost>,
        wake: Wake,
    ) -> Result<JobSummary> {
        self.start_work(spec, remote, wake, None)
    }
    /// # Errors
    /// Returns invalid transfer paths or job startup errors.
    pub fn start_transfer(
        &self,
        spec: TransferSpec,
        remote: Option<RemoteHost>,
        wake: Wake,
    ) -> Result<JobSummary> {
        spec.validate()?;
        let job = JobSpec {
            program: "file-transfer".to_owned(),
            args: Vec::new(),
            cwd: spec.local_path.clone(),
            timeout_seconds: spec.timeout_seconds,
        };
        self.start_work(job, remote, wake, Some(spec))
    }
    /// # Errors
    /// Returns an error if the prior job is active, is not a transfer, or cannot be restarted.
    pub fn retry_transfer(&self, id: &str, wake: Wake) -> Result<JobSummary> {
        let previous = self.get(id)?;
        ensure!(
            previous
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .summary
                .status
                .finished(),
            "Wait for the transfer to stop before retrying"
        );
        let (spec, remote) = previous
            .transfer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("This job is not a file transfer"))?;
        self.start_transfer(spec, remote, wake)
    }
    fn start_work(
        &self,
        spec: JobSpec,
        remote: Option<RemoteHost>,
        wake: Wake,
        transfer: Option<TransferSpec>,
    ) -> Result<JobSummary> {
        spec.validate()?;
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure!(self.is_active(), "job owner has retired");
        ensure!(
            jobs.values()
                .filter(|job| !job
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .summary
                    .status
                    .finished())
                .count()
                < 8,
            "at most eight jobs may run per owner"
        );
        ensure!(
            jobs.len() < 64,
            "job history is full; forget completed jobs before starting another"
        );
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|error| anyhow::anyhow!("job identity: {error}"))?;
        let id = format!("job-{:032x}", u128::from_le_bytes(random));
        let summary = JobSummary {
            id: id.clone(),
            host: remote
                .as_ref()
                .map_or_else(|| "Local".to_owned(), RemoteHost::destination),
            program: spec.program.clone(),
            cwd: spec.cwd.clone(),
            status: JobStatus::Starting,
            cancel_requested: false,
            timed_out: false,
            next_cursor: 0,
            retained_from: 0,
            transfer: transfer.clone().map(TransferProgress::new),
        };
        let mut summaries = jobs
            .values()
            .map(|job| {
                job.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .summary
                    .clone()
            })
            .collect::<Vec<_>>();
        summaries.push(summary.clone());
        ensure!(
            serde_json::to_vec(&summaries)?.len() <= 512 * 1024,
            "Job metadata history is full; forget completed jobs"
        );
        let job = Arc::new(Job {
            transfer: transfer.clone().map(|spec| (spec, remote.clone())),
            state: Mutex::new(State {
                summary: summary.clone(),
                chunks: VecDeque::new(),
                bytes: 0,
            }),
            changed: Condvar::new(),
            cancel: AtomicBool::new(false),
            controller: Mutex::new(None),
            deadline: Instant::now()
                .checked_add(Duration::from_secs(u64::from(spec.timeout_seconds)))
                .context("job deadline is out of range")?,
            wake,
            changes: self.changes.clone(),
        });
        jobs.insert(id, job.clone());
        drop(jobs);
        job.notify();
        spawn_workers(&job, spec, remote, transfer)?;
        Ok(summary)
    }
    pub fn list(&self) -> Vec<JobSummary> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|job| {
                job.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .summary
                    .clone()
            })
            .collect()
    }
    fn get(&self, id: &str) -> Result<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown job"))
    }
    /// # Errors
    /// Returns an error for an unknown job, an invalid cursor, or an excessive wait.
    pub fn read(&self, id: &str, cursor: u64, wait_ms: u64) -> Result<JobRead> {
        ensure!(wait_ms <= 4000, "job read wait is limited to 4000 ms");
        self.get(id)?.read(cursor, Duration::from_millis(wait_ms))
    }
    /// # Errors
    /// Returns an error if the job does not exist.
    pub fn cancel(&self, id: &str) -> Result<JobSummary> {
        let job = self.get(id)?;
        job.cancel(false);
        let summary = job
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .summary
            .clone();
        Ok(summary)
    }
    /// # Errors
    /// Returns an error if the job is unknown or has not finished.
    pub fn forget(&self, id: &str) -> Result<()> {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(job) = jobs.get(id) else {
            bail!("unknown job")
        };
        ensure!(
            job.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .summary
                .status
                .finished(),
            "cancel and wait for the job before forgetting it"
        );
        jobs.remove(id);
        drop(jobs);
        if let Some(changes) = &self.changes {
            let _ = changes.try_send(());
        }
        Ok(())
    }
}

fn spawn_workers(
    job: &Arc<Job>,
    spec: JobSpec,
    remote: Option<RemoteHost>,
    transfer: Option<TransferSpec>,
) -> Result<()> {
    let timer = job.clone();
    std::thread::Builder::new()
        .name("job-deadline".to_owned())
        .spawn(move || {
            let (state, elapsed) = timer
                .changed
                .wait_timeout_while(
                    timer
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    timer.deadline.saturating_duration_since(Instant::now()),
                    |state| !state.summary.status.finished(),
                )
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(state);
            if elapsed.timed_out() {
                timer.cancel(true);
            }
        })
        .inspect_err(|error| {
            job.status(JobStatus::Failed {
                message: error.to_string(),
            });
        })?;
    let worker = job.clone();
    std::thread::Builder::new()
        .name("host-job".to_owned())
        .spawn(move || {
            let result = if let Some(transfer) = transfer {
                transfer::run(&worker, &transfer, remote.as_ref())
            } else {
                remote.map_or_else(
                    || process::run(&worker, &spec),
                    |remote| remote::run(&worker, &spec, &remote),
                )
            };
            if let Err(error) = result {
                worker.status(JobStatus::Failed {
                    message: format!("{error:#}"),
                });
            }
        })
        .inspect_err(|error| {
            job.status(JobStatus::Failed {
                message: error.to_string(),
            });
        })?;
    Ok(())
}
