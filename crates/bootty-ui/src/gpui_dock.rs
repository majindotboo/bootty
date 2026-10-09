//! Native tool-panel composition and presentation-only layout persistence.

mod browser;
mod layout;
mod registry;
pub mod surfaces;

use crate::commands::DockAction;
use layout::{LayoutSaveHandle, SavedLayout};
use registry::{PanelFactory, register, register_factory};

use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

use bootty_control::{BoundAppCommandSender, CommandTarget};
use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockEvent, DockLayout, DockPlacement, InsertTarget, NodeId, PaneNode,
    PaneRef, Panel, PanelEvent, PanelId, PanelInfo, panel_handle,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    Styled, Subscription, Window, div, prelude::*,
};

use crate::gpui_git_panel::{GitChangesPanel, GitDiffPanel, GitPanelContext, OpenDiff};
use crate::{
    gpui_document_panel::{DocumentClosed, DocumentPanel},
    gpui_files_panel::{FilesPanel, OpenDocument},
};

/// Dock metadata for tool panels without persistence or activation hooks.
macro_rules! tool_panel {
    ($panel:ty, $name:literal, $title:literal, padding = $padding:literal) => {
        impl gpui_kit::EventEmitter<gpui_kit::component::dock::PanelEvent> for $panel {}
        impl gpui_kit::component::dock::BasePanel for $panel {
            fn panel_name(&self) -> &'static str {
                $name
            }
        }
        impl gpui_kit::component::dock::Panel for $panel {
            fn tab_name(&self, _: &gpui_kit::App) -> Option<gpui_kit::SharedString> {
                Some($title.into())
            }
            fn title(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut gpui_kit::Context<Self>,
            ) -> impl gpui_kit::IntoElement {
                $title
            }
            fn inner_padding(&self, _: &gpui_kit::App) -> bool {
                $padding
            }
        }
    };
}
pub(crate) use tool_panel;

#[derive(Clone)]
pub struct TerminalWindowPresentation {
    pub(crate) id: bootty_mux::workspace::ScopedWindowId,
    pub(crate) title: String,
}
pub struct DockFocusChanged;

#[derive(Clone, Copy)]
enum InspectorPanel {
    Changes,
    Files,
    Agents,
}

/// Requests accepted while the persisted layout is still loading.
#[derive(Default)]
struct RestoreRequests {
    panel: Option<InspectorPanel>,
    commands: Vec<crate::commands::DockRequest>,
    browser: Vec<crate::commands::BrowserRequest>,
    documents: Vec<(String, u32, u32)>,
}

/// Session-bound content. Retain only unfinished work when navigation replaces it.
struct ContextPanels {
    context: GitPanelContext,
    changes: Entity<GitChangesPanel>,
    diff: Entity<GitDiffPanel>,
    files: Entity<FilesPanel>,
    subscriptions: Vec<Subscription>,
}

impl ContextPanels {
    fn new(
        context: GitPanelContext,
        sender: BoundAppCommandSender,
        local_git: Option<bootty_git::GitFactsCache>,
        window: &mut Window,
        cx: &mut Context<WorkspaceDock>,
    ) -> Self {
        let diff = cx.new(|cx| GitDiffPanel::new(window, cx));
        let changes = cx.new(|cx| {
            GitChangesPanel::new(
                context.clone(),
                sender.clone(),
                diff.clone(),
                local_git,
                window,
                cx,
            )
        });
        let files =
            cx.new(|cx| FilesPanel::new(context.clone(), sender, changes.clone(), window, cx));
        let mut panels = Self {
            context,
            changes,
            diff,
            files,
            subscriptions: Vec::new(),
        };
        let diff_subscription = cx.subscribe_in(
            &panels.changes,
            window,
            |this, source, _: &OpenDiff, window, cx| {
                if source != &this.panels.changes {
                    return;
                }
                let focus = window.focused(cx);
                let panel = panel_handle(this.panels.diff.clone());
                this.add_document_panel(panel, window, cx);
                this.area.update(cx, |area, cx| {
                    area.select_panel(PanelId::from(this.panels.diff.entity_id()), window, cx);
                });
                if let Some(focus) = focus {
                    focus.focus(window, cx);
                }
            },
        );
        let files_subscription = cx.subscribe_in(
            &panels.files,
            window,
            |this, source, event: &OpenDocument, window, cx| {
                if source != &this.panels.files {
                    return;
                }
                this.request_document(event.0.clone(), window, cx);
            },
        );
        let files_layout_subscription =
            cx.subscribe(&panels.files, |this, source, event: &PanelEvent, cx| {
                if source != this.panels.files {
                    return;
                }
                if matches!(event, PanelEvent::LayoutChanged) {
                    this.layout_changed(cx);
                }
            });
        panels.subscriptions = vec![
            diff_subscription,
            files_subscription,
            files_layout_subscription,
        ];
        panels
    }

    fn register(&self, area: &Entity<DockArea>, cx: &mut App) {
        register(area, self.changes.clone(), cx);
        register(area, self.diff.clone(), cx);
        // Root follows the active session; layout restoration must not restore a stale directory.
        register(area, self.files.clone(), cx);
    }

    fn has_pending_work(&self, cx: &App) -> bool {
        self.changes.read(cx).has_pending_work(cx)
    }
}

