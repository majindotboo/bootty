use crate::snapshot::{clamp_move_index, wrap_index};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context as _, Result, bail};

use crate::{
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation},
    command::MuxCommand,
    controller::SpaceId,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxSnapshot, MuxWindow},
    terminal::{
        BackendPanePolicy, MuxPaneTarget, PaneLayoutResizeRequest, PaneStartRequest,
        ScopedMuxPaneTarget, StartingNativeTerminal, TerminalRuntime,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct NativePane {
    id: String,
    cwd: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeWindow {
    id: String,
    index: u32,
    name: String,
    active_pane_id: String,
    panes: Vec<NativePane>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeSession {
    next_window: u64,
    id: String,
    name: String,
    active_window_id: String,
    windows: Vec<NativeWindow>,
    /// Native sessions live and die with the process, so this is only ever the tag the workspace
    /// handed over when it asked for the session — including when it is recreating one it
    /// persisted. Nothing outside Bootty can write here, and nothing survives a restart.
    tag: MuxSessionTag,
}

#[derive(Debug)]
struct NativeMuxState {
    active_session_id: String,
    sessions: Vec<NativeSession>,
    next_pane: u64,
}

impl NativeMuxState {
    const fn new() -> Self {
        Self {
            active_session_id: String::new(),
            sessions: Vec::new(),
            next_pane: 1,
        }
    }

    fn ensure_session(
        &mut self,
        session_id: &str,
        cwd: impl Into<PathBuf>,
        tag: MuxSessionTag,
    ) -> Result<()> {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if !tag.is_empty() {
                session.tag = tag;
            }
            session_id.clone_into(&mut self.active_session_id);
            return Ok(());
        }

        let pane_id = self.next_pane_id()?;
        let cwd = cwd.into();
        let window = NativeWindow {
            id: "tab-1".to_owned(),
            index: 1,
            name: default_window_name(),
            active_pane_id: pane_id.clone(),
            panes: vec![NativePane { id: pane_id, cwd }],
        };
        self.sessions.push(NativeSession {
            next_window: 2,
            id: session_id.to_owned(),
            name: session_id.to_owned(),
            active_window_id: window.id.clone(),
            windows: vec![window],
            tag,
        });
        session_id.clone_into(&mut self.active_session_id);
        Ok(())
    }

    fn stamp_session(&mut self, session_id: &str, tag: MuxSessionTag) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.tag = tag;
        }
    }

    fn activate_window(&mut self, session_id: &str, window_id: &str) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            && session.windows.iter().any(|window| window.id == window_id)
        {
            window_id.clone_into(&mut session.active_window_id);
            session_id.clone_into(&mut self.active_session_id);
        }
    }
    fn rename_window(&mut self, session_id: &str, window_id: &str, name: String) {
        if let Some(window) = self.window_mut(session_id, window_id) {
            window.name = name;
        }
    }

    fn rename_session(&mut self, session_id: &str, name: String) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.name = name;
        }
    }

    fn kill_session(&mut self, session_id: &str) {
        self.sessions.retain(|session| session.id != session_id);
        if self.active_session_id == session_id {
            self.active_session_id = self
                .sessions
                .first()
                .map_or_else(String::new, |session| session.id.clone());
        }
    }

    fn active_session_mut(&mut self, session_id: &str) -> Option<&mut NativeSession> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == session_id)
    }

    fn window_mut(&mut self, session_id: &str, window_id: &str) -> Option<&mut NativeWindow> {
        self.active_session_mut(session_id)?
            .windows
            .iter_mut()
            .find(|window| window.id == window_id)
    }

    fn new_window(&mut self, session_id: &str, cwd: Option<PathBuf>) -> Result<()> {
        let pane_id = self.next_pane_id()?;
        if let Some(session) = self.active_session_mut(session_id) {
            let cwd = cwd.unwrap_or_else(|| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == session.active_window_id)
                    .and_then(|window| window.panes.first())
                    .map_or_else(|| PathBuf::from("."), |pane| pane.cwd.clone())
            });
            let index = u32::try_from(session.windows.len())
                .ok()
                .and_then(|count| count.checked_add(1))
                .context("native window capacity exhausted")?;
            let window = NativeWindow {
                id: next_window_id(session)?,
                index,
                name: default_window_name(),
                active_pane_id: pane_id.clone(),
                panes: vec![NativePane { id: pane_id, cwd }],
            };
            session.active_window_id.clone_from(&window.id);
            session.windows.push(window);
            session_id.clone_into(&mut self.active_session_id);
        }
        Ok(())
    }

    fn activate_relative_window(&mut self, session_id: &str, delta: i32) {
        if let Some(session) = self.active_session_mut(session_id)
            && let Some(index) = session
                .windows
                .iter()
                .position(|window| window.id == session.active_window_id)
        {
            let Some(next) = wrap_index(index, delta, session.windows.len())
                .and_then(|next| session.windows.get(next))
            else {
                return;
            };
            session.active_window_id.clone_from(&next.id);
            session_id.clone_into(&mut self.active_session_id);
        }
    }

    fn activate_window_index(&mut self, session_id: &str, index: u32) {
        if let Some(session) = self.active_session_mut(session_id)
            && let Some(window) = session.windows.iter().find(|window| window.index == index)
        {
            session.active_window_id.clone_from(&window.id);
            session_id.clone_into(&mut self.active_session_id);
        }
    }

    fn move_window(&mut self, session_id: &str, window_id: Option<&str>, delta: i32) {
        if let Some(session) = self.active_session_mut(session_id) {
            let target = window_id.unwrap_or(&session.active_window_id).to_owned();
            if let Some(index) = session
                .windows
                .iter()
                .position(|window| window.id == target)
            {
                let next = clamp_move_index(index, delta, session.windows.len());
                let window = session.windows.remove(index);
                session.windows.insert(next, window);
                session.active_window_id = target;
                for (index, window) in session.windows.iter_mut().enumerate() {
                    window.index = u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX);
                }
            }
        }
    }

    fn active_window_mut(&mut self, session_id: &str) -> Option<&mut NativeWindow> {
        let session = self.active_session_mut(session_id)?;
        let active_window_id = session.active_window_id.clone();
        self.window_mut(session_id, &active_window_id)
    }

    fn split_pane(&mut self, session_id: &str, source_pane_id: Option<&str>) -> Result<()> {
        let (window_id, cwd) = {
            let session = self
                .active_session_mut(session_id)
                .context("session no longer exists")?;
            let (window, pane) = target_pane_location(session, source_pane_id)?;
            let window = session
                .windows
                .get(window)
                .context("window no longer exists")?;
            (
                window.id.clone(),
                window
                    .panes
                    .get(pane)
                    .context("pane no longer exists")?
                    .cwd
                    .clone(),
            )
        };
        let pane_id = self.next_pane_id()?;
        let window = self
            .window_mut(session_id, &window_id)
            .context("window no longer exists")?;
        window.active_pane_id.clone_from(&pane_id);
        window.panes.push(NativePane { id: pane_id, cwd });
        self.activate_window(session_id, &window_id);
        Ok(())
    }

    fn select_relative_pane(&mut self, session_id: &str, delta: i32) {
        if let Some(window) = self.active_window_mut(session_id)
            && let Some(index) = window
                .panes
                .iter()
                .position(|pane| pane.id == window.active_pane_id)
        {
            let Some(next) = wrap_index(index, delta, window.panes.len())
                .and_then(|next| window.panes.get(next))
            else {
                return;
            };
            window.active_pane_id.clone_from(&next.id);
            session_id.clone_into(&mut self.active_session_id);
        }
    }

    fn select_pane(&mut self, session_id: &str, window_id: Option<&str>, delta: i32) {
        if let Some(window_id) = window_id {
            self.activate_window(session_id, window_id);
        }
        self.select_relative_pane(session_id, delta);
    }

    fn select_directional_pane(
        &mut self,
        session_id: &str,
        window_id: Option<&str>,
        direction: crate::command::MuxDirection,
    ) {
        let delta = match direction {
            crate::command::MuxDirection::Left | crate::command::MuxDirection::Up => -1,
            crate::command::MuxDirection::Right | crate::command::MuxDirection::Down => 1,
        };
        self.select_pane(session_id, window_id, delta);
    }

    fn move_window_preserving_selection(
        &mut self,
        session_id: &str,
        window_id: &str,
        delta: i32,
        selected_window_id: &str,
    ) {
        self.move_window(session_id, Some(window_id), delta);
        self.activate_window(session_id, selected_window_id);
    }

    // Close the requested pane; when it was the last pane in its window, cascade to remove that
    // window. The target can belong to an inactive tab, so never route through active_window_mut.
    fn close_pane(
        &mut self,
        session_id: &str,
        pane_id: Option<&str>,
        close_window: bool,
    ) -> Result<()> {
        let changed_active_session = {
            let session = self
                .active_session_mut(session_id)
                .context("session no longer exists")?;
            let (window_index, pane_index) = target_pane_location(session, pane_id)?;
            let window = session
                .windows
                .get_mut(window_index)
                .context("window no longer exists")?;
            if !close_window && window.panes.len() <= 1 {
                return Ok(());
            }
            let target_was_active = window.id == session.active_window_id;
            let removed_active_pane = window
                .panes
                .get(pane_index)
                .context("pane no longer exists")?
                .id
                == window.active_pane_id;
            window.panes.remove(pane_index);
            if window.panes.is_empty() {
                session.windows.remove(window_index);
                for (position, window) in session.windows.iter_mut().enumerate() {
                    window.index = u32::try_from(position.saturating_add(1)).unwrap_or(u32::MAX);
                }
                if target_was_active {
                    session.active_window_id = session
                        .windows
                        .get(window_index.min(session.windows.len().saturating_sub(1)))
                        .map_or_else(String::new, |window| window.id.clone());
                }
            } else if removed_active_pane
                && let Some(next) = window
                    .panes
                    .get(pane_index.min(window.panes.len().saturating_sub(1)))
            {
                window.active_pane_id.clone_from(&next.id);
            }
            target_was_active
        };
        if changed_active_session {
            session_id.clone_into(&mut self.active_session_id);
        }
        Ok(())
    }

    fn snapshot(&self) -> MuxSnapshot {
        MuxSnapshot {
            active_session_id: (!self.active_session_id.is_empty())
                .then(|| self.active_session_id.clone()),
            sessions: self
                .sessions
                .iter()
                .map(|session| self.snapshot_session(session))
                .collect(),
            ..MuxSnapshot::default()
        }
    }

    fn snapshot_session(&self, session: &NativeSession) -> MuxSession {
        let active = session.id == self.active_session_id;
        let windows = session
            .windows
            .iter()
            .map(|window| {
                let anchor = anchor_for_window(&session.id, window);
                let panes = window
                    .panes
                    .iter()
                    .map(|pane| anchor_for_pane(&session.id, pane))
                    .collect();
                MuxWindow {
                    id: window.id.clone(),
                    index: window.index,
                    name: window.name.clone(),
                    active: active && window.id == session.active_window_id,
                    anchor,
                    panes,
                    layout: None,
                    // Native panes each own a PTY, so their progress arrives as OSC 9;4.
                    progress: None,
                }
            })
            .collect::<Vec<_>>();
        let anchor = windows
            .iter()
            .find(|window| window.id == session.active_window_id)
            .or_else(|| windows.first())
            .map_or_else(
                || MuxPaneAnchor {
                    session_id: session.id.clone(),
                    pane_id: None,
                    pane_pid: None,
                    cwd: None,
                    process: None,
                },
                |window| window.anchor.clone(),
            );

        MuxSession {
            id: session.id.clone(),
            name: session.name.clone(),
            active,
            anchor,
            active_window_id: Some(session.active_window_id.clone()),
            windows,
            tag: session.tag.clone(),
        }
    }

    fn next_pane_id(&mut self) -> Result<String> {
        let id = format!("pane-{}", self.next_pane);
        self.next_pane = self
            .next_pane
            .checked_add(1)
            .context("native pane identities exhausted")?;
        Ok(id)
    }
}

