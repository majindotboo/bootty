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
        let created = self
            .native_sessions
            .initialized
            .then(|| {
                records
                    .iter()
                    .rev()
                    .find(|record| {
                        !self
                            .native_sessions
                            .records
                            .iter()
                            .any(|old| old.id == record.id)
                    })
                    .cloned()
            })
            .flatten();
        self.native_sessions.initialized = true;
        self.native_sessions.records = records;
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| {
                tools.sync_native_sessions(&self.native_sessions.records, window, cx);
            });
        }
        if let Some(created) = created {
            self.select_native_session(&created, window, cx);
        } else if let Some(selected) = &self.native_sessions.selected {
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
            .strip_prefix(record.config.provider.module())
            .filter(|operation| *operation != ".start")?;
        Some(record.target())
    }

    pub(super) fn decorate_native_sessions(
        &self,
        snapshot: &mut ChromeSnapshot,
        projection: &crate::chrome_frame::ChromeProjection,
    ) {
        self.decorate_native_tab(snapshot);
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
                kind: SidebarRowKind::Session,
                active: selected == Some(record.id.as_str()),
                current: selected == Some(record.id.as_str()),
                selectable: true,
                target: Some(SessionTarget {
                    scope: SpaceKey(scope.persistence_value()),
                    session_id: record.id.clone(),
                }),
                reorder_anchor: None,
                native_context: Some(crate::gpui::chrome::NativeSessionSidebar {
                    target: record.target(),
                    provider: record.config.provider.to_string(),
                    title: record.title.clone(),
                    stopped: record.snapshot.status == NativeSessionStatus::Stopped,
                }),
                context: None,
            });
        }
        self.group_project_sessions(&mut sidebar.rows, projection);
        snapshot.titlebar.session_count = snapshot.titlebar.session_count.saturating_add(
            self.native_sessions
                .records
                .iter()
                .filter(|record| record.binding_id == binding)
                .count(),
        );
    }
    fn group_project_sessions(
        &self,
        rows: &mut Vec<SidebarRow>,
        projection: &crate::chrome_frame::ChromeProjection,
    ) {
        let remote = self.state.active_multiplexer().remote.is_some();
        let mut groups = std::collections::BTreeMap::<(bool, String), Vec<SidebarRow>>::new();
        let mut ungrouped = Vec::new();
        let mut current = None;
        for mut row in std::mem::take(rows) {
            if matches!(row.kind, SidebarRowKind::Group) {
                current = None;
                if !row.key.starts_with("group:") {
                    ungrouped.push(row);
                }
                continue;
            }
            if matches!(row.kind, SidebarRowKind::Session) {
                row.number = None;
                if row.native_context.is_none() {
                    row.icon = Some("terminal".to_owned());
                }
                current = self
                    .native_sessions
                    .records
                    .iter()
                    .find(|record| record.id == row.key)
                    .map(|record| (false, record.config.cwd.to_string_lossy().into_owned()))
                    .or_else(|| {
                        projection
                            .mux
                            .sessions
                            .iter()
                            .find(|session| session.id == row.key)
                            .and_then(|session| session.cwd.as_ref())
                            .filter(|cwd| !cwd.is_empty())
                            .map(|cwd| (remote, cwd.clone()))
                    });
            }
            if let Some(key) = &current {
                groups.entry(key.clone()).or_default().push(row);
            } else {
                row.artwork = None;
                ungrouped.push(row);
            }
        }
        for ((remote, directory), mut sessions) in groups {
            let Some(first) = sessions.first() else {
                continue;
            };
            let name = std::path::Path::new(&directory)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or(&directory);
            let mut heading = first.clone();
            heading.key = format!("project:{remote}:{directory}");
            name.clone_into(&mut heading.text);
            heading.kind = SidebarRowKind::Group;
            heading.icon = Some(if remote { "server" } else { "folder" }.to_owned());
            heading.trailing = sessions
                .iter()
                .find(|row| row.key.ends_with(":branch"))
                .map(|row| row.text.clone());
            heading.trailing_color = Some(first.dim_color);
            heading.trailing_shimmer = false;
            heading.artwork = sessions.iter().find_map(|row| row.artwork.clone());
            heading.diff = None;
            heading.current = false;
            heading.active = false;
            heading.selectable = false;
            heading.target = None;
            heading.context = None;
            heading.native_context = None;
            heading.reorder_anchor = None;
            heading.number = None;
            heading.indent = 0;
            heading.tree = None;
            rows.push(heading);
            sessions.retain(|row| !row.key.ends_with(":cwd") && !row.key.ends_with(":branch"));
            for row in &mut sessions {
                row.number = None;
                row.tree = None;
                row.artwork = None;
                row.indent = if matches!(row.kind, SidebarRowKind::Session) {
                    2
                } else {
                    4
                };
                if matches!(row.kind, SidebarRowKind::Session) && row.native_context.is_none() {
                    row.icon = Some("terminal".to_owned());
                    if row.text == name {
                        "Terminal".clone_into(&mut row.text);
                    }
                }
            }
            rows.extend(sessions);
        }
        rows.extend(ungrouped);
    }

    fn decorate_native_tab(&self, snapshot: &mut ChromeSnapshot) {
        let Some(record) = self.native_sessions.selected.as_ref().and_then(|id| {
            self.native_sessions
                .records
                .iter()
                .find(|record| &record.id == id)
        }) else {
            return;
        };
        let mut inserted = false;
        for bar in snapshot
            .top_status
            .iter_mut()
            .chain(snapshot.bottom_status.iter_mut())
        {
            for segment in &mut bar.segments {
                segment.items.retain_mut(|item| {
                    if item.tab_context.is_none() {
                        return true;
                    }
                    if inserted {
                        return false;
                    }
                    inserted = true;
                    item.key.clone_from(&record.id);
                    item.text.clone_from(&record.title);
                    item.icon = Some(
                        match record.config.provider {
                            bootty_agents::AgentKind::Codex => "openai",
                            bootty_agents::AgentKind::Claude => "claude",
                            bootty_agents::AgentKind::Pi => "terminal",
                        }
                        .to_owned(),
                    );
                    item.active = true;
                    item.action = None;
                    item.reorder_anchor = None;
                    item.tab_context = None;
                    true
                });
            }
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
        let command = format!("{}.rename", record.config.provider.module());
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
                        format!("{}.remove", record.config.provider.module()),
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