pub struct WorkspaceDock {
    owner: gpui_kit::WeakEntity<crate::gpui_workspace::GpuiWorkspace>,
    area: Entity<DockArea>,
    panels: ContextPanels,
    retained_panels: Vec<ContextPanels>,
    surfaces: surfaces::CenterSurfaces,
    centers: std::collections::BTreeMap<String, gpui_kit::component::dock::PanelState>,
    center_task: Option<String>,
    restored_agent_destination: Option<(String, String, String)>,
    document_context: Rc<RefCell<GitPanelContext>>,
    pub(crate) titlebar: Entity<crate::gpui_dock_skin::WorkspaceTitleBar>,
    /// The initial terminal window adapter; subsequent windows have distinct center panels.
    terminal: Entity<crate::gpui_terminal_panel::TerminalPanel>,
    attachment: Entity<crate::gpui_terminal_panel::TerminalAttachmentPanel>,
    focused_group: Option<NodeId>,
    target: CommandTarget,
    sender: BoundAppCommandSender,
    restoring: Option<RestoreRequests>,
    focus: FocusHandle,
    error: Option<String>,
    pub(crate) empty_terminal: Option<(NodeId, crate::workspace_composition::EmptyTerminalState)>,
    agents: Entity<crate::gpui_agents_panel::AgentsPanel>,
    sessions: Entity<crate::gpui_sidebar_panel::SessionsPanel>,
    browser: browser::BrowserPanels,
    documents: Rc<RefCell<Vec<gpui_kit::WeakEntity<DocumentPanel>>>>,
    document_factory: PanelFactory,
    present: bool,
    save: LayoutSaveHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DockFocusChanged> for WorkspaceDock {}
impl Focusable for WorkspaceDock {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl WorkspaceDock {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        context: GitPanelContext,
        owner: gpui_kit::WeakEntity<crate::gpui_workspace::GpuiWorkspace>,
        terminal: Entity<crate::gpui_terminal_view::GpuiTerminalView>,
        chrome: Entity<crate::gpui::chrome::GpuiChrome>,
        scope: bootty_mux::controller::SpaceId,
        sender: BoundAppCommandSender,
        config_path: &std::path::Path,
        state_key: String,
        browser_profile: PathBuf,
        local_git: Option<bootty_git::GitFactsCache>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let legacy_key = format!(
            "{state_key}:space:{}:host:{}",
            scope.persistence_value(),
            context.host_identity
        );
        let dock_owner = cx.weak_entity();
        let area = cx.new(|cx| {
            let skin = crate::gpui_dock_skin::WorkspaceDockSkin::new(
                dock_owner.clone(),
                chrome.clone(),
                cx,
            );
            DockArea::new("workspace", Some(8), window, cx).with_renderer(skin)
        });
        let titlebar = cx.new(|cx| {
            crate::gpui_dock_skin::WorkspaceTitleBar::new(chrome.clone(), dock_owner, &area, cx)
        });
        let attachment = cx.new(|cx| {
            crate::gpui_terminal_panel::TerminalAttachmentPanel::new(terminal, owner.clone(), cx)
        });
        register(&area, attachment.clone(), cx);
        // The terminal center is a singleton: the mux owns the window and every split inside
        // it, so Dock restores whichever leaf the save holds onto this one panel.
        let terminal_panel = cx.new(|cx| {
            crate::gpui_terminal_panel::TerminalPanel::new(
                context.target.clone(),
                bootty_mux::workspace::ScopedWindowId::new(scope, String::new(), String::new()),
                "Terminal".to_owned(),
                owner.clone(),
                window,
                cx,
            )
        });
        register(&area, terminal_panel.clone(), cx);
        // Legacy sidebar values seed unsaved layouts; saved docks own their live geometry.
        let sidebar_defaults = chrome.read(cx).sidebar_defaults();
        let sessions =
            cx.new(|cx| crate::gpui_sidebar_panel::SessionsPanel::new(chrome.clone(), cx));
        register(&area, sessions.clone(), cx);
        let legacy_sessions = panel_handle(sessions.clone());
        register_factory(
            &area,
            "bootty.sidebar",
            Rc::new(move |_, _, _| legacy_sessions.clone()),
            cx,
        );
        let panels = ContextPanels::new(context.clone(), sender.clone(), local_git, window, cx);
        panels.register(&area, cx);
        let agents = cx.new(|cx| {
            crate::gpui_agents_panel::AgentsPanel::new(sender.clone(), chrome, window, cx)
        });
        register(&area, agents.clone(), cx);
        let browser =
            browser::BrowserPanels::new(&area, sender.clone(), browser_profile, window, cx);
        let document_context = Rc::new(RefCell::new(context));
        let current_document_context = document_context.clone();
        let documents: Rc<RefCell<Vec<gpui_kit::WeakEntity<DocumentPanel>>>> = Rc::default();
        let document_list = documents.clone();
        let target = document_context.borrow().target.clone();
        let open_sender = sender.clone();
        let dock = cx.weak_entity();
        let document_factory =
            Self::document_factory(current_document_context, document_list, sender, dock);
        register_factory(&area, "bootty.document", document_factory.clone(), cx);
        let surfaces = surfaces::CenterSurfaces::new(&area, owner.clone(), cx);
        Self::configure_default_layout(&area, &sessions, sidebar_defaults, window, cx);
        let path = config_path.with_file_name("native-panels.json");
        let save = Self::layout_writer(path.clone(), state_key.clone(), cx);
        let (focus, mut subscriptions) = Self::subscribe_layout(&area, window, cx);
        if let Some(owner) = owner.upgrade() {
            subscriptions.push(cx.observe(&owner, |_, _, cx| cx.notify()));
        }
        subscriptions.push(Self::observe_browser_pages(&browser.session, window, cx));
        Self::restore_layout(path, state_key, legacy_key, window, cx);
        Self {
            owner,
            panels,
            retained_panels: Vec::new(),
            surfaces,
            centers: std::collections::BTreeMap::new(),
            center_task: None,
            restored_agent_destination: None,
            document_context,
            area,
            titlebar,
            terminal: terminal_panel,
            attachment,
            focused_group: None,
            target,
            sender: open_sender,
            restoring: Some(RestoreRequests::default()),
            focus,
            error: None,
            empty_terminal: None,
            agents,
            sessions,
            browser,
            documents,
            document_factory,
            present: true,
            save,
            _subscriptions: subscriptions,
        }
    }

