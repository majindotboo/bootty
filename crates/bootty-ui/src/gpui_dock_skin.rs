//! Workspace header composition over Kit's Dock appearance and behavior.

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
    sync::Arc,
};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, Selectable as _,
    Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{
        BasePanelView, DockArea, DockAreaRenderer, DockContext, DockEvent, DockPlacement, DockSkin,
        DragPanel, DropIndicator, NodeId, PanelHandle, PanelState, TabGroupContext,
        TabGroupRenderer, TilesRenderer,
    },
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    tab::{Tab, TabBar},
};
use gpui_kit::{
    AnyElement, App, AsKeystroke as _, Axis, Context, Div, Entity, IntoElement, ParentElement,
    Render, ScrollHandle, SharedString, Stateful, Styled, WeakEntity, Window, WindowControlArea,
    div, prelude::*, px,
};

use crate::{gpui::chrome::GpuiChrome, gpui_dock::WorkspaceDock};

pub struct WorkspaceDockSkin {
    kit: Rc<DockSkin>,
    area: WeakEntity<DockArea>,
    owner: WeakEntity<WorkspaceDock>,
    chrome: Entity<GpuiChrome>,
    always_show_tabs: Rc<RefCell<HashSet<NodeId>>>,
    always_hide_tabs: Rc<RefCell<HashSet<NodeId>>>,
}

