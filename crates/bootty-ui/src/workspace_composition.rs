//! Reconciliation between backend-owned terminal topology and host-owned native panels.
//!
//! The mux binding owns every terminal split tree. Dock owns placement of native panels around
//! it. The compatibility layer between them is one Dock leaf per mux window: the leaf carries
//! the window identity, and the split tree inside it is rendered from the binding's projection
//! on every frame. Dock never mirrors mux splits as nested panels, and native leaves never
//! enter a backend command.

use bootty_mux::{snapshot::MuxSession, workspace::ScopedWindowId};
use gpui_kit::component::dock::{DockAreaState, DockPlacement, DockState, PanelInfo, PanelState};
use gpui_kit::px;

pub use crate::gpui_dock::surfaces::TerminalSurfaceOrigin;

/// Each outer tab owns one persisted center; split panels remain inside that center.
#[must_use]
pub fn surface_center_key(binding: &str, task: &str, kind: &str, id: &str) -> String {
    serde_json::json!([binding, task, kind, id]).to_string()
}

/// A saved agent destination has stable record identity; current generation comes from the catalog.
#[must_use = "Validate the saved destination before focusing its current catalog target"]
pub fn restored_agent_center_destination(key: &str) -> Option<(String, String, String)> {
    let parts = serde_json::from_str::<Vec<String>>(key).ok()?;
    let [binding, task, kind, id] = parts.as_slice() else {
        return None;
    };
    (kind == "agent" && !binding.is_empty() && !task.is_empty() && !id.is_empty())
        .then(|| (binding.clone(), task.clone(), id.clone()))
}

#[must_use]
pub fn center_contains_conversation(state: &PanelState, id: &str) -> bool {
    (state.panel_name == "bootty.conversation"
        && matches!(&state.info, PanelInfo::Panel(info) if info.get("target").and_then(|target| target.get("handle")).and_then(serde_json::Value::as_str) == Some(id)))
        || state
            .children
            .iter()
            .any(|child| center_contains_conversation(child, id))
}

#[must_use]
pub fn center_contains_window(
    state: &PanelState,
    origin: &crate::gpui_dock::surfaces::TerminalSurfaceOrigin,
) -> bool {
    (state.panel_name == "bootty.terminal-window"
        && matches!(&state.info, PanelInfo::Panel(info) if serde_json::from_value::<crate::gpui_dock::surfaces::TerminalSurfaceOrigin>(info.clone()).ok().as_ref() == Some(origin)))
        || state
            .children
            .iter()
            .any(|child| center_contains_window(child, origin))
}

/// Adopt only old outer surface tabs. Actual saved splits keep their content and geometry.
pub fn take_legacy_surface_tabs(
    state: &mut PanelState,
    binding: &str,
    task: &str,
) -> Vec<(String, PanelState)> {
    if state.panel_name != "TabPanel" {
        return Vec::new();
    }
    let mut tabs = Vec::new();
    state.children.retain(|child| {
        let PanelInfo::Panel(info) = &child.info else {
            return true;
        };
        let key = match child.panel_name.as_str() {
            "bootty.conversation" => info
                .get("target")
                .and_then(|target| target.get("handle"))
                .and_then(serde_json::Value::as_str)
                .map(|id| surface_center_key(binding, task, "agent", id)),
            "bootty.terminal-window" => serde_json::from_value::<
                crate::gpui_dock::surfaces::TerminalSurfaceOrigin,
            >(info.clone())
            .ok()
            .filter(|origin| origin.binding_id == binding && origin.task_identity == task)
            .map(|origin| surface_center_key(binding, task, "window", &origin.window_key)),
            _ => None,
        };
        key.is_none_or(|key| {
            tabs.push((key, tab_group_state(vec![child.clone()])));
            false
        })
    });
    if !tabs.is_empty() {
        state.info = PanelInfo::tabs(0);
    }
    tabs
}

pub fn center_primary_window_key(state: &PanelState) -> Option<String> {
    if let PanelInfo::Panel(info) = &state.info {
        let field = match state.panel_name.as_str() {
            "bootty.terminal" => Some("window"),
            "bootty.terminal-window" => Some("window_key"),
            _ => None,
        };
        if let Some(field) = field {
            return info
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
        }
    }
    state.children.iter().find_map(center_primary_window_key)
}

