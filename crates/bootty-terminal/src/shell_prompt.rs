//! Explicit shell prompt leases. User input invalidates a lease before it reaches the PTY.
use crate::shell_lifecycle::ShellEvent;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptReport {
    Ready {
        shell: String,
        history_file: String,
        editable: bool,
    },
    Command(String),
}
impl PromptReport {
    pub fn parse(value: &str) -> Option<Self> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let mut fields = value.split(';');
        match fields.next()? {
            "P" => {
                let shell = fields.next()?;
                if !matches!(shell, "bash" | "zsh" | "fish") {
                    return None;
                }
                let path = fields.next()?;
                if path.len() > 8192 {
                    return None;
                }
                let history_file = String::from_utf8(STANDARD.decode(path).ok()?).ok()?;
                if history_file.contains(['\0', '\x1b']) {
                    return None;
                }
                let editable = fields.next()? == "1";
                Some(Self::Ready {
                    shell: shell.to_owned(),
                    history_file,
                    editable,
                })
            }
            "E" => {
                let encoded = fields.next()?;
                if encoded.len() > 24 * 1024 {
                    return None;
                }
                let command = String::from_utf8(STANDARD.decode(encoded).ok()?).ok()?;
                if command.is_empty()
                    || command.starts_with(char::is_whitespace)
                    || command.contains(['\0', '\x1b'])
                {
                    return None;
                }
                Some(Self::Command(command))
            }
            _ => None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellCommandRecord {
    pub command: String,
    pub cwd: String,
    pub timestamp: u64,
    pub exit_code: Option<i32>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptSnapshot {
    pub revision: u64,
    pub editable: bool,
    pub shell: String,
    pub history_file: String,
    pub cwd: String,
    pub recent: Vec<ShellCommandRecord>,
}
#[allow(
    clippy::struct_excessive_bools,
    reason = "Prompt readiness, user edits, and shell integration support are independent facts."
)]
pub struct ShellPrompt {
    revision: u64,
    ready: bool,
    untouched: bool,
    at_prompt: bool,
    entered: bool,
    typeahead: bool,
    shell: String,
    history_file: String,
    pending: Option<ShellCommandRecord>,
    recent: VecDeque<ShellCommandRecord>,
}
impl Default for ShellPrompt {
    fn default() -> Self {
        Self {
            revision: 0,
            ready: false,
            untouched: true,
            at_prompt: false,
            entered: false,
            typeahead: false,
            shell: String::new(),
            history_file: String::new(),
            pending: None,
            recent: VecDeque::new(),
        }
    }
}
impl ShellPrompt {
    /// An Enter observed at a reported prompt starts a fresh input cycle. Input after it,
    /// or while a command runs, may already be queued in the shell when the next prompt draws.
    pub const fn input(&mut self, enter: bool) {
        if enter && self.at_prompt {
            self.typeahead = false;
        } else if self.entered || !self.at_prompt {
            self.typeahead = true;
        }
        if enter {
            self.entered = true;
            self.at_prompt = false;
        }
        self.invalidate();
    }

    pub const fn invalidate(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.ready = false;
        self.untouched = false;
    }
    pub fn lifecycle(&mut self, event: ShellEvent) {
        match event {
            ShellEvent::PromptStart => {
                self.revision = self.revision.saturating_add(1);
                self.ready = false;
            }
            ShellEvent::CommandStart => {
                self.at_prompt = false;
                self.invalidate();
            }
            ShellEvent::CommandFinish { exit_code } => {
                self.invalidate();
                self.untouched = !self.typeahead;
                self.entered = false;
                self.at_prompt = false;
                if let Some(mut record) = self.pending.take() {
                    record.exit_code = exit_code;
                    self.recent.push_back(record);
                    while self.recent.len() > 128
                        || self
                            .recent
                            .iter()
                            .map(|entry| entry.command.len().saturating_add(entry.cwd.len()))
                            .sum::<usize>()
                            > 128 * 1024
                    {
                        self.recent.pop_front();
                    }
                }
            }
            ShellEvent::PromptEnd => {}
        }
    }
    pub fn report(&mut self, report: &PromptReport, cwd: &str, timestamp: u64) {
        match report {
            PromptReport::Ready {
                shell,
                history_file,
                editable,
            } => {
                self.at_prompt = true;
                self.shell.clone_from(shell);
                self.history_file.clone_from(history_file);
                self.ready = *editable && self.untouched;
            }
            PromptReport::Command(command) => {
                self.pending = Some(ShellCommandRecord {
                    command: command.clone(),
                    cwd: cwd.to_owned(),
                    timestamp,
                    exit_code: None,
                });
            }
        }
    }
    #[must_use]
    pub fn snapshot(&self, cwd: String, terminal_allows: bool) -> PromptSnapshot {
        PromptSnapshot {
            revision: self.revision,
            editable: self.ready && terminal_allows,
            shell: self.shell.clone(),
            history_file: self.history_file.clone(),
            cwd,
            recent: self.recent.iter().cloned().collect(),
        }
    }
    ///
    /// # Errors
    /// Returns an error if the prompt revision is stale or the terminal no longer allows prompt editing.
    pub fn claim(&mut self, revision: u64, terminal_allows: bool) -> Result<(), String> {
        if !self.ready || !terminal_allows || self.revision != revision {
            return Err("Prompt changed or shell owns its input; return to an empty supported prompt and refresh".to_owned());
        }
        self.invalidate();
        Ok(())
    }
}
