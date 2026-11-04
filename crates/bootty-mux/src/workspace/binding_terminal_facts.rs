use std::collections::HashMap;

use crate::pane_layout::PaneLayout;
use crate::{
    RepaintHandle,
    snapshot::{MuxSession, MuxWindow},
};

use super::{BindingRuntime, ScopedPaneId, ScopedWindowId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalProgressState {
    Normal,
    Error,
    Indeterminate,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalProgress {
    pub state: TerminalProgressState,
    pub value: Option<u8>,
}

impl TerminalProgress {
    fn from_conemu(state: &str, value: Option<u8>) -> Option<Self> {
        let state = match state {
            "normal" => TerminalProgressState::Normal,
            "error" => TerminalProgressState::Error,
            "indeterminate" => TerminalProgressState::Indeterminate,
            "warning" => TerminalProgressState::Warning,
            _ => return None,
        };
        Some(Self { state, value })
    }

    fn percent(self) -> Option<u8> {
        self.value
            .or_else(|| (self.state == TerminalProgressState::Indeterminate).then_some(50))
    }
}

#[derive(Default)]
pub(super) struct BindingTerminalFacts {
    pane_progress: HashMap<ScopedPaneId, TerminalProgress>,
    unscoped_progress: Option<TerminalProgress>,
    pane_ports: HashMap<ScopedPaneId, Vec<u16>>,
    unscoped_ports: Vec<u16>,
}

impl BindingRuntime {
    pub fn current_window_id(&self) -> ScopedWindowId {
        let session = self
            .mux
            .selected_session_id()
            .unwrap_or_else(|| "local".to_owned());
        let window = self
            .mux
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.mux
                    .session_by_id_or_name(&session)
                    .and_then(|session| session.active_window_id.clone())
            })
            .unwrap_or_default();
        self.window_id(session, window)
    }

    /// The window that currently holds `pane_id`. Window titles are per window, so this searches
    /// the topology; it runs on a title event, never per frame.
    pub fn window_id_for_pane(&self, pane_id: &str) -> Option<ScopedWindowId> {
        self.mux.sessions().iter().find_map(|session| {
            session
                .windows
                .iter()
                .find(|window| Self::window_pane_ids(window).any(|id| id == pane_id))
                .map(|window| self.window_id(session.id.clone(), window.id.clone()))
        })
    }

    pub fn set_custom_window_name(
        &mut self,
        session_id: &str,
        window_id: &str,
        name: &str,
        repaint: &RepaintHandle,
    ) {
        self.mux.rename_window(
            session_id,
            window_id,
            name.to_owned(),
            repaint,
            &self.multiplexer,
        );
    }

    pub fn record_terminal_progress(
        &mut self,
        source_pane_id: Option<&str>,
        state: &str,
        value: Option<u8>,
    ) {
        if state == "unknown" {
            return;
        }
        let progress = TerminalProgress::from_conemu(state, value);
        match source_pane_id.map(|pane_id| self.pane_id(pane_id)) {
            Some(pane) => match progress {
                Some(progress) => {
                    self.terminal_facts.pane_progress.insert(pane, progress);
                }
                None => {
                    self.terminal_facts.pane_progress.remove(&pane);
                }
            },
            None => self.terminal_facts.unscoped_progress = progress,
        }
    }

    pub fn record_terminal_ports(&mut self, source_pane_id: Option<&str>, ports: Vec<u16>) {
        match source_pane_id.map(|pane_id| self.pane_id(pane_id)) {
            Some(pane) => {
                self.terminal_facts.pane_ports.insert(pane, ports);
            }
            None => self.terminal_facts.unscoped_ports = ports,
        }
    }

    pub fn pane_progress(&self, pane_id: &str) -> Option<TerminalProgress> {
        self.terminal_facts
            .pane_progress
            .get(&self.pane_id(pane_id))
            .copied()
    }

    fn pane_ports(&self, pane_id: &str) -> Option<&[u16]> {
        self.terminal_facts
            .pane_ports
            .get(&self.pane_id(pane_id))
            .map(Vec::as_slice)
    }

    pub fn session_ports(&self, session: &MuxSession) -> Vec<u16> {
        let selected = self.mux.selected_session();
        let mut ports =
            if selected == Some(session.id.as_str()) || selected == Some(session.name.as_str()) {
                self.terminal_facts.unscoped_ports.clone()
            } else {
                Vec::new()
            };
        for pane in session.windows.iter().flat_map(Self::window_pane_ids) {
            if let Some(reported) = self.pane_ports(pane) {
                for port in reported {
                    if !ports.contains(port) {
                        ports.push(*port);
                    }
                }
            }
        }
        ports
    }

    pub fn window_has_indeterminate_progress(&self, window: &MuxWindow) -> bool {
        self.window_terminal_progresses(window)
            .any(|progress| progress.state == TerminalProgressState::Indeterminate)
    }

    pub fn window_progress(&self, window: &MuxWindow) -> Option<u8> {
        self.window_terminal_progresses(window)
            .filter_map(TerminalProgress::percent)
            .max()
    }

    fn window_terminal_progresses<'a>(
        &'a self,
        window: &'a MuxWindow,
    ) -> impl Iterator<Item = TerminalProgress> + 'a {
        Self::window_pane_ids(window).filter_map(|id| self.pane_progress(id))
    }

    fn window_pane_ids(window: &MuxWindow) -> impl Iterator<Item = &str> {
        window
            .panes
            .iter()
            .chain(std::iter::once(&window.anchor))
            .filter_map(|pane| pane.pane_id.as_deref())
    }

    pub fn current_pane_layout(&self) -> Option<&PaneLayout> {
        self.pane_layouts.get(&self.current_window_id())
    }
}
