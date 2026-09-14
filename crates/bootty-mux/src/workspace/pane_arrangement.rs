//! Capture native placement before dispatch; publish it only after authoritative completion.
use super::{BindingRuntime, ScopedWindowId};
use crate::{
    command::{MuxCommand, MuxDirection},
    pane_layout::{Direction, PaneLayout, SplitDirection},
    provider::PaneTopology,
};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct PreparedPaneArrangement {
    layouts: HashMap<ScopedWindowId, PaneLayout>,
    extracted: Option<(String, String)>,
}

impl BindingRuntime {
    pub(crate) fn prepare_pane_arrangement(
        &self,
        command: &MuxCommand,
    ) -> Option<PreparedPaneArrangement> {
        if self.backend_policy.panes.topology != PaneTopology::ProcessLocal {
            return None;
        }
        if let MuxCommand::MergeWindows {
            session_id,
            source_window_id,
            target_window_id,
        } = command
        {
            let session = self
                .mux
                .all_sessions()
                .iter()
                .find(|session| session.id == *session_id)?;
            let first_pane = |id: &str| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == id)?
                    .panes
                    .first()?
                    .pane_id
                    .as_ref()
            };
            let (source_key, source) =
                self.layout_for_pane(session_id, first_pane(source_window_id)?)?;
            let (target_key, mut target) =
                self.layout_for_pane(session_id, first_pane(target_window_id)?)?;
            if !target.merge(source.clone()) {
                return None;
            }
            return Some(PreparedPaneArrangement {
                layouts: HashMap::from([(source_key, source), (target_key, target)]),
                extracted: None,
            });
        }
        let (session, source, target) = match command {
            MuxCommand::SwapPanes {
                session_id,
                source_pane_id,
                target_pane_id,
            } => (session_id, source_pane_id, Some(target_pane_id)),
            MuxCommand::MovePane {
                session_id,
                pane_id,
                target_pane_id,
                ..
            } => (session_id, pane_id, Some(target_pane_id)),
            MuxCommand::ExtractPane {
                session_id,
                pane_id,
            } => (session_id, pane_id, None),
            _ => return None,
        };
        let (source_key, mut source_layout) = self.layout_for_pane(session, source)?;
        let mut layouts = HashMap::new();
        let mut extracted = None;
        if let Some(target) = target {
            let (target_key, mut target_layout) = self.layout_for_pane(session, target)?;
            match command {
                MuxCommand::SwapPanes { .. } => {
                    if source_key == target_key {
                        source_layout.swap(source, target);
                        source_layout.set_focus(source);
                    } else {
                        source_layout.replace(source, target.clone());
                        target_layout.replace(target, source.clone());
                        target_layout.set_focus(source);
                        layouts.insert(target_key, target_layout);
                    }
                }
                MuxCommand::MovePane { direction, .. } => {
                    let direction = match direction {
                        MuxDirection::Left => Direction::Left,
                        MuxDirection::Right => Direction::Right,
                        MuxDirection::Up => Direction::Up,
                        MuxDirection::Down => Direction::Down,
                    };
                    if source_key == target_key {
                        source_layout.move_beside(source, target, direction);
                    } else {
                        source_layout.remove(source);
                        target_layout.insert_beside(source.clone(), target, direction);
                        layouts.insert(target_key, target_layout);
                    }
                }
                _ => return None,
            }
        } else {
            source_layout.remove(source);
            extracted = Some((session.clone(), source.clone()));
        }
        layouts.insert(source_key, source_layout);
        Some(PreparedPaneArrangement { layouts, extracted })
    }

    fn layout_for_pane(
        &self,
        session_id: &str,
        pane_id: &str,
    ) -> Option<(ScopedWindowId, PaneLayout)> {
        let session = self
            .mux
            .all_sessions()
            .iter()
            .find(|session| session.id == session_id)?;
        let window = session.windows.iter().find(|window| {
            window
                .panes
                .iter()
                .any(|pane| pane.pane_id.as_deref() == Some(pane_id))
        })?;
        let key = self.window_id(session.id.clone(), window.id.clone());
        let layout = self.pane_layouts.get(&key).cloned().or_else(|| {
            let ids = window
                .panes
                .iter()
                .filter_map(|pane| pane.pane_id.as_ref())
                .collect::<Vec<_>>();
            let mut layout = PaneLayout::single((*ids.first()?).clone());
            for pane in ids.into_iter().skip(1) {
                layout.split_focused(pane.clone(), SplitDirection::Right);
            }
            Some(layout)
        })?;
        Some((key, layout))
    }

    pub(crate) fn apply_pane_arrangement(&mut self, prepared: &PreparedPaneArrangement) {
        for (key, layout) in &prepared.layouts {
            let live = self
                .mux
                .all_sessions()
                .iter()
                .find(|session| session.id == key.session_id)
                .and_then(|session| {
                    session
                        .windows
                        .iter()
                        .find(|window| window.id == key.window_id)
                });
            let Some(live) = live else {
                self.pane_layouts.remove(key);
                continue;
            };
            let ids = live
                .panes
                .iter()
                .filter_map(|pane| pane.pane_id.as_ref())
                .collect::<Vec<_>>();
            if ids.len() == layout.panes().len() && ids.iter().all(|pane| layout.contains(pane)) {
                self.pane_layouts.insert(key.clone(), layout.clone());
            }
        }
        if let Some((session, pane)) = &prepared.extracted
            && let Some((key, _)) = self.layout_for_pane(session, pane)
        {
            self.pane_layouts
                .insert(key, PaneLayout::single(pane.clone()));
        }
    }
}