impl WorkspaceDockSkin {
    pub(crate) fn new(
        owner: WeakEntity<WorkspaceDock>,
        chrome: Entity<GpuiChrome>,
        always_show_tabs: Rc<RefCell<HashSet<NodeId>>>,
        always_hide_tabs: Rc<RefCell<HashSet<NodeId>>>,
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
            always_show_tabs,
            always_hide_tabs,
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
        self.kit
            .center_frame(window, cx)
            .relative()
            // Bottom segments belong below the terminals, within the side docks.
            .pb(self.chrome.read(cx).dock_bottom_height())
            .child(
                self.chrome
                    .clone()
                    .cached(gpui_kit::StyleRefinement::default().absolute().size_full()),
            )
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
        let (show_left, show_right, _, _) = self.chrome.read(cx).dock_presentation();
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
            always_show_tabs: self.always_show_tabs.clone(),
            always_hide_tabs: self.always_hide_tabs.clone(),
            scroll: ScrollHandle::default(),
            active: Cell::new(None),
            reveal_active: Rc::new(Cell::new(true)),
        })
    }

    fn tiles_renderer(&self) -> Rc<dyn TilesRenderer> {
        self.kit.tiles_renderer()
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
    scroll: ScrollHandle,
    active: Cell<Option<usize>>,
    reveal_active: Rc<Cell<bool>>,
    kit: Rc<dyn TabGroupRenderer>,
    area: WeakEntity<DockArea>,
    owner: WeakEntity<WorkspaceDock>,
    chrome: Entity<GpuiChrome>,
    always_show_tabs: Rc<RefCell<HashSet<NodeId>>>,
    always_hide_tabs: Rc<RefCell<HashSet<NodeId>>>,
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let area = self.area.upgrade();
        let left_open = area
            .as_ref()
            .is_some_and(|area| area.read(cx).is_dock_open(DockPlacement::Left));
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
        let (status, tabs, inset, show_left, show_right) = self.chrome.update(cx, |chrome, cx| {
            let (show_left, show_right, _, _) = chrome.dock_presentation();
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
                chrome.dock_tabs(cx),
                if left_open {
                    0.0
                } else {
                    chrome.window_controls_inset()
                },
                show_left && !left_open,
                show_right && !right_open,
            )
        });

        div()
            .id("workspace-titlebar")
            .flex_none()
            .flex()
            .items_center()
            .w_full()
            .min_w_0()
            .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
            .overflow_hidden()
            .pl(px(inset))
            .pr_1()
            .bg(self.chrome.read(cx).panel_background())
            .window_control_area(WindowControlArea::Drag)
            .when(show_left, |element| {
                element.child(panel_toggle(
                    self.dock.clone(),
                    DockPlacement::Left,
                    false,
                    &self.chrome,
                    cx,
                ))
            })
            .child(div().flex_1().min_w_0().h_full().children(tabs))
            .children(status)
            .when(show_right, |element| {
                element.child(panel_toggle(
                    self.dock.clone(),
                    DockPlacement::Right,
                    false,
                    &self.chrome,
                    cx,
                ))
            })
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
    fn tabs_visible(&self, group: &TabGroupContext, cx: &App) -> bool {
        use bootty_config::config::PanelTabs;
        if group
            .active_panel()
            .is_some_and(|panel| terminal_panel_name(panel.panel_name(cx)))
        {
            return false;
        }
        if self.always_hide_tabs.borrow().contains(&group.node()) {
            return false;
        }
        if self.always_show_tabs.borrow().contains(&group.node()) {
            return true;
        }
        let placement = self
            .area
            .upgrade()
            .and_then(|area| group_placement(area.read(cx), group.node()));
        if !matches!(placement, Some(DockPlacement::Left | DockPlacement::Right)) {
            return true;
        }
        match self.chrome.read(cx).dock_presentation().3 {
            PanelTabs::Automatic => group.panels().iter().filter(|p| p.visible(cx)).count() > 1,
            PanelTabs::Always => true,
            PanelTabs::Never => false,
        }
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
        let tab_config = self.chrome.read(cx).dock_tabs_config();
        let style = if matches!(placement, Some(DockPlacement::Left | DockPlacement::Right)) {
            self.chrome.read(cx).dock_presentation().2
        } else {
            bootty_config::config::PanelTabStyle::IconsAndText
        };
        let visible = group
            .panels()
            .iter()
            .enumerate()
            .filter(|(_, panel)| panel.visible(cx))
            .map(|(ix, _)| ix)
            .collect::<Vec<_>>();
        if self.active.replace(Some(group.active_ix())) != Some(group.active_ix()) {
            self.reveal_active.set(true);
        }
        let selected = visible
            .iter()
            .position(|ix| *ix == group.active_ix())
            .unwrap_or(0);
        let tab_indices = visible.clone();
        let select_group = group.clone();
        let tabs = visible
            .into_iter()
            .filter_map(|ix| self.render_group_tab(group, ix, style, tab_config, window, cx))
            .collect::<Vec<_>>();
        let target = group.clone();
        let tabs = TabBar::new("workspace-tabs")
            .with_variant(crate::gpui::tabs::variant(tab_config.appearance))
            .with_size(crate::gpui::tabs::size(tab_config.appearance))
            .min_w_full()
            .flex_shrink_0()
            .selected_index(selected)
            .on_click(move |ix, window, cx| {
                if let Some(index) = tab_indices.get(*ix) {
                    select_group.select_tab(*index, window, cx);
                }
            })
            .children(tabs)
            // Kit includes the trailing context-menu target when a suffix is present.
            .suffix(gpui_kit::Empty)
            .last_empty_space(div().id("after-tabs").h_full().flex_1().min_w_4().when(
                group.is_droppable(),
                |e| {
                    e.on_drop(move |drag: &DragPanel, window, cx| {
                        let ix = (drag.source() == target.node())
                            .then(|| target.panels().len().saturating_sub(1));
                        target.drop_panel(drag.clone(), ix, false, window, cx);
                    })
                },
            ));
        let tabs = crate::gpui::tabs::blend_bar(
            tabs,
            tab_config.appearance,
            self.chrome.read(cx).panel_background(),
        );
        div()
            .id("workspace-tab-scroll")
            .flex()
            .w_full()
            .min_w_0()
            .h_full()
            .overflow_x_scroll()
            .track_scroll(&self.scroll)
            .child(tabs)
            .into_any_element()
    }
}

