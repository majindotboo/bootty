use serde::{Deserialize, Serialize};

/// The `/` prefix a label shares with its siblings, or `""` for a session on its own.
///
/// Grouping is read off the label rather than stored, so nothing has to be kept in step with it.
fn label_group(label: &str) -> &str {
    label.split_once('/').map_or("", |(group, _)| group)
}

/// Saved work state, independent of the backend's process and attachment state.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    #[default]
    Active,
    Settled,
}

/// Durable presentation state. Archive and deletion retain every other value for restore.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
// These independent flags survive archive/delete so restoring preserves prior presentation.
#[allow(clippy::struct_excessive_bools)]
pub struct SessionState {
    pub lifecycle: SessionLifecycle,
    pub pinned: bool,
    pub archived: bool,
    pub deleted: bool,
    pub hidden: bool,
    pub snoozed_until: Option<i64>,
    /// Absolute UTC seconds of accepted input; absent for work with no recorded activity.
    pub last_activity_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionView {
    Active,
    Settled,
    Archived,
    Snoozed,
    Hidden,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStateChange {
    SetLifecycle(SessionLifecycle),
    SetPinned(bool),
    Archive,
    RestoreArchive,
    SnoozeUntil(i64),
    ClearSnooze,
    SetHidden(bool),
    Delete,
    RestoreDeleted,
    RecordActivity { at: i64, now: i64 },
    AcceptInput { at: i64, now: i64 },
}

impl SessionState {
    /// `now` is supplied by the caller as absolute UTC seconds; this owner never starts a timer.
    #[must_use]
    pub fn view(self, now: i64) -> SessionView {
        if self.deleted {
            SessionView::Deleted
        } else if self.archived {
            SessionView::Archived
        } else if self.hidden {
            SessionView::Hidden
        } else if self.is_snoozed(now) {
            SessionView::Snoozed
        } else {
            match self.lifecycle {
                SessionLifecycle::Active => SessionView::Active,
                SessionLifecycle::Settled => SessionView::Settled,
            }
        }
    }

    #[must_use]
    pub fn is_snoozed(self, now: i64) -> bool {
        self.snoozed_until.is_some_and(|until| until > now)
    }

    #[must_use]
    pub fn is_overdue(self, now: i64) -> bool {
        !self.deleted
            && !self.archived
            && !self.hidden
            && self.snoozed_until.is_some_and(|until| until <= now)
    }

    #[must_use]
    pub fn is_visible(self, now: i64) -> bool {
        matches!(self.view(now), SessionView::Active | SessionView::Settled)
    }
}

/// One session a Space claims, keyed by the identity the multiplexer carries for it.
///
/// Nothing here keys on a name: a name is only ever a hint or a label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSession {
    pub identity: String,
    /// The backend's last reported name, used to address its attachment.
    pub backend_name: String,
    /// What bootty calls the session, or empty for "whatever the backend calls it". This is why a
    /// shared server's `-2` suffix never reaches the sidebar.
    pub display_name: String,
    /// Whether the user chose `display_name` instead of bootty generating it from the directory.
    pub explicit: bool,
    /// The saved project directory used when restoring terminal state.
    pub cwd: String,
    pub state: SessionState,
    pub terminal_snapshot: Option<std::sync::Arc<crate::session_snapshot::SavedTerminalSession>>,
}

impl WorkspaceSession {
    /// The name to show. Bootty's own if it has one, otherwise the backend's.
    #[must_use]
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.backend_name
        } else {
            &self.display_name
        }
    }
}

/// The sessions one Space claims, in sidebar order. Membership and order are one list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionMembership {
    sessions: Vec<WorkspaceSession>,
}

impl SessionMembership {
    #[must_use]
    pub const fn from_sessions(sessions: Vec<WorkspaceSession>) -> Self {
        Self { sessions }
    }