fn anchor_for_window(session_id: &str, window: &NativeWindow) -> MuxPaneAnchor {
    let pane = window
        .panes
        .iter()
        .find(|pane| pane.id == window.active_pane_id)
        .or_else(|| window.panes.first());
    anchor_for_optional_pane(session_id, pane)
}

fn anchor_for_pane(session_id: &str, pane: &NativePane) -> MuxPaneAnchor {
    anchor_for_optional_pane(session_id, Some(pane))
}

fn anchor_for_optional_pane(session_id: &str, pane: Option<&NativePane>) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session_id.to_owned(),
        pane_id: pane.map(|pane| pane.id.clone()),
        pane_pid: None,
        cwd: pane.map(|pane| pane.cwd.to_string_lossy().into_owned()),
        process: Some("shell".to_owned()),
    }
}

fn next_window_id(session: &mut NativeSession) -> Result<String> {
    // Never reuse a closed tab's identity: queued UI commands may still name it.
    let id = format!("tab-{}", session.next_window);
    session.next_window = session
        .next_window
        .checked_add(1)
        .context("native window identities exhausted")?;
    Ok(id)
}

fn default_window_name() -> String {
    std::env::var("BOOTTY_SHELL")
        .ok()
        .or_else(|| std::env::var("SHELL").ok())
        .and_then(|shell| {
            Path::new(&shell)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "shell".to_owned())
}

impl NativeMuxState {
    fn merge_windows(&mut self, session_id: &str, source: &str, target: &str) -> Result<()> {
        let session = self
            .active_session_mut(session_id)
            .context("session no longer exists")?;
        let source_index = session
            .windows
            .iter()
            .position(|window| window.id == source)
            .context("source tab no longer exists")?;
        if source == target {
            bail!("a tab cannot be merged into itself");
        }
        let target_index = session
            .windows
            .iter()
            .position(|window| window.id == target)
            .context("target tab no longer exists")?;
        let [source, target] = session
            .windows
            .get_disjoint_mut([source_index, target_index])
            .context("tabs must be distinct and live")?;
        target.panes.append(&mut source.panes);
        target.active_pane_id.clone_from(&source.active_pane_id);
        session.active_window_id.clone_from(&target.id);
        session.windows.remove(source_index);
        renumber_windows(session);
        session_id.clone_into(&mut self.active_session_id);
        Ok(())
    }

    fn swap_panes(&mut self, session_id: &str, source: &str, target: &str) -> Result<()> {
        let session = self
            .active_session_mut(session_id)
            .context("session no longer exists")?;
        let (source_window, source_index) =
            pane_location(session, source).context("source pane no longer exists")?;
        let (target_window, target_index) =
            pane_location(session, target).context("target pane no longer exists")?;
        if source == target {
            bail!("a pane cannot be swapped with itself");
        }
        if source_window == target_window {
            let window = session
                .windows
                .get_mut(source_window)
                .context("window no longer exists")?;
            let [source_pane, target_pane] = window
                .panes
                .get_disjoint_mut([source_index, target_index])
                .context("panes must be distinct and live")?;
            std::mem::swap(source_pane, target_pane);
            source.clone_into(&mut window.active_pane_id);
            session.active_window_id.clone_from(&window.id);
        } else {
            let [source_window, target_window] = session
                .windows
                .get_disjoint_mut([source_window, target_window])
                .context("windows no longer exist")?;
            let source_pane = source_window
                .panes
                .get_mut(source_index)
                .context("source pane no longer exists")?;
            let target_pane = target_window
                .panes
                .get_mut(target_index)
                .context("target pane no longer exists")?;
            std::mem::swap(source_pane, target_pane);
            if source_window.active_pane_id == source {
                target.clone_into(&mut source_window.active_pane_id);
            }
            source.clone_into(&mut target_window.active_pane_id);
            session.active_window_id.clone_from(&target_window.id);
        }
        session_id.clone_into(&mut self.active_session_id);
        Ok(())
    }

    fn move_pane(
        &mut self,
        session_id: &str,
        source: &str,
        target: &str,
        direction: crate::command::MuxDirection,
    ) -> Result<()> {
        let session = self
            .active_session_mut(session_id)
            .context("session no longer exists")?;
        let (source_window, source_index) =
            pane_location(session, source).context("source pane no longer exists")?;
        let (target_window, _) =
            pane_location(session, target).context("target pane no longer exists")?;
        if source == target {
            bail!("a pane cannot be moved beside itself");
        }
        let target_window_id = session
            .windows
            .get(target_window)
            .context("target window no longer exists")?
            .id
            .clone();
        let old = session
            .windows
            .get_mut(source_window)
            .context("source window no longer exists")?;
        let pane = old.panes.remove(source_index);
        if old.active_pane_id == source
            && let Some(neighbor) = old.panes.first()
        {
            old.active_pane_id.clone_from(&neighbor.id);
        }
        session.windows.retain(|window| !window.panes.is_empty());
        let destination = session
            .windows
            .iter_mut()
            .find(|window| window.id == target_window_id)
            .context("target window no longer exists")?;
        let target_index = destination
            .panes
            .iter()
            .position(|pane| pane.id == target)
            .context("target pane no longer exists")?;
        let after = usize::from(matches!(
            direction,
            crate::command::MuxDirection::Right | crate::command::MuxDirection::Down
        ));
        destination
            .panes
            .insert(target_index.saturating_add(after), pane);
        source.clone_into(&mut destination.active_pane_id);
        session.active_window_id = target_window_id;
        renumber_windows(session);
        session_id.clone_into(&mut self.active_session_id);
        Ok(())
    }

    fn extract_pane(&mut self, session_id: &str, pane_id: &str) -> Result<()> {
        let session = self
            .active_session_mut(session_id)
            .context("session no longer exists")?;
        let (window_index, pane_index) =
            pane_location(session, pane_id).context("pane no longer exists")?;
        if session
            .windows
            .get(window_index)
            .context("window no longer exists")?
            .panes
            .len()
            < 2
        {
            bail!("this pane already has its own tab");
        }
        let window_id = next_window_id(session)?;
        let source = session
            .windows
            .get_mut(window_index)
            .context("window no longer exists")?;
        let pane = source.panes.remove(pane_index);
        if source.active_pane_id == pane_id
            && let Some(next) = source.panes.first()
        {
            source.active_pane_id.clone_from(&next.id);
        }
        let name = source.name.clone();
        session.windows.insert(
            window_index.saturating_add(1),
            NativeWindow {
                id: window_id.clone(),
                index: 0,
                name,
                active_pane_id: pane_id.to_owned(),
                panes: vec![pane],
            },
        );
        session.active_window_id = window_id;
        renumber_windows(session);
        session_id.clone_into(&mut self.active_session_id);
        Ok(())
    }
}

fn target_pane_location(session: &NativeSession, pane_id: Option<&str>) -> Result<(usize, usize)> {
    if let Some(pane_id) = pane_id {
        return pane_location(session, pane_id).context("pane no longer exists in this session");
    }
    let window = session
        .windows
        .iter()
        .position(|window| window.id == session.active_window_id)
        .context("active window no longer exists")?;
    let active_window = session
        .windows
        .get(window)
        .context("active window no longer exists")?;
    let pane = active_window
        .panes
        .iter()
        .position(|pane| pane.id == active_window.active_pane_id)
        .context("active pane no longer exists")?;
    Ok((window, pane))
}

fn pane_location(session: &NativeSession, pane_id: &str) -> Option<(usize, usize)> {
    session
        .windows
        .iter()
        .enumerate()
        .find_map(|(window_index, window)| {
            window
                .panes
                .iter()
                .position(|pane| pane.id == pane_id)
                .map(|index| (window_index, index))
        })
}
fn renumber_windows(session: &mut NativeSession) {
    for (index, window) in session.windows.iter_mut().enumerate() {
        window.index = u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX);
    }
}

