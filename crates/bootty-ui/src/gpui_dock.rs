//! Native tool-panel composition and presentation-only layout persistence.

use crate::commands::DockAction;

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc},
};

use bootty_control::{BoundAppCommandSender, CommandTarget};
use gpui_kit::component::{
    dock::{
        BasePanel, BasePanelView, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement,
        InsertTarget, NodeId, PaneNode, PaneRef, Panel, PanelEvent, PanelId, PanelInfo,
        panel_handle, register_panel,
    },
    menu::{PopupMenu, PopupMenuItem},
};
use gpui_kit::{
    App, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, Global, IntoElement,
    ParentElement, Render, Styled, Subscription, Window, div, prelude::*,
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

type PanelFactory = Rc<dyn Fn(&PanelInfo, &mut Window, &mut App) -> Arc<dyn BasePanelView>>;

#[derive(Clone)]
pub struct TerminalWindowPresentation {
    pub(crate) id: bootty_mux::workspace::ScopedWindowId,
    pub(crate) title: String,
}
#[derive(Default)]
struct NativePanels(HashMap<(EntityId, String), PanelFactory>);
impl Global for NativePanels {}

fn register_factory(area: &Entity<DockArea>, name: &str, factory: PanelFactory, cx: &mut App) {
    if cx.try_global::<NativePanels>().is_none() {
        cx.set_global(NativePanels::default());
    }
    cx.global_mut::<NativePanels>()
        .0
        .insert((area.entity_id(), name.to_owned()), factory);
    let name = name.to_owned();
    register_panel(cx, &name.clone(), move |context, window, cx| {
        let factory = cx
            .global::<NativePanels>()
            .0
            .get(&(context.dock_area().entity_id(), name.clone()))
            .cloned();
        if let Some(factory) = factory {
            factory(context.info(), window, cx)
        } else {
            let state = context.state().clone();
            panel_handle(cx.new(|cx| UnavailablePanel {
                state,
                focus: cx.focus_handle(),
            }))
        }
    });
}
struct UnavailablePanel {
    state: gpui_kit::component::dock::PanelState,
    focus: FocusHandle,
}
impl BasePanel for UnavailablePanel {
    fn panel_name(&self) -> &'static str {
        "bootty.unavailable"
    }
    fn dump(&self, _: &App) -> gpui_kit::component::dock::PanelState {
        self.state.clone()
    }
}
impl Panel for UnavailablePanel {}
impl EventEmitter<PanelEvent> for UnavailablePanel {}
impl Focusable for UnavailablePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for UnavailablePanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_2().child(format!(
            "The {} panel is unavailable in this workspace.",
            self.state.panel_name
        ))
    }
}

fn register<P: Panel>(area: &Entity<DockArea>, panel: Entity<P>, cx: &mut App) {
    let handle = panel_handle(panel);
    let name = handle.panel_name(cx);
    register_factory(area, name, Rc::new(move |_, _, _| handle.clone()), cx);
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedLayout {
    #[serde(flatten)]
    layout: DockAreaState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    always_show_tabs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    always_hide_tabs: Vec<String>,
}

struct LayoutSave {
    path: PathBuf,
    key: String,
    state: SavedLayout,
    result: async_channel::Sender<Result<(), String>>,
}

struct LayoutWriter(mpsc::Sender<LayoutSave>);
impl Global for LayoutWriter {}

impl LayoutWriter {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<LayoutSave>();
        std::thread::spawn(move || {
            while let Ok(first) = receiver.recv() {
                let mut pending = BTreeMap::new();
                for save in std::iter::once(first).chain(receiver.try_iter()) {
                    pending.insert((save.path.clone(), save.key.clone()), save);
                }
                for save in pending.into_values() {
                    let result = save_layout(&save.path, &save.key, save.state)
                        .map_err(|error| error.to_string());
                    // A closed Dock must not stop saves for other windows or Spaces.
                    let _ = save.result.send_blocking(result);
                }
            }
        });
        Self(sender)
    }
}

