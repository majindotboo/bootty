//! Server-owned layout transformations alongside topology mutations.
use std::collections::HashMap;

use crate::{
    command::{MuxCommand, MuxDirection, MuxSplitDirection},
    pane_layout::{Direction, PaneLayout, SplitDirection},
    snapshot::{MuxPaneLayout, MuxSnapshot},
};

pub(super) fn updated_layouts(
    before: &MuxSnapshot,
    after: &MuxSnapshot,
    command: &MuxCommand,
) -> anyhow::Result<HashMap<(String, String), MuxPaneLayout>> {
    let mut layouts = HashMap::new();
    for session in &before.sessions {
        for window in &session.windows {
            if let Some(layout) = window.layout.as_ref().and_then(PaneLayout::from_mux_layout) {
                layouts.insert((session.id.clone(), window.id.clone()), layout);
            }
        }
    }
    rearrange(before, command, &mut layouts)?;
    let session_id = command.session_id();
    let mut result = HashMap::new();
    for session in &after.sessions {
        for window in &session.windows {
            let ids: Vec<_> = window
                .panes
                .iter()
                .filter_map(|p| p.pane_id.clone())
                .collect();
            let Some(first) = ids.first() else {
                continue;
            };
            let key = (session.id.clone(), window.id.clone());
            let mut layout = layouts
                .remove(&key)
                .or_else(|| window.layout.as_ref().and_then(PaneLayout::from_mux_layout))
                .unwrap_or_else(|| PaneLayout::single(first.clone()));
            match command {
                MuxCommand::SplitPane {
                    pane_id, direction, ..
                }
                | MuxCommand::CreatePane {
                    pane_id, direction, ..
                } if session.id == session_id => {
                    if let Some(source) = pane_id.as_deref().or_else(|| {
                        before
                            .sessions
                            .iter()
                            .find(|s| s.id == session_id)
                            .and_then(|s| s.anchor.pane_id.as_deref())
                    }) {
                        layout.set_focus(source);
                    }
                    let direction = match direction {
                        MuxSplitDirection::Right => SplitDirection::Right,
                        MuxSplitDirection::Down => SplitDirection::Down,
                    };
                    layout.reconcile_with_new_pane_direction(&ids, direction);
                }
                _ => layout.reconcile(&ids),
            }
            let leaves = layout.panes();
            anyhow::ensure!(
                leaves.len() == ids.len() && ids.iter().all(|id| leaves.contains(id)),
                "pane layout exceeds representable split weights"
            );
            if let Some(focus) = &window.anchor.pane_id {
                layout.set_focus(focus);
            }
            result.insert(key, layout.snapshot());
        }
    }
    Ok(result)
}

fn rearrange(
    before: &MuxSnapshot,
    command: &MuxCommand,
    layouts: &mut HashMap<(String, String), PaneLayout>,
) -> anyhow::Result<()> {
    let session_id = command.session_id();
    let key_for = |pane: &str| {
        before
            .sessions
            .iter()
            .find(|s| s.id == session_id)
            .and_then(|s| {
                s.windows
                    .iter()
                    .find(|w| w.panes.iter().any(|p| p.pane_id.as_deref() == Some(pane)))
            })
            .map(|w| (session_id.to_owned(), w.id.clone()))
    };
    match command {
        MuxCommand::SwapPanes {
            source_pane_id: source,
            target_pane_id: target,
            ..
        } => {
            if let (Some(a), Some(b)) = (key_for(source), key_for(target)) {
                if a == b {
                    if let Some(layout) = layouts.get_mut(&a) {
                        layout.swap(source, target);
                    }
                } else {
                    if let Some(layout) = layouts.get_mut(&a) {
                        layout.replace(source, target.clone());
                    }
                    if let Some(layout) = layouts.get_mut(&b) {
                        layout.replace(target, source.clone());
                    }
                }
            }
        }
        MuxCommand::MovePane {
            pane_id,
            target_pane_id,
            direction,
            ..
        } => {
            if let (Some(a), Some(b)) = (key_for(pane_id), key_for(target_pane_id)) {
                let direction = match direction {
                    MuxDirection::Left => Direction::Left,
                    MuxDirection::Right => Direction::Right,
                    MuxDirection::Up => Direction::Up,
                    MuxDirection::Down => Direction::Down,
                };
                if a == b {
                    if let Some(layout) = layouts.get_mut(&a) {
                        anyhow::ensure!(
                            layout.move_beside(pane_id, target_pane_id, direction),
                            "pane move exceeds representable split weights"
                        );
                    }
                } else {
                    if let Some(layout) = layouts.get_mut(&a) {
                        layout.remove(pane_id);
                    }
                    if let Some(layout) = layouts.get_mut(&b) {
                        anyhow::ensure!(
                            layout.insert_beside(pane_id.clone(), target_pane_id, direction),
                            "pane insertion exceeds representable split weights"
                        );
                    }
                }
            }
        }
        MuxCommand::MergeWindows {
            source_window_id,
            target_window_id,
            ..
        } => {
            let source = layouts.remove(&(session_id.to_owned(), source_window_id.clone()));
            if let (Some(source), Some(target)) = (
                source,
                layouts.get_mut(&(session_id.to_owned(), target_window_id.clone())),
            ) {
                anyhow::ensure!(
                    target.merge(source),
                    "pane merge exceeds representable split weights"
                );
            }
        }
        _ => {}
    }
    Ok(())
}
