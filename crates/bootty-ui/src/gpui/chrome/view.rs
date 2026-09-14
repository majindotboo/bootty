use num_traits::ToPrimitive as _;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use super::{
    ChromeIntent, ChromeSnapshot, Rgba, SessionContextSnapshot, SessionTarget, SidebarPosition,
    SpaceSnapshot, TabContextSnapshot, TabDragGesture, TabInsertionTarget, WindowDragGesture,
    sidebar, space_switcher, status_bar,
};

use gpui_kit::component::{ActiveTheme as _, menu::PopupMenuItem};
use gpui_kit::{
    App, Bounds, Context, DragMoveEvent, EventEmitter, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Render, SharedString, Styled, Window, WindowControlArea, div, prelude::*, px,
};

/// GPUI-owned interaction state for Bootty chrome.
///
/// Semantic icons, session progress, ports, labels, colors, and actions are rendered here while
/// product mutations remain behind typed [`ChromeIntent`] values.
pub struct GpuiChrome {
    pub(super) snapshot: ChromeSnapshot,
    docked_status: bool,
    keymap_context: String,
    pub(super) focus: FocusHandle,
    status_tab_focus_handles: HashMap<String, FocusHandle>,
    pub(super) pointer_hovered_session: Option<SessionTarget>,
    pub(super) sidebar_dragging: bool,
    pub(super) sidebar_reconcile_hover: bool,
    pub(super) sidebar_reveal_current: Rc<Cell<bool>>,
    pub(super) sidebar_row_bounds: sidebar::SidebarRowBounds,
    pub(super) window_drag: WindowDragGesture,
    pub(super) tab_drag: TabDragGesture,
    pub(super) tab_bounds: TabBounds,
}

#[derive(Clone, Copy)]
pub(super) enum StatusTabFocusMovement {
    Previous,
    Next,
    First,
    Last,
}

pub(super) type TabBounds = Rc<RefCell<HashMap<String, Bounds<Pixels>>>>;

#[derive(Clone)]
pub(super) enum ContextMenu {
    Session {
        target: SessionTarget,
        options: SessionContextSnapshot,
    },
    Space(SpaceSnapshot),
    Tab(TabContextSnapshot),
}

fn current_sidebar_session(snapshot: &ChromeSnapshot) -> Option<&SessionTarget> {
    snapshot
        .sidebar
        .as_ref()?
        .rows
        .iter()
        .find(|row| row.current && matches!(row.kind, super::SidebarRowKind::Session))?
        .target
        .as_ref()
}

impl GpuiChrome {
    #[must_use]
    pub fn new(snapshot: ChromeSnapshot, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut chrome = Self {
            snapshot,
            docked_status: false,
            keymap_context: crate::gpui_actions::WORKSPACE_KEY_CONTEXT.to_owned(),
            focus: cx.focus_handle(),
            status_tab_focus_handles: HashMap::new(),
            pointer_hovered_session: None,
            sidebar_dragging: false,
            sidebar_reconcile_hover: false,
            sidebar_reveal_current: Rc::new(Cell::new(true)),
            sidebar_row_bounds: Rc::new(RefCell::new(Vec::new())),
            window_drag: WindowDragGesture::default(),
            tab_drag: TabDragGesture::default(),
            tab_bounds: Rc::new(RefCell::new(HashMap::new())),
        };
        chrome.sync_status_tab_focus_handles(cx);
        chrome
    }

    pub(crate) fn with_keymap_context(mut self, context: String) -> Self {
        self.keymap_context = context;
        self
    }

    pub(crate) fn keymap_context(&self) -> &str {
        &self.keymap_context
    }

    pub(crate) fn set_docked_status(&mut self, docked: bool, cx: &mut Context<Self>) {
        if self.docked_status != docked {
            self.docked_status = docked;
            cx.notify();
        }
    }

