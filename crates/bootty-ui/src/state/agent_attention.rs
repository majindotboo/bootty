use std::collections::HashMap;

use bootty_agents::{AgentKind, AgentPaneKey};
use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandTarget, ResourceKind};
use serde::{Deserialize, Serialize};

use super::{AppEffect, AppState};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentOverview {
    pub provider: AgentKind,
    pub target: CommandTarget,
    pub scope: String,
    pub pane: String,
    pub host: String,
    pub title: String,
    pub status: String,
    pub unread: bool,
    pub attention_sequence: String,
    pub can_resume: bool,
    pub cwd: Option<String>,
    /// `existing` once the pane reported to this process; `restored` while it shows what it
    /// reported before a restart.
    pub source: String,
    pub session_id: Option<String>,
    /// Provider transcript or session file (Claude's `transcript_path`).
    pub session_file: Option<String>,
    pub last_event: Option<String>,
    /// The start of the last turn's final text, at most [`LAST_MESSAGE_PREVIEW`] bytes.
    pub last_message: Option<String>,
    /// Whether `last_message` was cut; `agents.<provider>.state` on `target` has all of it.
    pub last_message_truncated: bool,
    /// Unix milliseconds, as a decimal string like other sequence values.
    pub turn_ended_at: Option<String>,
}

/// Deliberate limit: `agents.list` answers in one control response of at most 1 MiB, so each entry
/// carries only this much of its final message. Paginate the listing before raising it, or once a
/// workspace runs several hundred agents.
const LAST_MESSAGE_PREVIEW: usize = 1024;

/// The first [`LAST_MESSAGE_PREVIEW`] bytes of `message`, and whether anything was cut.
pub fn message_preview(message: Option<&str>) -> (Option<String>, bool) {
    let Some(message) = message else {
        return (None, false);
    };
    let preview = message
        .get(..message.floor_char_boundary(LAST_MESSAGE_PREVIEW))
        .unwrap_or_default();
    (Some(preview.to_owned()), preview.len() < message.len())
}

#[derive(Default)]
pub(super) struct AgentNotifications {
    observed: HashMap<(AgentKind, AgentPaneKey), u64>,
    acknowledgements: HashMap<(AgentKind, AgentPaneKey), u64>,
}

impl AppState {
    pub fn agent_overview(&self) -> Vec<AgentOverview> {
        let Some(agents) = self.agent_service() else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for provider in AgentKind::ALL {
            for (key, state) in agents.pane_states(provider) {
                let Some(binding) = self
                    .workspace
                    .all_bindings()
                    .find(|binding| binding.scope().persistence_value().to_string() == key.scope)
                else {
                    continue;
                };
                for session in binding.member_sessions() {
                    for window in &session.windows {
                        if !std::iter::once(&window.anchor)
                            .chain(&window.panes)
                            .any(|pane| pane.pane_id.as_deref() == Some(&key.pane))
                        {
                            continue;
                        }
                        let Some(generation) =
                            binding
                                .mux()
                                .terminal_generation(&session.id, &window.id, &key.pane)
                        else {
                            continue;
                        };
                        let handle = self.binding_target_handle(
                            binding.scope(),
                            binding.mux().binding_generation(),
                        );
                        let target = CommandTarget {
                            kind: ResourceKind::Terminal,
                            handle: serde_json::Value::from(vec![
                                handle.as_str(),
                                session.id.as_str(),
                                window.id.as_str(),
                                key.pane.as_str(),
                            ])
                            .to_string(),
                            generation,
                        };
                        let (last_message, last_message_truncated) =
                            message_preview(state.last_message.as_deref());
                        result.push(AgentOverview {
                            provider,
                            target,
                            scope: key.scope.clone(),
                            pane: key.pane.clone(),
                            host: binding.multiplexer().remote.as_ref().map_or_else(
                                || "Local".to_owned(),
                                bootty_mux::RemoteTarget::label,
                            ),
                            title: state
                                .session_name
                                .clone()
                                .unwrap_or_else(|| session.name.clone()),
                            status: state.display_status(),
                            unread: state.unread(),
                            attention_sequence: state.attention_sequence.to_string(),
                            can_resume: (state.session_id.is_some()
                                || state.thread_id.is_some()
                                || state.session_file.is_some())
                                && !state.launch.as_ref().is_some_and(|launch| launch.ephemeral),
                            cwd: state.cwd.clone(),
                            source: state.source.as_str().to_owned(),
                            session_id: state.session_id.clone(),
                            session_file: state.session_file.clone(),
                            last_event: state.last_event.clone(),
                            last_message,
                            last_message_truncated,
                            turn_ended_at: state.turn_ended_at.map(|at| at.to_string()),
                        });
                    }
                }
            }
        }
        result.sort_by(|a, b| {
            b.unread
                .cmp(&a.unread)
                .then_with(|| a.host.cmp(&b.host))
                .then_with(|| a.title.cmp(&b.title))
                .then_with(|| a.pane.cmp(&b.pane))
        });
        result
    }

    pub(super) fn sync_agent_attention(
        &mut self,
        window_focused: bool,
        effects: &mut Vec<AppEffect>,
    ) {
        let Some(agents) = self.agent_service() else {
            return;
        };
        let entries = self.agent_overview();
        let focused = self.focused_pane();
        let scope = self
            .workspace
            .active
            .binding
            .scope()
            .persistence_value()
            .to_string();
        let mut live = HashMap::new();
        let mut acknowledgements = HashMap::new();
        for entry in entries {
            let key = AgentPaneKey::new(&entry.scope, &entry.pane);
            let state =
                agents.snapshot_scoped(entry.provider, Some(&entry.scope), Some(&entry.pane));
            let observed = self
                .agent_notifications
                .observed
                .get(&(entry.provider, key.clone()))
                .copied()
                .unwrap_or_default();
            live.insert((entry.provider, key.clone()), state.attention_sequence);
            let visible = window_focused
                && self.terminal_focused()
                && entry.scope == scope
                && focused.as_deref() == Some(&entry.pane);
            if entry.unread
                && state.attention_sequence > observed
                && self.config().session.agent_notifications.allows(visible)
            {
                effects.push(AppEffect::DesktopNotification {
                    title: format!("{}: {}", entry.provider, entry.status),
                    body: format!("{} · {}", entry.host, entry.title),
                });
            }
            if entry.unread && visible {
                if self
                    .agent_notifications
                    .acknowledgements
                    .get(&(entry.provider, key.clone()))
                    .is_some_and(|sequence| *sequence >= state.attention_sequence)
                {
                    acknowledgements.insert((entry.provider, key), state.attention_sequence);
                    continue;
                }
                let mut command = CommandInvocation::from_action(
                    &format!("agents.{}.acknowledge", entry.provider),
                    Caller::Internal,
                );
                command.target = Some(entry.target);
                command.arguments = vec![entry.attention_sequence];
                // The command acknowledges only the displayed sequence; a later event remains unread.
                if self
                    .app_command_sender(Caller::Internal)
                    .submit(
                        command,
                        {
                            let now = std::time::Instant::now();
                            now.checked_add(std::time::Duration::from_secs(5))
                                .unwrap_or(now)
                        },
                        CommandCancellation::new(),
                    )
                    .is_ok()
                {
                    acknowledgements.insert((entry.provider, key), state.attention_sequence);
                }
            }
        }
        self.agent_notifications.observed = live;
        self.agent_notifications.acknowledgements = acknowledgements;
    }
}