    fn observe_browser_pages(
        browser: &Entity<crate::gpui_browser_panel::BrowserPanel>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            browser,
            window,
            |_, _, _: &crate::gpui_browser_panel::BrowserPagesChanged, window, cx| {
                cx.defer_in(window, |this, window, cx| {
                    this.sync_browser_pages(window, cx);
                });
            },
        )
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
            area.set_center(DockLayout::tabs(), window, cx);
            area.set_dock(DockPlacement::Right, layout, window, cx);
            area.set_dock_size(
                DockPlacement::Right,
                gpui_kit::px(f32::from(window.rem_size()) * 20.0),
                window,
                cx,
            );
            area.toggle_dock(DockPlacement::Right, window, cx);
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
        let focus_in = cx.on_focus_in(&focus, window, |this, _, cx| {
            this.browser
                .session
                .update(cx, crate::gpui_browser_panel::BrowserPanel::focus_host);
            cx.emit(DockFocusChanged);
        });
        let focus_out = cx.on_focus_out(&focus, window, |_, _, _, cx| cx.emit(DockFocusChanged));
        let subscription = cx.subscribe(area, |this, _, event: &DockEvent, cx| {
            if matches!(event, DockEvent::LayoutChanged) {
                this.layout_changed(cx);
            }
        });
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
                match pending.panel {
                    Some(InspectorPanel::Changes) => this.refresh(window, cx),
                    Some(InspectorPanel::Files) => this.show_files(window, cx),
                    Some(InspectorPanel::Agents) => this.show_agents(window, cx),
                    None => {}
                }
                for request in pending.commands {
                    this.apply_request(request, window, cx);
                }
                for request in pending.browser {
                    this.execute_browser(request, window, cx);
                }
                for (path, line, column) in pending.documents {
                    this.open_document(path, line, column, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn set_context(
        &mut self,
        context: GitPanelContext,
        local_git: Option<bootty_git::GitFactsCache>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Selection reconciliation retries after startup restoration; a content change
        // must not discard a saved layout that is still being read.
        if self.restoring.is_some() || self.panels.context == context {
            return;
        }
        for document in self.documents() {
            document.update(cx, |document, cx| document.set_host_visible(false, cx));
        }
        let saved = self.saved_layout(cx);
        let next = match self
            .retained_panels
            .iter()
            .position(|panels| panels.context == context)
        {
            Some(index) => self.retained_panels.remove(index),
            None => ContextPanels::new(context.clone(), self.sender.clone(), local_git, window, cx),
        };
        let previous = std::mem::replace(&mut self.panels, next);
        previous
            .changes
            .update(cx, |changes, _| changes.set_host_visible(false));
        if previous.has_pending_work(cx) {
            self.retained_panels.push(previous);
        }
        self.retained_panels
            .retain(|panels| panels.has_pending_work(cx));
        self.target = context.target.clone();
        *self.document_context.borrow_mut() = context;
        self.terminal.update(cx, |terminal, _| {
            terminal.set_binding_target(self.target.clone());
        });
        self.panels.register(&self.area, cx);
        self.load_layout(saved, window, cx);
        self.resume(cx);
        self.panels
            .files
            .update(cx, |files, cx| files.refresh(window, cx));
        self.panels
            .changes
            .update(cx, |changes, cx| changes.refresh(window, cx));
        cx.notify();
    }

    pub(crate) fn browse_repository(
        &mut self,
        directory: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.panels
            .changes
            .update(cx, |changes, cx| changes.browse(directory, window, cx));
        self.refresh(window, cx);
    }

    pub(crate) fn matches_target(
        &self,
        target: &CommandTarget,
        terminal: Option<&CommandTarget>,
    ) -> bool {
        self.target == *target && self.panels.context.terminal.as_ref() == terminal
    }

    fn request_document(&mut self, path: String, window: &Window, cx: &Context<Self>) {
        use bootty_control::{Caller, CommandInvocation};
        let mut invocation = CommandInvocation::from_action("files.open", Caller::Internal);
        invocation.target = Some(self.target.clone());
        invocation.arguments = vec![path];
        self.submit_command(invocation, window, cx);
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
        let receiver = match self.sender.submit(
            invocation,
            std::time::Instant::now()
                .checked_add(std::time::Duration::from_mins(2))
                .unwrap_or_else(std::time::Instant::now),
            bootty_control::CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.error = Some(format!("Command unavailable: {error:?}"));
                return;
            }
        };
        cx.spawn_in(window, async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, _, cx| {
                this.error = result.map_or_else(
                    |error| Some(error.to_string()),
                    |outcome| crate::commands::command_outcome_message(&outcome),
                );
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn documents(&self) -> Vec<Entity<DocumentPanel>> {
        self.documents
            .borrow()
            .iter()
            .filter_map(gpui_kit::WeakEntity::upgrade)
            .collect()
    }

    pub(crate) fn update_agents(
        &self,
        entries: Vec<crate::state::agent_attention::AgentOverview>,
        selected: Option<CommandTarget>,
        cx: &mut Context<Self>,
    ) {
        self.agents.update(cx, |agents, cx| {
            agents.update_entries(entries, selected, cx);
        });
    }

    pub(crate) fn show_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = &mut self.restoring {
            pending.panel = Some(InspectorPanel::Agents);
            return;
        }
        self.show_sidebar(window, cx);
    }

    pub(crate) fn browse_files(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.panels
            .files
            .update(cx, |files, cx| files.browse(path, window, cx));
        self.show_files(window, cx);
    }

    pub(crate) fn show_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = &mut self.restoring {
            pending.panel = Some(InspectorPanel::Files);
            return;
        }
        self.refresh_tool(bootty_config::config::PanelKind::Files, window, cx);
        self.show_tool(bootty_config::config::PanelKind::Files, window, cx);
    }

    fn refresh_tool(
        &mut self,
        kind: bootty_config::config::PanelKind,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        use bootty_config::config::PanelKind;
        match kind {
            PanelKind::Files => self
                .panels
                .files
                .update(cx, |files, cx| files.refresh(window, cx)),
            PanelKind::Changes => {
                self.resume(cx);
                self.panels
                    .changes
                    .update(cx, |changes, cx| changes.refresh(window, cx));
            }
            _ => {}
        }
    }

    fn show_diff(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tool(bootty_config::config::PanelKind::Diff, window, cx);
    }

    /// Documents live in the right dock, tabbed with the inspector panels. The terminal
    /// center is locked: documents never split it and never join its tab group.
    fn add_document_panel(
        &self,
        panel: Arc<dyn BasePanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = panel.panel_id(cx);
        let existing_placement = panel_placement_in_area(self.area.read(cx), id);
        if let Some(placement) = existing_placement {
            self.area.update(cx, |area, cx| {
                if placement != DockPlacement::Center && !area.is_dock_open(placement) {
                    area.toggle_dock(placement, window, cx);
                }
            });
            return;
        }
        self.area.update(cx, |area, cx| {
            area.add_panel_view(panel, DockPlacement::Right, None, window, cx);
            area.select_panel(id, window, cx);
            if !area.is_dock_open(DockPlacement::Right) {
                area.toggle_dock(DockPlacement::Right, window, cx);
            }
        });
    }

    pub(crate) fn open_document(
        &mut self,
        path: String,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pending) = &mut self.restoring {
            pending.documents.push((path, line, column));
            return;
        }
        let panel = (self.document_factory)(
            &PanelInfo::panel(serde_json::json!({"path":path,"line":line,"column":column})),
            window,
            cx,
        );
        let id = panel.panel_id(cx);
        self.add_document_panel(panel, window, cx);
        if let Some(document) = self
            .documents()
            .into_iter()
            .find(|document| PanelId::from(document.entity_id()) == id)
        {
            self.area.update(cx, |area, cx| {
                area.select_panel(id, window, cx);
            });
            document.update(cx, |document, cx| document.go_to(line, column, window, cx));
        }
    }

    pub(crate) fn resume(&mut self, cx: &mut Context<Self>) {
        self.present = true;
        for document in self.documents() {
            document.update(cx, |document, cx| document.set_host_visible(true, cx));
        }
        self.panels
            .changes
            .update(cx, |changes, _| changes.set_host_visible(true));
    }

    pub(crate) fn attachment_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<crate::gpui_terminal_panel::TerminalAttachmentPanel> {
        if self.has_surface_chooser() {
            return self.attachment.clone();
        }
        let panel = panel_handle(self.attachment.clone());
        let id = panel.panel_id(cx);
        self.area.update(cx, |area, cx| {
            let attached = area
                .layout(DockPlacement::Center)
                .is_some_and(|tree| tree.contains_panel(id) && tree.panels().count() == 1);
            if !attached {
                // Saved center panels cannot create a second topology beside the backend.
                area.set_center(DockLayout::tabs().panel_view(panel, cx), window, cx);
            }
        });
        self.attachment.clone()
    }

    pub(crate) fn clear_terminals(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.area.update(cx, |area, cx| {
            area.remove_panel(self.terminal.clone(), window, cx);
            area.remove_panel(self.attachment.clone(), window, cx);
        });
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

    pub(crate) fn set_inspector_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !visible && let Some(pending) = &mut self.restoring {
            pending.panel = None;
        }
        self.area.update(cx, |area, cx| {
            if area.is_dock_open(DockPlacement::Right) != visible {
                area.toggle_dock(DockPlacement::Right, window, cx);
            }
        });
    }

    /// Legacy saves can strand native panels in the mux-owned center. The
    /// center's leading group then steals the titlebar's primary slot (hiding
    /// the mux window tabs) and the stranded panels sit tab-less beside the
    /// terminal. Move them home; the leaf-count repair in `sync_terminals`
    /// then locks the center to the terminal singleton.
    fn evict_center_natives(&self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal_id = PanelId::from(self.terminal.entity_id());
        if self
            .area
            .read(cx)
            .layout(DockPlacement::Center)
            .is_none_or(|tree| {
                tree.panels()
                    .all(|panel| panel == terminal_id || self.surfaces.contains(panel))
            })
        {
            return;
        }
        let sessions_id = PanelId::from(self.sessions.entity_id());
        // The sidebar is wherever its chrome already lives; a fresh tree has no
        // docked sibling yet, and the sidebar commands dock to the left.
        let sidebar = [
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ]
        .into_iter()
        .find(|placement| {
            self.area
                .read(cx)
                .layout(*placement)
                .is_some_and(|tree| tree.contains_panel(sessions_id))
        })
        .unwrap_or(DockPlacement::Left);
        let attachment = self.attachment.clone();
        let changes = self.panels.changes.clone();
        let diff = self.panels.diff.clone();
        let files = self.panels.files.clone();
        let agents = self.agents.clone();
        let sessions = self.sessions.clone();
        let documents = self.documents();
        let browser_pages = self
            .browser
            .pages
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        self.area.update(cx, |area, cx| {
            evict_center_panel(area, attachment, sidebar, window, cx);
            evict_center_panel(area, changes, sidebar, window, cx);
            evict_center_panel(area, diff, sidebar, window, cx);
            evict_center_panel(area, files, sidebar, window, cx);
            evict_center_panel(area, agents, sidebar, window, cx);
            evict_center_panel(area, sessions, sidebar, window, cx);
            for page in browser_pages {
                evict_center_panel(area, page, DockPlacement::Right, window, cx);
            }
            for document in documents {
                evict_center_panel(area, document, sidebar, window, cx);
            }
        });
    }

    pub(crate) fn sync_terminals(
        &mut self,
        windows: &[TerminalWindowPresentation],
        selected: &bootty_mux::workspace::ScopedWindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring.is_some() {
            return;
        }
        let Some(selected_window) = windows.iter().find(|candidate| &candidate.id == selected)
        else {
            self.clear_terminals(window, cx);
            return;
        };
        self.area.update(cx, |area, cx| {
            area.remove_panel(self.attachment.clone(), window, cx);
        });
        let terminal_center = self.center_task.as_ref().is_some_and(|key| {
            serde_json::from_str::<Vec<String>>(key)
                .is_ok_and(|parts| parts.get(2).is_some_and(|kind| kind == "window"))
        });
        if self.has_surface_chooser() || !terminal_center {
            return;
        }
        let tree = self.area.read(cx).layout(DockPlacement::Center);
        let independent = self.surfaces.terminals.borrow().values().any(|surface| {
            tree.is_some_and(|tree| tree.contains_panel(PanelId::from(surface.entity_id())))
                && surface
                    .read(cx)
                    .terminal_view()
                    .is_some_and(|panel| panel.read(cx).window_id == *selected)
        });
        if independent {
            self.area.update(cx, |area, cx| {
                area.remove_panel(self.terminal.clone(), window, cx);
            });
            return;
        }
        self.terminal.update(cx, |panel, cx| {
            panel.set_window_id(selected_window.id.clone(), cx);
            panel.set_title(selected_window.title.clone(), cx);
            panel.set_visible(true, cx);
        });
        // Stale saves can also strand native panels in the mux-owned center; move
        // them home before counting leaves so the repair below sees a clean tree.
        self.evict_center_natives(window, cx);
        // Only stale persisted saves (a missing or duplicated terminal leaf) trigger a
        // repair; otherwise the saved geometry is untouched.
        let id = PanelId::from(self.terminal.entity_id());
        let leaves = self
            .area
            .read(cx)
            .layout(DockPlacement::Center)
            .map_or(0, |tree| tree.panels().filter(|panel| *panel == id).count());
        if leaves != 1 {
            let leaf =
                crate::workspace_composition::terminal_leaf_state(selected, &selected_window.title);
            self.area.update(cx, |area, cx| {
                let mut state = area.dump(cx);
                state.center =
                    crate::workspace_composition::replace_terminal_region(state.center, leaf);
                if let Err(error) = area.load(state, window, cx) {
                    self.error = Some(error.to_string());
                }
            });
        }
    }

    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = &mut self.restoring {
            pending.panel = Some(InspectorPanel::Changes);
            return;
        }
        self.refresh_tool(bootty_config::config::PanelKind::Changes, window, cx);
        self.show_tool(bootty_config::config::PanelKind::Changes, window, cx);
    }
}

impl WorkspaceDock {
    fn tool_panel(&self, kind: bootty_config::config::PanelKind) -> Arc<dyn BasePanelView> {
        use bootty_config::config::PanelKind;
        match kind {
            PanelKind::Sessions => panel_handle(self.sessions.clone()),
            PanelKind::Files => panel_handle(self.panels.files.clone()),
            PanelKind::Changes => panel_handle(self.panels.changes.clone()),
            PanelKind::Diff => panel_handle(self.panels.diff.clone()),
            PanelKind::Agents => panel_handle(self.agents.clone()),
        }
    }