    pub(crate) fn dock_status(
        &self,
        right_width: f32,
        in_right: bool,
        cx: &Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let mut status = self.snapshot.top_status.clone()?;
        status
            .segments
            .retain(|segment| segment.surface != "windows");
        Some(status_bar::render(
            status_bar::RenderParams {
                tab_config: self.snapshot.layout.terminal_tabs,
                keymap_context: &self.keymap_context,
                snapshot: &status,
                row_height: self
                    .snapshot
                    .layout
                    .status_height
                    .max(crate::gpui::UI_TAB_BAR_HEIGHT),
                top_padding: 0.0,
                compact: true,
                partition: Some((right_width, in_right)),
                colors: self.snapshot.palette,
                tab_bounds: self.tab_bounds.clone(),
                insertion_target: None,
                tab_focus_handles: &self.status_tab_focus_handles,
            },
            cx,
        ))
    }

    pub(crate) fn dock_tabs(&self, cx: &Context<Self>) -> Option<gpui_kit::AnyElement> {
        let mut status = self.snapshot.top_status.clone()?;
        status
            .segments
            .retain(|segment| segment.surface == "windows");
        if status.segments.is_empty() {
            return None;
        }
        let insertion_target = self.tab_drag.insertion_target().cloned();
        Some(status_bar::render(
            status_bar::RenderParams {
                tab_config: self.snapshot.layout.terminal_tabs,
                keymap_context: &self.keymap_context,
                snapshot: &status,
                row_height: self
                    .snapshot
                    .layout
                    .status_height
                    .max(crate::gpui::UI_TAB_BAR_HEIGHT),
                top_padding: 0.0,
                compact: false,
                partition: None,
                colors: self.snapshot.palette,
                tab_bounds: self.tab_bounds.clone(),
                insertion_target: insertion_target.as_ref(),
                tab_focus_handles: &self.status_tab_focus_handles,
            },
            cx,
        ))
    }

    pub(crate) fn dock_sidebar(&mut self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let snapshot = self.snapshot.sidebar.clone()?;
        Some(sidebar::render(
            &snapshot,
            &self.snapshot.titlebar,
            self.pointer_hovered_session.as_ref(),
            &self.snapshot.spaces,
            self.snapshot.space_transition,
            &self.snapshot.layout,
            0.0,
            true,
            self.snapshot.palette,
            &self.sidebar_row_bounds,
            &self.sidebar_reveal_current,
            self.sidebar_reconcile_hover,
            cx,
        ))
    }

    pub(crate) fn dock_codexbar(&self) -> Option<gpui_kit::AnyElement> {
        sidebar::render_codexbar(self.snapshot.sidebar.as_ref()?, self.snapshot.palette)
    }

    pub(crate) const fn dock_presentation(
        &self,
    ) -> (
        bool,
        bool,
        bootty_config::config::PanelTabStyle,
        bootty_config::config::PanelTabs,
    ) {
        let l = &self.snapshot.layout;
        (
            l.left_dock_toggle,
            l.right_dock_toggle,
            l.panel_tab_style,
            l.panel_tabs,
        )
    }

    pub(crate) const fn window_controls_inset(&self) -> f32 {
        if self.snapshot.titlebar.reserve_window_controls && !self.snapshot.layout.fullscreen {
            76.0
        } else {
            0.0
        }
    }

    pub(crate) fn panel_background(&self) -> gpui_kit::Hsla {
        color(
            self.snapshot
                .sidebar
                .as_ref()
                .map_or(self.snapshot.palette.base, |sidebar| sidebar.tint),
        )
    }

    pub(crate) const fn dock_tabs_config(&self) -> bootty_config::config::TabConfig {
        self.snapshot.layout.dock_tabs
    }

    pub(crate) fn sidebar_defaults(&self) -> (SidebarPosition, f32, bool) {
        let layout = &self.snapshot.layout;
        (
            layout.sidebar_position,
            layout.effective_sidebar_width(),
            layout.sidebar_visible,
        )
    }

    pub fn update(
        &mut self,
        snapshot: &ChromeSnapshot,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if &self.snapshot == snapshot {
            return;
        }
        if self.tab_drag.source().is_some() && !snapshot.window_focused {
            self.tab_drag.cancel();
        }
        if !snapshot.window_focused {
            self.sidebar_dragging = false;
            self.sidebar_reconcile_hover = false;
            self.pointer_hovered_session = None;
        }
        if current_sidebar_session(&self.snapshot) != current_sidebar_session(snapshot) {
            self.sidebar_reveal_current.set(true);
        }
        self.sidebar_row_bounds.borrow_mut().clear();
        self.tab_bounds.borrow_mut().clear();
        self.snapshot.clone_from(snapshot);
        self.sync_status_tab_focus_handles(cx);
        cx.notify();
    }

