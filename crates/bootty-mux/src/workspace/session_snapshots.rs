//! Capture receipts freeze one binding's topology; history persistence happens on a worker.
use super::{ScopedWindowId, WorkspaceRuntime};
use crate::{
    controller::{CommandFence, SpaceId},
    repository::{WorkspacePersistenceError, WorkspaceRepository},
    session_snapshot::{
        SavedTerminalPane, SavedTerminalSession, SavedTerminalWindow, SessionPaneCapture,
    },
};
use std::collections::{HashMap, HashSet};

pub struct PreparedSessionCheckpoint {
    scope: SpaceId,
    identity: String,
    generation: u64,
    fence: CommandFence,
    repository: WorkspaceRepository,
    previous: Option<std::sync::Arc<SavedTerminalSession>>,
    snapshot: SavedTerminalSession,
}

pub struct SavedSessionCheckpoint {
    scope: SpaceId,
    identity: String,
    generation: u64,
    snapshot: SavedTerminalSession,
}

impl PreparedSessionCheckpoint {
    /// Advance the expected checkpoint only from a committed receipt of this exact owner.
    /// # Errors
    /// Rejects another task, Space or binding generation without weakening the save fence.
    pub fn follow_checkpoint(
        &mut self,
        receipt: &SavedSessionCheckpoint,
    ) -> Result<(), WorkspacePersistenceError> {
        if self.scope != receipt.scope
            || self.identity != receipt.identity
            || self.generation != receipt.generation
        {
            return Err(WorkspacePersistenceError::operation(
                "checkpoint receipt belongs to another owner",
            ));
        }
        self.previous = Some(std::sync::Arc::new(receipt.snapshot.clone()));
        Ok(())
    }

    /// Persist styled history after asynchronous capture completes, away from the UI thread.
    /// # Errors
    /// Rejects stale bindings, incomplete captures, malformed text and changed persisted owners.
    pub fn save(
        mut self,
        captures: Vec<SessionPaneCapture>,
    ) -> Result<SavedSessionCheckpoint, WorkspacePersistenceError> {
        let count = captures.len();
        let mut captures = captures
            .into_iter()
            .map(|capture| (capture.pane_id.clone(), capture))
            .collect::<HashMap<_, _>>();
        if captures.len() != count {
            return Err(WorkspacePersistenceError::operation(
                "duplicate checkpoint pane",
            ));
        }
        for window in &mut self.snapshot.windows {
            for pane in &mut window.panes {
                let capture = captures.remove(&pane.backend_id).ok_or_else(|| {
                    WorkspacePersistenceError::operation("terminal checkpoint is incomplete")
                })?;
                if let Some(cwd) = capture.cwd {
                    pane.cwd = cwd;
                }
                pane.cols = capture.cols;
                pane.rows = capture.rows;
                pane.text = capture.text;
                pane.omitted_lines = capture.omitted_lines;
            }
        }
        if !captures.is_empty() {
            return Err(WorkspacePersistenceError::operation(
                "foreign checkpoint pane",
            ));
        }
        self.snapshot
            .validate()
            .map_err(WorkspacePersistenceError::operation)?;
        self.fence
            .claim()
            .map_err(|error| WorkspacePersistenceError::operation(error.to_string()))?;
        self.repository.commit_terminal_snapshot(
            self.scope,
            &self.identity,
            self.previous.as_deref(),
            &self.snapshot,
        )?;
        Ok(SavedSessionCheckpoint {
            scope: self.scope,
            identity: self.identity,
            generation: self.generation,
            snapshot: self.snapshot,
        })
    }
}