impl WorkspaceTabGroup {
    fn render_group_tab(
        &self,
        group: &TabGroupContext,
        ix: usize,
        style: bootty_config::config::PanelTabStyle,
        tab_config: bootty_config::config::TabConfig,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Tab> {
        let panel = group.panels().get(ix)?;
        let terminal_panel = terminal_panel_name(panel.panel_name(cx));
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
        let scroll = self.scroll.clone();
        let reveal = self.reveal_active.clone();
        let selected = ix == group.active_ix();
        let hover_group = SharedString::from(format!("panel-tab-hover-{id:?}"));
        let title = div()
            .on_prepaint(move |bounds, window, _| {
                reveal_tab(bounds, &scroll, &reveal, selected, window);
            })
            .id(SharedString::from(format!("panel-tab-{id:?}")))
            .flex()
            .items_center()
            .gap_2()
            .when(style != bootty_config::config::PanelTabStyle::Text, |e| {
                e.child(Icon::new(panel_identity(panel.panel_name(cx)).1).small())
            })
            .when(style != bootty_config::config::PanelTabStyle::Icons, |e| {
                e.child(title)
            });
        let select = group.clone();
        let tab = Tab::new()
            .group(hover_group.clone())
            .aria_label(label.clone())
            .tooltip({
                let label = label.clone();
                move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
                }
            })
            .child(crate::gpui::tabs::content(
                title.into_any_element(),
                suffix,
                hover_group,
                tab_config,
                (selected
                    && tab_config.appearance == bootty_config::config::TabAppearance::Segmented)
                    .then_some(cx.theme().secondary_active),
            ))
            .selected(
                !group.is_collapsed() && group.active_panel().is_some_and(|p| p.panel_id(cx) == id),
            )
            .on_click(move |_, window, cx| select.select_tab(ix, window, cx));
        let drag = (!terminal_panel && group.is_draggable())
            .then(|| group.drag_panel(ix, cx))
            .flatten();
        Some(
            tab.when_some(drag, |tab, drag| {
                tab.on_drag(drag, move |drag, offset, window, cx| {
                    cx.stop_propagation();
                    drag.set_drag_offset(offset);
                    drag.set_preview_size(gpui_kit::size(
                        px(f32::from(window.rem_size()) * 10.),
                        px(crate::gpui::UI_TAB_BAR_HEIGHT),
                    ));
                    cx.new(|_| TabPreview(label.clone()))
                })
            })
            .when(group.is_droppable(), |tab| {
                let group = group.clone();
                tab.drag_over::<DragPanel>(|tab, _, _, cx| {
                    tab.border_l_2().border_color(cx.theme().drag_border)
                })
                .on_drop(move |drag: &DragPanel, window, cx| {
                    group.drop_panel(drag.clone(), Some(ix), true, window, cx);
                })
            }),
        )
    }
}

fn reveal_tab(
    bounds: gpui_kit::Bounds<gpui_kit::Pixels>,
    scroll: &ScrollHandle,
    reveal: &Cell<bool>,
    selected: bool,
    window: &mut Window,
) {
    if selected && reveal.replace(false) {
        let viewport = scroll.bounds();
        let mut offset = scroll.offset();
        if bounds.right() > viewport.right() {
            offset.x =
                px(f32::from(offset.x) - (f32::from(bounds.right()) - f32::from(viewport.right())));
        }
        if bounds.left() < viewport.left() {
            offset.x =
                px(f32::from(offset.x) + (f32::from(viewport.left()) - f32::from(bounds.left())));
        }
        if offset != scroll.offset() {
            scroll.set_offset(offset);
            window.refresh();
        }
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

struct TabPreview(SharedString);
impl Render for TabPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_40()
            .h_8()
            .px_3()
            .flex()
            .items_center()
            .overflow_hidden()
            .bg(cx.theme().tab_active)
            .text_color(cx.theme().tab_foreground)
            .child(self.0.clone())
    }
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
        let tabs_visible = self.tabs_visible(group, cx);
        if !tabs_visible {
            return div().into_any_element();
        }
        let tabs = self.render_tabs(group, window, cx);
        let owner = self.owner.clone();
        let always = self.always_show_tabs.borrow().contains(&group.node());
        let menu_group = group.clone();
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
            .context_menu(move |menu, window, cx| {
                panel_menu(&owner, &menu_group, always, menu, window, cx)
            })
            .into_any_element()
    }

    fn render_active_panel(
        &self,
        panel: gpui_kit::AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let content = self.kit.render_active_panel(panel, group, window, cx);
        if group
            .active_panel()
            .is_some_and(|panel| terminal_panel_name(panel.panel_name(cx)))
        {
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
        if self.tabs_visible(group, cx) {
            return content;
        }
        let owner = self.owner.clone();
        let group = group.clone();
        let always = self.always_show_tabs.borrow().contains(&group.node());
        let drag = group
            .is_draggable()
            .then(|| group.drag_panel(group.active_ix(), cx))
            .flatten();
        let drag_label = group
            .active_panel()
            .and_then(PanelHandle::of)
            .and_then(|panel| panel.tab_name(cx))
            .unwrap_or_else(|| "Panel".into());
        // A tab-less native panel gets a full-width hover strip along its top edge, tty7-style.
        // The strip reveals the drag pill without consuming layout space. Terminal leaves are
        // excluded: their mux pane grips already own the top-center hover affordance.
        let hover_group = SharedString::from(format!("hidden-panel-handle-{:?}", group.node()));
        let handle = {
            let pill = div()
                .id("hidden-panel-drag-handle")
                .mx_auto()
                .mt_1()
                .w(px(28.0))
                .h(px(14.0))
                .rounded(cx.theme().radius)
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .opacity(0.0)
                .group_hover(hover_group.clone(), |style| style.opacity(0.75))
                .hover(|style| style.bg(cx.theme().secondary).opacity(1.0))
                .child("•••")
                .when_some(drag, |handle, drag| {
                    let drag_label = drag_label.clone();
                    handle.on_drag(drag, move |drag, offset, window, cx| {
                        cx.stop_propagation();
                        drag.set_drag_offset(offset);
                        drag.set_preview_size(gpui_kit::size(
                            px(f32::from(window.rem_size()) * 3.0),
                            window.rem_size(),
                        ));
                        cx.new(|_| TabPreview(drag_label.clone()))
                    })
                });
            div()
                .id("hidden-panel-hover-strip")
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(22.0))
                .group(hover_group)
                .child(pill.context_menu(move |menu, window, cx| {
                    panel_menu(&owner, &group, always, menu, window, cx)
                }))
        };
        div()
            .id("single-panel-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .relative()
            .size_full()
            .child(content)
            .child(handle)
            .into_any_element()
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
        let owner = self.owner.clone();
        let group = group.clone();
        Some(
            div()
                .id("empty-group")
                .size_full()
                .children(content)
                .context_menu(move |menu, window, cx| {
                    group_menu(owner.clone(), &group, false, menu, window, cx)
                })
                .into_any_element(),
        )
    }
}

