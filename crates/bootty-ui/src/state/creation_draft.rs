//! The New Session form's draft, kept across restarts: written once it stops changing for a
//! second, and read back when the app starts.

use crate::presentation::new_session_form::{
    NewSessionAttachment, NewSessionDraft, NewSessionMode,
};
use bootty_mux::controller::SpaceId;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// How long a draft must stay the same before it is written.
const SETTLE: Duration = Duration::from_secs(1);

/// What of a draft survives a restart. Pasted files live in temporary files that end with the
/// app, so only attachments of real files are kept.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "The saved form mirrors the draft's independent choices"
)]
struct SavedDraft {
    scope: SpaceId,
    cwd: String,
    terminal: bool,
    projectless: bool,
    prompt: String,
    attachments: Vec<SavedAttachment>,
    applications: Vec<bootty_agents::NativeApplicationMention>,
    provider_mentions: Vec<bootty_agents::NativeProviderMention>,
    goal: String,
    command: String,
    provider: String,
    profiles: BTreeMap<String, String>,
    model_selection: Option<bootty_agents::NativeModelSelection>,
    permissions: bootty_agents::NativePermissionMode,
    isolated: bool,
    isolation_preference: bool,
    branch: String,
    folder: String,
    start_ref: String,
    suffix: String,
    identity: String,
    directories: Vec<(SpaceId, String)>,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
struct SavedAttachment {
    path: PathBuf,
    size_bytes: u64,
    prompt_ranges: Vec<std::ops::Range<usize>>,
}

impl From<&NewSessionDraft> for SavedDraft {
    fn from(draft: &NewSessionDraft) -> Self {
        Self {
            scope: draft.scope,
            cwd: draft.cwd.clone(),
            terminal: draft.mode == NewSessionMode::Terminal,
            projectless: draft.projectless,
            prompt: draft.prompt.clone(),
            attachments: draft
                .attachments
                .iter()
                .filter(|attachment| attachment.temporary.is_none())
                .map(|attachment| SavedAttachment {
                    path: attachment.path.clone(),
                    size_bytes: attachment.size_bytes,
                    prompt_ranges: attachment.prompt_ranges.clone(),
                })
                .collect(),
            applications: draft.applications.clone(),
            provider_mentions: draft.provider_mentions.clone(),
            goal: draft.goal.clone(),
            command: draft.command.clone(),
            provider: draft.provider.clone(),
            profiles: draft.profiles.clone(),
            model_selection: draft.model_selection.clone(),
            permissions: draft.permissions,
            isolated: draft.isolated,
            isolation_preference: draft.isolation_preference,
            branch: draft.branch.clone(),
            folder: draft.folder.clone(),
            start_ref: draft.start_ref.clone(),
            suffix: draft.suffix.clone(),
            identity: draft.identity.clone(),
            directories: draft
                .directories
                .iter()
                .map(|(scope, directory)| (*scope, directory.clone()))
                .collect(),
        }
    }
}

impl From<SavedDraft> for NewSessionDraft {
    fn from(saved: SavedDraft) -> Self {
        Self {
            scope: saved.scope,
            cwd: saved.cwd,
            mode: if saved.terminal {
                NewSessionMode::Terminal
            } else {
                NewSessionMode::Agent
            },
            projectless: saved.projectless,
            prompt: saved.prompt,
            attachments: saved
                .attachments
                .into_iter()
                .filter(|attachment| attachment.path.is_file())
                .map(|attachment| NewSessionAttachment {
                    path: attachment.path,
                    size_bytes: attachment.size_bytes,
                    prompt_ranges: attachment.prompt_ranges,
                    preview: None,
                    temporary: None,
                })
                .collect(),
            applications: saved.applications,
            provider_mentions: saved.provider_mentions,
            goal: saved.goal,
            command: saved.command,
            provider: saved.provider,
            profiles: saved.profiles,
            model_selection: saved.model_selection,
            permissions: saved.permissions,
            isolated: saved.isolated,
            isolation_preference: saved.isolation_preference,
            branch: saved.branch,
            folder: saved.folder,
            start_ref: saved.start_ref,
            suffix: saved.suffix,
            identity: saved.identity,
            directories: saved.directories.into_iter().collect::<HashMap<_, _>>(),
        }
    }
}

/// Where the draft lives, what was last written there, and a change waiting to settle.
pub(super) struct CreationDraftStore {
    path: PathBuf,
    written: Option<SavedDraft>,
    pending: Option<(Option<SavedDraft>, Instant)>,
}

impl CreationDraftStore {
    /// Opens the draft saved beside `config_path`, if one is there.
    pub(super) fn open(config_path: &Path) -> (Self, Option<NewSessionDraft>) {
        let path = config_path.with_file_name("new-session-draft.json");
        let written = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SavedDraft>(&bytes).ok());
        let draft = written.clone().map(NewSessionDraft::from);
        (
            Self {
                path,
                written,
                pending: None,
            },
            draft,
        )
    }

    /// Notes the draft as it is at `now`, and writes it once it has stayed that way for a
    /// second. No draft removes the file.
    pub(super) fn observe(&mut self, draft: Option<&NewSessionDraft>, now: Instant) {
        let current = draft.map(SavedDraft::from);
        if current == self.written {
            self.pending = None;
            return;
        }
        match &self.pending {
            Some((waiting, since)) if *waiting == current => {
                if now.saturating_duration_since(*since) < SETTLE {
                    return;
                }
            }
            _ => {
                self.pending = Some((current, now));
                return;
            }
        }
        self.pending = None;
        // A lost write keeps the previous draft, which the next change replaces.
        let _ = match &current {
            Some(draft) => serde_json::to_vec(draft)
                .map_err(std::io::Error::other)
                .and_then(|bytes| write(&self.path, &bytes)),
            None => std::fs::remove_file(&self.path),
        };
        self.written = current;
    }
}

fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    bootty_write::WriteTarget::resolve(path)
        .map_err(bootty_write::ResolveTargetError::into_io)?
        .lock()?
        .replace(bytes, bootty_write::NewFileMode::Private)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    Ok(())
}
