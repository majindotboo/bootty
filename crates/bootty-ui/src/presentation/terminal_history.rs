//! Saved provider metadata and explicit resume in the captured session.

use std::{
    path::Path,
    sync::mpsc::{Receiver, TryRecvError},
};

use bootty_agents::{AgentKind, TerminalHistoryEntry};
use bootty_control::{Caller, CommandInvocation, CommandOutcome, CommandTarget};

use crate::clock::Moment;
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
    /// Each provider with sessions older than those shown: the time its next page starts before.
    older: std::collections::HashMap<AgentKind, i64>,
    /// Whether the queries in flight fetch older pages, which add to the list.
    paging: bool,
}

#[derive(serde::Deserialize)]
struct HistoryResponse {
    entries: Vec<TerminalHistoryEntry>,
    account_directory: std::path::PathBuf,
    #[serde(default)]
    omitted_entries: usize,
    /// The provider has older sessions than this page.
    #[serde(default)]
    more: bool,
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
            older: std::collections::HashMap::new(),
            paging: false,
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
        self.older.clear();
        self.paging = false;
        self.list.replace_entries(Vec::new());
        self.entries.clear();
        self.query_index = 0;
        self.provider_query()
    }

    /// Asks each provider with older sessions for its next page.
    fn load_older(&mut self) -> Option<TerminalHistoryEvent> {
        self.paging = true;
        self.query_index = 0;
        if self.older.contains_key(&self.queried().provider) {
            self.provider_query()
        } else {
            self.next_provider()
        }
    }

    fn queried(&self) -> &TerminalHistoryContext {
        self.providers
            .get(self.query_index)
            .unwrap_or(&self.context)
    }

    fn provider_query(&self) -> Option<TerminalHistoryEvent> {
        let context = self.queried();
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
            if self.paging
                && let Some(before) = self.older.get(&context.provider)
            {
                invocation.arguments.push(before.to_string());
            }
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
                        let provider = self.queried().provider;
                        self.accounts.insert(provider, response.account_directory);
                        // The next page starts before the oldest session this one showed.
                        match response
                            .entries
                            .iter()
                            .filter_map(|entry| entry.updated_at)
                            .min()
                        {
                            Some(oldest) if response.more || response.omitted_entries > 0 => {
                                self.older.insert(provider, oldest);
                            }
                            _ => {
                                self.older.remove(&provider);
                            }
                        }
                        for entry in response.entries {
                            if !self.entries.iter().any(|shown| {
                                shown.provider == entry.provider
                                    && shown.session_id == entry.session_id
                            }) {
                                self.entries.push(entry);
                            }
                        }
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
        // Older pages come only from providers that have them.
        while self.paging
            && self.query_index < self.providers.len()
            && !self.older.contains_key(&self.queried().provider)
        {
            self.query_index = self.query_index.saturating_add(1);
        }
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
        let now = chrono::Local::now();
        // Sessions group by when they were last used; a search ranks them instead.
        let dated = self.list.filter().trim().is_empty();
        let mut section = None;
        let mut rows = Vec::new();
        for (index, entry) in self.list.rows().into_iter().enumerate() {
            let updated = entry
                .value
                .updated_at
                .and_then(chrono::DateTime::from_timestamp_millis);
            let heading = history_section(updated, now);
            if dated && section != Some(heading) {
                section = Some(heading);
                rows.push(DialogRow::section(format!("section-{heading}"), heading));
            }
            let mut row = DialogRow::action(
                index.to_string(),
                entry.primary,
                DialogAction::new("open-history"),
            );
            row.detail = entry.secondary.map(str::to_owned);
            row.trailing = Some(history_time(updated, now));
            row.icon = Some(entry.value.provider.icon().to_owned());
            row.enabled = !busy;
            rows.push(row);
        }
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
        if !self.older.is_empty() {
            let mut older =
                DialogRow::action("older", "Load older sessions", DialogAction::new("older"));
            older.enabled = !busy;
            older.icon = Some("history".to_owned());
            rows.push(older);
        }
        let mut spec = DialogSpec::searchable(
            HISTORY_ID,
            if !self.providers.is_empty() {
                "History".to_owned()
            } else if self.context.session.kind == bootty_control::ResourceKind::Binding {
                format!("Resume {} session", self.context.provider.display_name())
            } else {
                format!("{} history", self.context.provider.display_name())
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
                if action.0 == "older" {
                    return self.load_older();
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
    let keywords = vec![
        entry.session_id.clone(),
        entry.cwd.to_string_lossy().into_owned(),
    ];
    let mut row = SearchableEntry::new(entry, primary);
    row.secondary = Some(secondary);
    row.keywords = keywords;
    row
}

/// When a session was last used, as a heading for sessions used about then.
fn history_section(
    updated: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Local>,
) -> &'static str {
    let Some(updated) = updated else {
        return "Undated";
    };
    let days = now
        .date_naive()
        .signed_duration_since(updated.with_timezone(&chrono::Local).date_naive())
        .num_days();
    match days {
        ..=0 => "Today",
        1 => "Yesterday",
        2..=6 => "Previous 7 days",
        7..=29 => "Previous 30 days",
        _ => "Older",
    }
}

/// When a session was last used, as short as its section allows: the time today and
/// yesterday, the weekday this week, then the date.
fn history_time(
    updated: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Local>,
) -> String {
    let Some(updated) = updated.map(|updated| updated.with_timezone(&chrono::Local)) else {
        return String::new();
    };
    let moment = match history_section(Some(updated.to_utc()), now) {
        "Today" | "Yesterday" => Moment::Time,
        "Previous 7 days" => Moment::WeekdayAndTime,
        _ if chrono::Datelike::year(&updated) == chrono::Datelike::year(&now) => Moment::Day,
        _ => Moment::DayAndYear,
    };
    crate::clock::local_moment(updated, moment)
}

fn project_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
