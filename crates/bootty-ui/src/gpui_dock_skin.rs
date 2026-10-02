//! Workspace header composition over Kit's Dock appearance and behavior.

use std::{rc::Rc, sync::Arc};

use gpui_kit::base::Tab;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    dock::{
        BasePanelView, DockArea, DockAreaRenderer, DockContext, DockEvent, DockPlacement, DockSkin,
        DragPanel, DropIndicator, NodeId, PanelHandle, PanelState, TabGroupContext,
        TabGroupRenderer,
    },
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::{
    AnyElement, App, AsKeystroke as _, Axis, Context, Div, Entity, FocusHandle, IntoElement,
    ParentElement, Render, SharedString, Stateful, Styled, WeakEntity, Window, WindowControlArea,
    div, prelude::*, px,
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
    fn tabs_visible(&self, group: &TabGroupContext, cx: &App) -> bool {
        self.area.upgrade().is_some_and(|area| {
            group_placement(area.read(cx), group.node()) == Some(DockPlacement::Right)
        })
    }

    fn render_tabs(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
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
        let focuses = visible
            .iter()
            .filter_map(|ix| {
                let panel = group.panels().get(*ix)?;
                let id = format!("tool-tab-focus:{:?}", panel.panel_id(cx));
                let state =
                    window.use_keyed_state(id, cx, |_, cx| cx.focus_handle().tab_stop(true));
                Some((*ix, state.read(cx).clone()))
            })
            .collect::<Vec<_>>();
        let tabs = visible
            .into_iter()
            .filter_map(|ix| {
                let panel = group.panels().get(ix)?;
                let id = format!("{:?}", panel.panel_id(cx));
                let focus = focuses
                    .iter()
                    .find(|(candidate, _)| *candidate == ix)?
                    .1
                    .clone();
                Some(crate::gpui::tabs::ScrollableTab {
                    id,
                    title: None,
                    focus: Some(focus.clone()),
                    tab: self.render_group_tab(
                        group, ix, tab_config, tab_accent, &focuses, &focus, window, cx,
                    )?,
                })
            })
            .collect();
        div()
            .flex()
            .items_center()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(crate::gpui::tabs::ScrollableTabBar {
                        id: format!("workspace-tabs-{:?}", group.node()).into(),
                        config: tab_config,
                        background: self.chrome.read(cx).panel_background(),
                        tabs,
                        selected: Some(selected),
                        notch: None,
                        end: div().min_w_1().into_any_element(),
                    }),
            )
            .child(sidebar_tab_picker(self.owner.clone(), cx))
            .into_any_element()
    }
}

impl WorkspaceTabGroup {
    #[allow(clippy::too_many_arguments)]
    fn render_group_tab(
        &self,
        group: &TabGroupContext,
        ix: usize,
        tab_config: bootty_config::config::TabConfig,
        accent: gpui_kit::Hsla,
        focuses: &[(usize, FocusHandle)],
        focus: &FocusHandle,
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
        let selected = !group.is_collapsed() && ix == group.active_ix();
        let foreground =
            crate::gpui::tabs::tab_foreground(tab_config.appearance, selected, accent, cx);
        let suffix = handle.and_then(|h| h.title_suffix(window, cx)).or_else(|| {
            (panel.panel_name(cx) != "bootty.sessions")
                .then(|| close_panel_tab(self.owner.clone(), group, id, &label, foreground, cx))
        });
        let hover_group = SharedString::from(format!("panel-tab-hover-{id:?}"));
        let title = div()
            .id(SharedString::from(format!("panel-tab-{id:?}")))
            .flex()
            .items_center()
            .gap_1()
            .child(
                Icon::new(panel_identity(panel.panel_name(cx)).1)
                    .small()
                    .text_color(foreground),
            )
            .child(title);
        let select = group.clone();
        let click_focus = focus.clone();
        let keyboard_group = group.clone();
        let keyboard_focuses = focuses.to_vec();
        let focus_color = cx.theme().primary;
        let tab = crate::gpui::tabs::tab(
            hover_group.clone(),
            tab_config.appearance,
            selected,
            accent,
            cx,
        )
        .group(hover_group.clone())
        .track_focus(focus)
        .focusable()
        .tab_index(0_isize)
        .focus_visible(move |style| style.border_1().border_color(focus_color))
        .on_key_down(move |event, window, cx| {
            let position = keyboard_focuses
                .iter()
                .position(|(candidate, _)| *candidate == ix)
                .unwrap_or_default();
            let next = match event.keystroke.key.as_str() {
                "left" => Some(position.saturating_sub(1)),
                "right" => Some(
                    position
                        .saturating_add(1)
                        .min(keyboard_focuses.len().saturating_sub(1)),
                ),
                "home" => Some(0),
                "end" => Some(keyboard_focuses.len().saturating_sub(1)),
                "enter" | "space" => {
                    keyboard_group.select_tab(ix, window, cx);
                    cx.stop_propagation();
                    window.prevent_default();
                    return;
                }
                _ => None,
            };
            if let Some(next) = next.and_then(|next| keyboard_focuses.get(next)) {
                next.1.focus(window, cx);
                window.prevent_default();
                cx.stop_propagation();
            }
        })
        .accessibility_label(label.clone())
        .tooltip({
            move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
            }
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
        .on_click(move |_, window, cx| {
            select.select_tab(ix, window, cx);
            click_focus.focus(window, cx);
        });
        Some(tab)
    }
}

fn close_panel_tab(
    owner: WeakEntity<WorkspaceDock>,
    group: &TabGroupContext,
    id: gpui_kit::component::dock::PanelId,
    label: &str,
    foreground: gpui_kit::Hsla,
    cx: &App,
) -> AnyElement {
    let group = group.clone();
    Button::new("close-tab")
        .icon(IconName::Close)
        .custom(
            ButtonCustomVariant::new(cx)
                .foreground(foreground)
                .hover(cx.theme().secondary_hover)
                .active(cx.theme().secondary_hover),
        )
        .xsmall()
        .size_4()
        .tooltip("Close tab")
        .accessibility_label(format!("Close {label}"))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let closed = owner
                .update(cx, |dock, cx| dock.close_tool_tab(id, window, cx))
                .unwrap_or(false);
            if !closed {
                group.close(id, window, cx);
            }
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
        let tabs_visible = self.tabs_visible(group, cx);
        if !tabs_visible {
            return div().into_any_element();
        }
        let tabs = self.render_tabs(group, window, cx);
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
            // Kit cached views omit accessibility registrations. Render directly until
            // the pinned renderer can replay those nodes for assistive technology.
            div()
                .id("tab-content")
                .size_full()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(panel)
                .into_any_element()
        } else {
            self.kit.render_active_panel(panel, group, window, cx)
        };
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
        if self.tabs_visible(group, cx) {
            return Some(empty_sidebar(&self.owner, cx));
        }
        self.kit.render_empty(group, window, cx)
    }
}

