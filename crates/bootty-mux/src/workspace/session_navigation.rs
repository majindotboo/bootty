use std::collections::HashMap;

use crate::{controller::SpaceId, snapshot::MuxSession};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct ScopedSessionTarget {
    pub scope: SpaceId,
    pub session_id: String,
}

impl ScopedSessionTarget {
    pub fn new(scope: SpaceId, session_id: impl Into<String>) -> Self {
        Self {
            scope,
            session_id: session_id.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingSessionGroup {
    pub scope: SpaceId,
    pub label: String,
    pub sessions: Vec<MuxSession>,
    pub selected_session: Option<String>,
    pub active: bool,
    pub can_return_to_last_session: bool,
    /// What Bootty calls each session when the backend name has an internal uniqueness suffix.
    pub display_names: HashMap<String, String>,
}

impl BindingSessionGroup {
    #[must_use]
    pub fn target(&self, session: &MuxSession) -> ScopedSessionTarget {
        ScopedSessionTarget::new(self.scope, session.id.clone())
    }

    pub fn display_name<'a>(&'a self, session: &'a MuxSession) -> &'a str {
        self.display_names
            .get(&session.id)
            .map_or(session.name.as_str(), String::as_str)
    }

    #[must_use]
    pub fn session_is_current(&self, session: &MuxSession) -> bool {
        self.active
            && self
                .selected_session
                .as_deref()
                .map_or(session.active, |selected| {
                    selected == session.id.as_str() || selected == session.name.as_str()
                })
    }
}