impl WorkspaceRuntime {
    /// Freeze exact logical ownership and native topology before history capture leaves the host.
    /// # Errors
    /// Rejects stale bindings, missing originals and unclaimed identities without writing.
    pub fn prepare_session_checkpoint(
        &self,
        scope: SpaceId,
        identity: &str,
        generation: u64,
        captured_at: i64,
    ) -> Result<PreparedSessionCheckpoint, WorkspacePersistenceError> {
        let binding = self.binding(scope).ok_or_else(|| {
            WorkspacePersistenceError::operation("checkpoint Space is unavailable")
        })?;
        if generation != binding.mux().binding_generation() {
            return Err(WorkspacePersistenceError::operation(
                "checkpoint binding is stale",
            ));
        }
        let saved = binding.sessions().get(identity).ok_or_else(|| {
            WorkspacePersistenceError::operation("checkpoint identity is not held")
        })?;
        let session = binding.session_attachment(identity).ok_or_else(|| {
            WorkspacePersistenceError::operation("checkpoint original is unavailable")
        })?;
        let windows = session
            .windows
            .iter()
            .map(|window| binding.checkpoint_window(identity, &session.id, window))
            .collect::<Result<Vec<_>, _>>()?;
        let active_window_id = session
            .active_window_id
            .as_ref()
            .and_then(|active| windows.iter().find(|window| &window.backend_id == active))
            .map(|window| window.id.clone());
        let snapshot = SavedTerminalSession {
            captured_at,
            session_id: identity.to_owned(),
            backend_id: session.id.clone(),
            active_window_id,
            windows,
        };
        snapshot
            .validate()
            .map_err(WorkspacePersistenceError::operation)?;
        Ok(PreparedSessionCheckpoint {
            scope,
            identity: identity.to_owned(),
            generation,
            fence: binding.mux().command_fence(binding.multiplexer()),
            repository: self.repository.clone(),
            previous: saved.terminal_snapshot.clone(),
            snapshot,
        })
    }

    /// Admit a committed worker receipt while its Space still holds the logical session.
    /// Returns true even for identical data; false means the receipt was not admitted.
    /// # Errors
    /// Returns malformed receipt data; a closed session leaves live state untouched.
    pub fn publish_session_checkpoint(
        &mut self,
        receipt: SavedSessionCheckpoint,
    ) -> Result<bool, WorkspacePersistenceError> {
        let Some(binding) = self.binding_mut(receipt.scope) else {
            return Ok(false);
        };
        // The receipt is already committed for this Space's logical session. A backend refresh
        // that recovered meanwhile bumps the binding generation, but live state must still mirror
        // the stored checkpoint, or every later checkpoint fails its compare-and-swap.
        if !binding.sessions.contains(&receipt.identity) {
            return Ok(false);
        }
        receipt
            .snapshot
            .validate()
            .map_err(WorkspacePersistenceError::operation)?;
        if binding
            .sessions
            .get(&receipt.identity)
            .and_then(|saved| saved.terminal_snapshot.as_ref())
            .is_some_and(|old| old.captured_at > receipt.snapshot.captured_at)
        {
            return Ok(false);
        }
        binding
            .sessions
            .set_terminal_snapshot(&receipt.identity, receipt.snapshot);
        Ok(true)
    }
}

#[derive(Default)]
pub(super) struct RestoredSessionMapping {
    generation: u64,
    windows: HashMap<String, String>,
    pub(super) panes: HashMap<String, String>,
    pub(super) history: HashMap<String, String>,
}

impl super::BindingRuntime {
    pub(super) fn retain_restored_generation(&mut self) {
        let generation = self.mux.binding_generation();
        self.restored_sessions
            .retain(|_, mapping| mapping.generation == generation);
    }

    #[must_use]
    pub fn saved_selected_session_identity(&self) -> Option<&str> {
        self.saved_selected_session_identity.as_deref()
    }

    /// Stable saved window identity, including the current cold-restoration mapping.
    #[must_use]
    pub fn saved_window_key(&self, identity: &str, actual: &str) -> String {
        self.restored_sessions
            .get(identity)
            .filter(|map| map.generation == self.mux.binding_generation())
            .and_then(|map| {
                map.windows
                    .iter()
                    .find(|(_, id)| id.as_str() == actual)
                    .map(|(key, _)| key.clone())
            })
            .or_else(|| {
                self.sessions
                    .get(identity)?
                    .terminal_snapshot
                    .as_ref()?
                    .windows
                    .iter()
                    .find(|window| {
                        window.backend_id == actual
                            && !self
                                .restored_sessions
                                .get(identity)
                                .filter(|map| map.generation == self.mux.binding_generation())
                                .is_some_and(|map| map.windows.contains_key(&window.id))
                    })
                    .map(|window| window.id.clone())
            })
            .unwrap_or_else(|| {
                unused_saved_key(
                    actual,
                    self.sessions
                        .get(identity)
                        .and_then(|saved| saved.terminal_snapshot.as_ref())
                        .into_iter()
                        .flat_map(|snapshot| &snapshot.windows)
                        .map(|window| window.id.as_str())
                        .chain(
                            self.restored_sessions
                                .get(identity)
                                .into_iter()
                                .flat_map(|map| map.windows.keys().map(String::as_str)),
                        )
                        .chain(
                            self.session_attachment(identity)
                                .into_iter()
                                .flat_map(|session| &session.windows)
                                .filter(|window| window.id != actual)
                                .map(|window| window.id.as_str()),
                        ),
                )
            })
    }

