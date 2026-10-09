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

    /// Project a live mux resource into its wire target. Unknown resources have no target.
    /// The host owns `binding_handle`; this module owns all paths below that binding.
    #[must_use]
    pub fn command_target(
        &self,
        kind: ResourceKind,
        mux: &MuxController,
        binding_handle: &str,
    ) -> Option<CommandTarget> {
        let (path, generation) = match (kind, self) {
            (ResourceKind::Binding, Self::Binding(_)) => {
                return Some(CommandTarget {
                    kind,
                    handle: binding_handle.to_owned(),
                    generation: mux.binding_generation(),
                });
            }
            (ResourceKind::Terminal, Self::Binding(_)) => (
                vec![binding_handle, "active_terminal"],
                mux.binding_generation(),
            ),
            (ResourceKind::Session | ResourceKind::Terminal, Self::Session(_, session)) => (
                vec![binding_handle, session.as_str()],
                mux.session_generation(session)?,
            ),
            (ResourceKind::MuxWindow, Self::Window(_, session, window)) => (
                vec![binding_handle, session.as_str(), window.as_str()],
                mux.window_generation(session, window)?,
            ),
            (ResourceKind::Pane | ResourceKind::Terminal, Self::Pane(_, session, window, pane)) => {
                (
                    vec![
                        binding_handle,
                        session.as_str(),
                        window.as_str(),
                        pane.as_str(),
                    ],
                    mux.pane_generation(session, window, pane)?,
                )
            }
            _ => return None,
        };
        Some(CommandTarget {
            kind,
            handle: serde_json::Value::from(path).to_string(),
            generation,
        })
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
    let candidate = if target.kind == ResourceKind::Binding {
        ExactMuxTarget::Binding(scope)
    } else {
        // The encoded path is only a lookup hint. Authority comes from the observed resource
        // generation and the complete, canonical wire target comparison below.
        let path: Vec<String> = serde_json::from_str(&target.handle).ok()?;
        match (target.kind, path.as_slice()) {
            (ResourceKind::Terminal, [binding, active])
                if binding == binding_handle && active == "active_terminal" =>
            {
                ExactMuxTarget::Binding(scope)
            }
            (ResourceKind::Session | ResourceKind::Terminal, [binding, session])
                if binding == binding_handle =>
            {
                ExactMuxTarget::Session(scope, session.clone())
            }
            (ResourceKind::MuxWindow, [binding, session, window]) if binding == binding_handle => {
                ExactMuxTarget::window(scope, session, window)
            }
            (ResourceKind::Pane | ResourceKind::Terminal, [binding, session, window, pane])
                if binding == binding_handle =>
            {
                ExactMuxTarget::Pane(scope, session.clone(), window.clone(), pane.clone())
            }
            _ => return None,
        }
    };
    (candidate
        .command_target(target.kind, mux, binding_handle)
        .as_ref()
        == Some(target))
    .then_some(candidate)
}
