//! Each browser page is a peer panel; the browser session owns its native views.
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

use bootty_control::{BoundAppCommandSender, CommandOutcome};
use gpui_kit::component::dock::{
    DockArea, DockPlacement, PaneRef, PanelId, PanelInfo, panel_handle,
};
use gpui_kit::{App, Context, Entity, Focusable, Window, prelude::*};

use super::{WorkspaceDock, registry::register_factory};
use crate::{
    commands::{BrowserAction, BrowserRequest},
    gpui_browser_panel::{BrowserPagePanel, BrowserPanel},
};

pub(super) struct BrowserPanels {
    pub(super) session: Entity<BrowserPanel>,
    pub(super) pages: Rc<RefCell<HashMap<u64, Entity<BrowserPagePanel>>>>,
    allowed: bool,
}

impl BrowserPanels {
    pub(super) fn new(
        area: &Entity<DockArea>,
        sender: BoundAppCommandSender,
        profile: PathBuf,
        window: &mut Window,
        cx: &mut Context<WorkspaceDock>,
    ) -> Self {
        let session = cx.new(|cx| BrowserPanel::new(profile, sender, window, cx));
        let pages: Rc<RefCell<HashMap<u64, Entity<BrowserPagePanel>>>> = Rc::default();
        let factory_session = session.clone();
        let factory_pages = pages.clone();
        let factory: super::registry::PanelFactory = Rc::new(move |info, window, cx| {
            let state = match info {
                PanelInfo::Panel(state) => state.clone(),
                _ => serde_json::Value::Null,
            };
            let page = state
                .get("page")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if let Some(panel) = factory_pages.borrow().get(&page).cloned() {
                return panel_handle(panel);
            }
            let address = state
                .get("address")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("about:blank");
            let title = state
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Browser");
            let result = factory_session.update(cx, |browser, cx| {
                browser.restore_page(page, address, title, window, cx)
            });
            if let Err(error) = result {
                let saved = gpui_kit::component::dock::PanelState {
                    panel_name: "bootty.browser.page".to_owned(),
                    children: Vec::new(),
                    info: info.clone(),
                };
                return panel_handle(cx.new(|cx| {
                    BrowserPagePanel::unavailable(
                        factory_session.clone(),
                        page,
                        saved,
                        error.to_string(),
                        cx,
                    )
                }));
            }
            let panel = cx.new(|cx| BrowserPagePanel::new(factory_session.clone(), page, cx));
            factory_pages.borrow_mut().insert(page, panel.clone());
            panel_handle(panel)
        });
        register_factory(area, "bootty.browser.page", factory.clone(), cx);
        register_factory(
            area,
            "bootty.browser",
            Rc::new(move |info, window, cx| {
                // The old singleton saved no page state. Restore its empty browser home.
                let info = match info {
                    PanelInfo::Panel(serde_json::Value::Null) => PanelInfo::Panel(
                        serde_json::json!({"page":1,"address":"about:blank","title":"Browser"}),
                    ),
                    other => other.clone(),
                };
                factory(&info, window, cx)
            }),
            cx,
        );
        Self {
            session,
            pages,
            allowed: true,
        }
    }

    pub(super) fn page_for_panel(&self, panel: PanelId) -> Option<u64> {
        self.pages
            .borrow()
            .iter()
            .find_map(|(page, view)| (PanelId::from(view.entity_id()) == panel).then_some(*page))
    }
}

impl WorkspaceDock {
    pub(crate) fn browser_session(&self) -> Entity<BrowserPanel> {
        self.browser.session.clone()
    }

    pub(crate) fn configure_browser(
        &self,
        config: bootty_config::config::BrowserConfig,
        conversation_target: Option<bootty_control::CommandTarget>,
        window_target: Option<bootty_control::CommandTarget>,
        attachment: bootty_agents::NativeBrowserAccess,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.browser.session.update(cx, |browser, cx| {
            browser.configure(
                config,
                conversation_target,
                window_target,
                attachment,
                window,
                cx,
            );
        });
    }

