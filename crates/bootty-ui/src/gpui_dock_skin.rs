//! Workspace header composition over Kit's Dock appearance and behavior.

use std::{rc::Rc, sync::Arc};

use gpui_kit::base::Tab;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{
        BasePanelView, DockArea, DockAreaRenderer, DockContext, DockEvent, DockPlacement, DockSkin,
        DragPanel, DropIndicator, NodeId, PanelHandle, PanelState, TabGroupContext,
        TabGroupRenderer,
    },
};
use gpui_kit::{
    AnyElement, App, AsKeystroke as _, Axis, Context, Div, Entity, IntoElement, ParentElement,
    Render, SharedString, Stateful, Styled, WeakEntity, Window, WindowControlArea, div, prelude::*,
    px,
};

use crate::{gpui::chrome::GpuiChrome, gpui_dock::WorkspaceDock};

pub struct WorkspaceDockSkin {
    kit: Rc<DockSkin>,
    area: WeakEntity<DockArea>,
    owner: WeakEntity<WorkspaceDock>,
    chrome: Entity<GpuiChrome>,
}

impl WorkspaceDockSkin {
    pub(crate) fn new(
        owner: WeakEntity<WorkspaceDock>,
        chrome: Entity<GpuiChrome>,
        cx: &mut Context<DockArea>,
    ) -> Rc<Self> {
        let mut bottom_height = chrome.read(cx).dock_bottom_height();
        cx.observe(&chrome, move |_, chrome, cx| {
            let height = chrome.read(cx).dock_bottom_height();
            if height != bottom_height {
                bottom_height = height;
                cx.notify();
            }
        })
        .detach();
        Rc::new(Self {
            kit: DockSkin::new(cx),
            area: cx.weak_entity(),
            owner,
            chrome,
        })
    }
}

#[derive(Clone)]
struct DraggedDockResize(DockContext);

impl DockAreaRenderer for WorkspaceDockSkin {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        let area = self.area.clone();
        self.kit
            .frame(window, cx)
            // Blank dock chrome is not a keyboard input destination.
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| {
                window.prevent_default();
            })
            .on_drag_move::<DraggedDockResize>(|event, window, cx| {
                let dock = event.drag(cx).0.clone();
                dock.resize_to(event.event.position, window, cx);
            })
            .on_drop(move |drag: &DraggedDockResize, window, cx| {
                drag.0.resize_to(window.mouse_position(), window, cx);
                _ = area.update(cx, |_, cx| cx.emit(DockEvent::LayoutChanged));
            })
    }

    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        let chrome = if window.is_a11y_active() {
            div()
                .absolute()
                .size_full()
                .child(self.chrome.clone())
                .into_any_element()
        } else {
            self.chrome
                .clone()
                .cached(gpui_kit::StyleRefinement::default().absolute().size_full())
                .into_any_element()
        };
        self.kit
            .center_frame(window, cx)
            .relative()
            // Bottom segments belong below the terminals, within the side docks.
            .pb(self.chrome.read(cx).dock_bottom_height())
            .child(chrome)
            .when_some(self.owner.upgrade(), |frame, owner| {
                frame.child(owner.read(cx).titlebar.clone())
            })
    }

    fn split_frame(
        &self,
        node: NodeId,
        axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        let empty_terminal = self.owner.upgrade().and_then(|owner| {
            owner
                .read(cx)
                .empty_terminal
                .as_ref()
                .and_then(|(root, state)| (*root == node).then(|| state.clone()))
        });
        self.kit
            .split_frame(node, axis, window, cx)
            .relative()
            .when_some(empty_terminal, |frame, state| {
                frame.child(empty_terminal_view(
                    state,
                    self.owner.clone(),
                    self.chrome.read(cx).keymap_context(),
                    window,
                    cx,
                ))
            })
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let placement = dock.placement();
        let handle = dock_resize_handle(dock, cx);
        let (show_left, show_right) = self.chrome.read(cx).dock_presentation();
        let inset = self.chrome.read(cx).window_controls_inset();
        let header = if placement.is_left() {
            Some(
                dock_title_row(self.chrome.read(cx).panel_background())
                    .pl(px(inset))
                    .gap_2()
                    .child(
                        div()
                            .id("sidebar-window-drag-region")
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .items_center()
                            .gap_2()
                            .window_control_area(WindowControlArea::Drag)
                            // macOS requires an explicit move for custom titlebar content.
                            .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                window.start_window_move();
                            })
                            .child(crate::gpui::icon("bootty", 16.0, cx.theme().primary))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .truncate()
                                    .child("Bootty"),
                            ),
                    )
                    .when(show_left, |header| {
                        header.child(panel_toggle(
                            self.owner.clone(),
                            placement,
                            true,
                            &self.chrome,
                            cx,
                        ))
                    }),
            )
        } else if placement.is_right() {
            let width = dock_status_width(f32::from(dock.size()), show_right);
            let status = self
                .chrome
                .update(cx, |chrome, cx| chrome.dock_status(width, true, cx));
            Some(
                dock_title_row(self.chrome.read(cx).panel_background())
                    .when(show_right, |header| {
                        header.child(panel_toggle(
                            self.owner.clone(),
                            placement,
                            true,
                            &self.chrome,
                            cx,
                        ))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .justify_end()
                            .children(status),
                    ),
            )
        } else {
            None
        };
        div()
            .relative()
            .flex()
            .size_full()
            .flex_col()
            .children(header)
            .child(div().flex_1().min_h_0().w_full().child(content))
            .child(handle)
            .into_any_element()
    }

    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn BasePanelView>> {
        self.kit.build_placeholder(state, window, cx)
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(WorkspaceTabGroup {
            kit: self.kit.tab_group_renderer(),
            area: self.area.clone(),
            owner: self.owner.clone(),
            chrome: self.chrome.clone(),
        })
    }
}

