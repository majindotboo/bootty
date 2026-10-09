use std::collections::HashMap;

use bootty_agents::NativeSessionRecord;
use bootty_control::{Caller, CommandInvocation, CommandTarget, ResourceKind};
use gpui_kit::{App, AppContext, Context, Entity, Focusable, Subscription, Window, prelude::*};

use super::GpuiWorkspace;
use crate::gpui_agent_session::{NativeAgentSessionView, OpenNativeSession};
use crate::surface_creation::{PendingNewSurface, SurfaceParent};

#[derive(Default)]
pub(super) struct NativeConversations {
    records: Vec<NativeSessionRecord>,
    views: HashMap<String, Entity<NativeAgentSessionView>>,
    subscriptions: HashMap<String, Subscription>,
    requested: Option<CommandTarget>,
    reconnect: HashMap<String, crate::presentation::native_reconnect::NativeReconnect>,
    reconnect_pending: HashMap<String, PendingNativeReconnect>,
    annotations_subscription: Option<Subscription>,
    focus_requested: bool,
    pub(super) revision: u64,
    refreshing: bool,
    surface_request: Option<PendingNewSurface>,
    surface_attachment: Option<(PendingNewSurface, CommandTarget)>,
    surface_agent_form: bool,
    created_terminal_focus: Option<CreatedTerminalFocus>,
}

struct CreatedTerminalFocus {
    target: CommandTarget,
    panel: gpui_kit::FocusHandle,
}

struct PendingNativeReconnect {
    response: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
    cancellation: bootty_control::CommandCancellation,
}

impl Drop for PendingNativeReconnect {
    fn drop(&mut self) {
        _ = self.cancellation.cancel();
    }
}

impl GpuiWorkspace {
    pub(super) const fn surface_agent_form_presented(&self) -> bool {
        self.native_conversations.surface_agent_form
    }
    pub(super) fn navigate_surface_chooser(
        &self,
        id: u64,
        action: crate::commands::SurfaceChooserAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(dock) = &self.tools {
            dock.update(cx, |dock, cx| {
                dock.navigate_surface_chooser(id, action, window, cx);
            });
        }
    }

