//! Bounded terminal presentation and topology saved under a logical session identity.

use crate::snapshot::MuxPaneLayout;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path};

pub const MAX_SAVED_PANE_TEXT: usize = bootty_control::terminal_history::MAX_HISTORY_BYTES;
pub const MAX_SAVED_SESSION_TEXT: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedTerminalSession {
    pub captured_at: i64,
    /// The saved logical task identity, independent of backend process identifiers.
    pub session_id: String,
    pub backend_id: String,
    pub active_window_id: Option<String>,
    pub windows: Vec<SavedTerminalWindow>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedTerminalWindow {
    /// Stable saved key, independent of later backend attachment identities.
    pub id: String,
    pub backend_id: String,
    pub title: String,
    pub focused_pane_id: String,
    pub layout: Option<MuxPaneLayout>,
    pub panes: Vec<SavedTerminalPane>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedTerminalPane {
    pub id: String,
    pub backend_id: String,
    pub cwd: String,
    /// Both dimensions are zero when a backend capture has no observed geometry.
    pub cols: u16,
    pub rows: u16,
    pub text: String,
    pub omitted_lines: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_agent: Option<String>,
}

/// A renderer's validated styled history capture; topology and directory come from the binding owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPaneCapture {
    pub pane_id: String,
    /// Cached observed directory when the runtime has one; topology remains binding-owned.
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub text: String,
    pub omitted_lines: u64,
}

impl SavedTerminalSession {
    /// # Errors
    /// Rejects malformed topology, unsafe metadata and snapshots beyond the bounded history budget.
    pub fn validate(&self) -> Result<(), String> {
        if self.captured_at < 0
            || !metadata(&self.session_id, false)
            || !metadata(&self.backend_id, false)
            || self.windows.is_empty()
            || self.windows.len() > 32
        {
            return Err("invalid saved terminal session".into());
        }
        let mut window_ids = HashSet::new();
        let mut pane_ids = HashSet::new();
        let mut backend_windows = HashSet::new();
        let mut backend_panes = HashSet::new();
        let mut native_agents = HashSet::new();
        let mut bytes = 0usize;
        for window in &self.windows {
            if !metadata(&window.id, false)
                || !metadata(&window.backend_id, false)
                || !window_ids.insert(&window.id)
                || !backend_windows.insert(&window.backend_id)
                || !metadata(&window.title, true)
                || window.panes.is_empty()
                || window.panes.len() > 32
            {
                return Err("invalid saved terminal window".into());
            }
            let mut leaves = HashSet::new();
            if let Some(layout) = &window.layout {
                validate_layout(layout, &mut leaves, 0)?;
            }
            let mut current = HashSet::new();
            for pane in &window.panes {
                validate_pane(pane)?;
                if pane
                    .native_agent
                    .as_ref()
                    .is_some_and(|id| !native_agents.insert(id))
                {
                    return Err("saved native conversation occupies more than one pane".into());
                }
                if !pane_ids.insert(&pane.id) {
                    return Err("saved terminal pane repeats a logical id".into());
                }
                if !backend_panes.insert(&pane.backend_id) {
                    return Err("saved terminal pane repeats a backend id".into());
                }
                current.insert(pane.id.as_str());
                bytes = bytes.saturating_add(pane.text.len());
            }
            if !current.contains(window.focused_pane_id.as_str())
                || (window.layout.is_some() && leaves != current)
            {
                return Err("saved terminal layout does not match its panes".into());
            }
        }
        if bytes > MAX_SAVED_SESSION_TEXT
            || self
                .active_window_id
                .as_ref()
                .is_some_and(|id| !window_ids.contains(id))
        {
            return Err("saved terminal session exceeds its budget or has invalid focus".into());
        }
        Ok(())
    }
}

fn validate_pane(pane: &SavedTerminalPane) -> Result<(), String> {
    if pane
        .native_agent
        .as_deref()
        .is_some_and(|id| !crate::snapshot::native_agent_identity_is_valid(id))
    {
        return Err("saved native agent pane has invalid identity metadata".into());
    }
    if !metadata(&pane.id, false) {
        return Err("saved terminal pane has invalid logical id metadata".into());
    }
    if !metadata(&pane.backend_id, false) {
        return Err("saved terminal pane has invalid backend id metadata".into());
    }
    if !metadata(&pane.cwd, false) {
        return Err("saved terminal pane has invalid cwd metadata".into());
    }
    if !(pane.cwd.starts_with('/') || Path::new(&pane.cwd).is_absolute()) {
        return Err("saved terminal pane cwd is not an absolute path".into());
    }
    if (pane.cols == 0) != (pane.rows == 0) {
        return Err(format!(
            "saved terminal pane has incomplete geometry ({} cols, {} rows)",
            pane.cols, pane.rows
        ));
    }
    if pane.text.len() > MAX_SAVED_PANE_TEXT {
        return Err(format!(
            "saved terminal pane history exceeds its byte budget ({})",
            pane.text.len()
        ));
    }
    bootty_control::terminal_history::validate_history(&pane.text)
        .map_err(|error| format!("saved terminal pane history: {error}"))
}

fn metadata(value: &str, empty: bool) -> bool {
    (empty || !value.is_empty()) && value.len() <= 4096 && !value.chars().any(char::is_control)
}

fn validate_layout<'a>(
    layout: &'a MuxPaneLayout,
    leaves: &mut HashSet<&'a str>,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 {
        return Err("saved terminal layout is too deep".into());
    }
    match layout {
        MuxPaneLayout::Pane(id) => {
            if !leaves.insert(id) {
                return Err("saved terminal layout repeats a pane".into());
            }
        }
        MuxPaneLayout::Split {
            ratio_millis,
            first,
            second,
            ..
        } => {
            if !(1..=999).contains(ratio_millis) {
                return Err("invalid saved terminal split ratio".into());
            }
            validate_layout(first, leaves, depth.saturating_add(1))?;
            validate_layout(second, leaves, depth.saturating_add(1))?;
        }
    }
    Ok(())
}