fn dock_resize_handle(dock: &DockContext, cx: &App) -> Stateful<Div> {
    let placement = dock.placement();
    let id = match placement {
        DockPlacement::Left => "resize-dock-left",
        DockPlacement::Right => "resize-dock-right",
        DockPlacement::Bottom => "resize-dock-bottom",
        DockPlacement::Center => "resize-dock-center",
    };
    // Keep the grab area inside the dock: Base clips each region, including
    // the portion of its default resize handle that extends across the edge.
    div()
        .id(id)
        .group(id)
        .absolute()
        .occlude()
        .when(placement.is_left(), |handle| {
            handle
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(8.))
                .cursor_col_resize()
        })
        .when(placement.is_right(), |handle| {
            handle
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(8.))
                .cursor_col_resize()
        })
        .when(placement.is_bottom(), |handle| {
            handle
                .top_0()
                .left_0()
                .right_0()
                .h(px(8.))
                .cursor_row_resize()
        })
        .child(
            div()
                .absolute()
                .bg(cx.theme().border)
                .group_hover(id, |line| line.bg(cx.theme().primary))
                .when(placement.is_left(), |line| {
                    line.right_0().top_0().bottom_0().w(px(1.))
                })
                .when(placement.is_right(), |line| {
                    line.left_0().top_0().bottom_0().w(px(1.))
                })
                .when(placement.is_bottom(), |line| {
                    line.top_0().left_0().right_0().h(px(1.))
                }),
        )
        .on_drag(DraggedDockResize(dock.clone()), |drag, _, window, cx| {
            cx.stop_propagation();
            if !drag.0.is_open() {
                drag.0.toggle(window, cx);
            }
            drag.0.resize_to(window.mouse_position(), window, cx);
            cx.new(|_| gpui_kit::Empty)
        })
}

#[derive(Clone)]
struct WorkspaceTabGroup {
    kit: Rc<dyn TabGroupRenderer>,
    area: WeakEntity<DockArea>,
    owner: WeakEntity<WorkspaceDock>,
    chrome: Entity<GpuiChrome>,
}

pub struct WorkspaceTitleBar {
    chrome: Entity<GpuiChrome>,
    dock: WeakEntity<WorkspaceDock>,
    area: WeakEntity<DockArea>,
}

impl WorkspaceTitleBar {
    pub(crate) fn new(
        chrome: Entity<GpuiChrome>,
        dock: WeakEntity<WorkspaceDock>,
        area: &Entity<DockArea>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&chrome, |_, _, cx| cx.notify()).detach();
        cx.observe(area, |_, _, cx| cx.notify()).detach();
        Self {
            chrome,
            dock,
            area: area.downgrade(),
        }
    }
}

