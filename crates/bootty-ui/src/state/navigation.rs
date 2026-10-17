//! Back and Forward through the sessions a window showed.

use bootty_mux::workspace::ScopedSessionTarget;

use super::AppState;

/// How many sessions Back can return through.
const NAVIGATION_LIMIT: usize = 64;

/// A step through the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationStep {
    Back,
    Forward,
}

/// The sessions a window showed, oldest first, and which of them it shows now. It lives in
/// memory only: a restart starts a new history.
#[derive(Default)]
pub(super) struct NavHistory {
    entries: Vec<ScopedSessionTarget>,
    current: usize,
}

impl NavHistory {
    /// Records `shown` as the current session. A session reached some other way than Back or
    /// Forward drops the entries ahead of the current one, as a browser does.
    fn visit(&mut self, shown: ScopedSessionTarget) {
        if self.entries.get(self.current) == Some(&shown) {
            return;
        }
        self.entries.truncate(self.current.saturating_add(1));
        self.entries.push(shown);
        if self.entries.len() > NAVIGATION_LIMIT {
            self.entries.remove(0);
        }
        self.current = self.entries.len().saturating_sub(1);
    }

    /// The entry one `step` from the current one, with its index.
    fn neighbor(&self, step: NavigationStep) -> Option<(usize, &ScopedSessionTarget)> {
        let index = match step {
            NavigationStep::Back => self.current.checked_sub(1)?,
            NavigationStep::Forward => self.current.checked_add(1)?,
        };
        self.entries.get(index).map(|entry| (index, entry))
    }

    fn forget(&mut self, index: usize) {
        self.entries.remove(index);
        if index < self.current {
            self.current = self.current.saturating_sub(1);
        }
    }
}

impl AppState {
    /// Whether `step` has a session to go to.
    #[must_use]
    pub fn can_navigate(&self, step: NavigationStep) -> bool {
        self.navigation.neighbor(step).is_some()
    }

    /// Shows the session one `step` away in this window's history, passing over sessions that
    /// have since closed.
    pub(super) fn navigate(&mut self, step: NavigationStep) -> bool {
        while let Some((index, target)) = self
            .navigation
            .neighbor(step)
            .map(|(index, target)| (index, target.clone()))
        {
            if !self.session_is_live(&target) {
                self.navigation.forget(index);
                continue;
            }
            if !self.activate_scoped_session_from_ui(&target) {
                return false;
            }
            self.navigation.current = index;
            return true;
        }
        false
    }

    /// Records the session this window shows now. Runs once a frame, after every way a
    /// selection can change.
    pub(super) fn record_navigation(&mut self) {
        let binding = &self.workspace.active.binding;
        let Some(session) = binding
            .mux()
            .selected_session()
            .and_then(|selected| binding.mux().session_by_id_or_name(selected))
        else {
            return;
        };
        let shown = ScopedSessionTarget::new(self.workspace.active.id, session.id.clone());
        self.navigation.visit(shown);
    }

    fn session_is_live(&self, target: &ScopedSessionTarget) -> bool {
        self.workspace.binding(target.scope).is_some_and(|binding| {
            binding
                .mux()
                .session_by_id_or_name(&target.session_id)
                .is_some()
        })
    }
}
