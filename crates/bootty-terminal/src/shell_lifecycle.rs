//! Shell-reported lifecycle. Time is supplied by the host; terminal output is never a heuristic.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellEvent {
    PromptStart,
    PromptEnd,
    CommandStart,
    CommandFinish { exit_code: Option<i32> },
}

impl ShellEvent {
    #[must_use]
    pub fn parse_osc133(value: &str) -> Option<Self> {
        let mut fields = value.split(';');
        match fields.next()? {
            "A" => Some(Self::PromptStart),
            "B" => Some(Self::PromptEnd),
            "C" => Some(Self::CommandStart),
            "D" => Some(Self::CommandFinish {
                exit_code: match fields.next() {
                    None | Some("") => None,
                    Some(code) => Some(code.parse().ok()?),
                },
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandCompletion {
    pub elapsed: Duration,
    pub exit_code: Option<i32>,
}

#[derive(Default)]
pub struct ShellLifecycle {
    started: Option<Instant>,
}

impl ShellLifecycle {
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.started.is_some()
    }
    pub fn apply(&mut self, event: ShellEvent, now: Instant) -> Option<CommandCompletion> {
        match event {
            ShellEvent::CommandStart => {
                self.started.get_or_insert(now);
            }
            ShellEvent::PromptStart => {
                self.started = None;
            }
            ShellEvent::PromptEnd => {}
            ShellEvent::CommandFinish { exit_code } => {
                return self.started.take().map(|started| CommandCompletion {
                    elapsed: now.saturating_duration_since(started),
                    exit_code,
                });
            }
        }
        None
    }
}