pub struct NativeBackend {
    state: Arc<Mutex<NativeMuxState>>,
}

impl NativeBackend {
    /// A backend on the state shared by every caller that has no workspace to name.
    #[must_use]
    pub fn new() -> Self {
        Self::for_workspace(Path::new(""))
    }

    /// The mux state belonging to `workspace`, creating it on first use.
    ///
    /// Native sessions live in this process, not in a server, so they have to outlive any single
    /// `AppState` -- closing and reopening a window keeps its sessions. Keying by workspace gives
    /// that while stopping two unrelated workspaces from seeing each other's sessions, which is what
    /// a single process-wide state could not do: in tests it accumulated every session every test
    /// created, so any assertion on a session list saw all of them and flaked.
    pub fn for_workspace(workspace: &Path) -> Self {
        static STATES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<NativeMuxState>>>>> =
            OnceLock::new();
        let mut states = STATES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self {
            state: Arc::clone(
                states
                    .entry(workspace.to_path_buf())
                    .or_insert_with(|| Arc::new(Mutex::new(NativeMuxState::new()))),
            ),
        }
    }
}

impl Default for NativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MuxBackend for NativeBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        self.state
            .lock()
            .map(|state| state.snapshot())
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        state.execute(command)
    }
}

impl NativeMuxState {
    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            } => self.activate_window(&session_id, &window_id),
            MuxCommand::NewWindow { session_id, cwd } => {
                self.new_window(&session_id, cwd.map(PathBuf::from))?;
            }
            MuxCommand::RenameWindow {
                session_id,
                window_id,
                name,
            } => {
                self.rename_window(&session_id, &window_id, name);
            }
            MuxCommand::ActivateNextWindow { session_id } => {
                self.activate_relative_window(&session_id, 1);
            }
            MuxCommand::ActivatePreviousWindow { session_id }
            | MuxCommand::ActivateLastWindow { session_id } => {
                self.activate_relative_window(&session_id, -1);
            }
            MuxCommand::ActivateWindowIndex { session_id, index } => {
                self.activate_window_index(&session_id, index);
            }
            MuxCommand::MoveWindow {
                session_id,
                window_id,
                delta,
            } => {
                self.move_window(&session_id, window_id.as_deref(), delta);
            }
            MuxCommand::MoveWindowPreservingSelection {
                session_id,
                window_id,
                delta,
                selected_window_id,
            } => {
                self.move_window_preserving_selection(
                    &session_id,
                    &window_id,
                    delta,
                    &selected_window_id,
                );
            }

            MuxCommand::CreateProjectSession {
                session_id,
                cwd,
                tag,
            }
            | MuxCommand::CreateWorktreeSession {
                session_id,
                cwd,
                tag,
            } => {
                self.ensure_session(&session_id, cwd, tag)?;
            }
            MuxCommand::RenameSession { session_id, name } => {
                self.rename_session(&session_id, name);
            }
            MuxCommand::DitchSession { session_id } => self.kill_session(&session_id),
            MuxCommand::StampSession { session_id, tag } => self.stamp_session(&session_id, tag),

            command => self.execute_pane_command(command)?,
        }
        Ok(())
    }
    fn execute_pane_command(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::SplitPane {
                session_id,
                pane_id,
                ..
            } => self.split_pane(&session_id, pane_id.as_deref())?,
            MuxCommand::MergeWindows {
                session_id,
                source_window_id,
                target_window_id,
            } => self.merge_windows(&session_id, &source_window_id, &target_window_id)?,
            MuxCommand::SwapPanes {
                session_id,
                source_pane_id,
                target_pane_id,
            } => self.swap_panes(&session_id, &source_pane_id, &target_pane_id)?,
            MuxCommand::MovePane {
                session_id,
                pane_id,
                target_pane_id,
                direction,
            } => self.move_pane(&session_id, &pane_id, &target_pane_id, direction)?,
            MuxCommand::ExtractPane {
                session_id,
                pane_id,
            } => self.extract_pane(&session_id, &pane_id)?,
            MuxCommand::SelectPane {
                session_id,
                window_id,
                direction,
            } => {
                self.select_directional_pane(&session_id, window_id.as_deref(), direction);
            }
            MuxCommand::SelectNextPane {
                session_id,
                window_id,
            } => self.select_pane(&session_id, window_id.as_deref(), 1),
            MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            } => self.select_pane(&session_id, window_id.as_deref(), -1),
            MuxCommand::KillPane {
                session_id,
                pane_id,
            } => self.close_pane(&session_id, pane_id.as_deref(), false)?,
            MuxCommand::ClosePane {
                session_id,
                pane_id,
            } => self.close_pane(&session_id, pane_id.as_deref(), true)?,
            MuxCommand::TogglePaneZoom { .. } => {}
            _ => anyhow::bail!("command is not a pane operation"),
        }
        Ok(())
    }
}