    fn remove_tool(
        &self,
        kind: bootty_config::config::PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use bootty_config::config::PanelKind;
        self.area.update(cx, |area, cx| match kind {
            PanelKind::Sessions => area.remove_panel(self.sessions.clone(), window, cx),
            PanelKind::Files => area.remove_panel(self.panels.files.clone(), window, cx),
            PanelKind::Changes => area.remove_panel(self.panels.changes.clone(), window, cx),
            PanelKind::Diff => area.remove_panel(self.panels.diff.clone(), window, cx),
            PanelKind::Agents => area.remove_panel(self.agents.clone(), window, cx),
        });
    }

    pub(crate) fn panel_present(&self, kind: bootty_config::config::PanelKind, cx: &App) -> bool {
        let id = self.tool_panel(kind).panel_id(cx);
        self.area
            .read(cx)
            .layout(DockPlacement::Right)
            .is_some_and(|tree| tree.find_panel_node(id).is_some())
    }

    pub(crate) fn panel_visible(&self, kind: bootty_config::config::PanelKind, cx: &App) -> bool {
        let id = self.tool_panel(kind).panel_id(cx);
        let area = self.area.read(cx);
        [
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ]
        .into_iter()
        .any(|placement| {
            area.is_dock_open(placement)
                && area.layout(placement).is_some_and(|tree| {
                    tree.find_panel_node(id)
                        .and_then(|node| tree.find_node(node))
                        .is_some_and(|node| match node.kind() {
                            PaneRef::Tabs { panels, active_ix } => {
                                panels.get(active_ix) == Some(&id)
                            }
                            PaneRef::Split { .. } => false,
                        })
                })
        })
    }

