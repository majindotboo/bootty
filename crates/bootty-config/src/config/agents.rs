use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ConfigLoadError;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseStreaming {
    #[default]
    Live,
    Paragraph,
    Complete,
}

impl ResponseStreaming {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Paragraph => "paragraph",
            Self::Complete => "complete",
        }
    }
    /// What of a reply's `text` shows while it streams. Live holds back an inline span whose
    /// closing marker hasn't arrived, so `**`, backticks and links don't show raw and then
    /// change; the shown text only ever grows. Paragraph shows finished blocks only.
    #[must_use]
    pub fn visible_text(self, text: &str, complete: bool) -> &str {
        if complete {
            return text;
        }
        if self == Self::Complete {
            return "";
        }
        let (boundary, fence) = block_boundary(text);
        if self == Self::Paragraph {
            return text.get(..boundary).unwrap_or_default();
        }
        let line = text.rfind('\n').map_or(0, |at| at.saturating_add(1));
        if let Some((opened, marker)) = fence {
            let last = text.get(line..).unwrap_or_default();
            let start = last.trim_start();
            let indent = last.len().saturating_sub(start.len());
            // An opening line is a fence only once it ends, since a backtick may yet make it
            // code, and a line that starts, or may yet start, with the marker may close it.
            let closing =
                indent <= 3 && (start.is_empty() || start.starts_with(char::from(marker)));
            return if opened == line || closing {
                text.get(..line.saturating_add(indent)).unwrap_or_default()
            } else {
                text
            };
        }
        let held = text
            .get(line..)
            .and_then(unclosed_markup)
            .map_or(text.len(), |at| line.saturating_add(at));
        text.get(..held).unwrap_or(text)
    }
}

/// Where `text`'s last finished Markdown block ends, never inside a fenced code block, and
/// where and with which marker the fenced code block its end is inside opens, if it is.
fn block_boundary(text: &str) -> (usize, Option<(usize, u8)>) {
    let mut fence: Option<(u8, usize, usize)> = None;
    let mut boundary = 0;
    let mut offset: usize = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if line.len().saturating_sub(trimmed.len()) <= 3 {
            let marker = trimmed.as_bytes().first().copied();
            if let Some(marker @ (b'`' | b'~')) = marker {
                let length = trimmed.bytes().take_while(|byte| *byte == marker).count();
                if length >= 3 {
                    let rest = trimmed.get(length..).unwrap_or_default();
                    if fence.is_some_and(|(opening, count, _)| opening == marker && length >= count)
                        && rest.trim().is_empty()
                    {
                        fence = None;
                    } else if fence.is_none() && (marker != b'`' || !rest.contains('`')) {
                        fence = Some((marker, length, offset));
                    }
                }
            }
        }
        offset = offset.saturating_add(line.len());
        if fence.is_none() && line.trim().is_empty() {
            boundary = offset;
        }
    }
    (boundary, fence.map(|(marker, _, opened)| (opened, marker)))
}

/// Where the earliest inline span in `line` opens that hasn't closed: a code span, `**`, `__`
/// or `~~`, or a bracket that may still become a link. Single `*`, `_` and `~` stay, since lists
/// and names use them, except at the very end, where the next character may pair them.
fn unclosed_markup(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut open: Vec<(u8, usize)> = Vec::new();
    let mut stopped = None;
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'\\' => at = at.saturating_add(2),
            b'`' => {
                let run = bytes
                    .get(at..)
                    .map_or(0, |rest| rest.iter().take_while(|b| **b == b'`').count());
                let after = at.saturating_add(run);
                let ticks = line.get(at..after).unwrap_or_default();
                if let Some(end) = line.get(after..).and_then(|rest| rest.find(ticks)) {
                    at = after.saturating_add(end).saturating_add(run);
                } else {
                    stopped = Some(at);
                    break;
                }
            }
            b'*' | b'_' | b'~' if bytes.get(at.saturating_add(1)) == Some(&byte) => {
                match open.iter().position(|(marker, _)| *marker == byte) {
                    Some(opened) => open.truncate(opened),
                    None => open.push((byte, at)),
                }
                at = at.saturating_add(2);
            }
            b'*' | b'_' | b'~' if at.saturating_add(1) == bytes.len() => {
                stopped = Some(at);
                break;
            }
            _ => at = at.saturating_add(1),
        }
    }
    open.first()
        .map(|(_, first)| *first)
        .into_iter()
        .chain(stopped)
        .chain(unsettled_bracket(bytes))
        .min()
}