impl Render for WorkspaceTitleBar {
    #[expect(
        clippy::too_many_lines,
        reason = "one titlebar layout owns the notch split"
    )]
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let area = self.area.upgrade();
        let left_open = area
            .as_ref()
            .is_some_and(|area| area.read(cx).is_dock_open(DockPlacement::Left));
        let left_width = if left_open {
            area.as_ref()
                .and_then(|area| area.read(cx).dock_size(DockPlacement::Left))
                .map_or(0.0, f32::from)
        } else {
            0.0
        };
        let right_open = area
            .as_ref()
            .is_some_and(|area| area.read(cx).is_dock_open(DockPlacement::Right));
        let right_width = if right_open {
            area.as_ref()
                .and_then(|area| area.read(cx).dock_size(DockPlacement::Right))
                .map_or(0.0, f32::from)
        } else {
            0.0
        };
        let (status, inset, show_left, show_right, notch_span) =
            self.chrome.update(cx, |chrome, cx| {
                let (show_left, show_right) = chrome.dock_presentation();
                (
                    chrome.dock_status(
                        if right_open {
                            dock_status_width(right_width, show_right)
                        } else {
                            0.0
                        },
                        false,
                        cx,
                    ),
                    if left_open {
                        0.0
                    } else {
                        chrome.window_controls_inset()
                    },
                    show_left && !left_open,
                    show_right && !right_open,
                    chrome.notch_span(),
                )
            });
        let first_inset = inset + if show_left { 32.0 } else { 0.0 };
        let notch = notch_span.map(|(left, _)| crate::gpui::tabs::NotchTabLayout {
            width: (left - left_width - first_inset).max(0.0),
            inset: first_inset,
            height: crate::gpui::UI_TAB_BAR_HEIGHT,
            wrap: self.chrome.read(cx).wrap_tabs_at_notch(),
        });
        let tabs = self
            .chrome
            .update(cx, |chrome, cx| chrome.dock_tabs(notch, cx));
        let controls = div()
            .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
            .flex()
            .items_center()
            .when_some(notch_span, |controls, (_, right)| {
                controls
                    .absolute()
                    .top_0()
                    .right_0()
                    .left(px((right - left_width).max(0.0)))
                    .justify_end()
            })
            .children(status)
            .when(show_right, |element| {
                element.child(panel_toggle(
                    self.dock.clone(),
                    DockPlacement::Right,
                    false,
                    &self.chrome,
                    cx,
                ))
            });
        div()
            .id("workspace-titlebar")
            .relative()
            .flex_none()
            .w_full()
            .min_w_0()
            .bg(self.chrome.read(cx).panel_background())
            .window_control_area(WindowControlArea::Drag)
            .when(notch.is_none(), |bar| {
                bar.flex()
                    .items_center()
                    .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
                    .pl(px(inset))
                    .pr_1()
            })
            .when(show_left, |bar| {
                bar.child(
                    div()
                        .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
                        .flex()
                        .items_center()
                        .when(notch.is_some(), |toggle| {
                            toggle.absolute().top_0().left(px(inset))
                        })
                        .child(panel_toggle(
                            self.dock.clone(),
                            DockPlacement::Left,
                            false,
                            &self.chrome,
                            cx,
                        )),
                )
            })
            .child(
                div()
                    .min_w_0()
                    .when(notch.is_none(), |tabs| tabs.flex_1().h_full())
                    .children(tabs),
            )
            .child(controls)
    }
}

fn group_placement(area: &DockArea, target: NodeId) -> Option<DockPlacement> {
    [
        DockPlacement::Center,
        DockPlacement::Left,
        DockPlacement::Right,
        DockPlacement::Bottom,
    ]
    .into_iter()
    .find(|placement| {
        area.layout(*placement)
            .is_some_and(|tree| tree.find_node(target).is_some())
    })
}

