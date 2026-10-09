//! Saved provider metadata and explicit resume in the captured session.

use std::{
    path::Path,
    sync::mpsc::{Receiver, TryRecvError},
};

use bootty_agents::{AgentKind, TerminalHistoryEntry};
use bootty_control::{Caller, CommandInvocation, CommandOutcome, CommandTarget};

use crate::gpui::{DialogAction, DialogIntent, DialogRow, DialogSpec, RowId};
use crate::product_dialogs::searchable::{SearchableEntry, SearchableIntent, SearchableList};

const HISTORY_ID: &str = "terminal-agent-history";

pub struct TerminalHistoryContext {
    pub provider: AgentKind,
    pub binding: CommandTarget,
    pub session: CommandTarget,
    pub cwd: Option<String>,
    pub profile: String,
    pub program: String,
    pub arguments: Vec<String>,
}

pub enum TerminalHistoryEvent {
    Request {
        invocation: CommandInvocation,
        opening: bool,
    },
    Close,
}

pub struct TerminalHistoryDialog {
    context: TerminalHistoryContext,
    all_projects: bool,
    list: SearchableList<TerminalHistoryEntry>,
    pending: Option<(Receiver<CommandOutcome>, bool)>,
    error: Option<String>,
    account_directory: Option<std::path::PathBuf>,
    omitted_entries: usize,
}

#[derive(serde::Deserialize)]
struct HistoryResponse {
    entries: Vec<TerminalHistoryEntry>,
    account_directory: std::path::PathBuf,
    #[serde(default)]
    omitted_entries: usize,
}

impl TerminalHistoryDialog {
    #[must_use]
    pub fn new(context: TerminalHistoryContext) -> Self {
        Self {
            context,
            all_projects: false,
            list: SearchableList::new(Vec::new()),
            pending: None,
            error: None,
            account_directory: None,
            omitted_entries: 0,
        }
    }

    #[must_use]
    pub fn is_in_flight(&self) -> bool {
        self.pending.as_ref().is_some_and(|(_, opening)| *opening)
    }

    pub fn query(&mut self) -> Option<TerminalHistoryEvent> {
        self.error = None;
        self.omitted_entries = 0;
        self.list.replace_entries(Vec::new());
        let cwd = if self.all_projects {
            String::new()
        } else if let Some(cwd) = &self.context.cwd {
            cwd.clone()
        } else {
            self.error = Some(
                "The current session has no project directory. Choose All projects.".to_owned(),
            );
            return None;
        };
        let mut invocation = CommandInvocation::new(
            format!("agents.{}.history", self.context.provider),
            vec![cwd, self.context.profile.clone()],
            Caller::Internal,
        );
        if let Some(account) = &self.account_directory {
            invocation
                .arguments
                .push(account.to_string_lossy().into_owned());
        }
        invocation.target = Some(self.context.binding.clone());
        Some(TerminalHistoryEvent::Request {
            invocation,
            opening: false,
        })
    }

    pub fn started(&mut self, response: Receiver<CommandOutcome>, opening: bool) {
        self.pending = Some((response, opening));
    }

    pub fn failed(&mut self, message: String) {
        self.pending = None;
        self.error = Some(message);
    }

