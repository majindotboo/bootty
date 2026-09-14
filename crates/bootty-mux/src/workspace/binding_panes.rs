use crate::{
    RepaintHandle,
    command::{MuxCommand, MuxDirection, MuxSplitDirection},
    provider::{PaneTopology, selected_backend},
    snapshot::{MuxPaneAnchor, MuxPaneLayout},
    terminal::TerminalRuntime,
};
use anyhow::{Context as _, Result};
use bootty_terminal::geometry::{SurfaceRect, TerminalGeometry};

use super::{BindingRuntime, ScopedWindowId};
use crate::pane_layout::{Direction, Divider, PaneLayout, SplitDirection};

#[must_use]
pub const fn mux_split_direction(direction: SplitDirection) -> MuxSplitDirection {
    match direction {
        SplitDirection::Right => MuxSplitDirection::Right,
        SplitDirection::Down => MuxSplitDirection::Down,
    }
}

const fn layout_direction(direction: MuxDirection) -> Direction {
    match direction {
        MuxDirection::Left => Direction::Left,
        MuxDirection::Right => Direction::Right,
        MuxDirection::Up => Direction::Up,
        MuxDirection::Down => Direction::Down,
    }
}

fn pane_sets_match(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|pane| b.contains(pane))
}

fn focus_after_reconcile(
    restored_from_server: bool,
    new_panes: &[String],
    selected_pane: Option<&str>,
) -> Option<String> {
    if restored_from_server
        || selected_pane.is_some_and(|selected| new_panes.iter().any(|pane| pane == selected))
    {
        return selected_pane.map(str::to_owned);
    }
    new_panes.first().cloned()
}

impl BindingRuntime {
    pub fn uses_native_terminal_layout(&self) -> bool {
        self.backend_policy.panes.topology != PaneTopology::Attach
    }

    pub fn pane_widget_key(&self, pane_id: &str) -> String {
        let window = self
            .window_id_for_pane(pane_id)
            .unwrap_or_else(|| self.current_window_id());
        let backend = selected_backend(&self.multiplexer);
        format!(
            "{}:{backend:?}:{}:{}:{pane_id}",
            window.scope.persistence_value(),
            window.session_id,
            window.window_id,
        )
    }

    fn take_pending_split_direction(&mut self, key: &ScopedWindowId) -> Option<SplitDirection> {
        self.pending_pane_split_directions.remove(key).or_else(|| {
            if key.window_id.is_empty() {
                None
            } else {
                let fallback = self.window_id(key.session_id.clone(), String::new());
                self.pending_pane_split_directions.remove(&fallback)
            }
        })
    }

    fn prune_pane_layouts(&mut self) {
        if self.pane_layouts.is_empty() {
            return;
        }
        let mut live = Vec::new();
        for session in self.mux.sessions() {
            for window in &session.windows {
                live.push(self.window_id(session.id.clone(), window.id.clone()));
                live.push(self.window_id(session.name.clone(), window.id.clone()));
            }
        }
        live.push(self.current_window_id());
        self.pane_layouts.retain(|key, _| live.contains(key));
    }

