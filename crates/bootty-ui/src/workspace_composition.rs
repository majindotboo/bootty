//! Reconciliation between backend-owned terminal topology and host-owned native panels.
//!
//! The mux binding owns every terminal split tree. Dock owns placement of native panels around
//! it. The compatibility layer between them is one Dock leaf per mux window: the leaf carries
//! the window identity, and the split tree inside it is rendered from the binding's projection
//! on every frame. Dock never mirrors mux splits as nested panels, and native leaves never
//! enter a backend command.

use bootty_mux::{snapshot::MuxSession, workspace::ScopedWindowId};
use gpui_kit::component::dock::{DockPlacement, PanelInfo, PanelState};
use gpui_kit::px;

/// Presentation for a terminal region with no open backend terminals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmptyTerminalState {
    Loading,
    Unavailable(String),
    Ready { can_create: bool },
}

impl EmptyTerminalState {
    #[must_use]
    pub fn from_snapshot(
        has_snapshot: bool,
        unavailable_reason: Option<&str>,
        can_create: bool,
    ) -> Self {
        unavailable_reason.map_or(
            if has_snapshot {
                Self::Ready { can_create }
            } else {
                Self::Loading
            },
            |reason| Self::Unavailable(reason.to_owned()),
        )
    }
}

/// The center shows only the selected terminal; populated sibling sessions do not fill it.
#[must_use]
pub fn has_selected_terminal(
    sessions: &[MuxSession],
    selected: &ScopedWindowId,
    native_layout: bool,
) -> bool {
    sessions
        .iter()
        .find(|session| session.id == selected.session_id())
        .is_some_and(|session| {
            !native_layout
                || session
                    .windows
                    .iter()
                    .any(|window| window.id == selected.window_id())
        })
}

/// A container's positional metadata must follow the children that survive a transform.
fn map_panel_children(
    mut state: PanelState,
    mut transform: impl FnMut(PanelState) -> Option<PanelState>,
) -> Option<PanelState> {
    if state.children.is_empty() {
        // Empty containers are layout, not registered panels. Older saves also encoded
        // empty groups with the default leaf info; never nest those inside a tab group.
        return (matches!(state.info, PanelInfo::Panel(_))
            && !matches!(
                state.panel_name.as_str(),
                "" | "StackPanel" | "TabPanel" | "Tiles"
            ))
        .then_some(state);
    }
    let original_len = state.children.len();
    let mut retained = Vec::new();
    state.children = state
        .children
        .into_iter()
        .enumerate()
        .filter_map(|(index, child)| {
            let child = transform(child)?;
            retained.push(index);
            Some(child)
        })
        .collect();
    if state.children.is_empty() {
        return None;
    }
    if retained.len() != original_len {
        match &mut state.info {
            PanelInfo::Stack { sizes, .. } => {
                *sizes = retained
                    .iter()
                    .map(|index| sizes.get(*index).copied().unwrap_or_default())
                    .collect();
            }
            PanelInfo::Tabs { active_index } => {
                *active_index = retained
                    .iter()
                    .position(|index| index >= active_index)
                    .unwrap_or_else(|| retained.len().saturating_sub(1));
            }
            PanelInfo::Tiles { metas } => {
                *metas = retained
                    .iter()
                    .map(|index| metas.get(*index).copied().unwrap_or_default())
                    .collect();
            }
            PanelInfo::Panel(_) => {}
        }
        if matches!(state.info, PanelInfo::Stack { .. }) && state.children.len() == 1 {
            return state.children.pop();
        }
    }
    Some(state)
}

/// A single terminal leaf for one mux window. Splits inside the window are rendered by the
/// leaf itself from the binding projection; they are never Dock panels.
#[must_use]
pub fn terminal_leaf_state(id: &ScopedWindowId, title: &str) -> PanelState {
    PanelState {
        panel_name: "bootty.terminal".to_owned(),
        children: Vec::new(),
        info: PanelInfo::panel(serde_json::json!({
            "session": id.session_id(), "window": id.window_id(),
            "title": title,
        })),
    }
}

/// A fresh center tab group holding one terminal leaf.
#[must_use]
pub fn terminal_panel_state(id: &ScopedWindowId, title: &str) -> PanelState {
    tab_group_state(vec![terminal_leaf_state(id, title)])
}

/// Swap the terminal leaf in place, keeping native siblings where they are.
///
/// Stale layouts that
/// still mirror mux splits as nested terminal leaves collapse to the first one; a tree with no
/// terminal leaf gains one. The returned tree holds at most one terminal leaf.
#[must_use]
pub fn replace_terminal_region(current: PanelState, leaf: PanelState) -> PanelState {
    let mut placed = false;
    let Some(base) = normalize_terminal_region(current, &leaf, &mut placed) else {
        return tab_group_state(vec![leaf]);
    };
    if placed {
        return base;
    }
    if base.panel_name == "TabPanel" {
        let mut base = base;
        base.children.push(leaf);
        return base;
    }
    PanelState {
        panel_name: "StackPanel".to_owned(),
        // Stack and tile containers remain siblings: Kit treats a container inside
        // Tabs as a registered leaf rather than recursively restoring its panels.
        children: vec![tab_group_state(vec![leaf]), base],
        info: PanelInfo::stack(vec![px(1.0), px(1.0)], gpui_kit::Axis::Horizontal),
    }
}

fn tab_group_state(children: Vec<PanelState>) -> PanelState {
    PanelState {
        panel_name: "TabPanel".to_owned(),
        children,
        info: PanelInfo::tabs(0),
    }
}

/// Where a native panel stranded in the mux-owned center belongs.
///
/// Sidebar chrome
/// (sessions and its Space switcher) goes home to the sidebar; every inspector and
/// document goes to the right dock. The terminal singleton never enters here.
#[must_use]
pub fn center_eviction_home(panel_name: &str, sidebar: DockPlacement) -> DockPlacement {
    match panel_name {
        "bootty.sessions" | "bootty.spaces" => sidebar,
        _ => DockPlacement::Right,
    }
}

/// Keep the first terminal leaf, drop the rest, and collapse groups emptied by the removal.
/// Returns `None` when the subtree holds no surviving panel.
fn normalize_terminal_region(
    state: PanelState,
    leaf: &PanelState,
    placed: &mut bool,
) -> Option<PanelState> {
    if state.panel_name == "bootty.terminal" {
        if *placed {
            return None;
        }
        *placed = true;
        return Some(leaf.clone());
    }
    map_panel_children(state, |child| {
        normalize_terminal_region(child, leaf, placed)
    })
}