    fn sync_status_tab_focus_handles(&mut self, cx: &Context<Self>) {
        let mut ids = self
            .snapshot
            .top_status
            .iter()
            .chain(self.snapshot.bottom_status.iter())
            .flat_map(status_bar::tab_ids)
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        self.status_tab_focus_handles
            .retain(|id, _| ids.binary_search(id).is_ok());
        for id in ids {
            self.status_tab_focus_handles
                .entry(id)
                .or_insert_with(|| cx.focus_handle().tab_index(0).tab_stop(true));
        }
    }

    pub(super) fn focus_status_tab(
        &self,
        tab_ids: &[String],
        current: &str,
        movement: StatusTabFocusMovement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = tab_ids.iter().position(|id| id == current) else {
            return;
        };
        let next = match movement {
            StatusTabFocusMovement::Previous => index
                .checked_sub(1)
                .unwrap_or_else(|| tab_ids.len().saturating_sub(1)),
            StatusTabFocusMovement::Next => index
                .saturating_add(1)
                .checked_rem(tab_ids.len())
                .unwrap_or(0),
            StatusTabFocusMovement::First => 0,
            StatusTabFocusMovement::Last => tab_ids.len().saturating_sub(1),
        };
        if let Some(handle) = tab_ids
            .get(next)
            .and_then(|id| self.status_tab_focus_handles.get(id))
        {
            handle.focus(window, cx);
        }
    }

    fn menu_rows(menu: &ContextMenu) -> Vec<MenuRow> {
        match menu {
            ContextMenu::Session { target, options } => sidebar::session_menu(target, *options),
            ContextMenu::Space(space) => space_switcher::space_menu(space),
            ContextMenu::Tab(tab) => status_bar::tab_menu(tab.clone()),
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            let tab_cancelled = self.tab_drag.source().is_some();
            if tab_cancelled {
                self.tab_drag.cancel();
            }
            let sidebar_cancelled = self.sidebar_dragging;
            if sidebar_cancelled {
                self.sidebar_dragging = false;
                self.sidebar_reconcile_hover = false;
                self.pointer_hovered_session = None;
            }
            if tab_cancelled || sidebar_cancelled {
                cx.stop_active_drag(window);
                cx.notify();
                cx.stop_propagation();
            }
        }
    }

    fn titlebar(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = self.snapshot.palette;
        let title = self.snapshot.titlebar.clone();
        let reserve_window_controls = title.reserve_window_controls;
        div()
            .id("bootty-gpui-titlebar")
            .h(px(self.snapshot.layout.titlebar_height))
            .w_full()
            .px_3()
            .when(reserve_window_controls, |element| element.pl(px(76.0)))
            .flex()
            .items_center()
            .gap_2()
            .bg(color(colors.mantle))
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, _| {
                    this.window_drag.arm();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.window_drag.cancel()),
            )
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, _| {
                this.window_drag.cancel();
            }))
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| {
                if this.window_drag.take_on_motion() {
                    cx.emit(ChromeIntent::StartWindowDrag);
                }
            }))
            .when_some(title.icon, |element, icon| {
                element.child(crate::gpui::icon(&icon, 15.0, color(colors.accent)))
            })
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(color(colors.text))
                    .child(title.title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(color(colors.muted))
                    .child(title.session_count.to_string()),
            )
    }
}

impl EventEmitter<ChromeIntent> for GpuiChrome {}

impl Focusable for GpuiChrome {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GpuiChrome {
    fn tab_drag_preview(&self) -> Option<(f32, f32, String)> {
        self.tab_drag
            .source()
            .zip(self.tab_drag.pointer())
            .and_then(|(source, (x, y))| {
                [
                    self.snapshot.top_status.as_ref(),
                    self.snapshot.bottom_status.as_ref(),
                ]
                .into_iter()
                .flatten()
                .flat_map(|status| &status.segments)
                .find_map(|segment| {
                    let label = segment
                        .items
                        .iter()
                        .filter(|item| item.reorder_anchor.as_deref() == Some(source))
                        .map(|item| item.text.trim())
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    (!label.is_empty()).then_some((x, y, label))
                })
            })
    }