fn panel_menu(
    owner: &WeakEntity<WorkspaceDock>,
    group: &TabGroupContext,
    always: bool,
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let mut menu = menu;
    if let Some(panel) = group.active_panel() {
        if let Some(handle) = PanelHandle::of(panel) {
            menu = handle.dropdown_menu(menu, window, cx);
        }
        if panel.closable(cx) {
            let id = panel.panel_id(cx);
            let group = group.clone();
            menu = menu.item(
                PopupMenuItem::new("Close panel")
                    .on_click(move |_, window, cx| group.close(id, window, cx)),
            );
        }
        if panel.zoomable(cx) {
            let group = group.clone();
            menu = menu.item(
                PopupMenuItem::new(if group.is_zoomed() {
                    "Restore pane"
                } else {
                    "Zoom pane"
                })
                .on_click(move |_, window, cx| group.toggle_zoom(window, cx)),
            );
        }
    }
    group_menu(owner.clone(), group, always, menu, window, cx)
}

fn group_menu(
    owner: WeakEntity<WorkspaceDock>,
    group: &TabGroupContext,
    always: bool,
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    // The locked terminal center offers no tab or panel management: no tab strip to force,
    // no panel to add beside the mux window.
    let terminal_locked = !group.panels().is_empty()
        && group
            .panels()
            .iter()
            .all(|panel| terminal_panel_name(panel.panel_name(cx)));
    if terminal_locked {
        return menu;
    }
    let node = group.node();
    let toggle_owner = owner.clone();
    let hide_owner = owner.clone();
    let hidden = owner
        .upgrade()
        .is_some_and(|owner| owner.read(cx).always_hide_tabs.borrow().contains(&node));
    menu.separator()
        .item(
            PopupMenuItem::new("Always show tabs")
                .checked(always)
                .on_click(move |_, window, cx| {
                    _ = toggle_owner.update(cx, |owner, cx| {
                        owner.invoke_action(
                            crate::commands::DockAction::ToggleTabBar,
                            Some(node),
                            window,
                            cx,
                        );
                    });
                }),
        )
        .item(
            PopupMenuItem::new("Always hide tabs")
                .checked(hidden)
                .on_click(move |_, window, cx| {
                    _ = hide_owner.update(cx, |owner, cx| {
                        owner.invoke_action(
                            crate::commands::DockAction::ToggleHiddenTabs,
                            Some(node),
                            window,
                            cx,
                        );
                    });
                }),
        )
        .submenu("Add panel", window, cx, move |menu, _, _| {
            WorkspaceDock::panel_menu(&owner, node, menu)
        })
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
        EmptyTerminalState::Ready { can_create } => {
            let action = crate::action_catalog::Command::NewTab.action();
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
                .child("No open terminals")
                .child(
                    Button::new("empty-terminal-new")
                        .label("New terminal")
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
                .when(!can_create, |content| {
                    content.child("This backend does not support creating terminals.")
                })
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