    fn execute_site_data_reset(
        &self,
        request: BrowserRequest,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = self
            .browser
            .session
            .update(cx, |browser, cx| browser.reset_site_data(window, cx));
        match receiver {
            Ok(receiver) => {
                cx.spawn_in(window, async move |_, _| {
                    let observed = receiver
                        .recv()
                        .await
                        .unwrap_or_else(|error| Err(error.to_string()));
                    request.complete(match observed {
                        Ok(()) => CommandOutcome::success(),
                        Err(message) => CommandOutcome::Failed {
                            code: "browser_reset_failed".into(),
                            message,
                        },
                    });
                })
                .detach();
            }
            Err(error) => request.complete(CommandOutcome::Failed {
                code: "browser_reset_failed".into(),
                message: error.to_string(),
            }),
        }
    }

    pub(crate) fn execute_browser(
        &mut self,
        request: BrowserRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pending) = &mut self.restoring {
            pending.browser.push(request);
            return;
        }
        if matches!(request.action, BrowserAction::Snapshot(_)) {
            self.browser.session.update(cx, |browser, cx| {
                browser.snapshot_request(request, window, cx);
            });
            return;
        }
        if matches!(
            request.action,
            BrowserAction::Capture | BrowserAction::CaptureAnnotation(_)
        ) {
            self.browser.session.update(cx, |browser, cx| {
                browser.capture_request(request, window, cx);
            });
            return;
        }
        if let Err(error) = request.begin() {
            request.complete(crate::commands::runtime::command_outcome_for_mux_error(
                error,
            ));
            return;
        }
        // A captured ID stays bound to that page, even after a different peer tab is selected.
        if request.page.is_some_and(|page| {
            !self
                .browser
                .session
                .read(cx)
                .pages()
                .iter()
                .any(|candidate| candidate.id == page)
        }) {
            request.complete(CommandOutcome::StaleTarget {
                message: "This browser page is closed.".into(),
            });
            return;
        }
        if matches!(request.action, BrowserAction::ResetSiteData) {
            self.execute_site_data_reset(request, window, cx);
            return;
        }
        let reveal = matches!(
            request.action,
            BrowserAction::NewTab(_) | BrowserAction::Open(_) | BrowserAction::Address
        );
        let close = matches!(request.action, BrowserAction::CloseTab(_));
        let result = self
            .browser
            .session
            .update(cx, |browser, cx| match request.page {
                Some(page) => browser.execute_on_page(page, request.action.clone(), window, cx),
                None => browser.execute(request.action.clone(), window, cx),
            });
        if let Err(error) = result {
            request.complete(CommandOutcome::Failed {
                code: "browser_unavailable".into(),
                message: error.to_string(),
            });
            return;
        }
        self.sync_browser_pages(window, cx);
        if reveal {
            let page = request
                .page
                .unwrap_or_else(|| self.browser.session.read(cx).selected());
            if let Some(panel) = self.browser.pages.borrow().get(&page).cloned() {
                self.area.update(cx, |area, cx| {
                    if !area.is_dock_open(DockPlacement::Right) {
                        area.toggle_dock(DockPlacement::Right, window, cx);
                    }
                    area.select_panel(PanelId::from(panel.entity_id()), window, cx);
                });
            }
        }
        self.sync_browser_visibility(cx);
        self.layout_changed(cx);
        if close {
            self.restore_terminal_after_browser_close(window, cx);
        }
        let outcome = if matches!(request.action, BrowserAction::NewTab(_)) {
            CommandOutcome::Success {
                value: serde_json::json!({"page_id": self.browser.session.read(cx).selected()}),
                warnings: Vec::new(),
            }
        } else {
            CommandOutcome::success()
        };
        request.complete(outcome);
    }

    fn restore_terminal_after_browser_close(&self, window: &Window, cx: &Context<Self>) {
        let browser = self.browser.session.read(cx);
        let page = browser.selected();
        let generation = browser.interaction_generation();
        // Kit delivers net panel activation in a spawned frame-end task, after defers.
        // Restore after that delivery, unless input or selection has since moved elsewhere.
        cx.spawn_in(window, async move |owner, cx| {
            _ = owner.update_in(cx, |this, window, cx| {
                let browser = this.browser.session.read(cx);
                if !window.is_window_active()
                    || browser.selected() != page
                    || browser.interaction_generation() != generation
                {
                    return;
                }
                if window.focused(cx).is_some_and(|focus| {
                    !this
                        .browser
                        .session
                        .focus_handle(cx)
                        .contains_focused(window, cx)
                        && !this.terminal.focus_handle(cx).contains_focused(window, cx)
                        && focus != this.focus
                }) {
                    return;
                }
                this.browser.session.update(cx, BrowserPanel::focus_host);
                this.terminal
                    .update(cx, |terminal, cx| terminal.focus_terminal(window, cx));
            });
        })
        .detach();
    }

    pub(super) fn sync_browser_pages(&self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshots = self.browser.session.read(cx).pages();
        let retired: Vec<_> = self
            .browser
            .pages
            .borrow()
            .iter()
            .filter(|(page, _)| !snapshots.iter().any(|snapshot| snapshot.id == **page))
            .map(|(page, panel)| (*page, panel.clone()))
            .collect();
        for (page, panel) in retired {
            self.area
                .update(cx, |area, cx| area.remove_panel(panel, window, cx));
            self.browser.pages.borrow_mut().remove(&page);
        }
        for snapshot in snapshots {
            if self.browser.pages.borrow().contains_key(&snapshot.id) {
                continue;
            }
            let panel =
                cx.new(|cx| BrowserPagePanel::new(self.browser.session.clone(), snapshot.id, cx));
            self.area.update(cx, |area, cx| {
                area.add_panel_view(
                    panel_handle(panel.clone()),
                    DockPlacement::Right,
                    None,
                    window,
                    cx,
                );
            });
            self.browser.pages.borrow_mut().insert(snapshot.id, panel);
        }
        self.area.update(cx, |_, cx| cx.notify());
        self.sync_browser_visibility(cx);
        self.layout_changed(cx);
    }

    pub(crate) fn set_browser_visible(&mut self, allowed: bool, cx: &mut Context<Self>) {
        let restoring = allowed && !self.browser.allowed;
        self.browser.allowed = allowed;
        if restoring {
            self.browser
                .session
                .update(cx, BrowserPanel::cancel_palette);
        }
        self.sync_browser_visibility(cx);
    }

    pub(crate) fn browser_page_icon(
        &self,
        panel: PanelId,
        cx: &App,
    ) -> Option<gpui_kit::component::IconName> {
        let page = self.browser.page_for_panel(panel)?;
        self.browser
            .session
            .read(cx)
            .pages()
            .into_iter()
            .find(|snapshot| snapshot.id == page)
            .map(|snapshot| snapshot.icon)
    }

    pub(crate) fn record_browser_interaction(&self, cx: &mut Context<Self>) {
        self.browser
            .session
            .update(cx, |browser, _| browser.record_interaction());
    }

    pub(crate) fn browser_has_native_views(&self, cx: &App) -> bool {
        self.browser.session.read(cx).has_native_views()
    }

    pub(crate) fn browser_page_active(&self, cx: &App) -> bool {
        let page = self.browser.session.read(cx).selected();
        let pages = self.browser.pages.borrow();
        let Some(panel) = pages.get(&page) else {
            return false;
        };
        let id = PanelId::from(panel.entity_id());
        let area = self.area.read(cx);
        self.present
            && self.browser.allowed
            && area.is_dock_open(DockPlacement::Right)
            && area.layout(DockPlacement::Right).is_some_and(|tree| {
                tree.find_panel_node(id)
                    .and_then(|node| tree.find_node(node))
                    .is_some_and(|node| match node.kind() {
                        PaneRef::Tabs { panels, active_ix } => panels.get(active_ix) == Some(&id),
                        PaneRef::Split { .. } => false,
                    })
            })
    }

    pub(super) fn sync_browser_visibility(&self, cx: &mut Context<Self>) {
        let visible = self.browser_page_active(cx);
        self.browser.session.update(cx, |browser, cx| {
            browser.set_visible(visible, cx);
        });
    }
}
