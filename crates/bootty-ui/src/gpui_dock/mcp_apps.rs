//! MCP apps open as right-dock tabs, one per tool call. Their tabs live while the window does: a
//! saved layout restores them closed, since their documents and bridges don't survive a restart.
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use gpui_kit::component::dock::{PanelId, PanelInfo, panel_handle};
use gpui_kit::{App, Context, Entity, Window, prelude::*};

use super::{WorkspaceDock, registry::register_factory};
use crate::gpui_mcp_app::{McpAppClose, McpAppPanel};

pub(super) type McpAppPanels = Rc<RefCell<BTreeMap<String, Entity<McpAppPanel>>>>;

impl WorkspaceDock {
    pub(super) fn mcp_app_panels(
        area: &Entity<gpui_kit::component::dock::DockArea>,
        cx: &mut App,
    ) -> McpAppPanels {
        let panels = McpAppPanels::default();
        let held = panels.clone();
        register_factory(
            area,
            "bootty.mcp_app",
            Rc::new(move |info, _, cx| {
                let value = match info {
                    PanelInfo::Panel(value) => value.clone(),
                    _ => serde_json::Value::Null,
                };
                let text = |key: &str| {
                    value
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let key = text("key");
                if let Some(panel) = held.borrow().get(&key).cloned() {
                    return panel_handle(panel);
                }
                panel_handle(cx.new(|cx| {
                    McpAppPanel::closed(
                        key,
                        text("title"),
                        Some("This app closed when Bootty restarted. Open it again from its tool call.".to_owned()),
                        cx,
                    )
                }))
            }),
            cx,
        );
        panels
    }

    /// Shows the app for the tool call `item` of `parent`'s conversation, opening its tab once.
    pub(crate) fn open_mcp_app(
        &self,
        key: String,
        parent: &Entity<crate::gpui_agent_session::NativeAgentSessionView>,
        item: String,
        app: bootty_agents::NativeMcpApp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.mcp_apps.borrow().get(&key).cloned();
        let panel = existing.unwrap_or_else(|| {
            let panel = cx.new(|cx| McpAppPanel::new(key.clone(), parent, item, app, window, cx));
            cx.subscribe_in(
                &panel,
                window,
                |this, panel, _: &McpAppClose, window, cx| {
                    this.close_tool_tab(PanelId::from(panel.entity_id()), window, cx);
                },
            )
            .detach();
            self.mcp_apps.borrow_mut().insert(key, panel.clone());
            panel
        });
        let id = PanelId::from(panel.entity_id());
        self.add_document_panel(panel_handle(panel), window, cx);
        self.area
            .update(cx, |area, cx| area.select_panel(id, window, cx));
        self.sync_browser_visibility(cx);
    }

    /// Closes an app's tab, giving the app its teardown. Returns whether `id` was an app's.
    pub(super) fn close_mcp_app(
        &self,
        id: PanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((key, panel)) = self
            .mcp_apps
            .borrow()
            .iter()
            .find(|(_, panel)| PanelId::from(panel.entity_id()) == id)
            .map(|(key, panel)| (key.clone(), panel.clone()))
        else {
            return false;
        };
        self.mcp_apps.borrow_mut().remove(&key);
        panel.update(cx, |panel, cx| panel.close(window, cx));
        self.area
            .update(cx, |area, cx| area.remove_panel(panel, window, cx));
        self.layout_changed(cx);
        true
    }

    pub(super) fn mcp_apps_have_views(&self, cx: &App) -> bool {
        self.mcp_apps
            .borrow()
            .values()
            .any(|panel| panel.read(cx).has_view())
    }

    pub(super) fn sync_mcp_app_visibility(&self, cx: &mut Context<Self>) {
        let panels: Vec<_> = self.mcp_apps.borrow().values().cloned().collect();
        for panel in panels {
            let visible = self.right_panel_shown(PanelId::from(panel.entity_id()), cx);
            panel.update(cx, |panel, cx| panel.set_visible(visible, cx));
        }
    }
}