/// Where the first `[` in `line` opens that may still become a link: no `]` after it is
/// followed by anything but `(`, and no `](` after it has closed its target. A bracket that
/// settles stays settled however the line goes on.
fn unsettled_bracket(line: &[u8]) -> Option<usize> {
    let settled = |rest: &[u8]| {
        rest.windows(2).enumerate().any(|(at, pair)| match pair {
            [b']', b'('] => rest
                .get(at.saturating_add(2)..)
                .is_some_and(|target| target.contains(&b')')),
            [b']', _] => true,
            _ => false,
        })
    };
    line.iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'[')
        .map(|(at, _)| at)
        .find(|at| !line.get(*at..).is_some_and(settled))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ComposerSendShortcut {
    #[default]
    Enter,
    ModEnterMultiline,
    ModEnter,
}

impl ComposerSendShortcut {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::ModEnterMultiline => "mod-enter-multiline",
            Self::ModEnter => "mod-enter",
        }
    }

    /// Returns the requested one-message follow-up override, or leaves Enter to the editor.
    #[must_use]
    pub const fn submission(self, multiline: bool, secondary: bool, shift: bool) -> Option<bool> {
        if matches!(self, Self::ModEnter) || (matches!(self, Self::ModEnterMultiline) && multiline)
        {
            if secondary { Some(shift) } else { None }
        } else if shift {
            None
        } else {
            Some(secondary)
        }
    }
}

/// How wide a conversation's messages run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptWidth {
    #[default]
    Normal,
    Wide,
    Full,
}

impl TranscriptWidth {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Wide => "wide",
            Self::Full => "full",
        }
    }

    /// The column's width in rems, or `None` for the full width of the view.
    #[must_use]
    pub const fn rems(self) -> Option<f32> {
        match self {
            Self::Normal => Some(48.),
            Self::Wide => Some(72.),
            Self::Full => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FollowUpBehavior {
    #[default]
    Queue,
    Steer,
    /// Stop the running turn and send the message next.
    Interrupt,
}

impl FollowUpBehavior {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Steer => "steer",
            Self::Interrupt => "interrupt",
        }
    }
    /// The one-message alternative the send shortcut's other modifier gives.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Queue => Self::Steer,
            Self::Steer | Self::Interrupt => Self::Queue,
        }
    }
}

/// Launch preferences only. Providers retain ownership of credentials and conversation history.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct AgentProfileConfig {
    pub name: String,
    pub directory: Option<String>,
    pub arguments: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct AgentProviderConfig {
    pub enabled: bool,
    pub program: String,
    pub default_model: String,
    pub default_effort: String,
    /// Empty exposes every effort advertised by the selected model.
    pub available_efforts: Vec<String>,
    pub fast_mode: bool,
    /// Empty selects the provider's existing account store and default launch preferences.
    pub selected: String,
    pub profiles: BTreeMap<String, AgentProfileConfig>,
}

impl Default for AgentProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            program: String::new(),
            default_model: String::new(),
            default_effort: String::new(),
            available_efforts: Vec::new(),
            fast_mode: false,
            selected: String::new(),
            profiles: BTreeMap::new(),
        }
    }
}

impl AgentProviderConfig {
    #[must_use]
    pub fn selected_profile(&self) -> Option<&AgentProfileConfig> {
        self.profiles.get(&self.selected)
    }