    #[must_use]
    pub fn sessions(&self) -> &[WorkspaceSession] {
        &self.sessions
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    #[must_use]
    pub fn get(&self, identity: &str) -> Option<&WorkspaceSession> {
        self.sessions
            .iter()
            .find(|session| session.identity == identity)
    }

    #[must_use]
    pub fn contains(&self, identity: &str) -> bool {
        self.get(identity).is_some()
    }

    /// The claimed sessions' backend names, in order, for applying that order to the backend.
    #[must_use]
    pub fn backend_names(&self) -> Vec<String> {
        self.sessions
            .iter()
            .map(|session| session.backend_name.clone())
            .collect()
    }

    /// Adds a session to the end of the Space, or does nothing if it is already claimed.
    pub fn claim(&mut self, session: WorkspaceSession) -> bool {
        if session.identity.is_empty() || self.contains(&session.identity) {
            return false;
        }
        let group = label_group(session.label()).to_owned();
        // A session joins its group rather than the end of the list, so `agents/review` lands
        // beside `agents/main` instead of below whatever happens to be last.
        let insert_at = (!group.is_empty())
            .then(|| {
                self.sessions
                    .iter()
                    .rposition(|existing| label_group(existing.label()) == group)
                    .map(|last| last.saturating_add(1))
            })
            .flatten()
            .unwrap_or(self.sessions.len());
        self.sessions.insert(insert_at, session);
        true
    }

    /// Drops a session from this Space. The session itself is untouched.
    pub fn release(&mut self, identity: &str) -> Option<WorkspaceSession> {
        let position = self
            .sessions
            .iter()
            .position(|session| session.identity == identity)?;
        Some(self.sessions.remove(position))
    }

    /// Records the name the backend now reports, which a rename from anywhere changes.
    pub fn observe_backend_name(&mut self, identity: &str, backend_name: &str) -> bool {
        let Some(session) = self.session_mut(identity) else {
            return false;
        };
        if session.backend_name == backend_name {
            return false;
        }
        backend_name.clone_into(&mut session.backend_name);
        true
    }

    /// Sets what bootty calls a session, and whether the user chose that name.
    pub fn set_display_name(&mut self, identity: &str, display_name: &str, explicit: bool) -> bool {
        let Some(session) = self.session_mut(identity) else {
            return false;
        };
        if session.display_name == display_name && session.explicit == explicit {
            return false;
        }
        display_name.clone_into(&mut session.display_name);
        session.explicit = explicit;
        true
    }

    pub fn set_cwd(&mut self, identity: &str, cwd: &str) -> bool {
        let Some(session) = self.session_mut(identity) else {
            return false;
        };
        if session.cwd == cwd {
            return false;
        }
        cwd.clone_into(&mut session.cwd);
        true
    }

    #[cfg(feature = "terminal-runtime")]
    pub(crate) fn set_terminal_snapshot(
        &mut self,
        identity: &str,
        snapshot: crate::session_snapshot::SavedTerminalSession,
    ) -> bool {
        let Some(session) = self.session_mut(identity) else {
            return false;
        };
        if session.terminal_snapshot.as_deref() == Some(&snapshot) {
            return false;
        }
        session.terminal_snapshot = Some(std::sync::Arc::new(snapshot));
        true
    }

    /// Change saved work state without changing membership or backend topology.
    pub fn set_state(&mut self, identity: &str, change: SessionStateChange) -> bool {
        let Some(session) = self.session_mut(identity) else {
            return false;
        };
        let previous = session.state;
        match change {
            SessionStateChange::SetLifecycle(lifecycle) => {
                session.state.lifecycle = lifecycle;
                if lifecycle == SessionLifecycle::Settled {
                    session.state.pinned = false;
                }
            }
            SessionStateChange::SetPinned(pinned) => {
                session.state.pinned = pinned;
                if pinned {
                    session.state.lifecycle = SessionLifecycle::Active;
                    session.state.snoozed_until = None;
                }
            }
            SessionStateChange::Archive => session.state.archived = true,
            SessionStateChange::RestoreArchive => session.state.archived = false,
            SessionStateChange::SnoozeUntil(until) => session.state.snoozed_until = Some(until),
            SessionStateChange::ClearSnooze => session.state.snoozed_until = None,
            SessionStateChange::SetHidden(hidden) => session.state.hidden = hidden,
            SessionStateChange::Delete => session.state.deleted = true,
            SessionStateChange::RestoreDeleted => session.state.deleted = false,
            SessionStateChange::RecordActivity { at, now } => {
                if at >= 0
                    && at <= now
                    && session.state.last_activity_at.is_none_or(|last| at > last)
                {
                    session.state.last_activity_at = Some(at);
                }
            }
            SessionStateChange::AcceptInput { at, now } => {
                if at >= 0
                    && at <= now
                    && session.state.last_activity_at.is_none_or(|last| at >= last)
                {
                    session.state.last_activity_at = Some(at);
                    session.state.lifecycle = SessionLifecycle::Active;
                }
            }
        }
        session.state != previous
    }

    /// Moves `source` before `before`, or to the end when `before` is `None`.
    ///
    /// A session in a group cannot leave it, so dragging one past another group takes the whole
    /// group with it. An ungrouped session travels alone.
    pub fn move_before(&mut self, source: &str, before: Option<&str>) -> bool {
        let Some(from) = self.position(source) else {
            return false;
        };
        let anchor = match before {
            Some(before) => match self.position(before) {
                Some(to) => Some(to),
                None => return false,
            },
            None => None,
        };
        let Some(source) = self.sessions.get(from) else {
            return false;
        };
        let source_group = label_group(source.label()).to_owned();
        if !source_group.is_empty()
            && anchor
                .and_then(|to| self.sessions.get(to))
                .is_some_and(|session| label_group(session.label()) == source_group)
        {
            return self.move_within_group(from, anchor);
        }
        self.move_block(&source_group, from, anchor)
    }

    /// Moves a session one place, carrying its group when it steps past a group boundary.
    pub fn move_by(&mut self, identity: &str, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        let Some(from) = self.position(identity) else {
            return false;
        };
        let neighbour = if delta < 0 {
            from.checked_sub(1)
        } else {
            from.checked_add(1)
                .filter(|next| *next < self.sessions.len())
        };
        let Some(neighbour) = neighbour else {
            return false;
        };
        let (Some(source), Some(neighbour_session)) =
            (self.sessions.get(from), self.sessions.get(neighbour))
        else {
            return false;
        };
        let group = label_group(source.label()).to_owned();
        if !group.is_empty() && label_group(neighbour_session.label()) == group {
            self.sessions.swap(from, neighbour);
            return true;
        }
        // Stepping down means landing after the neighbour's block, which is before whatever
        // follows it -- or the end of the list when nothing does.
        let anchor = if delta < 0 {
            self.block_start(neighbour)
        } else {
            self.block_end(neighbour)
        };
        self.move_block(&group, from, anchor)
    }

    /// The span `index` belongs to: its whole group, or just itself when it is ungrouped.
    fn block(&self, group: &str, index: usize) -> Option<(usize, usize)> {
        let (before, after) = self.sessions.split_at_checked(index)?;
        after.first()?;
        if group.is_empty() {
            return Some((index, index.saturating_add(1)));
        }
        let member = |session: &&WorkspaceSession| label_group(session.label()) == group;
        let preceding = before.iter().rev().take_while(member).count();
        let following = after.iter().take_while(member).count();
        Some((
            index.saturating_sub(preceding),
            index.saturating_add(following),
        ))
    }

    fn block_start(&self, index: usize) -> Option<usize> {
        let group = label_group(self.sessions.get(index)?.label());
        self.block(group, index).map(|(start, _)| start)
    }

    fn block_end(&self, index: usize) -> Option<usize> {
        let group = label_group(self.sessions.get(index)?.label());
        let (_, end) = self.block(group, index)?;
        (end < self.sessions.len()).then_some(end)
    }

    fn move_within_group(&mut self, from: usize, anchor: Option<usize>) -> bool {
        let to = anchor.unwrap_or(self.sessions.len());
        let insert_at = if to > from { to.saturating_sub(1) } else { to };
        if insert_at == from {
            return false;
        }
        let session = self.sessions.remove(from);
        self.sessions.insert(insert_at, session);
        true
    }

    /// Lifts the block containing `from` out and reinserts it at `anchor`.
    fn move_block(&mut self, group: &str, from: usize, anchor: Option<usize>) -> bool {
        let Some((start, end)) = self.block(group, from) else {
            return false;
        };
        let anchor = anchor.and_then(|anchor| self.block_start(anchor));
        let insert_at = anchor.unwrap_or(self.sessions.len());
        if insert_at >= start && insert_at <= end {
            return false;
        }
        let block = self.sessions.drain(start..end).collect::<Vec<_>>();
        let insert_at = if insert_at > start {
            insert_at.saturating_sub(block.len())
        } else {
            insert_at
        };
        self.sessions.splice(insert_at..insert_at, block);
        true
    }

    fn session_mut(&mut self, identity: &str) -> Option<&mut WorkspaceSession> {
        self.sessions
            .iter_mut()
            .find(|session| session.identity == identity)
    }

    fn position(&self, identity: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.identity == identity)
    }
}