fn sidebar_tools() -> impl Iterator<Item = &'static crate::commands::PanelDescriptor> {
    crate::commands::PANELS.iter().filter(|panel| {
        matches!(
            panel.creation,
            crate::commands::PanelCreation::Command(
                crate::commands::DockAction::Files
                    | crate::commands::DockAction::Changes
                    | crate::commands::DockAction::Diff
            )
        )
    })
}

fn open_tool(
    owner: &WeakEntity<WorkspaceDock>,
    action: crate::commands::DockAction,
    window: &Window,
    cx: &mut App,
) {
    _ = owner.update(cx, |dock, cx| {
        dock.submit_command(
            bootty_control::CommandInvocation::from_action(
                action.command().action(),
                bootty_control::Caller::Internal,
            ),
            window,
            cx,
        );
    });
}

fn sidebar_tab_picker(owner: WeakEntity<WorkspaceDock>, cx: &App) -> impl IntoElement {
    let available = |action: crate::commands::DockAction, cx: &App| {
        !action.panel().is_some_and(|kind| {
            owner
                .upgrade()
                .is_some_and(|owner| owner.read(cx).panel_present(kind, cx))
        })
    };
    let all_open = sidebar_tools().all(|panel| {
        let crate::commands::PanelCreation::Command(action) = panel.creation else {
            return false;
        };
        !available(action, cx)
    });
    let menu_owner = owner;
    Button::new("new-sidebar-tab")
        .icon(IconName::Plus)
        .ghost()
        .small()
        .size_6()
        .accessibility_label("Open a tool")
        .disabled(all_open)
        .tooltip(if all_open {
            "All tools are open"
        } else {
            "Open a tool"
        })
        .dropdown_menu(move |mut menu, _, cx| {
            for panel in sidebar_tools() {
                let crate::commands::PanelCreation::Command(action) = panel.creation else {
                    continue;
                };
                if action.panel().is_some_and(|kind| {
                    menu_owner
                        .upgrade()
                        .is_some_and(|owner| owner.read(cx).panel_present(kind, cx))
                }) {
                    continue;
                }
                let owner = menu_owner.clone();
                menu = menu.item(
                    PopupMenuItem::new(panel.label)
                        .icon(panel.icon.clone())
                        .on_click(move |_, window, cx| {
                            open_tool(&owner, action, window, cx);
                        }),
                );
            }
            menu
        })
}

fn empty_sidebar(owner: &WeakEntity<WorkspaceDock>, cx: &App) -> AnyElement {
    div()
        .id("empty-tool-sidebar")
        .debug_selector(|| "empty-tool-sidebar".to_owned())
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .child(
            div()
                .w_full()
                .max_w_64()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .mb_2()
                        .child("Open a tool"),
                )
                .children(sidebar_tools().filter_map(|panel| {
                    let crate::commands::PanelCreation::Command(action) = panel.creation else {
                        return None;
                    };
                    let owner = owner.clone();
                    Some(
                        Button::new(SharedString::from(format!("empty-open-{}", panel.name)))
                            .label(panel.label)
                            .icon(panel.icon.clone())
                            .outline()
                            .w_full()
                            .on_click(move |_, window, cx| {
                                open_tool(&owner, action, window, cx);
                            }),
                    )
                })),
        )
        .into_any_element()
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