    fn show_tool(
        &self,
        kind: bootty_config::config::PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.tool_panel(kind);
        let id = panel.panel_id(cx);
        let home = if kind == bootty_config::config::PanelKind::Sessions {
            DockPlacement::Left
        } else {
            DockPlacement::Right
        };
        self.area.update(cx, |area, cx| {
            if area.panel(id).is_none() {
                area.add_panel_view(panel, home, None, window, cx);
            }
            for placement in [
                DockPlacement::Left,
                DockPlacement::Right,
                DockPlacement::Bottom,
            ] {
                let node = area
                    .layout(placement)
                    .and_then(|tree| tree.find_panel_node(id));
                if let Some(node) = node {
                    let ix = panel_tab_index(area, placement, node, id);
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
                    if !area.is_dock_open(placement) {
                        area.toggle_dock(placement, window, cx);
                    }
                    break;
                }
            }
        });
    }

    fn show_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tool(bootty_config::config::PanelKind::Sessions, window, cx);
    }

    /// Layout edits before the saved layout has been read are startup noise (window frame
    /// restoration, first-frame measurements); persisting them would clobber the real save.
    fn layout_changed(&self, cx: &mut Context<Self>) {
        // Native child visibility is reconciled by the workspace render, not the dock area.
        cx.notify();
        if self.restoring.is_some() || !self.present {
            return;
        }
        self.save_layout(cx);
    }

