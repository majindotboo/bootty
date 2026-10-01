//! Window-local navigation for native sessions; the service owns processes and durable history.
use bootty_agents::{NativeSessionRecord, NativeSessionStatus};
use bootty_control::{CommandTarget, ResourceKind};
use gpui_kit::{AppContext as _, Context, Window};

use super::GpuiWorkspace;
use crate::gpui::chrome::{
    ChromeSnapshot, Rgba, SessionTarget, SidebarRow, SidebarRowKind, SpaceKey,
};

#[derive(Default)]
pub(super) struct NativeSessions {
    pub(super) records: Vec<NativeSessionRecord>,
    pub(super) selected: Option<String>,
    initialized: bool,
    pub(super) opened: bool,
    refreshing: bool,
    revision: Option<u64>,
    terminal_directory: Option<String>,
}

impl GpuiWorkspace {
    pub(super) fn refresh_native_sessions(&mut self, window: &Window, cx: &Context<Self>) {
        let Some(service) = self.state.native_agent_service() else {
            return;
        };
        let revision = service.revision();
        if self.native_sessions.refreshing || self.native_sessions.revision == Some(revision) {
            return;
        }
        self.native_sessions.refreshing = true;
        cx.spawn_in(window, async move |owner, cx| {
            let records = cx
                .background_executor()
                .spawn(async move { service.sessions() })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                this.native_sessions.refreshing = false;
                this.native_sessions.revision = Some(revision);
                this.update_native_sessions(records, window, cx);
                // A publication can arrive while the snapshot worker is running.
                this.refresh_native_sessions(window, cx);
            });
        })
        .detach();
    }

    fn update_native_sessions(
        &mut self,
        records: Vec<NativeSessionRecord>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let unchanged = self.native_sessions.records.len() == records.len()
            && self
                .native_sessions
                .records
                .iter()
                .zip(&records)
                .all(|(old, new)| {
                    old.id == new.id
                        && old.generation == new.generation
                        && old.title == new.title
                        && old.snapshot == new.snapshot
                });
        if unchanged && self.native_sessions.initialized {
            return;
        }
        self.native_sessions.initialized = true;
        self.native_sessions.records = records;
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| {
                tools.sync_native_sessions(&self.native_sessions.records, window, cx);
            });
        }
        if let Some(selected) = &self.native_sessions.selected {
            let binding = self.state.mux_scope().persistence_value().to_string();
            if !self
                .native_sessions
                .records
                .iter()
                .any(|record| &record.id == selected && record.binding_id == binding)
            {
                self.clear_native_session(window, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn select_native_session(
        &mut self,
        record: &NativeSessionRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let binding = self.state.mux_scope().persistence_value().to_string();
        if record.binding_id != binding {
            return;
        }
        self.native_sessions.opened = true;
        if let Some(old) = self
            .native_sessions
            .records
            .iter_mut()
            .find(|old| old.id == record.id)
        {
            old.clone_from(record);
        } else {
            self.native_sessions.records.push(record.clone());
        }
        self.ensure_tools(window, cx);
        if self.native_sessions.selected.is_none() {
            self.native_sessions.terminal_directory = self
                .tools
                .as_ref()
                .map(|tools| tools.read(cx).context_directory());
        }
        let scope = self.state.mux_scope();
        if let Some(target) = self
            .state
            .current_command_target_for("git.open", ResourceKind::Binding)
        {
            let context = crate::gpui_git_panel::GitPanelContext {
                terminal: self
                    .state
                    .current_command_target_for("shell.prompt", ResourceKind::Terminal),
                target,
                directory: record.config.cwd.to_string_lossy().into_owned(),
                host: "Local".to_owned(),
                host_identity: "local".to_owned(),
                remote: None,
            };
            self.set_workspace_context(scope, context, false, window, cx);
        }
        self.native_sessions.selected = Some(record.id.clone());
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| {
                tools.show_native_session(record, &self.native_sessions.records, window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn clear_native_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.native_sessions.selected = None;
        if let Some(directory) = self.native_sessions.terminal_directory.take()
            && let Some(target) = self
                .state
                .current_command_target_for("git.open", ResourceKind::Binding)
        {
            let scope = self.state.mux_scope();
            let context = crate::gpui_git_panel::GitPanelContext {
                terminal: self
                    .state
                    .current_command_target_for("shell.prompt", ResourceKind::Terminal),
                target,
                directory,
                host: "Local".to_owned(),
                host_identity: "local".to_owned(),
                remote: None,
            };
            self.set_workspace_context(scope, context, false, window, cx);
        }
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| tools.show_terminal_session(window, cx));
        }
        cx.notify();
    }

    pub(super) fn selected_native_target(&self, command: &str) -> Option<CommandTarget> {
        let id = self.native_sessions.selected.as_ref()?;
        let record = self
            .native_sessions
            .records
            .iter()
            .find(|record| &record.id == id)?;
        command
            .strip_prefix(format!("harness.{}.", record.config.provider).as_str())
            .filter(|operation| *operation != "start")?;
        Some(record.target())
    }

    pub(super) fn decorate_native_sessions(&self, snapshot: &mut ChromeSnapshot) {
        if !self.native_sessions.opened {
            return;
        }
        let Some(sidebar) = &mut snapshot.sidebar else {
            return;
        };
        let scope = self.state.mux_scope();
        let binding = scope.persistence_value().to_string();
        let selected = self.native_sessions.selected.as_deref();
        if selected.is_some() {
            for row in &mut sidebar.rows {
                row.current = false;
                row.active = false;
            }
        }
        let palette = self.state.ui_theme().palette;
        let color = Rgba {
            red: palette.text.red,
            green: palette.text.green,
            blue: palette.text.blue,
            alpha: palette.text.alpha,
        };
        let dim = Rgba {
            red: palette.subtext.red,
            green: palette.subtext.green,
            blue: palette.subtext.blue,
            alpha: palette.subtext.alpha,
        };
        if self
            .native_sessions
            .records
            .iter()
            .any(|record| record.binding_id == binding)
        {
            sidebar.rows.push(conversation_header(color, dim));
        }
        for record in self
            .native_sessions
            .records
            .iter()
            .filter(|record| record.binding_id == binding)
        {
            let status = match record.snapshot.status {
                NativeSessionStatus::Starting => "Connecting",
                NativeSessionStatus::Idle => "",
                NativeSessionStatus::Working => "Working",
                NativeSessionStatus::Waiting => "Needs input",
                NativeSessionStatus::Stopped => "Stopped",
                NativeSessionStatus::Error => "Failed",
            };
            sidebar.rows.push(SidebarRow {
                key: record.id.clone(),
                text: record.title.clone(),
                trailing: (!status.is_empty()).then(|| status.to_owned()),
                trailing_color: Some(dim),
                trailing_shimmer: false,
                number: None,
                indent: 0,
                tree: None,
                icon: Some(
                    match record.config.provider {
                        bootty_agents::AgentKind::Codex => "openai",
                        bootty_agents::AgentKind::Claude => "claude",
                        bootty_agents::AgentKind::Pi => "terminal",
                    }
                    .to_owned(),
                ),
                diff: None,
                artwork: None,
                color,
                dim_color: dim,
                kind: SidebarRowKind::Conversation(crate::gpui::chrome::NativeSessionSidebar {
                    target: record.target(),
                    provider: record.config.provider.to_string(),
                    title: record.title.clone(),
                    stopped: record.snapshot.status == NativeSessionStatus::Stopped,
                }),
                active: selected == Some(record.id.as_str()),
                current: selected == Some(record.id.as_str()),
                selectable: true,
                target: Some(SessionTarget {
                    scope: SpaceKey(scope.persistence_value()),
                    session_id: record.id.clone(),
                }),
                reorder_anchor: None,
                context: None,
            });
        }
    }
}

impl GpuiWorkspace {
    pub(super) fn native_session_history(
        &mut self,
        target: &CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(record) = self
            .native_sessions
            .records
            .iter()
            .find(|record| record.target() == *target)
            .cloned()
        else {
            return;
        };
        self.select_native_session(&record, window, cx);
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| tools.open_native_history(window, cx));
        }
    }

    pub(super) fn rename_native_session(
        &self,
        target: CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::{
            WindowExt as _,
            dialog::DialogButtonProps,
            input::{Input, InputState},
        };
        use gpui_kit::{Focusable as _, ParentElement as _, Styled as _};
        let Some(record) = self
            .native_sessions
            .records
            .iter()
            .find(|record| record.target() == target)
            .cloned()
        else {
            return;
        };
        let name = cx.new(|cx| InputState::new(window, cx).default_value(record.title.clone()));
        let owner = cx.weak_entity();
        let command = format!("harness.{}.rename", record.config.provider);
        let focus = name.focus_handle(cx);
        window.open_dialog(cx, move |dialog, _, _| {
            let input = name.clone();
            let owner = owner.clone();
            let target = target.clone();
            let command = command.clone();
            dialog
                .title("Rename conversation")
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Rename")
                        .show_cancel(true),
                )
                .child(
                    gpui_kit::div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child("Name")
                        .child(Input::new(&name))
                        .child(
                            gpui_kit::div()
                                .text_xs()
                                .child("Use a name of at most 256 bytes."),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let value = input.read(cx).value().trim().to_owned();
                    if value.is_empty() || value.len() > 256 {
                        return false;
                    }
                    _ = owner.update(cx, |this, cx| {
                        let mut invocation = bootty_control::CommandInvocation::new(
                            command.clone(),
                            vec![value],
                            bootty_control::Caller::Internal,
                        );
                        invocation.target = Some(target.clone());
                        this.invoke_gpui_command(invocation, window, cx);
                    });
                    true
                })
        });
        focus.focus(window, cx);
    }

    pub(super) fn remove_native_session(
        &self,
        target: CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(record) = self
            .native_sessions
            .records
            .iter()
            .find(|record| record.target() == target)
            .cloned()
        else {
            return;
        };
        let answer = crate::gpui::prompt(
            &format!("Remove “{}” from history?", record.title),
            Some(
                "This removes Bootty’s saved conversation entry. The provider keeps its own transcript.",
            ),
            &["Remove".into(), "Cancel".into()],
            window,
            cx,
        );
        cx.spawn_in(window, async move |owner, cx| {
            if answer.await == Ok(0) {
                _ = owner.update_in(cx, |this, window, cx| {
                    let mut invocation = bootty_control::CommandInvocation::new(
                        format!("harness.{}.remove", record.config.provider),
                        vec![],
                        bootty_control::Caller::Internal,
                    );
                    invocation.target = Some(target);
                    this.invoke_gpui_command(invocation, window, cx);
                });
            }
        })
        .detach();
    }
}

fn conversation_header(color: Rgba, dim: Rgba) -> SidebarRow {
    SidebarRow {
        key: "conversations".to_owned(),
        text: "Conversations".to_owned(),
        trailing: None,
        trailing_color: None,
        trailing_shimmer: false,
        number: None,
        indent: 0,
        tree: None,
        icon: Some("messages-square".to_owned()),
        diff: None,
        artwork: None,
        color,
        dim_color: dim,
        kind: SidebarRowKind::Group,
        active: false,
        current: false,
        selectable: false,
        target: None,
        reorder_anchor: None,
        context: None,
    }
}