    pub(super) fn validate(&self) -> Result<(), ConfigLoadError> {
        if [&self.default_model, &self.default_effort]
            .into_iter()
            .any(|value| value.len() > 256 || value.chars().any(char::is_control))
        {
            return Err(ConfigLoadError::new(
                "Agent model and effort defaults must be bounded identifiers",
            ));
        }
        if self.available_efforts.len() > 32
            || self.available_efforts.iter().any(|effort| {
                effort.is_empty() || effort.len() > 256 || effort.chars().any(char::is_control)
            })
        {
            return Err(ConfigLoadError::new(
                "Available efforts must be bounded identifiers",
            ));
        }
        if self.profiles.len() > 16
            || (!self.selected.is_empty() && !self.profiles.contains_key(&self.selected))
        {
            return Err(ConfigLoadError::new(
                "Agent providers support sixteen profiles and require an existing selected profile",
            ));
        }
        if self.program.starts_with('-')
            || self.program.len() > 8192
            || self.program.chars().any(char::is_control)
        {
            return Err(ConfigLoadError::new(
                "Agent program must be an executable name or path",
            ));
        }
        for (id, profile) in &self.profiles {
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || profile.name.trim().is_empty()
                || profile.name.len() > 256
            {
                return Err(ConfigLoadError::new(
                    "Agent profiles need a simple stable id and a display name",
                ));
            }
            if profile.directory.as_ref().is_some_and(|directory| {
                directory.is_empty()
                    || directory.len() > 8192
                    || directory.chars().any(char::is_control)
                    || !std::path::Path::new(directory).is_absolute()
            }) {
                return Err(ConfigLoadError::new(
                    "Agent account directories must be absolute paths",
                ));
            }
            if profile.arguments.len() > 64
                || profile
                    .arguments
                    .iter()
                    .any(|argument| argument.len() > 8192 || argument.contains('\0'))
                || profile.arguments.iter().map(String::len).sum::<usize>() > 64 * 1024
            {
                return Err(ConfigLoadError::new(
                    "Agent profile arguments exceed the launch limits",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentProvidersConfig {
    #[serde(rename = "allow-spawn")]
    pub allow_spawn: bool,
    #[serde(rename = "default-provider")]
    pub default_provider: String,
    #[serde(rename = "quick-model")]
    pub quick_model: String,
    #[serde(rename = "follow-up-behavior")]
    pub follow_up_behavior: FollowUpBehavior,
    #[serde(rename = "send-shortcut")]
    pub send_shortcut: ComposerSendShortcut,
    #[serde(rename = "response-streaming")]
    pub response_streaming: ResponseStreaming,
    #[serde(rename = "transcript-width")]
    pub transcript_width: TranscriptWidth,
    #[serde(rename = "collapse-composer-on-scroll")]
    pub collapse_composer_on_scroll: bool,
    pub codex: AgentProviderConfig,
    pub claude: AgentProviderConfig,
    pub pi: AgentProviderConfig,
}

impl Default for AgentProvidersConfig {
    fn default() -> Self {
        Self {
            allow_spawn: false,
            default_provider: "codex".to_owned(),
            quick_model: "gpt-6-luna".to_owned(),
            follow_up_behavior: FollowUpBehavior::default(),
            send_shortcut: ComposerSendShortcut::default(),
            response_streaming: ResponseStreaming::default(),
            transcript_width: TranscriptWidth::default(),
            collapse_composer_on_scroll: true,
            codex: AgentProviderConfig::default(),
            claude: AgentProviderConfig::default(),
            pi: AgentProviderConfig::default(),
        }
    }
}

impl AgentProvidersConfig {
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&AgentProviderConfig> {
        match id {
            "codex" => Some(&self.codex),
            "claude" => Some(&self.claude),
            "pi" => Some(&self.pi),
            _ => None,
        }
    }

    pub(super) fn validate(&self) -> Result<(), ConfigLoadError> {
        if !matches!(self.default_provider.as_str(), "codex" | "claude" | "pi") {
            return Err(ConfigLoadError::new(
                "Default agent provider must be codex, claude or pi",
            ));
        }
        if self.pi.fast_mode {
            return Err(ConfigLoadError::new("Pi does not support fast mode"));
        }
        if self.quick_model.trim().is_empty()
            || self.quick_model.len() > 256
            || self.quick_model.chars().any(char::is_control)
        {
            return Err(ConfigLoadError::new(
                "Quick model must be a model identifier",
            ));
        }
        for provider in [&self.codex, &self.claude, &self.pi] {
            provider.validate()?;
        }
        Ok(())
    }
}
