//! Dock projects the server's canonical split tree; only Sessions geometry is saved locally.
use crate::gpui::DraggedPane;
use bootty_control::{CommandInvocation, CommandTarget};
use bootty_mux::{
    snapshot::{MuxPaneLayout, MuxPaneSplitDirection, MuxSession},
    workspace::ScopedWindowId,
};
use gpui_kit::component::Placement;
use gpui_kit::component::dock::{DockAreaState, DockPlacement, DockState, PanelInfo, PanelState};
use gpui_kit::{Axis, Pixels, Size, px};

/// Presentation for a terminal region with no open backend terminals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmptyTerminalState {
    Loading,
    Unavailable(String),
    Ready,
}

impl EmptyTerminalState {
    #[must_use]
    pub fn from_snapshot(has_snapshot: bool, unavailable_reason: Option<&str>) -> Self {
        unavailable_reason.map_or(
            if has_snapshot {
                Self::Ready
            } else {
                Self::Loading
            },
            |reason| Self::Unavailable(reason.to_owned()),
        )
    }
}

/// The center shows only the selected terminal; populated sibling sessions do not fill it.
#[must_use]
pub fn has_selected_terminal(sessions: &[MuxSession], selected: &ScopedWindowId) -> bool {
    sessions
        .iter()
        .find(|session| session.id == selected.session_id())
        .is_some_and(|session| {
            session
                .windows
                .iter()
                .any(|window| window.id == selected.window_id())
        })
}

/// Project proportions to Dock's pixel slots. Panels are resolved by stable pane keys.
#[must_use]
pub fn pane_layout_state(
    layout: &MuxPaneLayout,
    size: Size<Pixels>,
    key: &impl Fn(&str) -> String,
) -> PanelState {
    match layout {
        MuxPaneLayout::Pane(id) => PanelState {
            panel_name: "TabPanel".into(),
            info: PanelInfo::tabs(0),
            children: vec![PanelState {
                panel_name: "bootty.terminal-pane".into(),
                children: Vec::new(),
                info: PanelInfo::panel(serde_json::json!({"pane": id, "key": key(id)})),
            }],
        },
        MuxPaneLayout::Split {
            direction,
            weights,
            children,
        } => {
            let horizontal = *direction == MuxPaneSplitDirection::Right;
            let extent = if horizontal { size.width } else { size.height };
            let sizes = weights
                .iter()
                .map(|weight| extent * *weight)
                .collect::<Vec<_>>();
            PanelState {
                panel_name: "StackPanel".into(),
                info: PanelInfo::stack(
                    sizes.clone(),
                    if horizontal {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    },
                ),
                children: children
                    .iter()
                    .zip(sizes)
                    .map(|(child, extent)| {
                        let child_size = if horizontal {
                            Size {
                                width: extent,
                                ..size
                            }
                        } else {
                            Size {
                                height: extent,
                                ..size
                            }
                        };
                        pane_layout_state(child, child_size, key)
                    })
                    .collect(),
            }
        }
    }
}

/// Read the same structure back, discarding Dock's required single-child center wrapper.
#[must_use]
pub fn mux_pane_layout(state: &PanelState) -> Option<MuxPaneLayout> {
    match &state.info {
        PanelInfo::Panel(info) if state.panel_name == "bootty.terminal-pane" => {
            Some(MuxPaneLayout::Pane(info.get("pane")?.as_str()?.to_owned()))
        }
        PanelInfo::Tabs { .. } if state.children.len() == 1 => {
            mux_pane_layout(state.children.first()?)
        }
        PanelInfo::Stack { axis, sizes } => {
            let children = state
                .children
                .iter()
                .map(mux_pane_layout)
                .collect::<Option<Vec<_>>>()?;
            if children.len() == 1 {
                return children.into_iter().next();
            }
            let weights = sizes
                .iter()
                .map(|size| f32::from(*size))
                .collect::<Vec<_>>();
            let layout = MuxPaneLayout::Split {
                direction: if *axis == 0 {
                    MuxPaneSplitDirection::Right
                } else {
                    MuxPaneSplitDirection::Down
                },
                weights,
                children,
            };
            bootty_mux::pane_layout::PaneLayout::from_mux_layout(&layout)
                .map(|layout| layout.snapshot())
        }
        _ => None,
    }
}