fn panel_toggle(
    owner: WeakEntity<WorkspaceDock>,
    placement: DockPlacement,
    open: bool,
    chrome: &Entity<GpuiChrome>,
    cx: &App,
) -> Button {
    let (id, label, icon) = if placement == DockPlacement::Left {
        ("toggle-left-dock", "Toggle left dock", IconName::PanelLeft)
    } else {
        (
            "toggle-right-dock",
            "Toggle right dock",
            IconName::PanelRight,
        )
    };
    let action = if placement == DockPlacement::Left {
        crate::commands::DockAction::ToggleLeft
    } else {
        crate::commands::DockAction::ToggleRight
    };
    Button::new(id)
        .icon(icon)
        .ghost()
        .small()
        .rounded(cx.theme().radius)
        .selected(open)
        .tooltip_with_action(
            label,
            &crate::gpui_actions::dock_binding_action(action),
            Some(chrome.read(cx).keymap_context()),
        )
        .accessibility_label(label)
        .on_click(move |_, window, cx| {
            _ = owner.update(cx, |owner, cx| {
                owner.invoke_action(
                    if placement == DockPlacement::Left {
                        crate::commands::DockAction::ToggleLeft
                    } else {
                        crate::commands::DockAction::ToggleRight
                    },
                    None,
                    window,
                    cx,
                );
            });
        })
}

impl WorkspaceTabGroup {
    fn tabs_visible(group: &TabGroupContext, cx: &App) -> bool {
        group.active_panel().is_none_or(|panel| {
            !terminal_panel_name(panel.panel_name(cx))
                && !matches!(
                    panel.panel_name(cx),
                    "bootty.native-session" | "bootty.sessions"
                )
        })
    }