    pub fn poll(&mut self) -> Option<TerminalHistoryEvent> {
        let (response, opening) = self.pending.as_ref()?;
        let outcome = match response.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => CommandOutcome::Failed {
                code: "shutdown".to_owned(),
                message: "Provider command host stopped".to_owned(),
            },
        };
        let opening = *opening;
        self.pending = None;
        match outcome {
            CommandOutcome::Success { value, .. } if !opening => {
                match serde_json::from_value::<HistoryResponse>(value) {
                    Ok(response) => {
                        self.account_directory = Some(response.account_directory);
                        self.omitted_entries = response.omitted_entries;
                        self.list.replace_entries(
                            response.entries.into_iter().map(history_entry).collect(),
                        );
                    }
                    Err(_) => {
                        self.failed("Provider history returned unsupported metadata".to_owned());
                    }
                }
                None
            }
            CommandOutcome::Success { .. } => Some(TerminalHistoryEvent::Close),
            outcome => {
                self.error = crate::commands::command_outcome_message(&outcome);
                None
            }
        }
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let busy = self.pending.is_some();
        let mut rows = self
            .list
            .rows()
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                let mut row = DialogRow::action(
                    index.to_string(),
                    entry.primary,
                    DialogAction::new("open-history"),
                );
                row.detail = entry.secondary.map(str::to_owned);
                row.trailing = entry.trailing.map(str::to_owned);
                row.icon = Some(self.context.provider.icon().to_owned());
                row.enabled = !busy;
                row
            })
            .collect::<Vec<_>>();
        if rows.is_empty() || self.error.is_some() || busy {
            let mut status = DialogRow::action(
                "status",
                self.error.clone().unwrap_or_else(|| {
                    if self.is_in_flight() {
                        "Opening session…"
                    } else if busy {
                        "Loading provider history…"
                    } else {
                        "No saved sessions match this scope"
                    }
                    .to_owned()
                }),
                DialogAction::new("status"),
            );
            status.enabled = false;
            rows.insert(0, status);
        }
        let mut scope = DialogRow::action(
            "scope",
            if self.all_projects {
                "Current project"
            } else {
                "All projects"
            },
            DialogAction::new("toggle-scope"),
        );
        scope.enabled = !busy;
        scope.icon = Some("folder".to_owned());
        rows.push(scope);
        if self.omitted_entries > 0 {
            let mut notice = DialogRow::action(
                "truncated",
                format!(
                    "{} additional sessions omitted from this response. Narrow the project scope.",
                    self.omitted_entries
                ),
                DialogAction::new("status"),
            );
            notice.enabled = false;
            rows.push(notice);
        }
        let mut spec = DialogSpec::searchable(
            HISTORY_ID,
            format!("{} history", self.context.provider),
            self.list.filter(),
            rows,
        );
        spec.hint = Some("Enter open selected session   Esc close".to_owned());
        spec.text_hint = Some("Search conversations…".to_owned());
        spec.footer = Some(format!(
            "{} · {} · {}",
            self.context.provider,
            if self.context.profile.is_empty() {
                "Default account"
            } else {
                &self.context.profile
            },
            if self.all_projects {
                "All projects".to_owned()
            } else {
                self.context.cwd.as_deref().map_or_else(
                    || "Project unavailable".to_owned(),
                    |cwd| project_label(Path::new(cwd)),
                )
            }
        ));
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<TerminalHistoryEvent> {
        if intent.dialog_id().0 != HISTORY_ID || self.is_in_flight() {
            return None;
        }
        match intent {
            DialogIntent::Dismiss { .. } => Some(TerminalHistoryEvent::Close),
            DialogIntent::TextChanged { value, .. } => {
                self.list.apply(SearchableIntent::SetFilter(value.clone()));
                None
            }
            DialogIntent::SelectionChanged { row, .. } => {
                self.select(row);
                None
            }
            DialogIntent::Activate { action, row, .. } if self.pending.is_none() => {
                if action.0 == "toggle-scope" {
                    self.all_projects = !self.all_projects;
                    return self.query();
                }
                if action.0 != "open-history" {
                    return None;
                }
                self.select(row);
                let entry = self.list.selected_value()?;
                let mut invocation = CommandInvocation::new(
                    format!("agents.{}.resume", self.context.provider),
                    vec![
                        entry.resume_id.clone(),
                        entry.cwd.to_string_lossy().into_owned(),
                        self.context.program.clone(),
                        serde_json::to_string(&self.context.arguments).ok()?,
                        self.context.profile.clone(),
                        entry.account_directory.to_string_lossy().into_owned(),
                    ],
                    Caller::Internal,
                );
                invocation.target = Some(self.context.session.clone());
                Some(TerminalHistoryEvent::Request {
                    invocation,
                    opening: true,
                })
            }
            _ => None,
        }
    }

    fn select(&mut self, row: &RowId) {
        if let Ok(index) = row.0.parse() {
            self.list.apply(SearchableIntent::Select(index));
        }
    }
}

fn history_entry(entry: TerminalHistoryEntry) -> SearchableEntry<TerminalHistoryEntry> {
    let primary = entry
        .title
        .clone()
        .unwrap_or_else(|| "Untitled conversation".to_owned());
    let secondary = project_label(&entry.cwd);
    let date = |epoch: Option<i64>| {
        epoch
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map_or_else(
                || "unavailable".to_owned(),
                |date| {
                    date.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                },
            )
    };
    let trailing = format!("Updated {}", date(entry.updated_at));
    let keywords = vec![
        entry.session_id.clone(),
        entry.cwd.to_string_lossy().into_owned(),
    ];
    let mut row = SearchableEntry::new(entry, primary);
    row.secondary = Some(secondary);
    row.trailing = Some(trailing);
    row.keywords = keywords;
    row
}

fn project_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