/// Adopt one retained cold layout without replacing another tab's nontrivial content.
pub fn adopt_saved_window_center(
    centers: &mut std::collections::BTreeMap<String, PanelState>,
    source: &str,
    owner: &str,
) -> bool {
    let (Ok(source_parts), Ok(owner_parts)) = (
        serde_json::from_str::<Vec<String>>(source),
        serde_json::from_str::<Vec<String>>(owner),
    ) else {
        return false;
    };
    let [binding, task, kind, window] = owner_parts.as_slice() else {
        return false;
    };
    if kind != "window" || window.is_empty() {
        return false;
    }
    let valid_source = match source_parts.as_slice() {
        [source_binding, source_task] => source_binding == binding && source_task == task,
        [source_binding, source_task, source_kind, source_window] => {
            source_binding == binding
                && source_task == task
                && source_kind == "window"
                && source_window.is_empty()
        }
        _ => false,
    };
    if !valid_source || !centers.contains_key(source) {
        return false;
    }
    if let Some(destination) = centers.get(owner) {
        let mut leaves = Vec::new();
        native_panel_leaves(destination, &mut leaves);
        if leaves.len() != 1
            || leaves
                .first()
                .is_none_or(|leaf| leaf.panel_name != "bootty.terminal")
        {
            return false;
        }
    }
    let Some(center) = centers.remove(source) else {
        return false;
    };
    centers.insert(owner.to_owned(), center);
    true
}

/// Remove one authoritatively closed terminal surface, preserving surviving split metadata.
#[must_use = "Use the remaining layout to retire the closed surface"]
pub fn remove_terminal_surface(
    state: PanelState,
    origin: &crate::gpui_dock::surfaces::TerminalSurfaceOrigin,
) -> Option<PanelState> {
    if state.panel_name == "bootty.terminal-window" && center_contains_window(&state, origin) {
        return None;
    }
    map_panel_children(state, |child| remove_terminal_surface(child, origin))
}

/// Closing an agent preserves its siblings and gives an orphaned outer center a surviving owner.
#[must_use = "Publish the remaining center and its owner together"]
pub fn close_conversation_center(
    key: &str,
    state: PanelState,
    id: &str,
) -> Option<(String, PanelState)> {
    let remaining = remove_conversation_surface(state, id)?;
    let owner = restored_agent_center_destination(key)
        .filter(|(_, _, owner)| owner == id)
        .and_then(|(binding, task, _)| {
            center_primary_window_key(&remaining)
                .filter(|window| !window.is_empty())
                .map(|window| surface_center_key(&binding, &task, "window", &window))
                .or_else(|| {
                    center_primary_conversation(&remaining)
                        .map(|agent| surface_center_key(&binding, &task, "agent", agent))
                })
        })
        .unwrap_or_else(|| key.to_owned());
    Some((owner, remaining))
}

/// Retire exact saved leaves, including inactive centers, without overwriting another tab.
///
/// # Errors
/// Returns an error for invalid saved ownership or a populated destination collision.
pub fn close_saved_conversation_centers(
    centers: &mut std::collections::BTreeMap<String, PanelState>,
    binding: &str,
    task: &str,
    id: &str,
) -> Result<Vec<(String, Option<String>)>, String> {
    let mut remaining = centers.clone();
    let mut transitions = Vec::new();
    for (key, state) in centers
        .iter()
        .filter(|(_, state)| center_contains_conversation(state, id))
    {
        let parts = serde_json::from_str::<Vec<String>>(key).map_err(|error| error.to_string())?;
        let [owner_binding, owner_task, ..] = parts.as_slice() else {
            continue;
        };
        if owner_binding != binding || owner_task != task {
            continue;
        }
        remaining.remove(key);
        let next = close_conversation_center(key, state.clone(), id);
        if let Some((owner, state)) = &next {
            if owner != key
                && remaining.get(owner).is_some_and(|existing| {
                    existing != state && !conversation_center_is_closed(existing)
                })
            {
                return Err("The surviving pane already belongs to another tab".to_owned());
            }
            remaining.insert(owner.clone(), state.clone());
        }
        transitions.push((key.clone(), next.map(|(owner, _)| owner)));
    }
    remaining.insert(
        surface_center_key(binding, task, "agent", id),
        PanelState {
            panel_name: "TabPanel".to_owned(),
            children: Vec::new(),
            info: PanelInfo::tabs(0),
        },
    );
    *centers = remaining;
    Ok(transitions)
}

/// An explicit empty agent center retains a closed presentation while its record stays in History.
#[must_use]
pub fn conversation_center_is_closed(state: &PanelState) -> bool {
    state.panel_name == "TabPanel" && state.children.is_empty()
}

fn remove_conversation_surface(state: PanelState, id: &str) -> Option<PanelState> {
    if state.panel_name == "bootty.conversation" && center_contains_conversation(&state, id) {
        return None;
    }
    map_panel_children(state, |child| remove_conversation_surface(child, id))
}

fn center_primary_conversation(state: &PanelState) -> Option<&str> {
    if state.panel_name == "bootty.conversation"
        && let PanelInfo::Panel(info) = &state.info
    {
        return info.get("target")?.get("handle")?.as_str();
    }
    state.children.iter().find_map(center_primary_conversation)
}

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
        // Stack containers remain siblings: Kit treats a container inside
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