    fn render_tool_navigation(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        use bootty_config::config::{TabCloseButton, TabClosePosition, TabConfig};
        let config = TabConfig {
            close_button: TabCloseButton::Always,
            close_position: TabClosePosition::Right,
            ..TabConfig::default()
        };
        let browser = self
            .owner
            .upgrade()
            .map(|owner| owner.read(cx).browser.clone());
        let mut tabs = Vec::new();
        let mut selected = None;
        for (ix, panel) in group.panels().iter().enumerate() {
            if !panel.visible(cx) {
                continue;
            }
            if panel.panel_name(cx) == "bootty.browser" {
                if let Some(browser) = &browser {
                    for (id, label) in browser.read(cx).tabs() {
                        let active = ix == group.active_ix() && browser.read(cx).selected() == id;
                        if active {
                            selected = Some(tabs.len());
                        }
                        tabs.push(Self::render_browser_tab(
                            browser,
                            (id, label),
                            group,
                            ix,
                            config,
                            cx,
                        ));
                    }
                }
            } else if let Some(tab) = self.render_tool_tab(group, ix, config, window, cx) {
                if ix == group.active_ix() {
                    selected = Some(tabs.len());
                }
                tabs.push(crate::gpui::tabs::ScrollableTab {
                    id: format!("tool-tab:{:?}", panel.panel_id(cx)),
                    title: None,
                    focus: None,
                    tab,
                });
            }
        }
        let owner = self.owner.clone();
        div()
            .id("workspace-tool-navigation")
            .debug_selector(|| "workspace-tool-navigation".to_owned())
            .flex()
            .w_full()
            .min_w_0()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(crate::gpui::tabs::ScrollableTabBar {
                        id: format!("workspace-tools:{:?}", group.node()).into(),
                        config,
                        background: self.chrome.read(cx).panel_background(),
                        tabs,
                        selected,
                        notch: None,
                        end: div().min_w_1().into_any_element(),
                    }),
            )
            .child(
                Button::new("new-sidebar-browser-tab")
                    .icon(IconName::Plus)
                    .ghost()
                    .small()
                    .size_6()
                    .accessibility_label("New browser tab")
                    .tooltip("New browser tab")
                    .on_click(move |_, window, cx| {
                        _ = owner.update(cx, |dock, cx| dock.new_browser_tab(window, cx));
                    }),
            )
            .into_any_element()
    }

    fn render_browser_tab(
        browser: &Entity<crate::gpui_browser_panel::BrowserPanel>,
        page: (u64, &str),
        group: &TabGroupContext,
        ix: usize,
        config: bootty_config::config::TabConfig,
        cx: &App,
    ) -> crate::gpui::tabs::ScrollableTab {
        let (id, label) = page;
        let active = ix == group.active_ix() && browser.read(cx).selected() == id;
        let tab_id = format!("browser-tab:{:?}:{id}", browser.entity_id());
        let select = group.clone();
        let select_browser = browser.clone();
        let close_browser = browser.clone();
        let close = Button::new(SharedString::from(format!("close-{tab_id}")))
            .icon(IconName::Close)
            .ghost()
            .xsmall()
            .size_4()
            .accessibility_label(format!("Close {label}"))
            .tooltip("Close tab")
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                close_browser.update(cx, |browser, cx| browser.close_tab(id, window, cx));
            })
            .into_any_element();
        let tab = crate::gpui::tabs::tab(
            tab_id.clone().into(),
            config.appearance,
            active,
            cx.theme().primary,
            cx,
        )
        .accessibility_label(label.to_owned())
        .child(crate::gpui::tabs::content(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(Icon::new(IconName::Globe).small())
                .child(div().max_w_40().truncate().child(label.to_owned()))
                .into_any_element(),
            Some(close),
            tab_id.clone().into(),
            config,
        ))
        .on_click(move |_, window, cx| {
            select_browser.update(cx, |browser, cx| browser.select_tab(id, window, cx));
            select.select_tab(ix, window, cx);
        });
        crate::gpui::tabs::ScrollableTab {
            id: tab_id,
            title: None,
            focus: None,
            tab,
        }
    }

    fn render_tool_tab(
        &self,
        group: &TabGroupContext,
        ix: usize,
        config: bootty_config::config::TabConfig,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Tab> {
        let panel = group.panels().get(ix)?;
        let id = panel.panel_id(cx);
        let handle = PanelHandle::of(panel);
        let (fallback, icon) = panel_identity(panel.panel_name(cx));
        let label = handle
            .and_then(|handle| handle.tab_name(cx))
            .unwrap_or_else(|| fallback.into());
        let title = handle.map_or_else(
            || label.clone().into_any_element(),
            |handle| handle.title(window, cx),
        );
        let suffix = handle
            .and_then(|handle| handle.title_suffix(window, cx))
            .or_else(|| {
                panel.closable(cx).then(|| {
                    let owner = self.owner.clone();
                    Button::new(SharedString::from(format!("close-tool:{id:?}")))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .size_4()
                        .accessibility_label(format!("Close {label}"))
                        .tooltip("Close tab")
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            _ = owner.update(cx, |dock, cx| dock.close_tool_tab(id, window, cx));
                        })
                        .into_any_element()
                })
            });
        let select = group.clone();
        let tab_id: SharedString = format!("tool-tab:{id:?}").into();
        Some(
            crate::gpui::tabs::tab(
                tab_id.clone(),
                config.appearance,
                ix == group.active_ix(),
                cx.theme().primary,
                cx,
            )
            .accessibility_label(label)
            .child(crate::gpui::tabs::content(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(Icon::new(icon).small())
                    .child(title)
                    .into_any_element(),
                suffix,
                tab_id,
                config,
            ))
            .on_click(move |_, window, cx| select.select_tab(ix, window, cx)),
        )
    }

    fn render_tabs(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let placement = self
            .area
            .upgrade()
            .and_then(|area| group_placement(area.read(cx), group.node()));
        if placement == Some(DockPlacement::Right) {
            return self.render_tool_navigation(group, window, cx);
        }
        let tab_config = self.chrome.read(cx).dock_tabs_config();
        let tab_accent = self.chrome.read(cx).tab_accent();
        let visible = group
            .panels()
            .iter()
            .enumerate()
            .filter(|(_, panel)| panel.visible(cx))
            .map(|(ix, _)| ix)
            .collect::<Vec<_>>();
        let selected = visible
            .iter()
            .position(|ix| *ix == group.active_ix())
            .unwrap_or(0);
        let tabs = visible
            .into_iter()
            .filter_map(|ix| {
                let panel = group.panels().get(ix)?;
                let id = format!("{:?}", panel.panel_id(cx));
                Some(crate::gpui::tabs::ScrollableTab {
                    id,
                    title: None,
                    focus: None,
                    tab: Self::render_group_tab(group, ix, tab_config, tab_accent, window, cx)?,
                })
            })
            .collect();
        crate::gpui::tabs::ScrollableTabBar {
            id: format!("workspace-tabs-{:?}", group.node()).into(),
            config: tab_config,
            background: self.chrome.read(cx).panel_background(),
            tabs,
            selected: Some(selected),
            notch: None,
            end: div()
                .id("after-tabs")
                .h_full()
                .flex_1()
                .min_w_4()
                .into_any_element(),
        }
        .into_any_element()
    }
}