struct LayoutSaveHandle {
    sender: mpsc::Sender<LayoutSave>,
    path: PathBuf,
    key: String,
    result: async_channel::Sender<Result<(), String>>,
}
impl LayoutSaveHandle {
    fn send(&self, state: SavedLayout) {
        if self
            .sender
            .send(LayoutSave {
                path: self.path.clone(),
                key: self.key.clone(),
                state,
                result: self.result.clone(),
            })
            .is_err()
        {
            let _ = self
                .result
                .try_send(Err("Layout writer is unavailable".to_owned()));
        }
    }
}

pub struct DockFocusChanged;

#[derive(Clone, Copy)]
enum InspectorPanel {
    Changes,
    Files,
    Agents,
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
                activate(
                    this.panels.diff.read(cx).group.clone(),
                    this.panels.diff.entity_id(),
                    window,
                    cx,
                );
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
    area: Entity<DockArea>,
    panels: ContextPanels,
    retained_panels: Vec<ContextPanels>,
    document_context: Rc<RefCell<GitPanelContext>>,
    pub(crate) titlebar: Entity<crate::gpui_dock_skin::WorkspaceTitleBar>,
    panel_preferences:
        BTreeMap<bootty_config::config::PanelKind, bootty_config::config::PanelConfig>,
    pub(crate) always_show_tabs: Rc<RefCell<HashSet<NodeId>>>,
    pub(crate) always_hide_tabs: Rc<RefCell<HashSet<NodeId>>>,
    /// The single locked terminal leaf. The mux owns the window and every split inside it;
    /// Dock never adds, removes, or relocates this panel after its first placement.
    terminal: Entity<crate::gpui_terminal_panel::TerminalPanel>,
    attachment: Entity<crate::gpui_terminal_panel::TerminalAttachmentPanel>,
    focused_group: Option<NodeId>,
    target: CommandTarget,
    sender: BoundAppCommandSender,
    restoring: bool,
    requested_panel: Option<InspectorPanel>,
    pending_commands: Vec<crate::commands::DockRequest>,
    pending_documents: Vec<(String, u32, u32)>,
    focus: FocusHandle,
    error: Option<String>,
    pub(crate) empty_terminal: Option<(NodeId, crate::workspace_composition::EmptyTerminalState)>,
    agents: Entity<crate::gpui_agents_panel::AgentsPanel>,
    sessions: Entity<crate::gpui_sidebar_panel::SessionsPanel>,
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
        local_git: Option<bootty_git::GitFactsCache>,
        panel_preferences: BTreeMap<
            bootty_config::config::PanelKind,
            bootty_config::config::PanelConfig,
        >,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let legacy_key = format!(
            "{state_key}:space:{}:host:{}",
            scope.persistence_value(),
            context.host_identity
        );
        let dock_owner = cx.weak_entity();
        let always_show_tabs = Rc::new(RefCell::new(HashSet::new()));
        let always_hide_tabs = Rc::new(RefCell::new(HashSet::new()));
        let area = cx.new(|cx| {
            let skin = crate::gpui_dock_skin::WorkspaceDockSkin::new(
                dock_owner.clone(),
                chrome.clone(),
                always_show_tabs.clone(),
                always_hide_tabs.clone(),
                cx,
            );
            DockArea::new("workspace", Some(8), window, cx).with_renderer(skin)
        });
        let titlebar = cx.new(|cx| {
            crate::gpui_dock_skin::WorkspaceTitleBar::new(chrome.clone(), dock_owner, &area, cx)
        });
        let attachment = cx.new(|_| {
            crate::gpui_terminal_panel::TerminalAttachmentPanel::new(terminal, owner.clone())
        });
        register(&area, attachment.clone(), cx);
        // The terminal center is a singleton: the mux owns the window and every split inside
        // it, so Dock restores whichever leaf the save holds onto this one panel.
        let terminal_panel = cx.new(|cx| {
            crate::gpui_terminal_panel::TerminalPanel::new(
                context.target.clone(),
                bootty_mux::workspace::ScopedWindowId::new(scope, String::new(), String::new()),
                "Terminal".to_owned(),
                owner,
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
        Self::configure_default_layout(&area, &panels, &sessions, sidebar_defaults, window, cx);
        let path = config_path.with_file_name("native-panels.json");
        let save = Self::layout_writer(path.clone(), state_key.clone(), cx);
        let (focus, subscriptions) = Self::subscribe_layout(&area, window, cx);
        Self::restore_layout(path, state_key, legacy_key, window, cx);
        Self {
            panels,
            retained_panels: Vec::new(),
            document_context,
            area,
            titlebar,
            // Seeded from the live config so the first settings sync only reacts to real
            // changes instead of relocating every panel out of the restored layout.
            panel_preferences,
            always_show_tabs,
            always_hide_tabs,
            terminal: terminal_panel,
            attachment,
            focused_group: None,
            target,
            sender: open_sender,
            restoring: true,
            requested_panel: None,
            pending_commands: Vec::new(),
            pending_documents: Vec::new(),
            focus,
            error: None,
            empty_terminal: None,
            agents,
            sessions,
            documents,
            document_factory,
            present: true,
            save,
            _subscriptions: subscriptions,
        }
    }

    fn configure_default_layout(
        area: &Entity<DockArea>,
        panels: &ContextPanels,
        sessions: &Entity<crate::gpui_sidebar_panel::SessionsPanel>,
        sidebar_defaults: (crate::gpui::chrome::SidebarPosition, f32, bool),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layout = DockLayout::tabs()
            .panel_view(panel_handle(panels.changes.clone()), cx)
            .panel_view(panel_handle(panels.files.clone()), cx);
        let sidebar_placement = match sidebar_defaults.0 {
            crate::gpui::chrome::SidebarPosition::Left => DockPlacement::Left,
            crate::gpui::chrome::SidebarPosition::Right => DockPlacement::Right,
        };
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
        if cx.try_global::<LayoutWriter>().is_none() {
            cx.set_global(LayoutWriter::new());
        }
        let (result_tx, result_rx) = async_channel::unbounded();
        let save = LayoutSaveHandle {
            sender: cx.global::<LayoutWriter>().0.clone(),
            path,
            key: state_key,
            result: result_tx,
        };
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
        let subscription = cx.subscribe(area, |this, _, event: &DockEvent, cx| {
            if matches!(event, DockEvent::LayoutChanged) {
                this.layout_changed(cx);
            }
        });
        let area_id = area.entity_id();
        cx.on_release(move |_, cx| {
            cx.global_mut::<NativePanels>()
                .0
                .retain(|(id, _), _| *id != area_id);
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
                .spawn(async move {
                    std::fs::read(path)
                        .ok()
                        .and_then(|bytes| {
                            serde_json::from_slice::<BTreeMap<String, SavedLayout>>(&bytes).ok()
                        })
                        .and_then(|mut states| {
                            states
                                .remove(&state_key)
                                .or_else(|| states.remove(&legacy_key))
                                .or_else(|| {
                                    let prefix = format!("{legacy_key}:directory:");
                                    states
                                        .into_iter()
                                        .find(|(key, _)| key.starts_with(&prefix))
                                        .map(|(_, state)| state)
                                })
                        })
                })
                .await;
            _ = weak.update_in(cx, |this, window, cx| {
                let tab_preferences = saved
                    .as_ref()
                    .map(|s| s.always_show_tabs.clone())
                    .unwrap_or_default();
                let hidden_preferences = saved
                    .as_ref()
                    .map(|s| s.always_hide_tabs.clone())
                    .unwrap_or_default();
                let saved = saved.map(|s| s.layout);
                // A save from another schema version is discarded; the area starts from its
                // defaults instead of guessing at a migration.
                if let Some(saved) = saved
                    && saved.version == this.area.read(cx).version()
                {
                    this.error = this
                        .area
                        .update(cx, |area, cx| area.load(saved, window, cx))
                        .err()
                        .map(|error| error.to_string());
                }
                *this.always_show_tabs.borrow_mut() = group_paths(this.area.read(cx))
                    .into_iter()
                    .filter(|(path, _)| tab_preferences.contains(path))
                    .map(|(_, node)| node)
                    .collect();
                *this.always_hide_tabs.borrow_mut() = group_paths(this.area.read(cx))
                    .into_iter()
                    .filter(|(path, _)| hidden_preferences.contains(path))
                    .map(|(_, node)| node)
                    .collect();
                this.restoring = false;
                match this.requested_panel.take() {
                    Some(InspectorPanel::Changes) => this.refresh(window, cx),
                    Some(InspectorPanel::Files) => this.show_files(window, cx),
                    Some(InspectorPanel::Agents) => this.show_agents(window, cx),
                    None => {}
                }
                for request in std::mem::take(&mut this.pending_commands) {
                    this.apply_request(request, window, cx);
                }
                for (path, line, column) in std::mem::take(&mut this.pending_documents) {
                    this.open_document(path, line, column, window, cx);
                }
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
        if self.restoring || self.panels.context == context {
            return;
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
        self.error = self
            .area
            .update(cx, |area, cx| area.load(saved.layout, window, cx))
            .err()
            .map(|error| error.to_string());
        *self.always_show_tabs.borrow_mut() = group_paths(self.area.read(cx))
            .into_iter()
            .filter(|(path, _)| saved.always_show_tabs.contains(path))
            .map(|(_, node)| node)
            .collect();
        *self.always_hide_tabs.borrow_mut() = group_paths(self.area.read(cx))
            .into_iter()
            .filter(|(path, _)| saved.always_hide_tabs.contains(path))
            .map(|(_, node)| node)
            .collect();
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
        if self.restoring {
            self.requested_panel = Some(InspectorPanel::Agents);
            return;
        }
        self.show_tool(bootty_config::config::PanelKind::Agents, window, cx);
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
        if self.restoring {
            self.requested_panel = Some(InspectorPanel::Files);
            return;
        }
        self.panels
            .files
            .update(cx, |files, cx| files.refresh(window, cx));
        self.show_tool(bootty_config::config::PanelKind::Files, window, cx);
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
        let documents = self
            .documents()
            .into_iter()
            .map(|document| PanelId::from(document.entity_id()))
            .chain(std::iter::once(PanelId::from(self.panels.diff.entity_id())))
            .filter(|candidate| *candidate != id)
            .collect::<Vec<_>>();
        self.area.update(cx, |area, cx| {
            let target = area.layout(DockPlacement::Right).and_then(|tree| {
                documents
                    .iter()
                    .find_map(|id| tree.find_panel_node(*id))
                    .map(|node| InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    })
            });
            area.add_panel_view(panel, DockPlacement::Right, None, window, cx);
            if let Some(target) = target {
                area.move_panel(id, target, window, cx);
            }
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
        if self.restoring {
            self.pending_documents.push((path, line, column));
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
            activate(
                document.read(cx).group.clone(),
                document.entity_id(),
                window,
                cx,
            );
            document.update(cx, |document, cx| document.go_to(line, column, window, cx));
        }
    }

    pub(crate) fn resume(&mut self, cx: &mut Context<Self>) {
        self.present = true;
        for document in self.documents() {
            document.update(cx, |document, _| document.set_host_visible(true));
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
        let panel = panel_handle(self.attachment.clone());
        let id = panel.panel_id(cx);
        self.area.update(cx, |area, cx| {
            area.remove_panel(self.terminal.clone(), window, cx);
            if area.panel(id).is_none() {
                area.add_panel_view(panel, DockPlacement::Center, None, window, cx);
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

    pub(crate) fn terminal_panel(&self) -> Entity<crate::gpui_terminal_panel::TerminalPanel> {
        self.terminal.clone()
    }

    pub(crate) fn set_inspector_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !visible {
            self.requested_panel = None;
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
            .is_none_or(|tree| tree.panels().all(|panel| panel == terminal_id))
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
        let home_of =
            |name: &str| crate::workspace_composition::center_eviction_home(name, sidebar);
        // Every panel that can exist in a saved layout except the terminal
        // singleton. Membership is tested by id; the home dock comes from the
        // panel's live registered name, so a rename falls back to the right
        // dock instead of stranding the panel.
        macro_rules! evict {
            ($area:expr, $panel:expr, $window:expr, $cx:expr) => {{
                let id = PanelId::from($panel.entity_id());
                let in_center = $area
                    .layout(DockPlacement::Center)
                    .is_some_and(|tree| tree.contains_panel(id));
                if in_center {
                    let name = $area
                        .panel(id)
                        .map(|view| view.panel_name($cx))
                        .unwrap_or("");
                    relocate_panel($area, $panel, home_of(name), $window, $cx);
                }
            }};
        }
        let attachment = self.attachment.clone();
        let changes = self.panels.changes.clone();
        let diff = self.panels.diff.clone();
        let files = self.panels.files.clone();
        let agents = self.agents.clone();
        let sessions = self.sessions.clone();
        let documents = self.documents();
        self.area.update(cx, |area, cx| {
            evict!(area, attachment, window, cx);
            evict!(area, changes, window, cx);
            evict!(area, diff, window, cx);
            evict!(area, files, window, cx);
            evict!(area, agents, window, cx);
            evict!(area, sessions, window, cx);
            for document in documents {
                evict!(area, document, window, cx);
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
        if self.restoring {
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
        // The terminal center is locked: window switches retarget the singleton leaf
        // instead of rebuilding layout, and mux splits never add or remove Dock panels.
        self.terminal.update(cx, |panel, cx| {
            panel.set_window_id(selected.clone(), cx);
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
        if self.restoring {
            self.requested_panel = Some(InspectorPanel::Changes);
            return;
        }
        self.resume(cx);
        self.panels.changes.update(cx, |changes, cx| {
            changes.set_host_visible(true);
            changes.refresh(window, cx);
        });
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
        self.area.update(cx, |area, cx| {
            match kind {
                PanelKind::Sessions => area.remove_panel(self.sessions.clone(), window, cx),
                PanelKind::Files => area.remove_panel(self.panels.files.clone(), window, cx),
                PanelKind::Changes => area.remove_panel(self.panels.changes.clone(), window, cx),
                PanelKind::Diff => area.remove_panel(self.panels.diff.clone(), window, cx),
                PanelKind::Agents => area.remove_panel(self.agents.clone(), window, cx),
            }
            for placement in [
                DockPlacement::Left,
                DockPlacement::Right,
                DockPlacement::Bottom,
            ] {
                if placement == DockPlacement::Bottom
                    && area
                        .layout(placement)
                        .is_some_and(|tree| tree.panels().next().is_none())
                {
                    // Kit keeps a closed bottom dock's tab strip. An empty one has no tabs to reopen.
                    area.remove_dock(placement, window, cx);
                } else if area.is_dock_open(placement) && area.is_empty(placement, cx) {
                    area.toggle_dock(placement, window, cx);
                }
            }
        });
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
                            PaneRef::Tiles { .. } => true,
                            PaneRef::Split { .. } => false,
                        })
                })
        })
    }

    pub(crate) fn sync_panel_settings(
        &mut self,
        preferences: &BTreeMap<
            bootty_config::config::PanelKind,
            bootty_config::config::PanelConfig,
        >,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring || &self.panel_preferences == preferences {
            return;
        }
        for kind in bootty_config::config::PanelKind::ALL {
            let previous = self
                .panel_preferences
                .get(&kind)
                .copied()
                .unwrap_or_default();
            let next = preferences.get(&kind).copied().unwrap_or_default();
            if previous.dock != next.dock {
                let panel = self.tool_panel(kind);
                if self.area.read(cx).panel(panel.panel_id(cx)).is_some() {
                    let visible = self.panel_visible(kind, cx);
                    self.remove_tool(kind, window, cx);
                    self.area.update(cx, |area, cx| {
                        let placement = panel_placement(next.dock(kind));
                        area.add_panel_view(panel, placement, None, window, cx);
                        if visible && !area.is_dock_open(placement) {
                            area.toggle_dock(placement, window, cx);
                        }
                    });
                }
            }
        }
        self.panel_preferences.clone_from(preferences);
        cx.notify();
    }

    fn show_tool(
        &self,
        kind: bootty_config::config::PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.tool_panel(kind);
        let id = panel.panel_id(cx);
        let home = panel_placement(
            self.panel_preferences
                .get(&kind)
                .copied()
                .unwrap_or_default()
                .dock(kind),
        );
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
    fn layout_changed(&self, cx: &App) {
        if self.restoring || !self.present {
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
                    });
                });
            });
            panel.update(cx, |panel, _| panel.retain_subscription(closed));
            document_list.borrow_mut().push(panel.downgrade());
            panel_handle(panel)
        })
    }

    fn save_layout(&self, cx: &App) {
        self.save.send(self.saved_layout(cx));
    }

    fn saved_layout(&self, cx: &App) -> SavedLayout {
        let area = self.area.read(cx);
        SavedLayout {
            layout: area.dump(cx),
            always_hide_tabs: group_paths(area)
                .into_iter()
                .filter(|(_, node)| self.always_hide_tabs.borrow().contains(node))
                .map(|(path, _)| path)
                .collect(),
            always_show_tabs: group_paths(area)
                .into_iter()
                .filter(|(_, node)| self.always_show_tabs.borrow().contains(node))
                .map(|(path, _)| path)
                .collect(),
        }
    }

    pub(crate) fn set_always_show_tabs(&self, node: NodeId, show: bool, cx: &mut Context<Self>) {
        if show {
            self.always_hide_tabs.borrow_mut().remove(&node);
            self.always_show_tabs.borrow_mut().insert(node);
        } else {
            self.always_show_tabs.borrow_mut().remove(&node);
        }
        self.save_layout(cx);
        self.area.update(cx, |_, cx| cx.notify());
        cx.notify();
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

    pub(crate) fn panel_menu(
        owner: &gpui_kit::WeakEntity<Self>,
        node: NodeId,
        mut menu: PopupMenu,
    ) -> PopupMenu {
        for descriptor in crate::commands::PANELS {
            let crate::commands::PanelCreation::Command(action) = descriptor.creation else {
                continue;
            };
            let owner = owner.clone();
            menu = menu.item(
                PopupMenuItem::new(descriptor.label)
                    .icon(descriptor.icon.clone())
                    .on_click(move |_, window, cx| {
                        _ = owner.update(cx, |this, cx| {
                            this.invoke_action(action, Some(node), window, cx);
                        });
                    }),
            );
        }
        menu
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
        if self.restoring {
            self.pending_commands.push(request);
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
        let existing = match action {
            DockAction::Sidebar | DockAction::Spaces => Some(self.sessions.entity_id()),
            DockAction::Files => Some(self.panels.files.entity_id()),
            DockAction::Changes => Some(self.panels.changes.entity_id()),
            DockAction::Diff => Some(self.panels.diff.entity_id()),
            DockAction::Agents | DockAction::CodexBar => Some(self.agents.entity_id()),
            _ => None,
        }
        .and_then(|id| {
            [
                DockPlacement::Center,
                DockPlacement::Left,
                DockPlacement::Right,
                DockPlacement::Bottom,
            ]
            .into_iter()
            .find_map(|placement| {
                self.area
                    .read(cx)
                    .layout(placement)
                    .and_then(|tree| tree.find_panel_node(PanelId::from(id)))
            })
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
                self.toggle_group_tabs(action, node, cx);
            }
            _ if let Some(node) = node => {
                self.open_panel_at(action, node, window, cx);
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

    fn toggle_group_tabs(&self, action: DockAction, node: Option<NodeId>, cx: &mut Context<Self>) {
        let area = self.area.read(cx);
        let groups = group_paths(area)
            .into_iter()
            .filter(|(_, node)| {
                area.layout(DockPlacement::Center)
                    .is_none_or(|tree| tree.find_node(*node).is_none())
            })
            .collect::<Vec<_>>();
        let focused = self
            .focused_group
            .filter(|node| groups.iter().any(|(_, current)| current == node));
        if let Some(node) = node
            .or(focused)
            .or_else(|| groups.first().map(|(_, node)| *node))
        {
            if action == DockAction::ToggleHiddenTabs {
                let hidden = self.always_hide_tabs.borrow_mut().remove(&node);
                if !hidden {
                    self.always_hide_tabs.borrow_mut().insert(node);
                }
                self.set_always_show_tabs(node, false, cx);
            } else {
                let always = self.always_show_tabs.borrow().contains(&node);
                self.set_always_show_tabs(node, !always, cx);
            }
        }
    }

    fn open_panel_at(
        &mut self,
        action: crate::commands::DockAction,
        node: NodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placement = [
            DockPlacement::Center,
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ]
        .into_iter()
        .find(|placement| {
            self.area
                .read(cx)
                .layout(*placement)
                .is_some_and(|tree| tree.find_node(node).is_some())
        });
        let Some(placement) = placement else {
            return;
        };
        let panel = match action {
            DockAction::Sidebar | DockAction::Spaces => panel_handle(self.sessions.clone()),
            DockAction::Files => {
                self.panels
                    .files
                    .update(cx, |files, cx| files.refresh(window, cx));
                panel_handle(self.panels.files.clone())
            }
            DockAction::Changes => {
                self.resume(cx);
                self.panels.changes.update(cx, |changes, cx| {
                    changes.set_host_visible(true);
                    changes.refresh(window, cx);
                });
                panel_handle(self.panels.changes.clone())
            }
            DockAction::Diff => panel_handle(self.panels.diff.clone()),
            DockAction::Agents | DockAction::CodexBar => panel_handle(self.agents.clone()),
            _ => return,
        };
        let id = panel.panel_id(cx);
        self.area.update(cx, |area, cx| {
            if area.panel(id).is_none() {
                area.add_panel_view(panel, placement, None, window, cx);
            }
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
            if placement != DockPlacement::Center && !area.is_dock_open(placement) {
                area.toggle_dock(placement, window, cx);
            }
        });
    }
}

impl Render for WorkspaceDock {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .children(
                self.error.clone().map(|error| {
                    gpui_kit::component::alert::Alert::error("dock-save-error", error)
                }),
            )
            .child(div().flex_1().min_h_0().min_w_0().child(self.area.clone()))
    }
}

fn save_layout(path: &std::path::Path, key: &str, state: SavedLayout) -> anyhow::Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("layout path has no parent"))?,
    )?;
    let target = bootty_write::WriteTarget::resolve(path)
        .map_err(bootty_write::ResolveTargetError::into_io)?
        .lock()?;
    let mut states: BTreeMap<String, SavedLayout> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(error.into()),
    };
    states.insert(key.to_owned(), state);
    target
        .replace(
            &serde_json::to_vec(&states)?,
            bootty_write::NewFileMode::Private,
        )
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    drop(target);
    Ok(())
}

fn activate(
    group: Option<gpui_kit::WeakEntity<gpui_kit::component::dock::TabGroup>>,
    panel: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(group) = group {
        _ = group.update(cx, |group, cx| {
            if let Some(index) = group
                .panels()
                .iter()
                .position(|candidate| candidate.panel_id(cx) == PanelId::from(panel))
            {
                group.select_tab(index, window, cx);
            }
        });
    }
}

// Paths are serialized with the same layout snapshot; live preferences follow stable node IDs.
/// Move one stranded center panel to its home dock. Remove-then-add, never the
/// reverse: the panel must leave the center tree before the destination adopts
/// it, or it briefly belongs to two trees.
fn relocate_panel<P: Panel + BasePanel>(
    area: &mut DockArea,
    panel: Entity<P>,
    home: DockPlacement,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    area.remove_panel(panel.clone(), window, cx);
    area.add_panel_view(panel_handle(panel), home, None, window, cx);
}

fn group_paths(area: &DockArea) -> Vec<(String, NodeId)> {
    fn visit(node: &PaneNode, path: String, groups: &mut Vec<(String, NodeId)>) {
        match node.kind() {
            PaneRef::Tabs { .. } => groups.push((path, node.id())),
            PaneRef::Split { children, .. } => {
                for (ix, child) in children.iter().enumerate() {
                    visit(child, format!("{path}/{ix}"), groups);
                }
            }
            PaneRef::Tiles { .. } => {}
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

const fn panel_placement(dock: bootty_config::config::PanelDock) -> DockPlacement {
    match dock {
        bootty_config::config::PanelDock::Left => DockPlacement::Left,
        bootty_config::config::PanelDock::Right => DockPlacement::Right,
        bootty_config::config::PanelDock::Bottom => DockPlacement::Bottom,
    }
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
            _ => None,
        })
}
