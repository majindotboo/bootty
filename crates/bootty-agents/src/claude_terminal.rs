use std::{
    sync::{Arc, Condvar, Mutex, PoisonError},
    thread::{self, JoinHandle},
    time::Duration,
};

use serde::Deserialize;

use crate::{
    AgentLaunch,
    terminal_observation::{AgentObservation, ObservationSink, TerminalAgentStatus},
};

#[derive(Deserialize)]
struct ClaudeSession {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    status: Option<String>,
    state: Option<String>,
    #[serde(rename = "waitingFor")]
    waiting_for: Option<String>,
}

/// Match only the full provider identity. Directory, PID and list order are never identity.
/// # Errors
/// Returns malformed or oversized provider output and duplicate identity errors.
pub fn claude_terminal_observation(
    bytes: &[u8],
    session_id: &str,
) -> Result<Option<AgentObservation>, String> {
    if bytes.len() > 1024 * 1024 {
        return Err("Claude session query exceeds 1 MiB".to_owned());
    }
    let sessions: Vec<ClaudeSession> =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let mut matches = sessions
        .into_iter()
        .filter(|session| session.session_id.as_deref() == Some(session_id));
    let Some(session) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err("Claude returned duplicate session identities".to_owned());
    }
    let status = match session.state.as_deref() {
        Some("failed") => TerminalAgentStatus::Error,
        Some("stopped") => TerminalAgentStatus::Stopped,
        Some("done") => TerminalAgentStatus::Finished,
        _ => match session.status.as_deref().or(session.state.as_deref()) {
            Some("busy" | "working") => TerminalAgentStatus::Working,
            Some("waiting" | "blocked") => TerminalAgentStatus::Waiting,
            Some("idle") => TerminalAgentStatus::Idle,
            _ => TerminalAgentStatus::Unavailable,
        },
    };
    Ok(Some(
        AgentObservation {
            session_id: Some(session_id.to_owned()),
            session_file: None,
            status,
            detail: session.waiting_for,
        }
        .bounded(),
    ))
}

pub struct ClaudeTerminalObserver {
    arguments: Vec<String>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl ClaudeTerminalObserver {
    /// # Errors
    /// Returns unsupported ambiguous session selectors or invalid explicit UUIDs.
    pub fn prepare(launch: &AgentLaunch, sink: ObservationSink) -> Result<Self, String> {
        let mut arguments = launch.arguments.clone();
        let selector = |name: &str| {
            arguments.windows(2).find_map(|pair| {
                pair.first()
                    .filter(|value| *value == name)
                    .and_then(|_| pair.get(1))
                    .cloned()
            })
        };
        let explicit = selector("--session-id");
        let resume = selector("--resume");
        let fork = arguments
            .iter()
            .any(|argument| argument == "--fork-session");
        if arguments
            .iter()
            .any(|argument| matches!(argument.as_str(), "--continue" | "-c"))
            || (arguments.iter().any(|argument| argument == "--resume") && resume.is_none())
        {
            return Err(
                "Choose an exact Claude session before observing a resumed terminal".to_owned(),
            );
        }
        let session_id = if let Some(explicit) = explicit {
            explicit
        } else if let Some(resume) = resume.filter(|_| !fork) {
            resume
        } else {
            let id = crate::terminal_observation::terminal_session_id()?;
            arguments.splice(0..0, ["--session-id".to_owned(), id.clone()]);
            id
        };
        if !valid_uuid(&session_id) {
            return Err("Claude observation requires a full session UUID".to_owned());
        }
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let stopping = stop.clone();
        let program = launch.program.clone();
        let worker = thread::spawn(move || {
            let mut was_working = false;
            loop {
                if *stopping.0.lock().unwrap_or_else(PoisonError::into_inner) {
                    break;
                }
                let queried = crate::terminal_process::query(
                    &program,
                    &["agents", "--json", "--all"],
                    || *stopping.0.lock().unwrap_or_else(PoisonError::into_inner),
                )
                .and_then(|bytes| claude_terminal_observation(&bytes, &session_id));
                let mut observation = match queried {
                    Ok(Some(observation)) => observation,
                    Ok(None) => AgentObservation {
                        status: TerminalAgentStatus::Unavailable,
                        ..Default::default()
                    },
                    Err(error) => AgentObservation {
                        status: TerminalAgentStatus::Unavailable,
                        detail: Some(error),
                        ..Default::default()
                    },
                };
                if observation.status == TerminalAgentStatus::Idle && was_working {
                    observation.status = TerminalAgentStatus::Finished;
                }
                if observation.status == TerminalAgentStatus::Working {
                    was_working = true;
                }
                if matches!(
                    observation.status,
                    TerminalAgentStatus::Unavailable
                        | TerminalAgentStatus::Stopped
                        | TerminalAgentStatus::Error
                ) {
                    was_working = false;
                }
                sink(observation);
                let (guard, _) = stopping
                    .1
                    .wait_timeout_while(
                        stopping.0.lock().unwrap_or_else(PoisonError::into_inner),
                        Duration::from_millis(750),
                        |stop| !*stop,
                    )
                    .unwrap_or_else(PoisonError::into_inner);
                let stopped = *guard;
                drop(guard);
                if stopped {
                    break;
                }
            }
        });
        Ok(Self {
            arguments,
            stop,
            worker: Some(worker),
        })
    }

    #[must_use]
    pub fn arguments(&self) -> Vec<String> {
        self.arguments.clone()
    }
}

impl Drop for ClaudeTerminalObserver {
    fn drop(&mut self) {
        *self.stop.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.stop.1.notify_all();
        if let Some(worker) = self.worker.take() {
            thread::spawn(move || {
                let _ = worker.join();
            });
        }
    }
}

fn valid_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