impl WorkspaceTabGroup {
    #[allow(clippy::too_many_arguments)]
    fn render_group_tab(
        group: &TabGroupContext,
        ix: usize,
        tab_config: bootty_config::config::TabConfig,
        accent: gpui_kit::Hsla,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Tab> {
        let panel = group.panels().get(ix)?;
        let id = panel.panel_id(cx);
        let handle = PanelHandle::of(panel);
        let label = handle
            .and_then(|h| h.tab_name(cx))
            .unwrap_or_else(|| panel_identity(panel.panel_name(cx)).0.into());
        let title =
            handle.map_or_else(|| label.clone().into_any_element(), |h| h.title(window, cx));
        let suffix = handle.and_then(|h| h.title_suffix(window, cx)).or_else(|| {
            panel
                .closable(cx)
                .then(|| close_panel_tab(group, id, &label))
        });
        let selected = !group.is_collapsed() && ix == group.active_ix();
        let hover_group = SharedString::from(format!("panel-tab-hover-{id:?}"));
        let title = div()
            .id(SharedString::from(format!("panel-tab-{id:?}")))
            .flex()
            .items_center()
            .gap_2()
            .child(Icon::new(panel_identity(panel.panel_name(cx)).1).small())
            .child(title);
        let select = group.clone();
        let tab = crate::gpui::tabs::tab(
            hover_group.clone(),
            tab_config.appearance,
            selected,
            accent,
            cx,
        )
        .group(hover_group.clone())
        .accessibility_label(label.clone())
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
        })
        .child(crate::gpui::tabs::content(
            title.into_any_element(),
            suffix,
            hover_group,
            tab_config,
        ))
        .selected(
            !group.is_collapsed() && group.active_panel().is_some_and(|p| p.panel_id(cx) == id),
        )
        .on_click(move |_, window, cx| select.select_tab(ix, window, cx));
        Some(tab)
    }
}

fn close_panel_tab(
    group: &TabGroupContext,
    id: gpui_kit::component::dock::PanelId,
    label: &str,
) -> AnyElement {
    let group = group.clone();
    Button::new("close-tab")
        .icon(IconName::Close)
        .ghost()
        .xsmall()
        .size_4()
        .tooltip("Close tab")
        .accessibility_label(format!("Close {label}"))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            group.close(id, window, cx);
        })
        .into_any_element()
}

impl TabGroupRenderer for WorkspaceTabGroup {
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.kit
            .frame(group, window, cx)
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| {
                window.prevent_default();
            })
            .bg(self.chrome.read(cx).panel_background())
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.kit.content_frame(group, window, cx)
    }

    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let tabs_visible = Self::tabs_visible(group, cx);
        if !tabs_visible {
            return div().into_any_element();
        }
        let tabs = self.render_tabs(group, window, cx);
        if self
            .area
            .upgrade()
            .and_then(|area| group_placement(area.read(cx), group.node()))
            == Some(DockPlacement::Right)
        {
            return tabs;
        }
        let classic = self.chrome.read(cx).dock_tabs_config().appearance
            == bootty_config::config::TabAppearance::Classic;
        div()
            .id("workspace-header")
            .flex()
            .items_center()
            .flex_shrink_0()
            .w_full()
            .min_w_0()
            .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
            .overflow_hidden()
            .bg(if classic {
                cx.theme().tab_bar
            } else {
                self.chrome.read(cx).panel_background()
            })
            .pr_1()
            .child(div().flex_1().min_w_0().h_full().child(tabs))
            .into_any_element()
    }

    fn render_active_panel(
        &self,
        panel: gpui_kit::AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let content = if window.is_a11y_active() && !group.is_collapsed() {
            // GPUI's cached replay omits accessibility nodes. Rebuild the subtree while
            // assistive technology is active until upstream replays those registrations.
            div()
                .id("tab-content")
                .overflow_y_scroll()
                .overflow_x_hidden()
                .flex_1()
                .child(div().absolute().size_full().child(panel))
                .into_any_element()
        } else {
            self.kit.render_active_panel(panel, group, window, cx)
        };
        if group.active_panel().is_some_and(|panel| {
            terminal_panel_name(panel.panel_name(cx))
                || panel.panel_name(cx) == "bootty.native-session"
        }) {
            // Kit allows drops into any unlocked group. The mux-owned center only accepts
            // terminal pane drags, whose separate type and command path stay inside its view.
            return div()
                .id("locked-terminal-content")
                .size_full()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .relative()
                .on_drag_move::<DragPanel>(|_, _, cx| cx.stop_propagation())
                .on_drop::<DragPanel>(|_, _, cx| cx.stop_propagation())
                .child(content)
                .into_any_element();
        }
        content
    }

    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.kit.render_drop_indicator(indicator, window, cx)
    }

    fn render_empty(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let content = self.kit.render_empty(group, window, cx);
        Some(
            div()
                .id("empty-group")
                .size_full()
                .children(content)
                .into_any_element(),
        )
    }
}