    fn saved_pane_key(&self, identity: &str, actual: &str) -> String {
        self.restored_sessions
            .get(identity)
            .filter(|map| map.generation == self.mux.binding_generation())
            .and_then(|map| {
                map.panes
                    .iter()
                    .find(|(_, id)| id.as_str() == actual)
                    .map(|(key, _)| key.clone())
            })
            .or_else(|| {
                self.sessions
                    .get(identity)?
                    .terminal_snapshot
                    .as_ref()?
                    .windows
                    .iter()
                    .flat_map(|window| &window.panes)
                    .find(|pane| {
                        pane.backend_id == actual
                            && !self
                                .restored_sessions
                                .get(identity)
                                .filter(|map| map.generation == self.mux.binding_generation())
                                .is_some_and(|map| map.panes.contains_key(&pane.id))
                    })
                    .map(|pane| pane.id.clone())
            })
            .unwrap_or_else(|| {
                unused_saved_key(
                    actual,
                    self.sessions
                        .get(identity)
                        .and_then(|saved| saved.terminal_snapshot.as_ref())
                        .into_iter()
                        .flat_map(|snapshot| &snapshot.windows)
                        .flat_map(|window| &window.panes)
                        .map(|pane| pane.id.as_str())
                        .chain(
                            self.restored_sessions
                                .get(identity)
                                .into_iter()
                                .flat_map(|map| map.panes.keys().map(String::as_str)),
                        )
                        .chain(
                            self.session_attachment(identity)
                                .into_iter()
                                .flat_map(|session| &session.windows)
                                .flat_map(|window| &window.panes)
                                .filter_map(|pane| pane.pane_id.as_deref())
                                .filter(|id| *id != actual),
                        ),
                )
            })
    }

    fn checkpoint_window(
        &self,
        identity: &str,
        session: &str,
        window: &crate::snapshot::MuxWindow,
    ) -> Result<SavedTerminalWindow, WorkspacePersistenceError> {
        let key = ScopedWindowId::new(self.scope, session.to_owned(), window.id.clone());
        let layout = self.pane_layouts.get(&key);
        let saved = self.sessions.get(identity).ok_or_else(|| {
            WorkspacePersistenceError::operation("checkpoint task is unavailable")
        })?;
        let anchors = if window.panes.is_empty() {
            vec![&window.anchor]
        } else {
            window.panes.iter().collect()
        };
        let panes = anchors
            .into_iter()
            .map(|pane| {
                let id = pane.pane_id.as_ref().ok_or_else(|| {
                    WorkspacePersistenceError::operation("checkpoint pane is unavailable")
                })?;
                Ok(SavedTerminalPane {
                    id: self.saved_pane_key(identity, id),
                    backend_id: id.clone(),
                    cwd: pane.cwd.clone().unwrap_or_else(|| saved.cwd.clone()),
                    cols: 0,
                    rows: 0,
                    text: String::new(),
                    omitted_lines: 0,
                })
            })
            .collect::<Result<Vec<_>, WorkspacePersistenceError>>()?;
        let ids = panes
            .iter()
            .map(|pane| (pane.backend_id.clone(), pane.id.clone()))
            .collect::<HashMap<_, _>>();
        let focused = layout
            .map(crate::pane_layout::PaneLayout::focused)
            .or(window.anchor.pane_id.as_deref())
            .ok_or_else(|| {
                WorkspacePersistenceError::operation("checkpoint focus is unavailable")
            })?;
        let focused_pane_id = ids.get(focused).cloned().ok_or_else(|| {
            WorkspacePersistenceError::operation("checkpoint focused pane is unavailable")
        })?;
        let layout = layout
            .map(crate::pane_layout::PaneLayout::snapshot)
            .or_else(|| window.layout.clone())
            .map(|layout| {
                remap_layout(&layout, &ids).ok_or_else(|| {
                    WorkspacePersistenceError::operation(
                        "checkpoint layout contains an unavailable pane",
                    )
                })
            })
            .transpose()?;
        Ok(SavedTerminalWindow {
            id: self.saved_window_key(identity, &window.id),
            backend_id: window.id.clone(),
            title: window.name.clone(),
            focused_pane_id,
            layout,
            panes,
        })
    }

