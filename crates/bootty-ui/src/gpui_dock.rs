//! The workspace Dock: Sessions on the left, the selected backend window in the center, and
//! presentation-only layout persistence.

mod layout;
pub mod motion;
mod panes;
mod registry;

use crate::commands::DockAction;
use layout::{LayoutSaveHandle, SavedLayout};
use registry::{register, register_factory};

use std::{cell::RefCell, path::PathBuf, rc::Rc};

use bootty_control::{BoundAppCommandSender, CommandTarget};
use gpui_kit::component::dock::{
    DockArea, DockEvent, DockLayout, DockPlacement, InsertTarget, NodeId, PaneNode, PaneRef,
    PanelId, panel_handle,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    Styled, Subscription, Window, div, prelude::*,
};

/// The binding and terminal a dock's commands address, and the host its saved layout is keyed by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockContext {
    pub target: CommandTarget,
    pub terminal: Option<CommandTarget>,
    pub host_identity: String,
}

pub struct DockFocusChanged;

/// Requests accepted while the persisted layout is still loading.
#[derive(Default)]
struct RestoreRequests {
    commands: Vec<crate::commands::DockRequest>,
}

pub struct WorkspaceDock {
    owner: gpui_kit::WeakEntity<crate::gpui_workspace::GpuiWorkspace>,
    area: Entity<DockArea>,
    context: DockContext,
    panes: panes::PanePanels,
    projected: Option<(
        bootty_mux::workspace::ScopedWindowId,
        bootty_mux::snapshot::MuxPaneLayout,
    )>,
    pane_target: Option<CommandTarget>,
    focused_pane: Option<String>,
    pub(crate) pane_divider: (f32, Option<gpui_kit::Hsla>),
    pending_resize: Option<bootty_mux::snapshot::MuxPaneLayout>,
    pub(crate) navigation: Entity<crate::gpui_dock_skin::WorkspaceNavigation>,
    pub(crate) titlebar: Entity<crate::gpui_dock_skin::WorkspaceTitleBar>,
    pub(crate) motion: Rc<RefCell<motion::DockMotion>>,
    focused_group: Option<NodeId>,
    target: CommandTarget,
    sender: BoundAppCommandSender,
    restoring: Option<RestoreRequests>,
    focus: FocusHandle,
    error: Option<String>,
    pub(crate) empty_terminal: Option<(NodeId, crate::workspace_composition::EmptyTerminalState)>,
    sessions: Entity<crate::gpui_sidebar_panel::SessionsPanel>,
    save: LayoutSaveHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DockFocusChanged> for WorkspaceDock {}
impl Focusable for WorkspaceDock {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The workspace parts a dock hosts, and where it sends commands.
pub struct DockHosts {
    pub owner: gpui_kit::WeakEntity<crate::gpui_workspace::GpuiWorkspace>,
    pub chrome: Entity<crate::gpui::chrome::GpuiChrome>,
    pub sender: BoundAppCommandSender,
}

/// Where a dock keeps its state: beside the config file under the window's state key.
pub struct DockStorage<'a> {
    pub config_path: &'a std::path::Path,
    pub state_key: String,
}

impl WorkspaceDock {
    pub(crate) fn new(
        context: DockContext,
        hosts: DockHosts,
        scope: bootty_mux::controller::SpaceId,
        storage: DockStorage<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let DockHosts {
            owner,
            chrome,
            sender,
        } = hosts;
        let DockStorage {
            config_path,
            state_key,
        } = storage;
        let legacy_key = format!(
            "{state_key}:space:{}:host:{}",
            scope.persistence_value(),
            context.host_identity
        );
        let motion = Rc::<RefCell<motion::DockMotion>>::default();
        let (area, navigation, titlebar) = Self::skinned_area(&chrome, &motion, window, cx);
        // Legacy sidebar values seed unsaved layouts; saved docks own their live geometry.
        let sidebar_defaults = chrome.read(cx).sidebar_defaults();
        let sessions = Self::sessions_panel(&area, &chrome, cx);
        let target = context.target.clone();
        let panes = panes::PanePanels::new(&area, cx);
        Self::configure_default_layout(&area, &sessions, sidebar_defaults, window, cx);
        let path = config_path.with_file_name("native-panels.json");
        let save = Self::layout_writer(path.clone(), state_key.clone(), cx);
        let (focus, mut subscriptions) = Self::subscribe_layout(&area, window, cx);
        if let Some(owner) = owner.upgrade() {
            subscriptions.push(cx.observe(&owner, |_, _, cx| cx.notify()));
        }
        Self::restore_layout(path, state_key, legacy_key, window, cx);
        Self {
            owner,
            context,
            panes,
            projected: None,
            pane_target: None,
            focused_pane: None,
            pane_divider: (1., None),
            pending_resize: None,
            area,
            titlebar,
            navigation,
            motion,
            focused_group: None,
            target,
            sender,
            restoring: Some(RestoreRequests::default()),
            focus,
            error: None,
            empty_terminal: None,
            sessions,
            save,
            _subscriptions: subscriptions,
        }
    }