    /// # Errors
    /// Returns terminal startup, attachment, or layout synchronization errors.
    pub fn sync_terminal_panes(&mut self) -> Result<()> {
        if self.mux.unavailable_reason().is_some() {
            return Ok(());
        }
        let phase = bootty_terminal::latency::start();
        self.prune_pane_layouts();
        bootty_terminal::latency::trace_slow("panes.prune_pane_layouts", phase, 2.0);
        let phase = bootty_terminal::latency::start();
        let config = self.multiplexer.clone();
        bootty_terminal::latency::trace_slow("panes.clone_config", phase, 2.0);
        if !self.uses_native_terminal_layout() {
            let phase = bootty_terminal::latency::start();
            let result = self.terminal.sync_scoped_mux_anchor(
                self.scope,
                &config,
                self.mux.selected_session_anchor(),
            );
            bootty_terminal::latency::trace_slow("panes.sync_scoped_mux_anchor", phase, 2.0);
            return result;
        }
        let panes: Vec<MuxPaneAnchor> = self.mux.selected_window_panes().to_vec();
        let pane_ids: Vec<String> = panes
            .iter()
            .filter_map(|pane| pane.pane_id.clone())
            .collect();
        if pane_ids.is_empty() {
            return self.terminal.sync_scoped_mux_anchor(
                self.scope,
                &config,
                self.mux.selected_session_anchor(),
            );
        }
        let key = self.current_window_id();
        let window_id = (!key.window_id.is_empty()).then(|| key.window_id.clone());
        let selected_pane = self
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        let server_layout = self.mux.selected_window_layout().cloned();
        let layout = self.reconcile_window_layout(
            &key,
            &pane_ids,
            server_layout.as_ref(),
            selected_pane.as_deref(),
        )?;
        let focused_id = layout.focused().to_owned();
        let focused_anchor = panes
            .iter()
            .find(|pane| pane.pane_id.as_deref() == Some(focused_id.as_str()))
            .cloned();
        self.terminal.sync_scoped_native_window(
            self.scope,
            &panes,
            focused_anchor.as_ref(),
            window_id.as_deref(),
            selected_backend(&config),
            config.hide_tmux_status,
        )
    }

    fn reconcile_window_layout(
        &mut self,
        key: &ScopedWindowId,
        pane_ids: &[String],
        server_layout: Option<&MuxPaneLayout>,
        selected_pane: Option<&str>,
    ) -> Result<&PaneLayout> {
        let first_pane = pane_ids.first().context("terminal window has no panes")?;
        let mut server_layout = server_layout
            .and_then(PaneLayout::from_mux_layout)
            .filter(|layout| pane_sets_match(&layout.panes(), pane_ids));
        let layout_missing_or_stale = self
            .pane_layouts
            .get(key)
            .is_none_or(|layout| layout.panes().iter().all(|pane| !pane_ids.contains(pane)));
        let mut restored_from_server =
            if layout_missing_or_stale && let Some(layout) = server_layout.take() {
                self.pane_layouts.insert(key.clone(), layout);
                true
            } else {
                false
            };

        let previous_panes = self
            .pane_layouts
            .get(key)
            .map(PaneLayout::panes)
            .unwrap_or_default();
        let new_panes = pane_ids
            .iter()
            .filter(|pane| !previous_panes.contains(pane))
            .cloned()
            .collect::<Vec<_>>();
        let pane_set_changed = !pane_sets_match(&previous_panes, pane_ids);
        let direction = if pane_set_changed && server_layout.is_none() {
            self.take_pending_split_direction(key)
                .unwrap_or(SplitDirection::Right)
        } else {
            SplitDirection::Right
        };
        let layout = self
            .pane_layouts
            .entry(key.clone())
            .or_insert_with(|| PaneLayout::single(first_pane.clone()));
        if layout.panes().iter().all(|pane| !pane_ids.contains(pane)) {
            *layout = PaneLayout::single(first_pane.clone());
        }
        if pane_set_changed && let Some(server_layout) = server_layout {
            *layout = server_layout;
            restored_from_server = true;
        } else if pane_set_changed {
            layout.reconcile_with_new_pane_direction(pane_ids, direction);
        }
        if let Some(focus) = focus_after_reconcile(restored_from_server, &new_panes, selected_pane)
        {
            layout.set_focus(&focus);
        }
        Ok(layout)
    }

