use bootty_control::{CommandTarget, ResourceKind};

use crate::controller::{MuxController, SpaceId};

/// A mux target after the opaque control target has been validated against one binding snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExactMuxTarget {
    Binding(SpaceId),
    Session(SpaceId, String),
    Window(SpaceId, String, String),
    Pane(SpaceId, String, String, String),
}

impl ExactMuxTarget {
    #[must_use]
    pub fn window(scope: SpaceId, session_id: &str, window_id: &str) -> Self {
        Self::Window(scope, session_id.to_owned(), window_id.to_owned())
    }

    #[must_use]
    pub const fn scope(&self) -> SpaceId {
        match self {
            Self::Binding(scope)
            | Self::Session(scope, ..)
            | Self::Window(scope, ..)
            | Self::Pane(scope, ..) => *scope,
        }
    }

    #[must_use]
    pub fn ids(&self) -> (Option<&str>, Option<&str>, Option<&str>) {
        match self {
            Self::Binding(_) => (None, None, None),
            Self::Session(_, session) => (Some(session), None, None),
            Self::Window(_, session, window) => (Some(session), Some(window), None),
            Self::Pane(_, session, window, pane) => (Some(session), Some(window), Some(pane)),
        }
    }
}

/// Resolve a complete command target against the controller's observed resources.
///
/// Handles and generations are equality tokens. Returning the typed path only after all three
/// fields match prevents a stale target from being reconstructed from a changed name.
#[must_use]
pub fn exact_mux_target(
    scope: SpaceId,
    mux: &MuxController,
    target: &CommandTarget,
    binding_handle: &str,
) -> Option<ExactMuxTarget> {
    for session in mux.sessions() {
        if let Some(generation) = mux.session_generation(&session.id) {
            let session_target = CommandTarget {
                kind: ResourceKind::Session,
                handle: serde_json::Value::from(vec![binding_handle, session.id.as_str()])
                    .to_string(),
                generation,
            };
            if target == &session_target {
                return Some(ExactMuxTarget::Session(scope, session.id.clone()));
            }
        }
        for window in &session.windows {
            if let Some(generation) = mux.window_generation(&session.id, &window.id) {
                let window_target = CommandTarget {
                    kind: ResourceKind::MuxWindow,
                    handle: serde_json::Value::from(vec![
                        binding_handle,
                        session.id.as_str(),
                        window.id.as_str(),
                    ])
                    .to_string(),
                    generation,
                };
                if target == &window_target {
                    return Some(ExactMuxTarget::Window(
                        scope,
                        session.id.clone(),
                        window.id.clone(),
                    ));
                }
            }
            for pane in std::iter::once(&window.anchor).chain(&window.panes) {
                let Some(pane_id) = pane.pane_id.as_deref() else {
                    continue;
                };
                let Some(generation) = mux.pane_generation(&session.id, &window.id, pane_id) else {
                    continue;
                };
                let pane_target = CommandTarget {
                    kind: ResourceKind::Pane,
                    handle: serde_json::Value::from(vec![
                        binding_handle,
                        session.id.as_str(),
                        window.id.as_str(),
                        pane_id,
                    ])
                    .to_string(),
                    generation,
                };
                if target == &pane_target {
                    return Some(ExactMuxTarget::Pane(
                        scope,
                        session.id.clone(),
                        window.id.clone(),
                        pane_id.to_owned(),
                    ));
                }
                let terminal_target = CommandTarget {
                    kind: ResourceKind::Terminal,
                    handle: pane_target.handle,
                    generation,
                };
                if target == &terminal_target {
                    return Some(ExactMuxTarget::Pane(
                        scope,
                        session.id.clone(),
                        window.id.clone(),
                        pane_id.to_owned(),
                    ));
                }
            }
        }
    }
    None
}