    fn prepare_restored_session_mapping(
        &mut self,
        saved: &SavedTerminalSession,
        session: &crate::snapshot::MuxSession,
    ) -> Result<RestoredSessionMapping, crate::controller::MuxCommandError> {
        use crate::controller::MuxCommandError;
        let scope = self.scope;
        if session.windows.len() != saved.windows.len() {
            return Err(MuxCommandError::Failed(
                "restored window topology is incomplete".into(),
            ));
        }
        let mut mapping = RestoredSessionMapping {
            generation: self.mux.binding_generation(),
            ..RestoredSessionMapping::default()
        };
        for (old, window) in saved.windows.iter().zip(&session.windows) {
            if old.panes.len() != window.panes.len() {
                return Err(MuxCommandError::Failed(
                    "restored pane topology is incomplete".into(),
                ));
            }
            mapping.windows.insert(old.id.clone(), window.id.clone());
            for (old, pane) in old.panes.iter().zip(&window.panes) {
                let id = pane.pane_id.as_ref().ok_or_else(|| {
                    MuxCommandError::Failed("restored pane has no backend identity".into())
                })?;
                mapping.panes.insert(old.id.clone(), id.clone());
                mapping.history.insert(id.clone(), old.text.clone());
            }
            if let Some(layout) = old
                .layout
                .as_ref()
                .and_then(|layout| remap_layout(layout, &mapping.panes))
                .and_then(|layout| crate::pane_layout::PaneLayout::from_mux_layout(&layout))
            {
                let mut layout = layout;
                if let Some(focus) = mapping.panes.get(&old.focused_pane_id) {
                    layout.set_focus(focus);
                }
                self.pane_layouts.insert(
                    ScopedWindowId::new(scope, session.id.clone(), window.id.clone()),
                    layout,
                );
            }
        }
        Ok(mapping)
    }

    /// Resolve a saved window key only within the same exact tagged task attachment.
    #[must_use]
    pub fn restored_terminal_window(
        &self,
        identity: &str,
        original: &str,
    ) -> Option<&crate::snapshot::MuxWindow> {
        let mapped = self
            .restored_sessions
            .get(identity)
            .filter(|mapping| mapping.generation == self.mux.binding_generation())
            .and_then(|mapping| mapping.windows.get(original))
            .map(String::as_str)
            .or_else(|| {
                self.sessions
                    .get(identity)?
                    .terminal_snapshot
                    .as_ref()?
                    .windows
                    .iter()
                    .find(|window| window.id == original)
                    .map(|window| window.backend_id.as_str())
            })?;
        self.session_attachment(identity)?
            .windows
            .iter()
            .find(|window| window.id == mapped)
    }

    /// A restored pane's saved prefix belongs to its renderer after admission.
    #[must_use]
    pub fn has_restored_terminal_pane(&self, pane: &str) -> bool {
        self.restored_sessions.values().any(|mapping| {
            mapping.generation == self.mux.binding_generation()
                && mapping.panes.values().any(|held| held == pane)
        })
    }

    /// Resolve a saved pane key only within the same exact tagged task attachment.
    #[must_use]
    pub fn restored_terminal_pane(
        &self,
        identity: &str,
        original: &str,
    ) -> Option<&crate::snapshot::MuxPaneAnchor> {
        let mapped = self
            .restored_sessions
            .get(identity)
            .filter(|mapping| mapping.generation == self.mux.binding_generation())
            .and_then(|mapping| mapping.panes.get(original))
            .map(String::as_str)
            .or_else(|| {
                self.sessions
                    .get(identity)?
                    .terminal_snapshot
                    .as_ref()?
                    .windows
                    .iter()
                    .flat_map(|window| &window.panes)
                    .find(|pane| pane.id == original)
                    .map(|pane| pane.backend_id.as_str())
            })?;
        self.session_attachment(identity)?
            .windows
            .iter()
            .flat_map(|window| &window.panes)
            .find(|pane| pane.pane_id.as_deref() == Some(mapped))
    }
}

