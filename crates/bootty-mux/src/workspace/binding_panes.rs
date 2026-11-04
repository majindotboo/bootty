use crate::{
    RepaintHandle,
    command::{MuxCommand, MuxSplitDirection},
    snapshot::{MuxPaneAnchor, MuxPaneLayout},
    terminal::TerminalRuntime,
};
use anyhow::{Context as _, Result};
use bootty_terminal::geometry::{SurfaceRect, TerminalGeometry};

use super::{BindingRuntime, ScopedWindowId};
use crate::pane_layout::{PaneLayout, SplitDirection};

#[must_use]
pub const fn mux_split_direction(direction: SplitDirection) -> MuxSplitDirection {
    match direction {
        SplitDirection::Right => MuxSplitDirection::Right,
        SplitDirection::Down => MuxSplitDirection::Down,
    }
}

pub(super) struct PendingPaneFocus {
    pane: String,
    window: ScopedWindowId,
    result: std::sync::mpsc::Receiver<anyhow::Result<crate::native::protocol::Response>>,
}

impl BindingRuntime {
    pub fn pane_widget_key(&self, pane_id: &str) -> String {
        format!("{}:{pane_id}", self.scope.persistence_value())
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
        self.poll_pane_focus();
        if self.mux.unavailable_reason().is_some() {
            return Ok(());
        }
        self.retain_restored_generation();
        let phase = bootty_terminal::latency::start();
        self.prune_pane_layouts();
        bootty_terminal::latency::trace_slow("panes.prune_pane_layouts", phase, 2.0);
        let phase = bootty_terminal::latency::start();
        let config = self.multiplexer.clone();
        bootty_terminal::latency::trace_slow("panes.clone_config", phase, 2.0);
        let panes: Vec<MuxPaneAnchor> = self.mux.selected_window_panes().to_vec();
        let pane_ids: Vec<String> = panes
            .iter()
            .filter_map(|pane| pane.pane_id.clone())
            .collect();
        if pane_ids.is_empty() {
            return self.terminal_owner.terminal.sync_scoped_mux_anchor(
                self.scope,
                &config,
                self.mux.selected_session_anchor(),
            );
        }
        let key = self.current_window_id();
        let window_id = (!key.window_id.is_empty()).then(|| key.window_id.clone());
        let selected_pane = if let Some(pending) = &self.pending_pane_focus
            && pending.window == key
            && pane_ids.contains(&pending.pane)
        {
            Some(pending.pane.clone())
        } else {
            self.mux
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.clone())
        };
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
        self.terminal_owner.terminal.sync_scoped_native_window(
            self.scope,
            &panes,
            focused_anchor.as_ref(),
            window_id.as_deref(),
        );
        for mapping in self.restored_sessions.values_mut() {
            mapping.history.clear();
        }
        Ok(())
    }

    fn reconcile_window_layout(
        &mut self,
        key: &ScopedWindowId,
        pane_ids: &[String],
        server_layout: Option<&MuxPaneLayout>,
        selected_pane: Option<&str>,
    ) -> Result<&PaneLayout> {
        let mut layout = server_layout
            .and_then(PaneLayout::from_mux_layout)
            .context("native mux server returned no pane layout")?;
        let leaves = layout.panes();
        anyhow::ensure!(
            leaves.len() == pane_ids.len() && leaves.iter().all(|p| pane_ids.contains(p)),
            "native mux layout does not match pane topology"
        );
        if let Some(focus) = selected_pane {
            layout.set_focus(focus);
        }
        self.pane_layouts.insert(key.clone(), layout);
        self.pane_layouts
            .get(key)
            .context("native pane projection missing")
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
        self.terminal_owner.terminal.prepare_scoped_native_panes(
            self.scope,
            &window.panes,
            geometry,
        );
        Ok(())
    }

    pub fn window_pane_layout(&self, session_id: &str, window_id: &str) -> Option<&PaneLayout> {
        self.pane_layouts
            .get(&self.window_id(session_id.to_owned(), window_id.to_owned()))
    }

    /// The nonblocking frame source for a presented pane, or the opaque attached terminal.
    /// Presentation cannot use this interface to perform terminal input or backend operations.
    pub fn visible_terminal_frame_source(
        &mut self,
        pane_id: Option<&str>,
    ) -> Option<&mut (dyn bootty_terminal::frame_source::TerminalFrameSource + '_)> {
        match pane_id {
            Some(pane_id) => Some(
                self.terminal_owner
                    .terminal
                    .scoped_terminal_runtime(self.scope, pane_id)?,
            ),
            None => Some(self.terminal_mut()),
        }
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
            .map_or_else(Default::default, |layout| layout.rects(area, gap))
    }

    pub fn focus_pane(&mut self, pane_id: &str) {
        let window = self.current_window_id();
        if !self
            .pane_layouts
            .get(&window)
            .is_some_and(|layout| layout.contains(pane_id))
        {
            return;
        }
        let result = self.mux.native_backend().submit_request_for_generation(
            crate::native::protocol::Request::Execute(MuxCommand::ActivatePane {
                session_id: window.session_id.clone(),
                window_id: window.window_id.clone(),
                pane_id: pane_id.to_owned(),
            }),
            self.mux.server_generation(),
        );
        self.pending_pane_focus = Some(PendingPaneFocus {
            pane: pane_id.to_owned(),
            window,
            result,
        });
        let _ = self.sync_terminal_panes();
    }

    fn poll_pane_focus(&mut self) {
        let Some(pending) = &self.pending_pane_focus else {
            return;
        };
        match pending.result.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Ok(Ok(crate::native::protocol::Response::Snapshot(snapshot))) => {
                self.mux.accept_server_snapshot(snapshot);
            }
            Ok(Err(error)) => self
                .mux
                .set_error(Some(format!("Could not focus pane: {error:#}"))),
            Ok(Ok(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => self
                .mux
                .set_error(Some("Native pane focus was not acknowledged".into())),
        }
        self.pending_pane_focus = None;
    }

    pub fn terminal_runtime_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        if self.terminal_owner.terminal.focused_pane_id() == Some(pane_id) {
            return None;
        }
        self.terminal_owner
            .terminal
            .scoped_terminal_runtime(self.scope, pane_id)
    }

    pub fn split_focused_pane(
        &mut self,
        repaint: &RepaintHandle,
        direction: SplitDirection,
        target_pane_id: Option<&str>,
    ) {
        let session = self.mux.selected_session().unwrap_or("local").to_owned();
        self.mux.execute_command(
            repaint,
            &self.multiplexer,
            MuxCommand::SplitPane {
                session_id: session,
                pane_id: target_pane_id
                    .map(str::to_owned)
                    .or_else(|| self.focused_pane()),
                direction: mux_split_direction(direction),
            },
        );
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
        let window = self.window_id_for_pane(pane_id);
        if let Some(window) = &window {
            let config = self.multiplexer.clone();
            self.mux.execute_command(
                repaint,
                &config,
                MuxCommand::ClosePane {
                    session_id: window.session_id.clone(),
                    pane_id: Some(pane_id.to_owned()),
                },
            );
        }
        self.terminal_owner.terminal.discard_pane(pane_id);
        if let Some(window) = window {
            self.remove_pane_from_layout(&window, pane_id, true);
        }
    }
}
