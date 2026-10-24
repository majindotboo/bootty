//! Per-window creation drafts. Edits save off the UI thread; committed entries drive navigation.

use crate::presentation::new_session_form::{
    NewSessionAttachment, NewSessionDraft, NewSessionMode,
};
use bootty_mux::controller::SpaceId;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    io::Read as _,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

/// How long a draft must stay the same before it is written.
const SETTLE: Duration = Duration::from_secs(1);

#[derive(Clone, PartialEq)]
pub struct CreationDraft {
    pub draft: NewSessionDraft,
    pub project: Option<String>,
    pub title: String,
}

impl CreationDraft {
    pub(crate) fn has_content(&self) -> bool {
        !self.draft.prompt.trim().is_empty()
            || !self.draft.command.trim().is_empty()
            || !self.draft.attachments.is_empty()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DraftSummary {
    pub identity: String,
    pub scope: SpaceId,
    pub title: String,
    pub cwd: String,
    pub project: Option<String>,
    pub provider: String,
}

/// What of a draft survives a restart. Pasted files live in temporary files that end with the
/// app, so only attachments of real files are kept.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "The saved form mirrors the draft's independent choices"
)]
struct SavedDraft {
    scope: SpaceId,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    fan_out: Vec<crate::presentation::new_session_form::FanOutModel>,
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

impl From<&CreationDraft> for SavedDraft {
    fn from(record: &CreationDraft) -> Self {
        let draft = &record.draft;
        Self {
            scope: draft.scope,
            project: record.project.clone(),
            title: record.title.clone(),
            fan_out: draft.fan_out.clone(),
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
            fan_out: saved.fan_out,
        }
    }
}

#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
struct SavedCatalog {
    drafts: BTreeMap<String, SavedDraft>,
    #[serde(default)]
    selected: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SavedState {
    Catalog(SavedCatalog),
    Legacy(Box<SavedDraft>),
}

struct PendingWrite {
    revision: u64,
    saved: SavedCatalog,
    records: BTreeMap<String, CreationDraft>,
    reply: mpsc::Receiver<Result<(), String>>,
}

pub(super) struct CreationDraftStore {
    path: PathBuf,
    written: SavedCatalog,
    committed: BTreeMap<String, CreationDraft>,
    written_revision: u64,
    pending: Option<(u64, Option<String>, Instant)>,
    writing: Option<PendingWrite>,
    discards: BTreeMap<String, mpsc::Sender<bootty_control::CommandOutcome>>,
    discarded: Vec<String>,
    repaint: bootty_mux::RepaintHandle,
}

impl CreationDraftStore {
    pub(super) fn open(
        path: PathBuf,
        legacy: Option<&Path>,
        repaint: bootty_mux::RepaintHandle,
    ) -> anyhow::Result<(Self, Vec<CreationDraft>, Option<String>)> {
        let source = if path.exists() {
            Some(path.as_path())
        } else {
            legacy.filter(|path| path.exists())
        };
        let written = if let Some(source) = source {
            let file = std::fs::File::open(source)?;
            anyhow::ensure!(
                file.metadata()?.len() <= 16 * 1024 * 1024,
                "Creation drafts exceed 16 MiB"
            );
            let mut bytes = Vec::new();
            file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes.len() <= 16 * 1024 * 1024,
                "Creation drafts exceed 16 MiB"
            );
            match serde_json::from_slice::<SavedState>(&bytes)? {
                SavedState::Catalog(catalog) => catalog,
                SavedState::Legacy(draft) => SavedCatalog {
                    drafts: [(draft.identity.clone(), *draft)].into(),
                    selected: None,
                },
            }
        } else {
            SavedCatalog::default()
        };
        let committed = written
            .drafts
            .iter()
            .map(|(id, saved)| {
                let record = CreationDraft {
                    project: saved
                        .project
                        .clone()
                        .or_else(|| (!saved.projectless).then(|| saved.cwd.clone())),
                    title: if saved.title.is_empty() {
                        saved
                            .prompt
                            .lines()
                            .find(|line| !line.trim().is_empty())
                            .unwrap_or("New session")
                            .chars()
                            .take(72)
                            .collect()
                    } else {
                        saved.title.clone()
                    },
                    draft: NewSessionDraft::from(saved.clone()),
                };
                (id.clone(), record)
            })
            .collect::<BTreeMap<_, _>>();
        let records = committed.values().cloned().collect();
        let selected = written.selected.clone();
        Ok((
            Self {
                path,
                written,
                committed,
                written_revision: 0,
                pending: None,
                writing: None,
                discards: BTreeMap::new(),
                discarded: Vec::new(),
                repaint,
            },
            records,
            selected,
        ))
    }