/// Compare projections without turning float roundoff into a resize command.
#[must_use]
pub fn pane_layout_matches(a: &MuxPaneLayout, b: &MuxPaneLayout) -> bool {
    match (a, b) {
        (MuxPaneLayout::Pane(a), MuxPaneLayout::Pane(b)) => a == b,
        (
            MuxPaneLayout::Split {
                direction: a,
                weights: aw,
                children: ac,
            },
            MuxPaneLayout::Split {
                direction: b,
                weights: bw,
                children: bc,
            },
        ) => {
            a == b
                && ac.len() == bc.len()
                && aw.len() == bw.len()
                && aw.iter().zip(bw).all(|(a, b)| (a - b).abs() <= 0.000_01)
                && ac.iter().zip(bc).all(|(a, b)| pane_layout_matches(a, b))
        }
        _ => false,
    }
}

/// Pane splits are never stored in native-panels.json. Legacy center saves are discarded.
#[must_use]
pub fn fixed_panel_layout(mut layout: DockAreaState) -> DockAreaState {
    fn sessions(state: &PanelState) -> bool {
        matches!(
            state.panel_name.as_str(),
            "bootty.sessions" | "bootty.sidebar"
        ) || state.children.iter().any(sessions)
    }
    let sidebar = [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
        .into_iter()
        .flatten()
        .find(|dock| sessions(dock.panel()));
    let size = sidebar.map_or(px(240.), DockState::size);
    let open = sidebar.is_none_or(DockState::open);
    layout.center = PanelState {
        panel_name: "TabPanel".into(),
        children: Vec::new(),
        info: PanelInfo::tabs(0),
    };
    layout.left_dock = Some(DockState::new(
        PanelState {
            panel_name: "TabPanel".into(),
            children: vec![PanelState::new("bootty.sessions")],
            info: PanelInfo::tabs(0),
        },
        DockPlacement::Left,
        size,
        open,
    ));
    layout.right_dock = None;
    layout.bottom_dock = None;
    layout
}

/// Dock already resolved edge/center geometry; Bootty only chooses the command.
#[must_use]
pub fn pane_drop_command(
    drag: &DraggedPane,
    target: &str,
    placement: Option<Placement>,
) -> Option<CommandInvocation> {
    if drag.pane == target {
        return None;
    }
    let mut args = vec![drag.pane.clone(), target.to_owned()];
    let command = placement.map_or("pane.swap", |placement| {
        args.push(
            match placement {
                Placement::Left => "left",
                Placement::Right => "right",
                Placement::Top => "up",
                Placement::Bottom => "down",
            }
            .into(),
        );
        "pane.move"
    });
    Some(crate::gpui::pane_invocation(
        command,
        args,
        drag.target.clone(),
    ))
}

/// One finished divider drag submits one weights mutation through the shared invocation path.
#[must_use]
pub fn pane_resize_command(
    window: &str,
    authoritative: &bootty_mux::snapshot::MuxPaneLayout,
    proposed: &bootty_mux::snapshot::MuxPaneLayout,
    target: CommandTarget,
) -> Option<CommandInvocation> {
    if crate::workspace_composition::pane_layout_matches(authoritative, proposed) {
        return None;
    }
    let mut layout = bootty_mux::pane_layout::PaneLayout::from_mux_layout(authoritative)?;
    if !layout.resize(proposed) {
        return None;
    }
    Some(crate::gpui::pane_invocation(
        "pane.resize",
        vec![
            window.to_owned(),
            serde_json::to_string(&layout.snapshot()).ok()?,
        ],
        target,
    ))
}