#[must_use]
pub fn native_capabilities(scope: SpaceId) -> BindingCapabilityDescriptor {
    BindingCapabilityDescriptor::new(
        scope,
        [
            BindingOperation::ActivateWindow,
            BindingOperation::CreateWindow,
            BindingOperation::RenameWindow,
            BindingOperation::NavigateWindow,
            BindingOperation::MoveWindow,
            BindingOperation::SplitPane,
            BindingOperation::MergeWindows,
            BindingOperation::SwapPanes,
            BindingOperation::MovePane,
            BindingOperation::ExtractPane,
            BindingOperation::NavigatePane,
            BindingOperation::ClosePane,
            BindingOperation::CreateProjectSession,
            BindingOperation::StampSession,
            BindingOperation::CreateWorktreeSession,
            BindingOperation::RenameSession,
            BindingOperation::DitchSession,
        ],
    )
}

pub struct NativePanePolicy;

impl BackendPanePolicy for NativePanePolicy {
    fn remote_target(&self) -> Option<crate::RemoteTarget> {
        None
    }

    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        if !matches!(request.target.mux_target(), MuxPaneTarget::Pane { .. }) {
            return Ok(None);
        }
        let mut config = request.terminal_config.clone();
        config.launch.working_directory =
            request.target.cwd().map(Path::new).map(Path::to_path_buf);
        config.launch.pane_id = request.target.pane_id().map(str::to_owned);
        config.side_effect_pane_id = request.target.side_effect_pane_id();
        Ok(Some(Box::new(StartingNativeTerminal::spawn(
            request.spawn_geometry,
            request.display_scale,
            request.render_cell,
            config,
            Arc::clone(request.repaint_wakeup),
        ))))
    }

    fn sync_target(&mut self, _target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {}

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(&mut self, _request: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }

    fn deactivate(&mut self) {}
}