fn native_panel_leaves(state: &PanelState, output: &mut Vec<PanelState>) {
    if state.children.is_empty() {
        if matches!(state.info, PanelInfo::Panel(_))
            && !matches!(
                state.panel_name.as_str(),
                "" | "StackPanel" | "TabPanel" | "Tiles"
            )
        {
            output.push(state.clone());
        }
    } else {
        for child in &state.children {
            native_panel_leaves(child, output);
        }
    }
}
fn selected_native_panel(state: &PanelState) -> Option<&PanelState> {
    if state.children.is_empty() {
        return Some(state);
    }
    let ix = state.info.active_index().unwrap_or_default();
    state.children.get(ix).and_then(selected_native_panel)
}

/// Restore native content into its fixed homes without changing backend terminal topology.
/// Older movable layouts retain every document and the side sizes and visibility.
#[must_use]
pub fn fixed_panel_layout(mut layout: DockAreaState) -> DockAreaState {
    let center = retained_center(layout.center.clone(), &mut false);
    let active = layout
        .right_dock
        .as_ref()
        .and_then(|dock| selected_native_panel(dock.panel()))
        .cloned();
    let mut panels = Vec::new();
    native_panel_leaves(&layout.center, &mut panels);
    for dock in [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
        .into_iter()
        .flatten()
    {
        native_panel_leaves(dock.panel(), &mut panels);
    }
    let terminal = panels
        .iter()
        .find(|panel| {
            matches!(
                panel.panel_name.as_str(),
                "bootty.terminal" | "bootty.attachment"
            )
        })
        .cloned();
    let sidebar_home = [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
        .into_iter()
        .flatten()
        .find(|dock| {
            let mut leaves_here = Vec::new();
            native_panel_leaves(dock.panel(), &mut leaves_here);
            leaves_here.iter().any(|panel| {
                matches!(
                    panel.panel_name.as_str(),
                    "bootty.sessions" | "bootty.sidebar"
                )
            })
        });
    let left_size = sidebar_home.map_or(px(240.0), DockState::size);
    let left_open = sidebar_home.is_none_or(DockState::open);
    let right_size = layout
        .right_dock
        .as_ref()
        .map_or(px(320.0), DockState::size);
    let right_open = layout.right_dock.as_ref().is_some_and(DockState::open);
    let mut tools = Vec::new();
    for panel in panels {
        if !matches!(
            panel.panel_name.as_str(),
            "bootty.terminal"
                | "bootty.attachment"
                | "bootty.sessions"
                | "bootty.sidebar"
                | "bootty.spaces"
                | "bootty.agents"
                | "bootty.runs"
                | "bootty.terminal-window"
                | "bootty.surface-chooser"
        ) && !tools.contains(&panel)
        {
            tools.push(panel);
        }
    }
    let active_ix = active
        .as_ref()
        .and_then(|panel| tools.iter().position(|candidate| candidate == panel))
        .unwrap_or_default();
    layout.center = center.unwrap_or_else(|| tab_group_state(terminal.into_iter().collect()));
    layout.left_dock = Some(DockState::new(
        tab_group_state(vec![PanelState::new("bootty.sessions")]),
        DockPlacement::Left,
        left_size,
        left_open,
    ));
    let mut right = tab_group_state(tools);
    right.info = PanelInfo::tabs(active_ix);
    layout.right_dock = Some(DockState::new(
        right,
        DockPlacement::Right,
        right_size,
        right_open,
    ));
    layout.bottom_dock = None;
    layout
}

fn retained_center(state: PanelState, terminal: &mut bool) -> Option<PanelState> {
    if matches!(state.info, PanelInfo::Panel(_)) && state.children.is_empty() {
        if state.panel_name == "bootty.terminal" {
            if *terminal {
                return None;
            }
            *terminal = true;
        }
        return matches!(
            state.panel_name.as_str(),
            "bootty.terminal" | "bootty.attachment" | "bootty.terminal-window"
        )
        .then_some(state);
    }
    map_panel_children(state, |child| retained_center(child, terminal))
}

/// An admitted window may be retired only after its exact binding publishes live topology.
#[must_use]
pub fn terminal_window_is_closed(
    binding: &bootty_mux::workspace::BindingRuntime,
    task_identity: &str,
    window: &bootty_mux::workspace::ScopedWindowId,
) -> bool {
    if !binding.mux().has_session_snapshot()
        || binding.window_id(
            window.session_id().to_owned(),
            window.window_id().to_owned(),
        ) != *window
        || binding.sessions().get(task_identity).is_none()
    {
        return false;
    }
    binding
        .session_attachment(task_identity)
        .is_none_or(|session| {
            session.id != window.session_id()
                || !session
                    .windows
                    .iter()
                    .any(|live| live.id == window.window_id())
        })
}