    fn document_factory(
        current_document_context: Rc<RefCell<GitPanelContext>>,
        document_list: Rc<RefCell<Vec<gpui_kit::WeakEntity<DocumentPanel>>>>,
        sender: BoundAppCommandSender,
        dock: gpui_kit::WeakEntity<Self>,
    ) -> PanelFactory {
        Rc::new(move |info, window, cx| {
            let context = current_document_context.borrow().clone();
            let state = match info {
                PanelInfo::Panel(state) => state.clone(),
                _ => serde_json::Value::Null,
            };
            let path = state
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let host = state
                .get("host")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&context.host_identity)
                .to_owned();
            document_list
                .borrow_mut()
                .retain(|document| document.upgrade().is_some());
            if let Some(panel) = document_list
                .borrow()
                .iter()
                .filter_map(gpui_kit::WeakEntity::upgrade)
                .find(|panel| {
                    panel.read(cx).path() == path && panel.read(cx).host_identity() == host
                })
            {
                return panel_handle(panel);
            }
            let line = state
                .get("line")
                .and_then(serde_json::Value::as_u64)
                .and_then(|line| u32::try_from(line).ok())
                .unwrap_or(1);
            let column = state
                .get("column")
                .and_then(serde_json::Value::as_u64)
                .and_then(|column| u32::try_from(column).ok())
                .unwrap_or(1);
            let panel = cx.new(|cx| {
                DocumentPanel::new(
                    context.clone(),
                    host,
                    path,
                    sender.clone(),
                    (line, column),
                    window,
                    cx,
                )
            });
            panel.update(cx, |panel, _| {
                panel.set_preview(
                    state
                        .get("preview")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                );
            });
            let owner = dock.clone();
            let subscription = cx.subscribe(&panel, move |_, event: &PanelEvent, cx| {
                if matches!(event, PanelEvent::LayoutChanged) {
                    _ = owner.update(cx, |this, cx| this.layout_changed(cx));
                }
            });
            panel.update(cx, |panel, _| panel.retain_subscription(subscription));
            let owner = dock.clone();
            let window_handle = window.window_handle();
            let closed = cx.subscribe(&panel, move |panel, _: &DocumentClosed, cx| {
                _ = cx.update_window(window_handle, |_, window, cx| {
                    _ = owner.update(cx, |this, cx| {
                        this.area
                            .update(cx, |area, cx| area.remove_panel(panel, window, cx));
                        cx.defer_in(window, |this, window, cx| {
                            this.terminal.update(cx, |terminal, cx| {
                                terminal.focus_terminal(window, cx);
                            });
                        });
                    });
                });
            });
            panel.update(cx, |panel, _| panel.retain_subscription(closed));
            document_list.borrow_mut().push(panel.downgrade());
            panel_handle(panel)
        })
    }

    fn load_layout(&mut self, saved: SavedLayout, window: &mut Window, cx: &mut Context<Self>) {
        if self.restoring.is_some() {
            self.restored_agent_destination = saved
                .active_center
                .as_deref()
                .and_then(crate::workspace_composition::restored_agent_center_destination);
        }
        self.centers = saved
            .centers
            .into_iter()
            .map(|(key, center)| {
                let mut layout = saved.layout.clone();
                layout.center = center;
                (
                    key,
                    crate::workspace_composition::fixed_panel_layout(layout).center,
                )
            })
            .collect();
        self.center_task = saved.active_center;
        let mut layout = crate::workspace_composition::fixed_panel_layout(saved.layout);
        if let Some(key) = &self.center_task {
            self.centers.insert(key.clone(), layout.center.clone());
        }
        let legacy = self
            .centers
            .keys()
            .filter_map(|key| {
                let parts = serde_json::from_str::<Vec<String>>(key).ok()?;
                let [binding, task] = parts.as_slice() else {
                    return None;
                };
                Some((key.clone(), binding.clone(), task.clone()))
            })
            .collect::<Vec<_>>();
        for (key, binding, task) in legacy {
            let tabs = self.centers.get_mut(&key).map_or_default(|center| {
                crate::workspace_composition::take_legacy_surface_tabs(center, &binding, &task)
            });
            for (key, center) in tabs {
                self.centers.entry(key).or_insert(center);
            }
        }
        if let Some(key) = &self.center_task
            && let Some(center) = self.centers.get(key)
        {
            layout.center = center.clone();
        }
        layout.version = self.area.read(cx).version();
        self.error = self
            .area
            .update(cx, |area, cx| area.load(layout, window, cx))
            .err()
            .map(|error| error.to_string());
    }

    pub(crate) fn sync_task_center(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_surface_chooser() || self.restored_agent_destination.is_some() {
            return;
        }
        self.select_center(key, window, cx);
    }

    pub(crate) const fn take_restored_agent_destination(
        &mut self,
    ) -> Option<(String, String, String)> {
        self.restored_agent_destination.take()
    }

    pub(crate) fn cancel_restored_agent_destination(&mut self) {
        self.restored_agent_destination = None;
    }

    fn select_center(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.restoring.is_some() || self.center_task.as_ref() == Some(&key) {
            return;
        }
        if self.center_task.is_none() {
            let initial_terminal = serde_json::from_str::<Vec<String>>(&key)
                .is_ok_and(|parts| parts.get(2).is_some_and(|kind| kind == "window"));
            if initial_terminal && !self.centers.contains_key(&key) {
                self.center_task = Some(key);
                return;
            }
        }
        let mut layout =
            crate::workspace_composition::fixed_panel_layout(self.area.read(cx).dump(cx));
        if let Some(previous) = &self.center_task {
            self.centers.insert(previous.clone(), layout.center.clone());
        }
        // A task-wide legacy layout belongs to one outer tab, never a copy on every window.
        if !self.centers.contains_key(&key)
            && let Ok(parts) = serde_json::from_str::<Vec<String>>(&key)
            && let [binding, task, kind, _] = parts.as_slice()
            && kind == "window"
        {
            let legacy = serde_json::json!([binding, task]).to_string();
            if let Some(center) = self.centers.remove(&legacy) {
                self.centers.insert(key.clone(), center);
            }
        }
        layout.center = self.centers.get(&key).cloned().unwrap_or_else(|| {
            gpui_kit::component::dock::PanelState {
                panel_name: "TabPanel".to_owned(),
                children: Vec::new(),
                info: PanelInfo::tabs(0),
            }
        });
        self.center_task = Some(key);
        self.error = self
            .area
            .update(cx, |area, cx| area.load(layout, window, cx))
            .err()
            .map(|error| error.to_string());
    }

    fn rename_center(&mut self, key: String, cx: &App) {
        if let Some(previous) = self.center_task.replace(key.clone()) {
            self.centers.remove(&previous);
        }
        self.centers.insert(key, self.area.read(cx).dump(cx).center);
    }

    pub(crate) fn terminal_center_location(&self) -> Option<(String, String)> {
        let parts = serde_json::from_str::<Vec<String>>(self.center_task.as_ref()?).ok()?;
        let [_binding, task, kind, id] = parts.as_slice() else {
            return None;
        };
        (kind == "window").then(|| (task.clone(), id.clone()))
    }

    fn save_layout(&self, cx: &App) {
        self.save.send(self.saved_layout(cx));
    }

    fn saved_layout(&self, cx: &App) -> SavedLayout {
        let area = self.area.read(cx);
        let layout = crate::workspace_composition::fixed_panel_layout(area.dump(cx));
        let mut centers = self.centers.clone();
        if let Some(key) = &self.center_task {
            centers.insert(key.clone(), layout.center.clone());
        }
        SavedLayout {
            layout,
            centers,
            active_center: self.center_task.clone(),
        }
    }

    pub(crate) fn close_tool_tab(
        &mut self,
        id: PanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(page) = self.browser.page_for_panel(id) {
            self.submit_command(
                bootty_control::CommandInvocation::new(
                    "browser.close_tab",
                    vec![page.to_string()],
                    bootty_control::Caller::Internal,
                ),
                window,
                cx,
            );
            return true;
        }
        if let Some(kind) = bootty_config::config::PanelKind::ALL
            .into_iter()
            .find(|kind| self.tool_panel(*kind).panel_id(cx) == id)
        {
            self.remove_tool(kind, window, cx);
            self.layout_changed(cx);
        } else {
            return false;
        }
        // Closing a focused panel must not leave keyboard input in a detached subtree.
        cx.defer_in(window, |this, window, cx| {
            this.terminal.update(cx, |terminal, cx| {
                terminal.focus_terminal(window, cx);
            });
        });
        true
    }

    pub(crate) fn toggle_dock(
        &self,
        placement: DockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.area.read(cx).has_dock(placement) {
            self.area
                .update(cx, |area, cx| area.toggle_dock(placement, window, cx));
        } else {
            self.area.update(cx, |area, cx| {
                area.set_dock(placement, DockLayout::tabs(), window, cx);
            });
        }
    }

    pub(crate) fn sessions_focused(&self, window: &Window, cx: &App) -> bool {
        self.sessions
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
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
        &mut self,
        request: &crate::commands::DockRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bootty_control::CommandOutcome {
        let action = if let crate::commands::DockAction::TogglePanel(kind) = request.action {
            if self.panel_visible(kind, cx) {
                self.remove_tool(kind, window, cx);
                return bootty_control::CommandOutcome::success();
            }
            crate::commands::DockAction::show_panel(kind)
        } else {
            request.action
        };
        let area = self.area.read(cx);
        let existing = action.panel().and_then(|kind| {
            let id = self.tool_panel(kind).panel_id(cx);
            let placement = panel_placement_in_area(area, id)?;
            area.layout(placement)?.find_panel_node(id)
        });
        let node = if let Some(id) = request.group {
            let node = group_paths(self.area.read(cx))
                .into_iter()
                .find_map(|(_, node)| (node.as_u64() == id).then_some(node));
            if node.is_none() {
                return bootty_control::CommandOutcome::StaleTarget {
                    message: "The requested tab group no longer exists.".into(),
                };
            }
            node
        } else {
            existing
        };
        if (action.panel().is_some()
            || matches!(
                action,
                DockAction::ToggleTabBar | DockAction::ToggleHiddenTabs
            ))
            && node.is_some_and(|node| {
                self.area
                    .read(cx)
                    .layout(DockPlacement::Center)
                    .is_some_and(|tree| tree.find_node(node).is_some())
            })
        {
            return bootty_control::CommandOutcome::Unavailable {
                message: "The terminal tab group is reserved for the active mux window.".into(),
            };
        }
        if let Some(directory) = request.directory.clone() {
            match action {
                DockAction::Files => self
                    .panels
                    .files
                    .update(cx, |files, cx| files.browse(directory, window, cx)),
                DockAction::Changes => self
                    .panels
                    .changes
                    .update(cx, |changes, cx| changes.browse(directory, window, cx)),
                _ => {}
            }
        }
        match action {
            DockAction::ToggleLeft => self.toggle_dock(DockPlacement::Left, window, cx),
            DockAction::ToggleRight => self.toggle_dock(DockPlacement::Right, window, cx),
            DockAction::ToggleTabBar | DockAction::ToggleHiddenTabs => {
                return bootty_control::CommandOutcome::Unavailable {
                    message: "Tool tabs always remain visible.".into(),
                };
            }
            DockAction::Sidebar | DockAction::Spaces => self.show_sidebar(window, cx),
            DockAction::Files => self.show_files(window, cx),
            DockAction::Changes => self.refresh(window, cx),
            DockAction::Diff => self.show_diff(window, cx),
            DockAction::Agents | DockAction::CodexBar => self.show_agents(window, cx),
            DockAction::TogglePanel(_) => {
                return bootty_control::CommandOutcome::Unavailable {
                    message: "The panel action could not be resolved.".into(),
                };
            }
        }
        bootty_control::CommandOutcome::success()
    }
}