    fn render_status_bars(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let layout = &self.snapshot.layout;
        let top_status = self.snapshot.top_status.clone();
        let bottom_status = self.snapshot.bottom_status.clone();
        let colors = self.snapshot.palette;
        let mut insertion_target = self.tab_drag.insertion_target().cloned();
        if matches!(&insertion_target, Some(TabInsertionTarget::Before(before)) if Some(before.as_str()) == self.tab_drag.source())
        {
            insertion_target = None;
        }
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .when_some(
                top_status.filter(|_| !self.docked_status),
                |element, status| {
                    element.child(status_bar::render(
                        status_bar::RenderParams {
                            tab_config: self.snapshot.layout.terminal_tabs,
                            keymap_context: &self.keymap_context,
                            snapshot: &status,
                            row_height: layout.status_height,
                            top_padding: layout.top_inset,
                            compact: false,
                            partition: None,
                            colors,
                            tab_bounds: self.tab_bounds.clone(),
                            insertion_target: insertion_target.as_ref(),
                            tab_focus_handles: &self.status_tab_focus_handles,
                        },
                        cx,
                    ))
                },
            )
            // The center deliberately remains transparent. The terminal surface is painted by the
            // host below this chrome entity, while this owner draws only chrome and hit targets.
            .child(div().flex_1())
            .when_some(bottom_status, |element, status| {
                element.child(status_bar::render(
                    status_bar::RenderParams {
                        tab_config: self.snapshot.layout.terminal_tabs,
                        keymap_context: &self.keymap_context,
                        snapshot: &status,
                        row_height: layout.status_height,
                        top_padding: 0.0,
                        compact: false,
                        partition: None,
                        colors,
                        tab_bounds: self.tab_bounds.clone(),
                        insertion_target: insertion_target.as_ref(),
                        tab_focus_handles: &self.status_tab_focus_handles,
                    },
                    cx,
                ))
            })
            .into_any_element()
    }

    fn render_body(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let layout = self.snapshot.layout.clone();
        let titlebar = self.snapshot.titlebar.clone();
        let sidebar = self
            .snapshot
            .sidebar
            .clone()
            .filter(|_| !self.docked_status && layout.sidebar_visible);
        let pointer_hovered_session = self.pointer_hovered_session.clone();
        let spaces = self.snapshot.spaces.clone();
        let transition = self.snapshot.space_transition;
        let top_status = self.snapshot.top_status.clone();
        let colors = self.snapshot.palette;
        let sidebar_reconcile_hover = self.sidebar_reconcile_hover;
        let sidebar_header_height = layout.top_inset
            + if self.docked_status {
                layout.status_height.max(crate::gpui::UI_TAB_BAR_HEIGHT)
            } else {
                top_status.as_ref().map_or(34.0, |status| {
                    status.rows.max(1).to_f32().unwrap_or(f32::MAX) * layout.status_height
                })
            };
        let content = self.render_status_bars(cx);
        let body_row = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .w_full()
            .flex()
            .overflow_hidden()
            .gap(px(layout.gap))
            .when_some(
                sidebar
                    .as_ref()
                    .filter(|_| layout.sidebar_position == SidebarPosition::Left),
                |element, sidebar| {
                    element.child(sidebar::render(
                        sidebar,
                        &titlebar,
                        pointer_hovered_session.as_ref(),
                        &spaces,
                        transition,
                        &layout,
                        sidebar_header_height,
                        false,
                        colors,
                        &self.sidebar_row_bounds,
                        &self.sidebar_reveal_current,
                        sidebar_reconcile_hover,
                        cx,
                    ))
                },
            )
            .child(content)
            .when_some(
                sidebar
                    .as_ref()
                    .filter(|_| layout.sidebar_position == SidebarPosition::Right),
                |element, sidebar| {
                    element.child(sidebar::render(
                        sidebar,
                        &titlebar,
                        pointer_hovered_session.as_ref(),
                        &spaces,
                        transition,
                        &layout,
                        sidebar_header_height,
                        false,
                        colors,
                        &self.sidebar_row_bounds,
                        &self.sidebar_reveal_current,
                        sidebar_reconcile_hover,
                        cx,
                    ))
                },
            );
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(body_row)
            .into_any_element()
    }
}

impl Render for GpuiChrome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sidebar_row_bounds.borrow_mut().clear();
        let layout = self.snapshot.layout.clone();
        let sidebar_drag_layout = layout.clone();
        let colors = self.snapshot.palette;
        let tab_drag_preview = self.tab_drag_preview();
        let body = self.render_body(cx);