    pub(super) fn summaries(&self) -> impl Iterator<Item = DraftSummary> + '_ {
        self.committed.values().map(|record| DraftSummary {
            identity: record.draft.identity.clone(),
            scope: record.draft.scope,
            title: record.title.clone(),
            cwd: record.draft.cwd.clone(),
            project: record.project.clone(),
            provider: if record.draft.mode == NewSessionMode::Terminal {
                "terminal".into()
            } else {
                record.draft.provider.clone()
            },
        })
    }

    pub(super) fn discard(
        &mut self,
        scope: SpaceId,
        identity: &str,
    ) -> Result<mpsc::Receiver<bootty_control::CommandOutcome>, String> {
        if !self
            .committed
            .get(identity)
            .is_some_and(|record| record.draft.scope == scope)
        {
            return Err("This draft is no longer available in this Space".into());
        }
        if self.discards.contains_key(identity) {
            return Err("This draft is already being discarded".into());
        }
        let (send, reply) = mpsc::channel();
        self.discards.insert(identity.to_owned(), send);
        Ok(reply)
    }

    pub(super) fn take_discarded(&mut self) -> Vec<String> {
        std::mem::take(&mut self.discarded)
    }

    pub(super) fn observe(
        &mut self,
        records: &BTreeMap<String, CreationDraft>,
        selected: Option<String>,
        revision: u64,
        now: Instant,
        force: bool,
    ) -> Result<bool, String> {
        self.poll()?;
        if self.writing.is_some() {
            return Ok(true);
        }
        let selected = selected.filter(|id| records.contains_key(id));
        if revision == self.written_revision
            && selected == self.written.selected
            && self.discards.is_empty()
            && self.discarded.is_empty()
        {
            self.pending = None;
            return Ok(false);
        }
        let force = force || selected != self.written.selected || !self.discards.is_empty();
        let settled = self
            .pending
            .as_ref()
            .is_some_and(|(waiting, active, since)| {
                *waiting == revision
                    && active == &selected
                    && now.saturating_duration_since(*since) >= SETTLE
            });
        if !force && !settled {
            if !self
                .pending
                .as_ref()
                .is_some_and(|(waiting, active, _)| *waiting == revision && active == &selected)
            {
                self.pending = Some((revision, selected, now));
            }
            return Ok(true);
        }
        let records = records
            .iter()
            .filter(|(id, _)| !self.discards.contains_key(*id) && !self.discarded.contains(id))
            .map(|(id, record)| (id.clone(), record.clone()))
            .collect::<BTreeMap<_, _>>();
        let current = SavedCatalog {
            selected: selected.filter(|id| records.contains_key(id)),
            drafts: records
                .iter()
                .map(|(id, record)| (id.clone(), SavedDraft::from(record)))
                .collect(),
        };
        if current == self.written {
            self.written_revision = revision;
            self.pending = None;
            return Ok(false);
        }
        let path = self.path.clone();
        let saved = current.clone();
        let repaint = self.repaint.clone();
        let (send, reply) = mpsc::channel();
        std::thread::spawn(move || {
            let result = serde_json::to_vec(&saved)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    if bytes.len() > 16 * 1024 * 1024 {
                        return Err("Creation drafts exceed 16 MiB".into());
                    }
                    write(&path, &bytes).map_err(|error| format!("{}: {error}", path.display()))
                });
            _ = send.send(result);
            repaint();
        });
        self.pending = None;
        self.writing = Some(PendingWrite {
            revision,
            saved: current,
            records,
            reply,
        });
        Ok(true)
    }

    fn poll(&mut self) -> Result<(), String> {
        let Some(pending) = &self.writing else {
            return Ok(());
        };
        let outcome = match pending.reply.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => Err("The draft save worker stopped".into()),
        };
        let Some(pending) = self.writing.take() else {
            return Ok(());
        };
        if let Err(error) = outcome {
            for (_, send) in std::mem::take(&mut self.discards) {
                _ = send.send(bootty_control::CommandOutcome::Failed {
                    code: "draft_save_failed".into(),
                    message: error.clone(),
                });
            }
            (self.repaint)();
            return Err(error);
        }
        self.written_revision = pending.revision;
        self.written = pending.saved;
        self.committed = pending.records;
        let completed = self
            .discards
            .keys()
            .filter(|id| !self.written.drafts.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in completed {
            if let Some(send) = self.discards.remove(&id) {
                _ = send.send(bootty_control::CommandOutcome::success());
            }
            self.discarded.push(id);
        }
        (self.repaint)();
        Ok(())
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