    /// Prepare a visible backend window without moving the binding's input focus.
    /// # Errors
    /// Returns an error for opaque attachments, missing windows or panes, or terminal startup failure.
    pub fn prepare_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        geometry: TerminalGeometry,
    ) -> Result<()> {
        anyhow::ensure!(
            self.uses_native_terminal_layout(),
            "opaque attachments have no inner windows"
        );
        let window = self
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.windows.iter().find(|window| window.id == window_id))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("terminal window no longer exists"))?;
        let pane_ids = window
            .panes
            .iter()
            .filter_map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        anyhow::ensure!(!pane_ids.is_empty(), "terminal window has no panes");
        let key = self.window_id(session_id.to_owned(), window_id.to_owned());
        self.reconcile_window_layout(
            &key,
            &pane_ids,
            window.layout.as_ref(),
            window.anchor.pane_id.as_deref(),
        )?;
        self.terminal
            .prepare_scoped_native_panes(self.scope, &window.panes, geometry)
    }

    pub fn window_pane_layout(&self, session_id: &str, window_id: &str) -> Option<&PaneLayout> {
        self.pane_layouts
            .get(&self.window_id(session_id.to_owned(), window_id.to_owned()))
    }

    pub fn visible_terminal_runtime(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        self.terminal.scoped_terminal_runtime(self.scope, pane_id)
    }

    /// The focused live pane of this window, including a retained selection in an inactive tab.
    pub fn window_focused_pane(&self, session_id: &str, window_id: &str) -> Option<&str> {
        let session = self.mux.session_by_id_or_name(session_id)?;
        let window = session
            .windows
            .iter()
            .find(|window| window.id == window_id)?;
        self.window_pane_layout(&session.id, window_id)
            .map(PaneLayout::focused)
            .filter(|focused| {
                window
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id.as_deref() == Some(focused))
            })
            .or(window.anchor.pane_id.as_deref())
            .or_else(|| window.panes.first()?.pane_id.as_deref())
    }

    pub fn native_multi_pane(&self) -> bool {
        self.current_pane_layout()
            .is_some_and(|layout| !layout.is_single())
    }

    pub fn focused_pane(&self) -> Option<String> {
        self.current_pane_layout()
            .map(|layout| layout.focused().to_owned())
    }

    pub fn pane_rects(&self, area: SurfaceRect, gap: f32) -> Vec<(String, SurfaceRect)> {
        self.current_pane_layout()
            .map(|layout| layout.rects(area, gap))
            .unwrap_or_default()
    }

    pub fn pane_dividers(&self, area: SurfaceRect, gap: f32) -> Vec<Divider> {
        self.current_pane_layout()
            .map(|layout| layout.dividers(area, gap))
            .unwrap_or_default()
    }

    pub fn focus_pane(&mut self, pane_id: &str) {
        let key = self.current_window_id();
        let moved = match self.pane_layouts.get_mut(&key) {
            Some(layout) if layout.focused() != pane_id => layout.set_focus(pane_id),
            _ => false,
        };
        if moved {
            let _ = self.sync_terminal_panes();
        }
    }

    pub fn set_pane_ratio(&mut self, path: &[u8], ratio: f32, min_fraction: f32) {
        let key = self.current_window_id();
        self.set_window_pane_ratio(&key, path, ratio, min_fraction);
    }

    pub fn set_window_pane_ratio(
        &mut self,
        key: &ScopedWindowId,
        path: &[u8],
        ratio: f32,
        min_fraction: f32,
    ) {
        if let Some(layout) = self.pane_layouts.get_mut(key) {
            layout.set_ratio_at(path, ratio, min_fraction, min_fraction);
        }
    }

    pub fn terminal_runtime_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        if self.terminal.focused_pane_id() == Some(pane_id) {
            return None;
        }
        self.terminal.scoped_terminal_runtime(self.scope, pane_id)
    }

    pub fn pane_terminal_window_size<F>(&self, leaf_size: F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        self.current_pane_layout()?.terminal_window_size(leaf_size)
    }

    /// # Errors
    /// Returns backend window resize or terminal transport errors.
    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.terminal.resize_native_layout_window(cols, rows)
    }

    pub fn split_focused_pane(
        &mut self,
        repaint: &RepaintHandle,
        direction: SplitDirection,
        target_pane_id: Option<&str>,
    ) {
        let session = self.mux.selected_session().unwrap_or("local").to_owned();
        let config = self.multiplexer.clone();
        let layout_update = self.uses_native_terminal_layout().then(|| {
            let key = self.current_window_id();
            let focused = target_pane_id.map(str::to_owned).or_else(|| {
                self.pane_layouts
                    .get(&key)
                    .map(|layout| layout.focused().to_owned())
                    .or_else(|| {
                        self.mux
                            .selected_session_anchor()
                            .and_then(|anchor| anchor.pane_id.clone())
                    })
            });
            (key, focused)
        });
        let pane_id = layout_update.as_ref().map_or_else(
            || target_pane_id.map(str::to_owned),
            |(_, focused)| focused.clone(),
        );
        self.mux.execute_command(
            repaint,
            &config,
            MuxCommand::SplitPane {
                session_id: session,
                pane_id,
                direction: mux_split_direction(direction),
            },
        );
        if let Some((key, focused)) = layout_update {
            self.apply_split_layout_after_command(key, focused.as_deref(), direction);
        }
    }

    fn apply_split_layout_after_command(
        &mut self,
        key: ScopedWindowId,
        focused: Option<&str>,
        direction: SplitDirection,
    ) {
        match self.backend_policy.panes.topology {
            PaneTopology::BackendReconciled => {
                self.pending_pane_split_directions.insert(key, direction);
                return;
            }
            PaneTopology::ProcessLocal => {}
            PaneTopology::Attach => return,
        }
        let Some(new_pane) = self
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone())
        else {
            return;
        };
        let layout = self
            .pane_layouts
            .entry(key.clone())
            .or_insert_with(|| PaneLayout::single(new_pane.clone()));
        if let Some(focused) = focused {
            layout.set_focus(focused);
        }
        if !layout.contains(&new_pane) {
            layout.split_focused(new_pane, direction);
        }
        self.pending_pane_split_directions.remove(&key);
        let _ = self.sync_terminal_panes();
    }

    pub fn focus_pane_neighbor(&mut self, direction: MuxDirection, area: SurfaceRect, gap: f32) {
        let key = self.current_window_id();
        let neighbor = self.pane_layouts.get(&key).and_then(|layout| {
            layout.neighbor(layout.focused(), layout_direction(direction), area, gap)
        });
        if let Some(neighbor) = neighbor {
            self.focus_pane(&neighbor);
        }
    }

    pub fn focus_pane_relative(&mut self, delta: isize) {
        let key = self.current_window_id();
        let Some(layout) = self.pane_layouts.get(&key) else {
            return;
        };
        let panes = layout.panes();
        if panes.len() < 2 {
            return;
        }
        let Some(index) = panes.iter().position(|pane| pane == layout.focused()) else {
            return;
        };
        if let Some(pane) = crate::snapshot::wrap_index(index, delta, panes.len())
            .and_then(|index| panes.get(index))
        {
            self.focus_pane(pane);
        }
    }

    pub fn remove_pane_from_layout(
        &mut self,
        window: &ScopedWindowId,
        pane_id: &str,
        sync_current_window: bool,
    ) {
        if let Some(layout) = self.pane_layouts.get_mut(window) {
            layout.remove(pane_id);
        }
        if sync_current_window {
            let _ = self.sync_terminal_panes();
        }
    }

    pub fn close_focused_pane(&mut self, repaint: &RepaintHandle, pane_id: &str) {
        let session_id = self.mux.selected_session().unwrap_or("local").to_owned();
        let config = self.multiplexer.clone();
        self.mux.execute_command(
            repaint,
            &config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: Some(pane_id.to_owned()),
            },
        );
        self.terminal.discard_pane(pane_id);
        let window = self.current_window_id();
        self.remove_pane_from_layout(&window, pane_id, true);
    }
}
