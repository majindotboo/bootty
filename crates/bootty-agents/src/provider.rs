use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A pane identity qualified by the active Space or binding scope. Pane labels such as `%1` are
/// backend local and may be reused after switching Spaces.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AgentPaneKey {
    pub scope: String,
    pub pane: String,
}

impl AgentPaneKey {
    #[must_use]
    pub fn new(scope: impl Into<String>, pane: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            pane: pane.into(),
        }
    }
}

/// A supported interactive agent.  Providers stay separate because their event names and
/// lifecycle meanings are part of the vendor protocol, not one generic agent state machine.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, Serialize, PartialOrd)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Pi,
    Codex,
    Claude,
}

impl AgentKind {
    pub const ALL: [Self; 3] = [Self::Pi, Self::Codex, Self::Claude];

    #[must_use]
    pub const fn module(self) -> &'static str {
        match self {
            Self::Pi => "agents.pi",
            Self::Codex => "agents.codex",
            Self::Claude => "agents.claude",
        }
    }

    #[must_use]
    pub const fn topic(self) -> &'static str {
        match self {
            Self::Pi => "agents.pi.event",
            Self::Codex => "agents.codex.event",
            Self::Claude => "agents.claude.event",
        }
    }

    #[must_use]
    pub const fn default_program(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    #[must_use]
    pub const fn event_kind(self) -> AgentEventKind {
        match self {
            Self::Pi => AgentEventKind::Native,
            Self::Codex | Self::Claude => AgentEventKind::Hook,
        }
    }

    #[must_use]
    pub const fn surface_order(self) -> u16 {
        match self {
            Self::Pi => 900,
            Self::Codex => 901,
            Self::Claude => 902,
        }
    }

    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Codex => "openai",
            Self::Claude => "claude",
        }
    }

    #[must_use]
    pub const fn integration_id(self) -> &'static str {
        match self {
            Self::Pi => "extension",
            Self::Codex | Self::Claude => "hooks",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pi => "pi",
            Self::Codex => "codex",
            Self::Claude => "claude",
        })
    }
}

/// Whether a pane has reported an event yet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AgentSource {
    #[default]
    None,
    Existing,
    /// Reported to a previous Bootty process; current once the pane reports again.
    Restored,
}

impl AgentSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Existing => "existing",
            Self::Restored => "restored",
        }
    }
}

/// Provider-specific attention state.  `Tool` deliberately retains the vendor tool name instead
/// of collapsing it into a generic “busy” bit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum AgentStatus {
    #[default]
    Stopped,
    Idle,
    Working,
    Waiting,
    Tool(String),
    Error,
}

impl AgentStatus {
    /// The agent is actively producing output or running a tool.
    #[must_use]
    pub const fn is_working(&self) -> bool {
        matches!(self, Self::Working | Self::Tool(_))
    }

    #[must_use]
    pub fn as_str(&self) -> String {
        match self {
            Self::Stopped => "stopped".to_owned(),
            Self::Idle => "idle".to_owned(),
            Self::Working => "working".to_owned(),
            Self::Waiting => "waiting".to_owned(),
            Self::Tool(tool) => format!("tool:{tool}"),
            Self::Error => "error".to_owned(),
        }
    }

    /// The status [`Self::as_str`] names.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Some(match label {
            "stopped" => Self::Stopped,
            "idle" => Self::Idle,
            "working" => Self::Working,
            "waiting" => Self::Waiting,
            "error" => Self::Error,
            _ => Self::Tool(label.strip_prefix("tool:")?.to_owned()),
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentAttention {
    Complete,
    Waiting,
    Error,
}

/// Typed state shown by the session row and returned by each provider's `.state` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentState {
    pub provider: AgentKind,
    pub attention: Option<AgentAttention>,
    pub attention_sequence: u64,
    pub acknowledged_sequence: u64,
    pub source: AgentSource,
    pub status: AgentStatus,
    pub session_id: Option<String>,
    pub session_file: Option<String>,
    pub session_name: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub last_event: Option<String>,
    /// Final assistant text of the last finished turn, when the provider reports it. Shared, so
    /// copying state for persistence or a listing does not copy the text.
    pub last_message: Option<Arc<str>>,
    /// Unix milliseconds when the last turn finished (working or tool use became idle or error).
    pub turn_ended_at: Option<u64>,
    pub error: Option<String>,
    pub cwd: Option<String>,
    pub launch: Option<crate::AgentLaunch>,
    /// The backend server the pane runs on, as its hook last reported it (`$TMUX` or `$RMUX`).
    /// Lets the record follow its pane to whichever Space owns it later.
    pub server: Option<String>,
}