    pub(super) fn open_surface_chooser(
        &mut self,
        mut request: PendingNewSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Capture the mux parent before updating Dock; Dock cannot read its owner
        // while this workspace command holds the entity's update lease.
        if let SurfaceParent::Conversation(target) = &request.parent {
            request.parent = self.native_parent_terminal(target).map_or_else(
                || SurfaceParent::Binding(request.binding.clone()),
                SurfaceParent::Terminal,
            );
        }
        self.native_conversations.surface_request = Some(request.clone());
        self.native_conversations.surface_agent_form = false;
        if let Some(dock) = self.tools.clone() {
            dock.update(cx, |dock, cx| {
                dock.open_surface_chooser(request, window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn open_surface_agent_form(
        &mut self,
        request: &PendingNewSurface,
        cx: &mut Context<Self>,
    ) {
        self.state.open_surface_agent_form(request);
        self.native_conversations.surface_agent_form = true;
        cx.notify();
    }

    pub(super) fn close_surface_chooser(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .native_conversations
            .surface_request
            .as_ref()
            .is_some_and(|request| request.id == id)
        {
            self.native_conversations.surface_request = None;
            self.native_conversations.surface_agent_form = false;
        }
        if let Some(dock) = self.tools.clone() {
            if self
                .native_conversations
                .surface_attachment
                .as_ref()
                .is_some_and(|(request, _)| request.id == id)
            {
                return;
            }
            dock.update(cx, |dock, cx| {
                dock.close_surface_chooser(Some(id), window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn attach_new_surface(
        &mut self,
        id: u64,
        target: CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(request) = self
            .native_conversations
            .surface_request
            .as_ref()
            .filter(|request| request.id == id)
            .cloned()
        else {
            return;
        };
        if target.kind == ResourceKind::Session {
            self.native_conversations.surface_attachment = Some((request, target.clone()));
            self.open_native_conversation(target, window, cx);
        } else {
            self.attach_terminal_surface(&request, &target, window, cx);
        }
        cx.notify();
    }

    fn attach_terminal_surface(
        &mut self,
        request: &PendingNewSurface,
        target: &CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.uses_native_terminal_layout() {
            // Attachment backends paint their real selected window through one view.
            // A second Dock terminal would create topology outside the mux.
            self.close_native_conversation();
            self.close_surface_chooser(request.id, window, cx);
            self.focus_terminal_surface(window, cx);
            return;
        }
        let binding = &self.state.workspace.active.binding;
        let scope = binding.scope();
        let handle = self
            .state
            .binding_target_handle(scope, binding.mux().binding_generation());
        let Some(exact) =
            bootty_mux::target::exact_mux_target(scope, binding.mux(), target, &handle)
        else {
            return;
        };
        let (Some(session), Some(id), _) = exact.ids() else {
            return;
        };
        let window_id = binding.window_id(session.to_owned(), id.to_owned());
        let Some(title) = binding
            .mux()
            .sessions()
            .iter()
            .find(|item| item.id == session)
            .and_then(|item| item.windows.iter().find(|item| item.id == id))
            .map(|item| item.name.clone())
        else {
            return;
        };
        let Some(binding_target) = bootty_mux::target::ExactMuxTarget::Binding(scope)
            .command_target(ResourceKind::Binding, binding.mux(), &handle)
        else {
            return;
        };
        let task = self
            .state
            .workspace
            .session_identity(scope, session)
            .unwrap_or_else(|| request.task_identity.clone());
        let origin = crate::gpui_dock::surfaces::TerminalSurfaceOrigin {
            binding_id: scope.persistence_value().to_string(),
            task_identity: task.clone(),
            window_key: binding.saved_window_key(&task, id),
        };
        self.close_native_conversation();
        if let Some(dock) = self.tools.clone() {
            let focus = dock.update(cx, |dock, cx| {
                dock.attach_window_surface(
                    &origin,
                    binding_target,
                    crate::gpui_dock::TerminalWindowPresentation {
                        id: window_id.clone(),
                        title,
                    },
                    request,
                    window,
                    cx,
                );
                dock.terminal_surface_focus(&window_id, cx)
            });
            if let Some(panel) = focus {
                panel.focus(window, cx);
                self.native_conversations.created_terminal_focus = Some(CreatedTerminalFocus {
                    target: target.clone(),
                    panel,
                });
            }
        }
    }

    fn focus_created_terminal(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(pending) = &self.native_conversations.created_terminal_focus else {
            return;
        };
        let pane = self.created_terminal_pane(pending);
        if !pending.panel.contains_focused(window, cx) || pane.is_none() {
            self.native_conversations.created_terminal_focus = None;
            return;
        }
        let focus = pane
            .as_deref()
            .and_then(|pane| self.terminal_panes.get(&self.state.pane_widget_key(pane)))
            .map(|pane| pane.view.focus_handle(cx));
        if let Some(focus) = focus {
            let pending = self.native_conversations.created_terminal_focus.take();
            cx.defer_in(window, move |this, window, cx| {
                if let Some(pending) = pending
                    && pending.panel.contains_focused(window, cx)
                    && this.created_terminal_pane(&pending).is_some()
                {
                    this.focus = focus.clone();
                    focus.focus(window, cx);
                }
            });
        }
    }

    fn created_terminal_pane(&self, pending: &CreatedTerminalFocus) -> Option<String> {
        if self
            .state
            .current_command_target(ResourceKind::Terminal)
            .as_ref()
            != Some(&pending.target)
        {
            return None;
        }
        self.state.focused_pane()
    }

    pub(crate) fn restore_terminal_surface(
        &self,
        origin: &crate::gpui_dock::surfaces::TerminalSurfaceOrigin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let binding = &self.state.workspace.active.binding;
        let scope = binding.scope();
        if origin.binding_id != scope.persistence_value().to_string() {
            return;
        }
        let Some(saved) = binding
            .sessions()
            .get(&origin.task_identity)
            .filter(|saved| !saved.state.deleted)
        else {
            return;
        };
        let Some(session) = binding.session_attachment(&saved.identity) else {
            return;
        };
        let Some(observed) = binding
            .restored_terminal_window(&saved.identity, &origin.window_key)
            .or_else(|| {
                session
                    .windows
                    .iter()
                    .find(|item| item.id == origin.window_key)
            })
        else {
            return;
        };
        let id = binding.window_id(session.id.clone(), observed.id.clone());
        let title = observed.name.clone();
        let handle = self
            .state
            .binding_target_handle(scope, binding.mux().binding_generation());
        let Some(target) = bootty_mux::target::ExactMuxTarget::Binding(scope).command_target(
            ResourceKind::Binding,
            binding.mux(),
            &handle,
        ) else {
            return;
        };
        if let Some(dock) = self.tools.clone() {
            dock.update(cx, |dock, cx| {
                dock.publish_window_surface(origin, target, id, title, window, cx);
            });
        }
    }

    fn sync_surface_form(&self, cx: &mut Context<Self>) {
        if self.native_conversations.surface_agent_form
            && let Some(request) = &self.native_conversations.surface_request
            && self.state.surface_agent_form_request_id() == Some(request.id)
            && let Some(view) = self.dialogs.new_session_surface()
            && let Some(dock) = &self.tools
        {
            dock.update(cx, |dock, cx| {
                dock.show_surface_agent_form(request.id, view, cx);
            });
        }
    }

    pub(super) fn close_dismissed_surface_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.native_conversations.surface_agent_form
            && let Some(request) = &self.native_conversations.surface_request
            && self.state.surface_agent_form_request_id() != Some(request.id)
        {
            self.close_surface_chooser(request.id, window, cx);
        }
    }

    pub(super) fn open_native_conversation(
        &mut self,
        target: CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.native_conversations.requested = Some(target);
        self.native_conversations.focus_requested = true;
        self.refresh_native_conversations(window, cx);
        self.present_native_conversation(window, cx);
    }

    pub(super) fn close_native_conversation(&mut self) {
        self.native_conversations.requested = None;
        self.native_conversations.focus_requested = false;
    }

    pub(super) fn selected_mux_pane_is_native(&self) -> bool {
        let binding = &self.state.workspace.active.binding;
        let Some(session) = binding.mux().selected_session() else {
            return false;
        };
        let Some(window) = binding.mux().selected_window() else {
            return false;
        };
        let pane = binding.window_focused_pane(session, window);
        binding
            .mux()
            .selected_window_panes()
            .iter()
            .any(|anchor| anchor.pane_id.as_deref() == pane && anchor.native_agent.is_some())
    }

    pub(super) fn selected_mux_native_record(&self) -> Option<NativeSessionRecord> {
        let binding = &self.state.workspace.active.binding;
        let session_id = binding.mux().selected_session()?;
        let window_id = binding.mux().selected_window()?;
        let pane_id = binding.window_focused_pane(session_id, window_id)?;
        self.native_conversations
            .records
            .iter()
            .find(|record| {
                self.state
                    .native_panel_target(record)
                    .is_some_and(|(exact, _)| {
                        let (session, window, pane) = exact.ids();
                        exact.scope() == binding.scope()
                            && session == Some(session_id)
                            && window == Some(window_id)
                            && pane == Some(pane_id)
                    })
            })
            .cloned()
    }

    pub(super) fn sync_selected_native_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(record) = self.selected_mux_native_record() else {
            // A pending explicit focus is published only after its backend command completes.
            if !self.native_conversations.focus_requested {
                self.close_native_conversation();
            }
            return false;
        };
        let target = record.target();
        let changed = self.native_conversations.requested.as_ref() != Some(&target);
        let changed_session = self
            .native_conversations
            .requested
            .as_ref()
            .is_none_or(|previous| previous.handle != target.handle);
        self.native_conversations.requested = Some(target.clone());
        let view = self.ensure_native_conversation_view(record.clone(), window, cx);
        if changed {
            self.sync_native_annotations(cx);
            self.sync_browser_configuration(window, cx);
            if changed_session
                && !self.state.native_resume_pending(&target)
                && !record.snapshot.transport_lost
                && matches!(
                    record.snapshot.status,
                    bootty_agents::NativeSessionStatus::Stopped
                        | bootty_agents::NativeSessionStatus::Error
                )
            {
                // The mux already selected this pane; a late resume must not select it again.
                let mut invocation =
                    CommandInvocation::from_action("agents.native.resume", Caller::Internal);
                invocation.arguments = vec![target.handle.clone(), target.generation.to_string()];
                invocation.target = Some(target);
                self.state.commands.queue(invocation);
            }
        }
        if changed_session || self.native_conversations.focus_requested {
            self.native_conversations.focus_requested = false;
            if !view.read(cx).contains_focused(window, cx) {
                view.focus_handle(cx).focus(window, cx);
            }
        }
        true
    }

    fn resume_lost_native_transports(&mut self, cx: &Context<Self>) {
        use crate::presentation::native_reconnect::NativeReconnectStep;
        let records = &self.native_conversations.records;
        self.native_conversations
            .reconnect
            .retain(|id, _| records.iter().any(|record| &record.id == id));
        self.native_conversations
            .reconnect_pending
            .retain(|id, _| records.iter().any(|record| &record.id == id));
        let mut wait: Option<std::time::Duration> = None;
        for record in records {
            let enabled = self
                .state
                .config()
                .agents
                .provider(&record.config.provider.to_string())
                .is_some_and(|provider| provider.enabled);
            if !enabled || record.snapshot.status == bootty_agents::NativeSessionStatus::Stopped {
                self.native_conversations
                    .reconnect_pending
                    .remove(&record.id);
                continue;
            }
            if let Some(pending) = self.native_conversations.reconnect_pending.get(&record.id) {
                if matches!(
                    pending.response.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ) {
                    let after = std::time::Duration::from_millis(100);
                    wait = Some(wait.map_or(after, |current| current.min(after)));
                    continue;
                }
                self.native_conversations
                    .reconnect_pending
                    .remove(&record.id);
            }
            match self
                .native_conversations
                .reconnect
                .entry(record.id.clone())
                .or_default()
                .poll(record, std::time::Instant::now())
            {
                NativeReconnectStep::Idle => {}
                NativeReconnectStep::Wait(after) => {
                    wait = Some(wait.map_or(after, |current| current.min(after)));
                }
                NativeReconnectStep::Resume(target) => {
                    let mut invocation =
                        CommandInvocation::from_action("agents.native.resume", Caller::Internal);
                    invocation.arguments =
                        vec![target.handle.clone(), target.generation.to_string()];
                    invocation.target = Some(target);
                    let now = std::time::Instant::now();
                    let cancellation = bootty_control::CommandCancellation::new();
                    if let Ok(response) = self.state.app_command_sender(Caller::Internal).submit(
                        invocation,
                        now.checked_add(std::time::Duration::from_secs(30))
                            .unwrap_or(now),
                        cancellation.clone(),
                    ) {
                        self.native_conversations.reconnect_pending.insert(
                            record.id.clone(),
                            PendingNativeReconnect {
                                response,
                                cancellation,
                            },
                        );
                    }
                    let after = std::time::Duration::from_millis(100);
                    wait = Some(wait.map_or(after, |current| current.min(after)));
                }
            }
        }
        if let Some(after) = wait {
            self.schedule_repaint_after(after, cx);
        }
    }

    pub(super) fn refresh_native_conversations(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_pending_permissions();
        self.resume_lost_native_transports(cx);
        self.focus_created_terminal(window, cx);
        self.sync_selected_native_pane(window, cx);
        self.sync_surface_form(cx);
        self.observe_browser_annotations(window, cx);
        for record in &self.native_conversations.records {
            if let Some(view) = self.native_conversations.views.get(&record.id) {
                let enabled = self
                    .state
                    .config()
                    .agents
                    .provider(&record.config.provider.to_string())
                    .is_some_and(|provider| provider.enabled);
                view.update(cx, |view, cx| {
                    view.set_availability(
                        enabled,
                        self.state.native_resume_pending(&record.target()),
                        cx,
                    );
                    view.set_animate_working(self.state.config().sidebar.animate_working, cx);
                });
            }
        }
        let Some(service) = self.state.native_agent_service() else {
            if let Some(dock) = &self.tools {
                dock.update(cx, |dock, _| dock.cancel_restored_agent_destination());
            }
            return;
        };
        let revision = service.revision();
        if self.native_conversations.refreshing {
            return;
        }
        if revision == self.native_conversations.revision {
            // Restoration must capture the published generation after any in-flight resume.
            self.restore_selected_agent_center(cx);
            return;
        }
        self.native_conversations.refreshing = true;
        let work = cx
            .background_executor()
            .spawn(async move { service.sessions() });
        cx.spawn_in(window, async move |owner, cx| {
            let records = work.await;
            _ = owner.update_in(cx, |this, window, cx| {
                this.native_conversations.refreshing = false;
                this.native_conversations.revision = revision;
                this.native_conversations.records = records;
                for record in &this.native_conversations.records {
                    if let Some(view) = this.native_conversations.views.get(&record.id) {
                        view.update(cx, |view, cx| {
                            view.update_record(record.clone(), window, cx);
                        });
                    }
                }
                this.sync_selected_native_pane(window, cx);
                this.sync_related_conversations(cx);
                this.present_native_conversation(window, cx);
                cx.notify();
                window.refresh();
            });
        })
        .detach();
    }

    fn apply_pending_permissions(&mut self) {
        let Some(record) = self.native_conversations.records.iter().find(|record| {
            record.permissions_pending
                && record.snapshot.status == bootty_agents::NativeSessionStatus::Idle
                && !self.state.native_resume_pending(&record.target())
                && self
                    .state
                    .config()
                    .agents
                    .provider(&record.config.provider.to_string())
                    .is_some_and(|provider| provider.enabled)
        }) else {
            return;
        };
        let target = record.target();
        let mut invocation = CommandInvocation::new(
            "agents.native.permissions",
            vec![
                record.id.clone(),
                record.generation.to_string(),
                record.config.permissions.id().into(),
            ],
            Caller::Internal,
        );
        invocation.target = Some(target);
        // Maintenance cannot replace an action the user has already queued.
        self.state.commands.queue_if_empty(invocation);
    }

    fn restore_selected_agent_center(&mut self, cx: &mut Context<Self>) {
        if self.native_conversations.revision == 0 {
            return;
        }
        let destination = self
            .tools
            .as_ref()
            .and_then(|dock| dock.update(cx, |dock, _| dock.take_restored_agent_destination()));
        let Some((binding, task, id)) = destination else {
            return;
        };
        let record = self.native_conversations.records.iter().find(|record| {
            record.id == id
                && record.binding_id == binding
                && record.task_identity.as_deref() == Some(task.as_str())
        });
        let Some(record) = record.filter(|record| {
            binding == self.state.mux_scope().persistence_value().to_string()
                && self.native_conversation_binding(record).is_some()
                && self
                    .state
                    .config()
                    .agents
                    .provider(&record.config.provider.to_string())
                    .is_some_and(|provider| provider.enabled)
        }) else {
            return;
        };
        let mut invocation =
            CommandInvocation::from_action("agents.native.focus", Caller::Internal);
        invocation.target = Some(record.target());
        // Keep the saved center selected while its provider resumes asynchronously.
        self.native_conversations
            .requested
            .clone_from(&invocation.target);
        self.state.commands.queue(invocation);
    }

    fn present_native_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(requested) = self.native_conversations.requested.clone() else {
            return;
        };
        let id = requested.handle.clone();
        let Some(record) = self
            .native_conversations
            .records
            .iter()
            .find(|record| record.id == id)
            .cloned()
        else {
            return;
        };
        // Focus waits for this generation's catalog publication, not the prior process view.
        if self.native_conversations.focus_requested && record.target() != requested {
            return;
        }
        if self.native_conversations.focus_requested {
            let Some(identity) = record.task_identity.as_deref() else {
                self.state
                    .record_error("This conversation has no saved task.");
                self.close_native_conversation();
                return;
            };
            if !self
                .state
                .activate_native_task_from_ui(&record.binding_id, identity)
            {
                self.close_native_conversation();
                return;
            }
        }
        let target = record.target();
        self.ensure_native_conversation_view(record, window, cx);
        self.sync_native_annotations(cx);
        self.sync_browser_configuration(window, cx);
        if let Some((request, attached)) = self.native_conversations.surface_attachment.take() {
            if attached == target {
                self.close_surface_chooser(request.id, window, cx);
            } else {
                self.native_conversations.surface_attachment = Some((request, attached));
            }
        }
        if self.native_conversations.focus_requested {
            self.native_conversations.focus_requested = false;
            if let Some(view) = self.native_conversations.views.get(&id)
                && !view.read(cx).contains_focused(window, cx)
            {
                view.focus_handle(cx).focus(window, cx);
            }
        }
    }

    pub(crate) fn native_parent_terminal(&self, target: &CommandTarget) -> Option<CommandTarget> {
        let record = self
            .native_conversations
            .records
            .iter()
            .find(|record| record.target() == *target)?;
        self.state
            .native_panel_target(record)
            .map(|(_, target)| target)
    }

    pub(super) fn native_mux_pane_content(
        &mut self,
        pane: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<crate::gpui_terminal_panel::WorkspacePaneView> {
        use gpui_kit::component::ActiveTheme as _;
        let marked = self
            .state
            .mux()
            .sessions()
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .any(|anchor| anchor.pane_id.as_deref() == Some(pane) && anchor.native_agent.is_some());
        if !marked {
            return None;
        }
        Some(self.native_mux_pane_view(pane, window, cx).map_or_else(
            || crate::gpui_terminal_panel::WorkspacePaneView::NativePlaceholder {
                loading: self.native_conversations.refreshing
                    || self.native_conversations.revision == 0,
                background: cx.theme().background,
                foreground: cx.theme().muted_foreground,
            },
            crate::gpui_terminal_panel::WorkspacePaneView::NativeAgent,
        ))
    }

    pub(super) fn native_mux_pane_view(
        &mut self,
        pane: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<NativeAgentSessionView>> {
        let scope = self.state.mux_scope();
        let record = self
            .native_conversations
            .records
            .iter()
            .find(|record| {
                self.state
                    .native_panel_target(record)
                    .is_some_and(|(exact, _)| exact.scope() == scope && exact.ids().2 == Some(pane))
            })?
            .clone();
        Some(self.ensure_native_conversation_view(record, window, cx))
    }

    pub(crate) fn native_agent_pane_overlays(
        &mut self,
        bounds: gpui_kit::Bounds<gpui_kit::Pixels>,
        panes: &[(String, String, bootty_terminal::geometry::SurfaceRect)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        use gpui_kit::{IntoElement as _, ParentElement as _, Styled as _, div, relative};
        let area = bootty_terminal::geometry::SurfaceRect {
            min_x: bounds.left().into(),
            min_y: bounds.top().into(),
            max_x: bounds.right().into(),
            max_y: bounds.bottom().into(),
        };
        panes
            .iter()
            .cloned()
            .filter_map(|(pane, _, rect)| {
                let view = self.native_mux_pane_content(&pane, window, cx)?;
                let target = self
                    .native_conversations
                    .records
                    .iter()
                    .find(|record| {
                        self.state
                            .native_panel_target(record)
                            .is_some_and(|(exact, _)| exact.ids().2 == Some(pane.as_str()))
                    })
                    .map(NativeSessionRecord::target);
                Some(
                    div()
                        .id(format!("native-mux-pane-{pane}"))
                        .absolute()
                        .left(relative((rect.min_x - area.min_x) / area.width().max(1.0)))
                        .top(relative((rect.min_y - area.min_y) / area.height().max(1.0)))
                        .w(relative(rect.width() / area.width().max(1.0)))
                        .h(relative(rect.height() / area.height().max(1.0)))
                        .overflow_hidden()
                        .on_mouse_down(
                            gpui_kit::MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if let Some(target) = &target
                                    && this.selected_native_conversation_target(cx).as_ref()
                                        != Some(target)
                                {
                                    let mut focus = CommandInvocation::from_action(
                                        "agents.native.focus",
                                        Caller::Internal,
                                    );
                                    focus.target = Some(target.clone());
                                    this.invoke_gpui_command(focus, window, cx);
                                }
                            }),
                        )
                        .child(view)
                        .into_any_element(),
                )
            })
            .collect()
    }

    fn ensure_native_conversation_view(
        &mut self,
        record: NativeSessionRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<NativeAgentSessionView> {
        let id = record.id.clone();
        let binding = self.native_conversation_binding(&record);
        if let Some(view) = self.native_conversations.views.get(&id) {
            view.update(cx, |view, cx| {
                view.set_completion_binding(binding, window, cx);
            });
            return view.clone();
        }
        let enabled = self
            .state
            .config()
            .agents
            .provider(&record.config.provider.to_string())
            .is_some_and(|provider| provider.enabled);
        let sender = self.state.app_command_sender(Caller::Internal);
        let view = cx.new(|cx| NativeAgentSessionView::new(record, sender, enabled, window, cx));
        view.update(cx, |view, cx| {
            view.set_completion_binding(binding, window, cx);
            view.set_animate_working(self.state.config().sidebar.animate_working, cx);
        });
        let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            this.handle_native_session_event(event, window, cx);
            cx.notify();
        });
        self.native_conversations
            .views
            .insert(id.clone(), view.clone());
        self.native_conversations
            .subscriptions
            .insert(id, subscription);
        self.sync_related_conversations(cx);
        view
    }

    fn native_conversation_binding(&self, record: &NativeSessionRecord) -> Option<CommandTarget> {
        let task_identity = record.task_identity.as_ref()?;
        let binding =
            self.state.workspace.all_bindings().find(|binding| {
                binding.scope().persistence_value().to_string() == record.binding_id
            })?;
        if binding.sessions().get(task_identity)?.state.deleted {
            return None;
        }
        let mux = binding.mux();
        let handle = self
            .state
            .binding_target_handle(binding.scope(), mux.binding_generation());
        crate::commands::ExactMuxTarget::Binding(binding.scope()).command_target(
            ResourceKind::Binding,
            mux,
            &handle,
        )
    }

    fn handle_native_session_event(
        &mut self,
        event: &OpenNativeSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            OpenNativeSession::SideChatCreated { source, record } => {
                self.present_side_chat(source, record.as_ref().clone(), window, cx);
            }
            OpenNativeSession::Related { target } => {
                let mut invocation =
                    CommandInvocation::from_action("agents.native.focus", Caller::Internal);
                invocation.target = Some(target.clone());
                self.state.commands.queue(invocation);
            }
            OpenNativeSession::DetachAnnotation { target, annotation } => {
                self.detach_native_annotations(
                    target,
                    vec![annotation.as_ref().clone()],
                    true,
                    window,
                    cx,
                );
            }
            OpenNativeSession::SentAnnotations {
                target,
                annotations,
            } => {
                self.detach_native_annotations(target, annotations.clone(), false, window, cx);
            }
        }
    }

    fn present_side_chat(
        &mut self,
        source: &CommandTarget,
        record: NativeSessionRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(parent) = self
            .native_conversations
            .records
            .iter()
            .find(|parent| parent.target() == *source)
            .cloned()
        else {
            return;
        };
        if self.native_conversation_binding(&parent).is_none() {
            return;
        }
        let target = record.target();
        if !self
            .native_conversations
            .records
            .iter()
            .any(|current| current.id == record.id)
        {
            self.native_conversations.records.push(record.clone());
        }
        // A completed fork must never steal focus from a different saved task or Space.
        if self.native_conversation_view(cx).is_none()
            || self
                .native_conversations
                .requested
                .as_ref()
                .is_none_or(|active| active != source)
        {
            return;
        }
        self.ensure_native_conversation_view(record, window, cx);
        let mut invocation =
            CommandInvocation::from_action("agents.native.focus", Caller::Internal);
        invocation.target = Some(target);
        self.invoke_gpui_command(invocation, window, cx);
    }

    fn sync_related_conversations(&self, cx: &mut Context<Self>) {
        for record in &self.native_conversations.records {
            if let Some(view) = self.native_conversations.views.get(&record.id) {
                let related = self
                    .native_conversations
                    .records
                    .iter()
                    .filter(|child| {
                        child
                            .side_chat
                            .as_ref()
                            .is_some_and(|fork| fork.source_id == record.id)
                    })
                    .map(|child| crate::gpui_agent_session::RelatedConversation {
                        target: child.target(),
                        title: child.title.clone(),
                        model: child.config.model.clone(),
                        status: child.snapshot.status,
                    })
                    .collect();
                view.update(cx, |view, cx| view.set_related_conversations(related, cx));
            }
        }
    }

    pub(crate) fn native_conversation_view(
        &self,
        _cx: &App,
    ) -> Option<Entity<NativeAgentSessionView>> {
        let record = self.selected_mux_native_record()?;
        let id = &self.native_conversations.requested.as_ref()?.handle;
        if record.id != *id {
            return None;
        }
        self.native_conversations.views.get(id).cloned()
    }

    pub(super) fn decorate_native_tabs(
        &self,
        snapshot: &mut crate::gpui::chrome::ChromeSnapshot,
        cx: &App,
    ) {
        if let Some(target) = self.selected_native_conversation_target(cx)
            && let Some(task) = self
                .native_conversations
                .records
                .iter()
                .find(|record| record.id == target.handle)
                .and_then(|record| record.task_identity.as_deref())
        {
            Self::decorate_native_task(snapshot, task, true);
        }
    }

    fn decorate_native_task(
        snapshot: &mut crate::gpui::chrome::ChromeSnapshot,
        identity: &str,
        selected: bool,
    ) {
        if selected && let Some(sidebar) = &mut snapshot.sidebar {
            for row in &mut sidebar.rows {
                if row.kind.is_session() {
                    row.active = row
                        .task
                        .as_ref()
                        .is_some_and(|task| task.identity == identity);
                    row.current = row.active;
                }
            }
        }
    }

    pub(super) fn selected_native_conversation_target(&self, cx: &App) -> Option<CommandTarget> {
        self.native_conversation_view(cx)?;
        let id = &self.native_conversations.requested.as_ref()?.handle;
        self.native_conversations
            .records
            .iter()
            .find(|record| &record.id == id)
            .map(NativeSessionRecord::target)
    }

    pub(super) fn shown_native_conversation_target(&self, cx: &App) -> Option<CommandTarget> {
        self.native_conversation_view(cx)?;
        let id = &self.native_conversations.requested.as_ref()?.handle;
        self.native_conversations
            .records
            .iter()
            .find(|record| {
                &record.id == id
                    && self
                        .state
                        .config()
                        .agents
                        .provider(&record.config.provider.to_string())
                        .is_some_and(|provider| provider.enabled)
            })
            .map(NativeSessionRecord::target)
    }

    pub(super) fn browser_attachment_context(
        &self,
        target: Option<&CommandTarget>,
    ) -> bootty_agents::NativeBrowserAccess {
        self.native_conversations
            .records
            .iter()
            .find(|record| target == Some(&record.target()))
            .map_or_else(bootty_agents::NativeBrowserAccess::default, |record| {
                record.snapshot.browser_access.clone()
            })
    }

    fn observe_browser_annotations(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.native_conversations.annotations_subscription.is_some() {
            return;
        }
        let Some(tools) = &self.tools else {
            return;
        };
        let browser = tools.read(cx).browser_session();
        self.native_conversations.annotations_subscription = Some(cx.subscribe_in(
            &browser,
            window,
            |this, _, _: &crate::gpui_browser_panel::BrowserAnnotationsChanged, _, cx| {
                this.sync_native_annotations(cx);
                cx.notify();
            },
        ));
        self.sync_native_annotations(cx);
    }

    fn sync_native_annotations(&mut self, cx: &mut Context<Self>) {
        let Some(tools) = &self.tools else {
            return;
        };
        let browser = tools.read(cx).browser_session();
        let annotations = browser.read(cx).annotation_records().to_vec();
        if let Some(status) = browser.read(cx).annotation_status() {
            self.state.record_error(status);
        }
        for record in &self.native_conversations.records {
            let Some(view) = self.native_conversations.views.get(&record.id) else {
                continue;
            };
            let projection = annotations
                .iter()
                .filter(|annotation| annotation.is_attached_to(&record.id))
                .cloned()
                .collect();
            let result = view.update(cx, |view, cx| {
                view.set_annotations(&record.target(), projection, cx)
            });
            if let Err(error) = result {
                self.state.record_error(error);
            }
        }
    }

    fn detach_native_annotations(
        &mut self,
        target: &CommandTarget,
        annotations: Vec<bootty_browser::Annotation>,
        require_shown: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if require_shown && self.shown_native_conversation_target(cx).as_ref() != Some(target) {
            self.state
                .record_error("This annotation’s conversation is no longer selected.");
            return;
        }
        let Some(record) = self
            .native_conversations
            .records
            .iter()
            .find(|record| record.target() == *target)
        else {
            self.state.record_error(
                "This annotation’s conversation changed. Its local note was retained.",
            );
            return;
        };
        let saved = record.task_identity.as_deref().is_some_and(|identity| {
            self.state.workspace.all_bindings().any(|binding| {
                binding.scope().persistence_value().to_string() == record.binding_id
                    && binding
                        .sessions()
                        .get(identity)
                        .is_some_and(|saved| !saved.state.deleted)
            })
        });
        if !saved {
            self.state
                .record_error("This conversation’s saved task is unavailable.");
            return;
        }
        if let Some(tools) = &self.tools {
            let browser = tools.read(cx).browser_session();
            browser.update(cx, |browser, cx| {
                browser.detach_annotations(target.clone(), annotations, window, cx);
            });
        }
    }
}