fn empty_terminal_view(
    state: crate::workspace_composition::EmptyTerminalState,
    owner: WeakEntity<WorkspaceDock>,
    keymap_context: &str,
    window: &Window,
    cx: &App,
) -> AnyElement {
    use crate::workspace_composition::EmptyTerminalState;

    let content = div()
        .id("empty-terminal")
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .p_6()
        .text_sm()
        .text_color(cx.theme().muted_foreground);
    match state {
        EmptyTerminalState::Loading => content.child("Loading terminals…").into_any_element(),
        EmptyTerminalState::Unavailable(reason) => content
            .child("Terminal unavailable")
            .child(div().max_w_full().child(reason))
            .into_any_element(),
        EmptyTerminalState::Ready {
            can_create,
            has_session,
        } => {
            let (action, title, label, unavailable) = if has_session {
                (
                    crate::action_catalog::Command::NewTab.action(),
                    "No open terminals",
                    "New terminal",
                    "This backend does not support creating terminals.",
                )
            } else {
                (
                    crate::action_catalog::Command::NewSession.action(),
                    "No sessions",
                    "New session",
                    "This backend does not support creating sessions.",
                )
            };
            let binding_action = crate::gpui_actions::InvokeCommand::new(
                bootty_control::CommandInvocation::from_action(
                    action,
                    bootty_control::Caller::Keybinding,
                ),
            );
            let shortcut = gpui_kit::KeyContext::parse(keymap_context)
                .ok()
                .and_then(|context| {
                    window
                        .highest_precedence_binding_for_action_in_context(&binding_action, context)
                });
            content
                .child(title)
                .child(
                    Button::new("empty-terminal-new")
                        .label(label)
                        .icon(IconName::Plus)
                        .primary()
                        .disabled(!can_create)
                        .on_click(move |_, window, cx| {
                            _ = owner.update(cx, |owner, cx| {
                                owner.submit_command(
                                    bootty_control::CommandInvocation::from_action(
                                        action,
                                        bootty_control::Caller::Internal,
                                    ),
                                    window,
                                    cx,
                                );
                            });
                        }),
                )
                .when(can_create, |content| {
                    content.when_some(shortcut, |content, binding| {
                        content.child(div().flex().gap_1().children(
                            binding.keystrokes().iter().map(|stroke| {
                                crate::gpui::keybinding_element(stroke.as_keystroke())
                            }),
                        ))
                    })
                })
                .when(!can_create, |content| content.child(unavailable))
                .into_any_element()
        }
    }
}

pub fn panel_identity(name: &str) -> (&'static str, IconName) {
    let name = match name {
        "bootty.sidebar" => "bootty.sessions",
        "bootty.attachment" => "bootty.terminal",
        name => name,
    };
    crate::commands::panel_descriptor(name).map_or(("Document", IconName::FileText), |panel| {
        (panel.label, panel.icon.clone())
    })
}

fn terminal_panel_name(name: &str) -> bool {
    matches!(name, "bootty.terminal" | "bootty.attachment")
}

fn dock_status_width(width: f32, toggle: bool) -> f32 {
    (width - if toggle { 36.0 } else { 8.0 }).max(0.0)
}

fn dock_title_row(background: gpui_kit::Hsla) -> Div {
    div()
        .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .min_w_0()
        .px_1()
        .bg(background)
        .window_control_area(WindowControlArea::Drag)
}
