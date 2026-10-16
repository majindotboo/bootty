//! Prompts put aside to send later, from any conversation, kept across restarts.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Stashed prompts kept, at most; the oldest go first.
// Raise it when someone keeps more prompts aside than fit a menu.
const ENTRIES: usize = 50;

/// One prompt put aside: its words and when.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct StashedPrompt {
    pub id: String,
    pub text: String,
    /// UTC seconds.
    pub stashed_at: i64,
}

/// The stash and the file it lives in, beside the configuration. Newest first.
pub struct PromptStash {
    path: PathBuf,
    entries: Vec<StashedPrompt>,
}

impl gpui_kit::Global for PromptStash {}

impl PromptStash {
    /// Opens the stash saved beside `config_path`, empty when there is none.
    #[must_use]
    pub fn open(config_path: &Path) -> Self {
        let path = config_path.with_file_name("prompt-stash.json");
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[StashedPrompt] {
        &self.entries
    }

    /// Puts `text` aside, newest first.
    /// # Errors
    /// Returns the failure to save the stash, which then keeps its earlier contents.
    pub fn stash(&mut self, text: String, now: i64) -> Result<(), String> {
        let mut entries = self.entries.clone();
        entries.insert(
            0,
            StashedPrompt {
                id: bootty_mux::snapshot::new_session_identity(),
                text,
                stashed_at: now,
            },
        );
        entries.truncate(ENTRIES);
        self.save(entries)
    }

    /// Takes the prompt `id` out of the stash, or the newest when `id` is `None`.
    /// # Errors
    /// Returns the failure to save the stash, which then keeps the prompt.
    pub fn take(&mut self, id: Option<&str>) -> Result<Option<StashedPrompt>, String> {
        let Some(index) = id.map_or_else(
            || (!self.entries.is_empty()).then_some(0),
            |id| self.entries.iter().position(|entry| entry.id == id),
        ) else {
            return Ok(None);
        };
        let mut entries = self.entries.clone();
        let taken = entries.remove(index);
        self.save(entries)?;
        Ok(Some(taken))
    }

    fn save(&mut self, entries: Vec<StashedPrompt>) -> Result<(), String> {
        let bytes = serde_json::to_vec(&entries).map_err(|error| error.to_string())?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        bootty_write::WriteTarget::resolve(&self.path)
            .map_err(|error| format!("{error:?}"))?
            .lock()
            .map_err(|error| error.to_string())?
            .replace(&bytes, bootty_write::NewFileMode::Private)
            .map_err(|error| format!("{error:?}"))?;
        self.entries = entries;
        Ok(())
    }
}