impl AgentState {
    #[must_use]
    pub const fn new(provider: AgentKind) -> Self {
        Self {
            provider,
            attention: None,
            attention_sequence: 0,
            acknowledged_sequence: 0,
            source: AgentSource::None,
            status: AgentStatus::Stopped,
            session_id: None,
            session_file: None,
            session_name: None,
            thread_id: None,
            turn_id: None,
            last_event: None,
            last_message: None,
            turn_ended_at: None,
            error: None,
            cwd: None,
            launch: None,
            server: None,
        }
    }

    #[must_use]
    pub const fn unread(&self) -> bool {
        self.attention.is_some() && self.attention_sequence > self.acknowledged_sequence
    }

    /// Status label for chrome. An idle agent with an unread completion reads "complete" so the
    /// finished turn stays visible until acknowledged.
    #[must_use]
    pub fn display_status(&self) -> String {
        if self.unread()
            && self.attention == Some(AgentAttention::Complete)
            && self.status == AgentStatus::Idle
        {
            "complete".to_owned()
        } else {
            self.status.as_str()
        }
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut value = Map::new();
        value.insert(
            "provider".to_owned(),
            Value::String(self.provider.to_string()),
        );
        value.insert(
            "source".to_owned(),
            Value::String(self.source.as_str().to_owned()),
        );
        value.insert("status".to_owned(), Value::String(self.status.as_str()));
        value.insert(
            "attention".to_owned(),
            self.attention.map_or(Value::Null, |attention| {
                match attention {
                    AgentAttention::Complete => "complete",
                    AgentAttention::Waiting => "waiting",
                    AgentAttention::Error => "error",
                }
                .into()
            }),
        );
        value.insert(
            "attention_sequence".to_owned(),
            self.attention_sequence.to_string().into(),
        );
        value.insert(
            "acknowledged_sequence".to_owned(),
            self.acknowledged_sequence.to_string().into(),
        );
        value.insert("unread".to_owned(), self.unread().into());
        insert_optional(&mut value, "session_id", self.session_id.as_deref());
        insert_optional(&mut value, "session_file", self.session_file.as_deref());
        insert_optional(&mut value, "session_name", self.session_name.as_deref());
        insert_optional(&mut value, "thread_id", self.thread_id.as_deref());
        insert_optional(&mut value, "turn_id", self.turn_id.as_deref());
        insert_optional(&mut value, "last_event", self.last_event.as_deref());
        insert_optional(&mut value, "last_message", self.last_message.as_deref());
        if let Some(turn_ended_at) = self.turn_ended_at {
            value.insert("turn_ended_at".to_owned(), turn_ended_at.to_string().into());
        }
        insert_optional(&mut value, "error", self.error.as_deref());
        insert_optional(&mut value, "cwd", self.cwd.as_deref());
        if let Some(launch) = &self.launch {
            value.insert("launch".to_owned(), launch.to_value());
        }
        Value::Object(value)
    }
}

fn insert_optional(map: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        map.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

/// The protocol shape used by the control event stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentEventKind {
    Native,
    Hook,
}

impl AgentEventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Hook => "hook",
        }
    }
}

impl AgentKind {
    #[must_use]
    pub const fn account_directory_variable(self) -> &'static str {
        match self {
            Self::Codex => "CODEX_HOME",
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Pi => "PI_CODING_AGENT_DIR",
        }
    }
}