        div()
            .id("bootty-gpui-chrome")
            .relative()
            // Docked controls own their focus. This transparent overlay must not
            // intercept clicks intended for the panels beneath it.
            .when(!self.docked_status, |element| {
                element
                    .track_focus(&self.focus)
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            })
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.sidebar_dragging {
                        this.sidebar_dragging = false;
                        this.sidebar_reconcile_hover = false;
                        this.pointer_hovered_session = None;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.sidebar_dragging {
                        this.sidebar_dragging = false;
                        this.sidebar_reconcile_hover = false;
                        this.pointer_hovered_session = None;
                        cx.notify();
                    }
                }),
            )
            .when(layout.sidebar_visible, |element| {
                element.on_drag_move(cx.listener(
                    move |_: &mut Self, event: &DragMoveEvent<sidebar::DraggedSidebar>, _, cx| {
                        let mut resized = sidebar_drag_layout.clone();
                        resized.sidebar_width = match resized.sidebar_position {
                            SidebarPosition::Left => f32::from(event.event.position.x),
                            SidebarPosition::Right => {
                                resized.width - f32::from(event.event.position.x)
                            }
                        };
                        cx.emit(ChromeIntent::SidebarResizeLive(
                            resized.effective_sidebar_width(),
                        ));
                    },
                ))
            })
            .w(px(layout.width))
            .h(px(layout.height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_color(color(colors.text))
            .when(layout.titlebar_visible, |element| {
                element.child(self.titlebar(cx))
            })
            .child(body)
            .when_some(tab_drag_preview, |element, (x, y, label)| {
                element.child(
                    div()
                        .absolute()
                        .left(px(x + 10.0))
                        .top(px(y + 8.0))
                        .h(px(28.0))
                        .max_w(px(240.0))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .bg(color(colors.pane))
                        .border_1()
                        .border_color(color(colors.border_variant))
                        .text_sm()
                        .text_color(color(colors.text))
                        .child(label),
                )
            })
    }
}

pub(super) struct MenuRow {
    pub label: String,
    pub enabled: bool,
    pub destructive: bool,
    pub starts_group: bool,
    pub intent: ChromeIntent,
}

pub(super) fn popup_menu(
    menu: gpui_kit::component::menu::PopupMenu,
    context: &ContextMenu,
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
) -> gpui_kit::component::menu::PopupMenu {
    GpuiChrome::menu_rows(context)
        .into_iter()
        .fold(menu, |menu, row| {
            let owner = owner.clone();
            let intent = row.intent;
            let enabled = row.enabled;
            let label = row.label;
            let item = if row.destructive {
                PopupMenuItem::element(move |_, cx| {
                    div()
                        .flex()
                        .items_center()
                        .id(SharedString::from(format!(
                            "destructive-popup-item-{label}"
                        )))
                        .role(gpui_kit::Role::MenuItem)
                        .aria_label(label.clone())
                        .text_color(cx.theme().danger)
                        .child(label.clone())
                })
            } else {
                PopupMenuItem::new(label)
            }
            .disabled(!enabled)
            .on_click(move |_, _, cx| {
                if !enabled {
                    return;
                }
                _ = owner.update(cx, |_, cx| cx.emit(intent.clone()));
            });
            if row.starts_group {
                menu.separator().item(item)
            } else {
                menu.item(item)
            }
        })
}

pub(super) fn color(value: Rgba) -> gpui_kit::Hsla {
    gpui_kit::Rgba {
        r: f32::from(value.red) / 255.0,
        g: f32::from(value.green) / 255.0,
        b: f32::from(value.blue) / 255.0,
        a: f32::from(value.alpha) / 255.0,
    }
    .into()
}