impl Render for WorkspaceDock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_browser_visibility(cx);
        div()
            .track_focus(&self.focus)
            // GPUI focus alone does not change a visible native child view's first responder.
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| {
                this.browser
                    .session
                    .update(cx, crate::gpui_browser_panel::BrowserPanel::focus_host);
            }))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .children(
                self.error.clone().map(|error| {
                    gpui_kit::component::alert::Alert::error("dock-save-error", error)
                }),
            )
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

/// Remove before adding so a panel never belongs to two trees.
fn evict_center_panel<P: Panel>(
    area: &mut DockArea,
    panel: Entity<P>,
    sidebar: DockPlacement,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    let id = PanelId::from(panel.entity_id());
    if !area
        .layout(DockPlacement::Center)
        .is_some_and(|tree| tree.contains_panel(id))
    {
        return;
    }
    let name = area.panel(id).map_or("", |view| view.panel_name(cx));
    let home = crate::workspace_composition::center_eviction_home(name, sidebar);
    area.remove_panel(panel.clone(), window, cx);
    area.add_panel_view(panel_handle(panel), home, None, window, cx);
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

fn panel_placement_in_area(area: &DockArea, id: PanelId) -> Option<DockPlacement> {
    [
        DockPlacement::Center,
        DockPlacement::Left,
        DockPlacement::Right,
        DockPlacement::Bottom,
    ]
    .into_iter()
    .find(|placement| {
        area.layout(*placement)
            .is_some_and(|tree| tree.contains_panel(id))
    })
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