impl WorkspaceRuntime {
    /// Consume history once when the restored pane is admitted to a renderer.
    #[must_use]
    pub fn take_restored_terminal_history(
        &mut self,
        scope: SpaceId,
        pane_id: &str,
    ) -> Option<String> {
        self.binding_mut(scope)?
            .restored_sessions
            .values_mut()
            .find_map(|mapping| mapping.history.remove(pane_id))
    }

    /// Apply restored styled history only to admitted runtimes, retaining failed or absent ones.
    /// # Errors
    /// Returns renderer queue errors without discarding the pending history.
    pub fn restore_available_terminal_history(&mut self, scope: SpaceId) -> anyhow::Result<()> {
        let histories = self.binding(scope).map_or_else(Vec::new, |binding| {
            binding
                .restored_sessions
                .values()
                .flat_map(|mapping| {
                    mapping
                        .history
                        .iter()
                        .map(|(id, text)| (id.clone(), text.clone()))
                })
                .collect::<Vec<_>>()
        });
        for (id, text) in histories {
            if let Some(runtime) = self.space_terminal_runtime(scope, &id) {
                runtime.restore_history(&text)?;
                let _ = self.take_restored_terminal_history(scope, &id);
            }
        }
        Ok(())
    }

    pub(super) fn restore_session_command(
        &mut self,
        scope: SpaceId,
        command: &crate::command::MuxCommand,
    ) -> Result<(), crate::controller::MuxCommandError> {
        use crate::controller::MuxCommandError;
        let crate::command::MuxCommand::RestoreSession { tag, .. } = command else {
            return Ok(());
        };
        let identity = tag
            .identity
            .as_deref()
            .ok_or_else(|| MuxCommandError::Failed("restored task has no identity".into()))?;
        let binding = self.binding_mut(scope).ok_or(MuxCommandError::Stale)?;
        let saved = binding
            .sessions
            .get(identity)
            .and_then(|saved| saved.terminal_snapshot.clone())
            .ok_or_else(|| MuxCommandError::Failed("restored task has no checkpoint".into()))?;
        let session = binding
            .session_attachment(identity)
            .cloned()
            .ok_or_else(|| {
                MuxCommandError::Failed("restored exact attachment is unavailable".into())
            })?;
        let mapping = binding.prepare_restored_session_mapping(&saved, &session)?;
        binding
            .restored_sessions
            .insert(identity.to_owned(), mapping);
        {
            for pane in session.windows.iter().flat_map(|window| &window.panes) {
                let id = pane.pane_id.as_deref().ok_or_else(|| {
                    MuxCommandError::Failed("restored pane has no identity".into())
                })?;
                let text = self
                    .binding(scope)
                    .and_then(|binding| binding.restored_sessions.get(identity))
                    .and_then(|mapping| mapping.history.get(id))
                    .cloned()
                    .ok_or_else(|| {
                        MuxCommandError::Failed("restored pane history is unavailable".into())
                    })?;
                self.space_terminal_owner(scope)
                    .and_then(|owner| {
                        owner
                            .terminal
                            .start_scoped_restored_native(scope, pane.clone(), &text)
                    })
                    .map_err(|error| MuxCommandError::Failed(format!("{error:#}")))?;
                let _ = self.take_restored_terminal_history(scope, id);
            }
        }
        Ok(())
    }
}

fn remap_layout(
    layout: &crate::snapshot::MuxPaneLayout,
    ids: &HashMap<String, String>,
) -> Option<crate::snapshot::MuxPaneLayout> {
    use crate::snapshot::MuxPaneLayout;
    Some(match layout {
        MuxPaneLayout::Pane(id) => MuxPaneLayout::Pane(ids.get(id)?.clone()),
        MuxPaneLayout::Split {
            direction,
            weights,
            children,
        } => MuxPaneLayout::Split {
            direction: direction.clone(),
            weights: weights.clone(),
            children: children
                .iter()
                .map(|child| remap_layout(child, ids))
                .collect::<Option<_>>()?,
        },
    })
}

fn unused_saved_key<'a>(actual: &str, reserved: impl Iterator<Item = &'a str>) -> String {
    let reserved = reserved.collect::<HashSet<_>>();
    let mut key = actual.to_owned();
    // Distinct suffixes need at most one more candidate than the reserved set.
    for suffix in 1..=reserved.len().saturating_add(1) {
        if !reserved.contains(key.as_str()) {
            break;
        }
        key = format!("{actual}-saved-{suffix}");
    }
    key
}
