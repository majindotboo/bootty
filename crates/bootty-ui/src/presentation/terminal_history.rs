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

#[derive(Clone)]
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
    Opened(serde_json::Value),
    Close,
}

pub struct TerminalHistoryDialog {
    context: TerminalHistoryContext,
    providers: Vec<TerminalHistoryContext>,
    query_index: usize,
    accounts: std::collections::HashMap<AgentKind, std::path::PathBuf>,
    entries: Vec<TerminalHistoryEntry>,
    provider_filter: Option<AgentKind>,
    provider_errors: std::collections::HashMap<AgentKind, String>,
    all_projects: bool,
    list: SearchableList<TerminalHistoryEntry>,
    pending: Option<(Receiver<CommandOutcome>, bool)>,
    error: Option<String>,
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
        let all_projects = context.cwd.is_none();
        Self {
            context,
            providers: Vec::new(),
            query_index: 0,
            accounts: std::collections::HashMap::new(),
            entries: Vec::new(),
            provider_filter: None,
            provider_errors: std::collections::HashMap::new(),
            all_projects,
            list: SearchableList::new(Vec::new()),
            pending: None,
            error: None,
            omitted_entries: 0,
        }
    }

    /// Browse all configured providers in one captured host, without starting an agent.
    #[must_use]
    pub fn all_providers(contexts: Vec<TerminalHistoryContext>) -> Option<Self> {
        let mut dialog = Self::new(contexts.first()?.clone());
        dialog.providers = contexts;
        dialog.all_projects = true;
        Some(dialog)
    }

    #[must_use]
    pub fn is_in_flight(&self) -> bool {
        self.pending.as_ref().is_some_and(|(_, opening)| *opening)
    }

    pub fn query(&mut self) -> Option<TerminalHistoryEvent> {
        self.error = None;
        self.provider_errors.clear();
        self.omitted_entries = 0;
        self.list.replace_entries(Vec::new());
        self.entries.clear();
        self.query_index = 0;
        self.provider_query()
    }

    fn provider_query(&self) -> Option<TerminalHistoryEvent> {
        let context = self
            .providers
            .get(self.query_index)
            .unwrap_or(&self.context);
        let cwd = if self.all_projects {
            String::new()
        } else if let Some(cwd) = &self.context.cwd {
            cwd.clone()
        } else {
            return None;
        };
        let mut invocation = CommandInvocation::new(
            format!("agents.{}.history", context.provider),
            vec![cwd, context.profile.clone()],
            Caller::Internal,
        );
        if let Some(account) = self.accounts.get(&context.provider) {
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

    pub fn failed(&mut self, message: &str) {
        self.pending = None;
        self.error = Some(bootty_agents::native_error_message(message));
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
                        let provider = self
                            .providers
                            .get(self.query_index)
                            .unwrap_or(&self.context)
                            .provider;
                        self.accounts.insert(provider, response.account_directory);
                        self.omitted_entries = self
                            .omitted_entries
                            .saturating_add(response.omitted_entries);
                        self.entries.extend(response.entries);
                        self.refresh_entries();
                    }
                    Err(_) => {
                        self.failed("Provider history returned unsupported metadata");
                    }
                }
                self.next_provider()
            }
            CommandOutcome::Success { value, .. } => Some(TerminalHistoryEvent::Opened(value)),
            outcome => {
                if opening || self.providers.is_empty() {
                    self.error = crate::commands::command_outcome_message(&outcome)
                        .map(|message| bootty_agents::native_error_message(&message));
                } else if let Some(context) = self.providers.get(self.query_index) {
                    self.provider_errors.insert(
                        context.provider,
                        format!(
                            "{} history could not load. Check its account in Settings.",
                            context.provider
                        ),
                    );
                }
                if opening { None } else { self.next_provider() }
            }
        }
    }

    fn next_provider(&mut self) -> Option<TerminalHistoryEvent> {
        self.query_index = self.query_index.saturating_add(1);
        (self.query_index < self.providers.len())
            .then(|| self.provider_query())
            .flatten()
    }

    fn refresh_entries(&mut self) {
        self.entries
            .sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));
        self.list.replace_entries(
            self.entries
                .iter()
                .filter(|entry| {
                    self.provider_filter
                        .is_none_or(|provider| entry.provider == provider)
                })
                .cloned()
                .map(history_entry)
                .collect(),
        );
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let busy = self.pending.is_some();
        let error = self.visible_error();
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
                row.icon = Some(entry.value.provider.icon().to_owned());
                row.enabled = !busy;
                row
            })
            .collect::<Vec<_>>();
        if rows.is_empty() || error.is_some() || busy {
            let mut status = DialogRow::action(
                "status",
                error.unwrap_or_else(|| {
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
        scope.enabled = !busy && self.context.cwd.is_some();
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
            if !self.providers.is_empty() {
                "History".to_owned()
            } else if self.context.session.kind == bootty_control::ResourceKind::Binding {
                format!("Resume {} session", self.context.provider)
            } else {
                format!("{} history", self.context.provider)
            },
            self.list.filter(),
            rows,
        );
        spec.hint = Some("Enter open selected session   Esc close".to_owned());
        spec.text_hint = Some("Search conversations…".to_owned());
        if !self.providers.is_empty() {
            spec.placement = crate::gpui::DialogPlacement::FullView;
            spec.fields.push(crate::gpui::DialogField {
                id: "provider".into(),
                label: "Provider".into(),
                placeholder: String::new(),
                value: self
                    .provider_filter
                    .map_or_else(|| "All providers".into(), |provider| provider.to_string()),
                kind: crate::gpui::DialogFieldKind::Choice(vec![
                    "All providers".into(),
                    "codex".into(),
                    "claude".into(),
                    "pi".into(),
                ]),
            });
        }
        spec.footer = Some(self.footer());
        spec
    }

    fn visible_error(&self) -> Option<String> {
        self.error.clone().or_else(|| {
            self.provider_filter.map_or_else(
                || {
                    self.providers
                        .iter()
                        .find_map(|context| self.provider_errors.get(&context.provider).cloned())
                },
                |provider| self.provider_errors.get(&provider).cloned(),
            )
        })
    }

    fn footer(&self) -> String {
        format!(
            "{} · {} · {}",
            if self.providers.is_empty() {
                self.context.provider.to_string()
            } else {
                self.provider_filter
                    .map_or_else(|| "All providers".into(), |provider| provider.to_string())
            },
            if !self.providers.is_empty() {
                "Configured accounts"
            } else if self.context.profile.is_empty() {
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
        )
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
            DialogIntent::FieldChanged { field, value, .. } if field == "provider" => {
                self.provider_filter = match value.as_str() {
                    "codex" => Some(AgentKind::Codex),
                    "claude" => Some(AgentKind::Claude),
                    "pi" => Some(AgentKind::Pi),
                    _ => None,
                };
                self.refresh_entries();
                None
            }
            DialogIntent::SelectionChanged { row, .. } => {
                self.select(row);
                None
            }
            DialogIntent::Activate { action, row, .. } if self.pending.is_none() => {
                if action.0 == "toggle-scope" && self.context.cwd.is_some() {
                    self.all_projects = !self.all_projects;
                    return self.query();
                }
                if action.0 != "open-history" {
                    return None;
                }
                self.select(row);
                let entry = self.list.selected_value()?;
                if self.context.session.kind == bootty_control::ResourceKind::Binding {
                    let invocation = self.native_resume_invocation(entry)?;
                    return Some(TerminalHistoryEvent::Request {
                        invocation,
                        opening: true,
                    });
                }
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

    fn native_resume_invocation(&self, entry: &TerminalHistoryEntry) -> Option<CommandInvocation> {
        use sha2::{Digest as _, Sha256};
        use std::fmt::Write as _;
        let context = self
            .providers
            .iter()
            .find(|context| context.provider == entry.provider)
            .unwrap_or(&self.context);
        let account = Sha256::digest(
            format!(
                "{}\n{}",
                entry.account_directory.display(),
                entry.session_id
            )
            .as_bytes(),
        );
        let fingerprint = account.iter().fold(String::new(), |mut text, byte| {
            _ = write!(text, "{byte:02x}");
            text
        });
        let short: String = fingerprint.chars().take(8).collect();
        let identity = format!(
            "import-{}-{fingerprint}-{}",
            entry.provider, entry.session_id
        );
        let title = entry
            .title
            .as_deref()
            .unwrap_or("Untitled conversation")
            .chars()
            .take(64)
            .collect();
        let mut invocation = CommandInvocation::new(
            "agents.native.start",
            vec![
                entry.provider.to_string(),
                entry.cwd.to_string_lossy().into_owned(),
                context.program.clone(),
                serde_json::to_string(&context.arguments).ok()?,
                format!("agent-{}-{short}", entry.provider),
                context.profile.clone(),
                identity,
                title,
                String::new(),
                entry.account_directory.to_string_lossy().into_owned(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                "false".into(),
                serde_json::to_string(entry).ok()?,
            ],
            Caller::Internal,
        );
        invocation.target = Some(self.context.binding.clone());
        Some(invocation)
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