    /// The sessions panel, which also answers saves from before it replaced the sidebar.
    fn sessions_panel(
        area: &Entity<DockArea>,
        chrome: &Entity<crate::gpui::chrome::GpuiChrome>,
        cx: &mut Context<Self>,
    ) -> Entity<crate::gpui_sidebar_panel::SessionsPanel> {
        let sessions =
            cx.new(|cx| crate::gpui_sidebar_panel::SessionsPanel::new(chrome.clone(), cx));
        register(area, sessions.clone(), cx);
        let legacy_sessions = panel_handle(sessions.clone());
        register_factory(
            area,
            "bootty.sidebar",
            Rc::new(move |_, _, _| Some(legacy_sessions.clone())),
            cx,
        );
        sessions
    }

    /// The dock area drawn with Bootty's skin, and the navigation and title bar that follow it.
    fn skinned_area(
        chrome: &Entity<crate::gpui::chrome::GpuiChrome>,
        motion: &Rc<RefCell<motion::DockMotion>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (
        Entity<DockArea>,
        Entity<crate::gpui_dock_skin::WorkspaceNavigation>,
        Entity<crate::gpui_dock_skin::WorkspaceTitleBar>,
    ) {
        let dock_owner = cx.weak_entity();
        let area = cx.new(|cx| {
            let skin = crate::gpui_dock_skin::WorkspaceDockSkin::new(
                dock_owner.clone(),
                chrome.clone(),
                motion.clone(),
                cx,
            );
            DockArea::new("workspace", Some(8), window, cx).with_renderer(skin)
        });
        let navigation = cx.new(|cx| {
            crate::gpui_dock_skin::WorkspaceNavigation::new(
                dock_owner.clone(),
                &area,
                chrome.clone(),
                cx,
            )
        });
        let titlebar = cx.new(|cx| {
            crate::gpui_dock_skin::WorkspaceTitleBar::new(chrome.clone(), dock_owner, &area, cx)
        });
        (area, navigation, titlebar)
    }

    fn configure_default_layout(
        area: &Entity<DockArea>,
        sessions: &Entity<crate::gpui_sidebar_panel::SessionsPanel>,
        sidebar_defaults: (crate::gpui::chrome::SidebarPosition, f32, bool),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layout = DockLayout::tabs();
        let sidebar_placement = DockPlacement::Left;
        area.update(cx, |area, cx| {
            area.set_center(layout, window, cx);
            area.set_dock(
                sidebar_placement,
                DockLayout::tabs().panel_view(panel_handle(sessions.clone()), cx),
                window,
                cx,
            );
            area.set_dock_size(
                sidebar_placement,
                gpui_kit::px(sidebar_defaults.1),
                window,
                cx,
            );
            if area.is_dock_open(sidebar_placement) != sidebar_defaults.2 {
                area.toggle_dock(sidebar_placement, window, cx);
            }
        });
    }

    fn layout_writer(path: PathBuf, state_key: String, cx: &mut Context<Self>) -> LayoutSaveHandle {
        let (save, result_rx) = LayoutSaveHandle::new(path, state_key, cx);
        cx.spawn(async move |weak, cx| {
            while let Ok(result) = result_rx.recv().await {
                if weak
                    .update(cx, |this, cx| {
                        this.error = result.err();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        save
    }

    fn subscribe_layout(
        area: &Entity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (FocusHandle, Vec<Subscription>) {
        let focus = cx.focus_handle();
        let focus_in = cx.on_focus_in(&focus, window, |_, _, cx| cx.emit(DockFocusChanged));
        let focus_out = cx.on_focus_out(&focus, window, |_, _, _, cx| cx.emit(DockFocusChanged));
        let subscription = cx.subscribe_in(
            area,
            window,
            |this, _, event: &DockEvent, window, cx| match event {
                DockEvent::LayoutChanged => this.layout_changed(window, cx),
                DockEvent::DragDrop { item, target } => this.drop_pane(item, *target, window, cx),
            },
        );
        let area_id = area.entity_id();
        cx.on_release(move |_, cx| {
            registry::unregister(area_id, cx);
        })
        .detach();
        (focus, vec![subscription, focus_in, focus_out])
    }

    fn restore_layout(
        path: PathBuf,
        state_key: String,
        legacy_key: String,
        window: &Window,
        cx: &Context<Self>,
    ) {
        cx.spawn_in(window, async move |weak, cx| {
            let saved = cx
                .background_executor()
                .spawn(async move { SavedLayout::load(&path, &state_key, &legacy_key) })
                .await;
            _ = weak.update_in(cx, |this, window, cx| {
                if let Some(saved) = saved {
                    this.load_layout(saved, window, cx);
                }
                let pending = this.restoring.take().unwrap_or_default();
                for request in pending.commands {
                    this.apply_request(request, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn set_context(&mut self, context: DockContext, cx: &mut Context<Self>) {
        // Selection reconciliation retries after startup restoration; a content change
        // must not discard a saved layout that is still being read.
        if self.restoring.is_some() || self.context == context {
            return;
        }
        self.target = context.target.clone();
        self.context = context;
        self.projected = None;
        cx.notify();
    }

    pub(crate) fn matches_target(
        &self,
        target: &CommandTarget,
        terminal: Option<&CommandTarget>,
    ) -> bool {
        self.target == *target && self.context.terminal.as_ref() == terminal
    }

    pub(crate) fn invoke_action(
        &mut self,
        action: crate::commands::DockAction,
        node: Option<NodeId>,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let mut invocation = bootty_control::CommandInvocation::from_action(
            action.command().action(),
            bootty_control::Caller::Internal,
        );
        invocation.arguments = node
            .into_iter()
            .map(|node| node.as_u64().to_string())
            .collect();
        self.submit_command(invocation, window, cx);
    }

    pub(crate) fn submit_command(
        &mut self,
        invocation: bootty_control::CommandInvocation,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let failed = crate::action_catalog::Command::from_action(&invocation.command).map_or_else(
            || "The command failed".to_owned(),
            |command| format!("{} failed", command.title()),
        );
        let receiver = match self.sender.submit(
            invocation,
            std::time::Instant::now()
                .checked_add(std::time::Duration::from_mins(2))
                .unwrap_or_else(std::time::Instant::now),
            bootty_control::CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.projected = None;
                self.pending_resize = None;
                self.error = Some(crate::error_catalog::command_send_message(error).to_owned());
                return;
            }
        };
        cx.spawn_in(window, async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, _, cx| {
                this.projected = None;
                this.pending_resize = None;
                this.error = result.map_or_else(
                    |_| Some("The command stopped before it finished.".to_owned()),
                    |outcome| {
                        crate::commands::command_outcome_message(&outcome).map(|message| {
                            crate::error_catalog::ErrorNotice::failed(&failed, message).to_string()
                        })
                    },
                );
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn clear_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.projected.take().is_some() {
            self.area.update(cx, |area, cx| {
                area.set_center(DockLayout::tabs(), window, cx);
            });
        }
    }

    pub(crate) fn set_empty_terminal(
        &mut self,
        state: Option<crate::workspace_composition::EmptyTerminalState>,
        cx: &mut Context<Self>,
    ) {
        let state = state.and_then(|state| {
            let tree = self.area.read(cx).layout(DockPlacement::Center)?;
            tree.panels()
                .next()
                .is_none()
                .then(|| (tree.root().id(), state))
        });
        if self.empty_terminal != state {
            self.empty_terminal = state;
            self.area.update(cx, |_, cx| cx.notify());
            cx.notify();
        }
    }

    pub(crate) fn new_session_surface(&self, cx: &App) -> Option<gpui_kit::AnyElement> {
        self.owner.upgrade()?.read(cx).new_session_surface(cx)
    }

    fn new_session_overlay(&self, cx: &App) -> Option<gpui_kit::AnyElement> {
        let surface = self.new_session_surface(cx)?;
        let area = self.area.read(cx);
        let left = if area.is_dock_open(DockPlacement::Left) {
            area.dock_size(DockPlacement::Left).unwrap_or_default()
        } else {
            gpui_kit::px(0.0)
        };
        Some(
            div()
                .id("new-session-workspace-overlay")
                .absolute()
                .left(left)
                .top(self.titlebar.read(cx).height())
                .right_0()
                .bottom_0()
                .occlude()
                .child(surface)
                .into_any_element(),
        )
    }
}

impl WorkspaceDock {
    /// Whether the Sessions panel is the active tab of an open dock.
    pub(crate) fn sessions_visible(&self, cx: &App) -> bool {
        let id = PanelId::from(self.sessions.entity_id());
        let area = self.area.read(cx);
        area.is_dock_open(DockPlacement::Left)
            && area.layout(DockPlacement::Left).is_some_and(|tree| {
                tree.find_panel_node(id)
                    .and_then(|node| tree.find_node(node))
                    .is_some_and(|node| match node.kind() {
                        PaneRef::Tabs { panels, active_ix } => panels.get(active_ix) == Some(&id),
                        PaneRef::Split { .. } => false,
                    })
            })
    }

    /// Opens the left dock on the Sessions panel, adding the panel when a layout lost it.
    fn show_sessions(&self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = panel_handle(self.sessions.clone());
        let id = panel.panel_id(cx);
        self.area.update(cx, |area, cx| {
            if area.panel(id).is_none() {
                area.add_panel_view(panel, DockPlacement::Left, None, window, cx);
            }
            let node = area
                .layout(DockPlacement::Left)
                .and_then(|tree| tree.find_panel_node(id));
            if let Some(node) = node {
                let ix = panel_tab_index(area, DockPlacement::Left, node, id);
                area.move_panel(
                    id,
                    InsertTarget::Tabs {
                        node,
                        ix,
                        activate: true,
                    },
                    window,
                    cx,
                );
                if !area.is_dock_open(DockPlacement::Left) {
                    area.toggle_dock(DockPlacement::Left, window, cx);
                }
            }
        });
    }

    fn show_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_sessions(window, cx);
        self.sessions.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn layout_changed(&mut self, window: &Window, cx: &mut Context<Self>) {
        cx.notify();
        if self.restoring.is_some() {
            return;
        }
        if let Some((id, authoritative)) = &self.projected
            && let Some(proposed) = self.pane_layout(cx)
            && !crate::workspace_composition::pane_layout_matches(authoritative, &proposed)
            && self.pending_resize.as_ref().is_none_or(|pending| {
                !crate::workspace_composition::pane_layout_matches(pending, &proposed)
            })
            && let Some(target) = self.pane_target.clone()
        {
            // Kit emits this event once at drag end. Programmatic reflections compare equal.
            let Some(invocation) = crate::workspace_composition::pane_resize_command(
                id.window_id(),
                authoritative,
                &proposed,
                target,
            ) else {
                return;
            };
            self.pending_resize = Some(proposed);
            self.submit_command(invocation, window, cx);
        }
        self.save_layout(cx);
    }

    fn load_layout(&mut self, saved: SavedLayout, window: &mut Window, cx: &mut Context<Self>) {
        let mut layout = crate::workspace_composition::fixed_panel_layout(saved.layout);
        layout.version = self.area.read(cx).version();
        self.error = self
            .area
            .update(cx, |area, cx| area.load(layout, window, cx))
            .err()
            .map(|error| error.to_string());
        self.projected = None;
    }
    fn save_layout(&self, cx: &App) {
        self.save.send(self.saved_layout(cx));
    }
    fn saved_layout(&self, cx: &App) -> SavedLayout {
        SavedLayout {
            layout: crate::workspace_composition::fixed_panel_layout(self.area.read(cx).dump(cx)),
        }
    }

    /// Opens or closes a dock for the user. A side dock slides; one sliding shut slides back.
    pub(crate) fn toggle_dock(
        &self,
        placement: DockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = self.area.read(cx);
        if area.has_dock(placement) {
            let open = area.is_dock_open(placement);
            let width = area.dock_size(placement).map_or(0.0, f32::from);
            let slides = placement.is_left() || placement.is_right();
            let now = cx.background_executor().now();
            let mut motion = self.motion.borrow_mut();
            if slides && !cx.reduce_motion() && open && area.is_dock_collapsible(placement) {
                let shut = !motion.closing(placement);
                motion.start(placement, shut, width, now);
                drop(motion);
                self.area.update(cx, |_, cx| cx.notify());
                return;
            }
            if slides && !cx.reduce_motion() && !open {
                motion.start(placement, false, width, now);
            }
            drop(motion);
            self.area
                .update(cx, |area, cx| area.toggle_dock(placement, window, cx));
        } else {
            self.area.update(cx, |area, cx| {
                area.set_dock(placement, DockLayout::tabs(), window, cx);
            });
        }
    }

    pub(crate) fn sessions_focused(&self, window: &Window, cx: &App) -> bool {
        // Bare navigation keys belong to the panel, not its search field or buttons.
        self.sessions.read(cx).focus_handle(cx).is_focused(window)
    }

    pub(crate) fn remember_focus(&mut self, window: &Window, cx: &App) {
        let area = self.area.read(cx);
        for placement in [
            DockPlacement::Center,
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ] {
            let Some(tree) = area.layout(placement) else {
                continue;
            };
            for (_, node) in group_paths(area) {
                let Some(node) = tree.find_node(node) else {
                    continue;
                };
                if let PaneRef::Tabs { panels, .. } = node.kind()
                    && panels.iter().any(|panel| {
                        area.panel(*panel).is_some_and(|panel| {
                            panel.focus_handle(cx).contains_focused(window, cx)
                        })
                    })
                {
                    self.focused_group = Some(node.id());
                    return;
                }
            }
        }
    }

    pub(crate) fn apply_request(
        &mut self,
        request: crate::commands::DockRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remember_focus(window, cx);
        if let Some(pending) = &mut self.restoring {
            pending.commands.push(request);
            return;
        }
        if let Err(error) = request.begin() {
            request.complete(crate::commands::runtime::command_outcome_for_mux_error(
                error,
            ));
            return;
        }
        let outcome = self.apply_action(&request, window, cx);
        request.complete(outcome);
    }

    fn apply_action(
        &self,
        request: &crate::commands::DockRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bootty_control::CommandOutcome {
        if let Some(id) = request.group {
            let Some(node) = group_paths(self.area.read(cx))
                .into_iter()
                .find_map(|(_, node)| (node.as_u64() == id).then_some(node))
            else {
                return bootty_control::CommandOutcome::StaleTarget {
                    message: "The requested tab group no longer exists.".into(),
                };
            };
            if self
                .area
                .read(cx)
                .layout(DockPlacement::Center)
                .is_some_and(|tree| tree.find_node(node).is_some())
            {
                return bootty_control::CommandOutcome::Unavailable {
                    message: "The terminal tab group is reserved for the active mux window.".into(),
                };
            }
        }
        match request.action {
            DockAction::ToggleLeft => self.toggle_dock(DockPlacement::Left, window, cx),
            DockAction::ToggleSessions if self.sessions_visible(cx) => {
                self.area.update(cx, |area, cx| {
                    area.remove_panel(self.sessions.clone(), window, cx);
                });
            }
            DockAction::ToggleSessions | DockAction::Sidebar | DockAction::Spaces => {
                self.show_sidebar(window, cx);
            }
            DockAction::ToggleTabBar | DockAction::ToggleHiddenTabs => {
                return bootty_control::CommandOutcome::Unavailable {
                    message: "Tool tabs always remain visible.".into(),
                };
            }
        }
        bootty_control::CommandOutcome::success()
    }
}

impl Render for WorkspaceDock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .children(self.error.clone().map(|error| {
                gpui_kit::component::alert::Alert::error("dock-command-error", error).on_close(
                    cx.listener(|this, _, _, cx| {
                        this.error = None;
                        cx.notify();
                    }),
                )
            }))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(self.area.clone())
                    .children(self.new_session_overlay(cx)),
            )
    }
}

// Paths are serialized with the same layout snapshot; live preferences follow stable node IDs.
fn group_paths(area: &DockArea) -> Vec<(String, NodeId)> {
    fn visit(node: &PaneNode, path: String, groups: &mut Vec<(String, NodeId)>) {
        match node.kind() {
            PaneRef::Tabs { .. } => groups.push((path, node.id())),
            PaneRef::Split { children, .. } => {
                for (ix, child) in children.iter().enumerate() {
                    visit(child, format!("{path}/{ix}"), groups);
                }
            }
        }
    }
    let mut groups = Vec::new();
    for (name, placement) in [
        ("center", DockPlacement::Center),
        ("left", DockPlacement::Left),
        ("right", DockPlacement::Right),
        ("bottom", DockPlacement::Bottom),
    ] {
        if let Some(tree) = area.layout(placement) {
            visit(tree.root(), name.into(), &mut groups);
        }
    }
    groups
}

fn panel_tab_index(
    area: &DockArea,
    placement: DockPlacement,
    node: NodeId,
    id: PanelId,
) -> Option<usize> {
    area.layout(placement)?
        .find_node(node)
        .and_then(|node| match node.kind() {
            PaneRef::Tabs { panels, .. } => panels.iter().position(|panel| *panel == id),
            PaneRef::Split { .. } => None,
        })
}
