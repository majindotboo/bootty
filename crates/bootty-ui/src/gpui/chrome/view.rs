use num_traits::ToPrimitive as _;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use super::{
    ChromeIntent, ChromeSnapshot, Rgba, SessionContextSnapshot, SessionTarget, SidebarPosition,
    SidebarTask, SpaceSnapshot, TabContextSnapshot, TabDragGesture, TabInsertionTarget, TaskView,
    WindowDragGesture, sidebar, space_switcher, status_bar,
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{InputEvent, InputState},
    menu::{PopupMenu, PopupMenuItem},
};
use gpui_kit::{
    App, Bounds, Context, DragMoveEvent, EventEmitter, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Render, SharedString, Styled, Window, WindowControlArea, div, prelude::*, px,
};

/// GPUI-owned interaction state for Bootty chrome.
///
/// Semantic icons, session progress, ports, labels, colors, and actions are rendered here while
/// product mutations remain behind typed [`ChromeIntent`] values.
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent dock, drag, attention and hover interaction state"
)]
pub struct GpuiChrome {
    pub(super) snapshot: ChromeSnapshot,
    pub(super) navigation_hints: Vec<(String, gpui_kit::Keystroke)>,
    pub(super) hint_modifiers: gpui_kit::Modifiers,
    docked_status: bool,
    keymap_context: String,
    pub(super) focus: FocusHandle,
    pub(super) status_tab_focus_handles: HashMap<String, FocusHandle>,
    pub(super) pointer_hovered_session: Option<SessionTarget>,
    pub(super) sidebar_project: Option<String>,
    pub(super) sidebar_task_view: TaskView,
    pub(super) sidebar_search: gpui_kit::Entity<InputState>,
    pub(super) sidebar_dragging: bool,
    sidebar_drag_focus: Option<FocusHandle>,
    pub(super) sidebar_drag_order: Option<(String, Option<String>)>,
    pub(super) sidebar_attention_only: bool,
    pub(super) sidebar_reconcile_hover: bool,
    pub(super) sidebar_reveal_current: Rc<Cell<bool>>,
    pub(super) sidebar_animation_epoch: std::time::Instant,
    pub(super) sidebar_animation_task: Option<gpui_kit::Task<()>>,
    pub(super) sidebar_row_bounds: sidebar::SidebarRowBounds,
    pub(super) sidebar_scroll: gpui_kit::ScrollHandle,
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
        task: Option<SidebarTask>,
        now: i64,
    },
    DetachedSession {
        target: SessionTarget,
        task: Option<SidebarTask>,
        now: i64,
    },
    SavedSession {
        task: SidebarTask,
        now: i64,
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
    pub fn new(snapshot: ChromeSnapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        cx.subscribe(&sidebar_search, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.sidebar_reveal_current.set(false);
                cx.notify();
            }
        })
        .detach();
        let mut chrome = Self {
            snapshot,
            navigation_hints: Vec::new(),
            hint_modifiers: gpui_kit::Modifiers::default(),
            docked_status: false,
            keymap_context: crate::gpui_actions::WORKSPACE_KEY_CONTEXT.to_owned(),
            focus: cx.focus_handle(),
            status_tab_focus_handles: HashMap::new(),
            pointer_hovered_session: None,
            sidebar_project: None,
            sidebar_task_view: TaskView::Active,
            sidebar_search,
            sidebar_dragging: false,
            sidebar_drag_focus: None,
            sidebar_drag_order: None,
            sidebar_attention_only: false,
            sidebar_reconcile_hover: false,
            sidebar_reveal_current: Rc::new(Cell::new(true)),
            sidebar_animation_epoch: std::time::Instant::now(),
            sidebar_animation_task: None,
            sidebar_row_bounds: Rc::new(RefCell::new(Vec::new())),
            sidebar_scroll: gpui_kit::ScrollHandle::new(),
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
                tab_config: self.snapshot.layout.tabs,
                keymap_context: &self.keymap_context,
                snapshot: &status,
                row_height: self
                    .snapshot
                    .layout
                    .status_height
                    .max(crate::gpui::UI_TAB_BAR_HEIGHT),
                top_padding: 0.0,
                notch_span: None,
                compact: true,
                partition: Some((right_width, in_right)),
                colors: self.snapshot.palette,
                tab_bounds: self.tab_bounds.clone(),
                insertion_target: None,
                tab_focus_handles: &self.status_tab_focus_handles,
                navigation_hints: &self.navigation_hints,
                hint_modifiers: self.hint_modifiers,
            },
            cx,
        ))
    }

    pub(crate) fn set_navigation_hints(
        &mut self,
        hints: Vec<(String, gpui_kit::Keystroke)>,
        cx: &mut Context<Self>,
    ) {
        self.navigation_hints = hints;
        cx.notify();
    }

    pub(crate) fn set_hint_modifiers(
        &mut self,
        modifiers: gpui_kit::Modifiers,
        cx: &mut Context<Self>,
    ) {
        if self.hint_modifiers != modifiers {
            self.hint_modifiers = modifiers;
            cx.notify();
        }
    }

    pub(crate) fn displayed_sessions(&self) -> Vec<SessionTarget> {
        let mut rows = self.sidebar_row_bounds.borrow().clone();
        rows.sort_by(|(_, left), (_, right)| {
            f32::from(left.origin.y).total_cmp(&f32::from(right.origin.y))
        });
        let mut targets = Vec::new();
        for (target, _) in rows {
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        targets
    }

    pub(crate) fn dock_tabs(
        &self,
        notch: Option<crate::gpui::tabs::NotchTabLayout>,
        cx: &Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        status_bar::dock_tabs(self, notch, cx)
    }

    pub(crate) const fn wrap_tabs_at_notch(&self) -> bool {
        self.snapshot.layout.wrap_tabs_at_notch
    }

    pub(crate) fn dock_bottom_height(&self) -> gpui_kit::Pixels {
        px(self.snapshot.bottom_status.as_ref().map_or(0.0, |status| {
            status.rows.max(1).to_f32().unwrap_or(f32::MAX) * self.snapshot.layout.status_height
        }))
    }

    pub(crate) fn dock_sidebar(&mut self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let snapshot = self.snapshot.sidebar.clone()?;
        Some(
            div()
                .size_full()
                .min_w_0()
                .min_h_0()
                .when(self.docked_status && !self.sidebar_dragging, |element| {
                    element.track_focus(&self.focus)
                })
                .child(sidebar::render(
                    &snapshot,
                    &self.sidebar_search,
                    self.sidebar_project.as_deref(),
                    self.sidebar_task_view,
                    self.sidebar_attention_only,
                    self.sidebar_drag_order.as_ref(),
                    &self.snapshot.titlebar,
                    self.pointer_hovered_session.as_ref(),
                    &self.snapshot.spaces,
                    self.snapshot.space_transition,
                    &self.snapshot.layout,
                    0.0,
                    true,
                    self.snapshot.palette,
                    &self.sidebar_row_bounds,
                    &self.sidebar_scroll,
                    &self.sidebar_reveal_current,
                    self.sidebar_reconcile_hover,
                    &self.navigation_hints,
                    self.hint_modifiers,
                    cx,
                ))
                .into_any_element(),
        )
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

    pub(crate) const fn notch_span(&self) -> Option<(f32, f32)> {
        self.snapshot.layout.notch_span
    }

    pub(crate) fn panel_background(&self) -> gpui_kit::Hsla {
        color(
            self.snapshot
                .sidebar
                .as_ref()
                .map_or(self.snapshot.palette.base, |sidebar| sidebar.tint),
        )
    }

    pub(crate) fn tab_accent(&self) -> gpui_kit::Hsla {
        color(self.snapshot.palette.tab_accent)
    }

    pub(crate) const fn dock_tabs_config(&self) -> bootty_config::config::TabConfig {
        self.snapshot.layout.tabs
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if &self.snapshot == snapshot {
            return;
        }
        if self.tab_drag.source().is_some() && !snapshot.window_focused {
            self.tab_drag.cancel();
        }
        if !snapshot.window_focused {
            self.finish_sidebar_drag(window, cx, false);
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
            cx.notify();
        }
    }

    fn menu_rows(menu: &ContextMenu) -> Vec<MenuRow> {
        match menu {
            ContextMenu::Session {
                target,
                options,
                task,
                now,
            } => {
                let mut rows = sidebar::session_menu(target, *options);
                if let Some(task) = task {
                    rows.extend(sidebar::saved_session_menu(task, *now));
                }
                rows
            }
            ContextMenu::DetachedSession { target, task, now } => {
                let mut rows = sidebar::detached_session_menu(target);
                if let Some(task) = task {
                    rows.extend(sidebar::saved_session_menu(task, *now));
                }
                rows
            }
            ContextMenu::SavedSession { task, now } => sidebar::saved_session_menu(task, *now),
            ContextMenu::Space(space) => space_switcher::space_menu(space),
            ContextMenu::Tab(tab) => status_bar::tab_menu(tab.clone()),
        }
    }

    pub(super) fn begin_sidebar_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sidebar_dragging {
            self.sidebar_drag_focus = window.focused(cx);
            self.sidebar_dragging = true;
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn finish_sidebar_drag(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        reconcile_hover: bool,
    ) {
        let was_dragging = self.sidebar_dragging;
        let changed = was_dragging
            || self.sidebar_drag_order.is_some()
            || self.pointer_hovered_session.is_some()
            || self.sidebar_reconcile_hover != reconcile_hover;
        self.sidebar_dragging = false;
        self.sidebar_drag_order = None;
        self.sidebar_reconcile_hover = reconcile_hover;
        self.pointer_hovered_session = None;
        let previous = self.sidebar_drag_focus.take();
        if was_dragging && self.focus.is_focused(window) {
            if let Some(previous) = previous {
                previous.focus(window, cx);
            } else {
                window.blur(cx);
            }
        }
        if changed {
            cx.notify();
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
                self.finish_sidebar_drag(window, cx, false);
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
                            tab_config: self.snapshot.layout.tabs,
                            keymap_context: &self.keymap_context,
                            snapshot: &status,
                            row_height: layout.status_height,
                            top_padding: layout.top_inset,
                            notch_span: layout.notch_span.map(|(left, right)| {
                                let offset = if layout.sidebar_visible
                                    && layout.sidebar_position == SidebarPosition::Left
                                {
                                    layout.effective_sidebar_width() + layout.gap
                                } else {
                                    0.0
                                };
                                ((left - offset).max(0.0), (right - offset).max(0.0))
                            }),
                            compact: false,
                            partition: None,
                            colors,
                            tab_bounds: self.tab_bounds.clone(),
                            insertion_target: insertion_target.as_ref(),
                            tab_focus_handles: &self.status_tab_focus_handles,
                            navigation_hints: &self.navigation_hints,
                            hint_modifiers: self.hint_modifiers,
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
                        tab_config: self.snapshot.layout.tabs,
                        keymap_context: &self.keymap_context,
                        snapshot: &status,
                        row_height: layout.status_height,
                        top_padding: 0.0,
                        notch_span: None,
                        compact: false,
                        partition: None,
                        colors,
                        tab_bounds: self.tab_bounds.clone(),
                        insertion_target: insertion_target.as_ref(),
                        tab_focus_handles: &self.status_tab_focus_handles,
                        navigation_hints: &self.navigation_hints,
                        hint_modifiers: self.hint_modifiers,
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
                        &self.sidebar_search,
                        self.sidebar_project.as_deref(),
                        self.sidebar_task_view,
                        self.sidebar_attention_only,
                        self.sidebar_drag_order.as_ref(),
                        &titlebar,
                        pointer_hovered_session.as_ref(),
                        &spaces,
                        transition,
                        &layout,
                        sidebar_header_height,
                        false,
                        colors,
                        &self.sidebar_row_bounds,
                        &self.sidebar_scroll,
                        &self.sidebar_reveal_current,
                        sidebar_reconcile_hover,
                        &self.navigation_hints,
                        self.hint_modifiers,
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
                        &self.sidebar_search,
                        self.sidebar_project.as_deref(),
                        self.sidebar_task_view,
                        self.sidebar_attention_only,
                        self.sidebar_drag_order.as_ref(),
                        &titlebar,
                        pointer_hovered_session.as_ref(),
                        &spaces,
                        transition,
                        &layout,
                        sidebar_header_height,
                        false,
                        colors,
                        &self.sidebar_row_bounds,
                        &self.sidebar_scroll,
                        &self.sidebar_reveal_current,
                        sidebar_reconcile_hover,
                        &self.navigation_hints,
                        self.hint_modifiers,
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
            .when(!self.docked_status || self.sidebar_dragging, |element| {
                element.track_focus(&self.focus)
            })
            .when(!self.docked_status, |element| {
                element.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            })
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    if this.sidebar_dragging {
                        this.finish_sidebar_drag(window, cx, false);
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    if this.sidebar_dragging {
                        this.finish_sidebar_drag(window, cx, false);
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
            .map(|element| {
                if self.docked_status {
                    element.size_full()
                } else {
                    element.w(px(layout.width)).h(px(layout.height))
                }
            })
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

#[derive(Clone)]
pub(super) struct MenuRow {
    pub label: String,
    pub enabled: bool,
    pub destructive: bool,
    pub starts_group: bool,
    pub intent: ChromeIntent,
}

pub(super) fn popup_menu(
    menu: PopupMenu,
    context: &ContextMenu,
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
) -> PopupMenu {
    GpuiChrome::menu_rows(context)
        .into_iter()
        .fold(menu, |menu, row| append_menu_row(menu, row, owner))
}

pub(super) fn popup_session_menu(
    mut menu: PopupMenu,
    context: &ContextMenu,
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    menu = popup_focus_context(menu, owner, cx);
    let (task, now) = match context {
        ContextMenu::Session { task, now, .. } | ContextMenu::DetachedSession { task, now, .. } => {
            (task.as_ref(), *now)
        }
        ContextMenu::SavedSession { task, now } => (Some(task), *now),
        _ => return popup_menu(menu, context, owner),
    };
    let mut snooze = task.map_or_else(Vec::new, |task| sidebar::saved_snooze_menu(task, now));
    for row in GpuiChrome::menu_rows(context) {
        if matches!(&row.intent, ChromeIntent::Command(invocation) if matches!(invocation.command.as_str(), "session.hide" | "session.show"))
            && !snooze.is_empty()
        {
            let rows = std::mem::take(&mut snooze);
            let owner = owner.clone();
            menu = if rows.iter().all(|row| !row.enabled) {
                menu.item(
                    PopupMenuItem::new("Snooze")
                        .icon(gpui_kit::assets::IconName::Clock)
                        .disabled(true),
                )
            } else {
                menu.submenu_with_icon(
                    Some(gpui_kit::component::Icon::new(
                        gpui_kit::assets::IconName::Clock,
                    )),
                    "Snooze",
                    window,
                    cx,
                    move |menu, _, _| {
                        rows.iter()
                            .cloned()
                            .fold(menu, |menu, row| append_menu_row(menu, row, &owner))
                    },
                )
            };
        }
        menu = append_menu_row(menu, row, owner);
    }
    menu
}

pub(super) fn popup_focus_context(
    menu: PopupMenu,
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
    cx: &App,
) -> PopupMenu {
    if let Some(owner) = owner.upgrade() {
        menu.action_context(owner.read(cx).focus_handle(cx))
    } else {
        menu
    }
}

fn append_menu_row(
    menu: PopupMenu,
    row: MenuRow,
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
) -> PopupMenu {
    let icon = menu_icon(&row.intent);
    let owner = owner.clone();
    let intent = row.intent;
    let enabled = row.enabled;
    let label = row.label;
    let item = if row.destructive {
        PopupMenuItem::element(move |_, cx| {
            div()
                .id(SharedString::from(format!("session-menu-{label}")))
                .role(gpui_kit::Role::MenuItem)
                .aria_label(label.clone())
                .text_color(cx.theme().danger)
                .child(label.clone())
        })
    } else {
        PopupMenuItem::new(label)
    }
    .when_some(icon, PopupMenuItem::icon)
    .disabled(!enabled)
    .on_click(move |_, window, cx| {
        if !enabled { return; }
        if matches!(&intent, ChromeIntent::Command(invocation) if invocation.command == "session.delete") {
            let receiver = crate::gpui::dialogs::prompt(
                "Delete this session permanently?",
                Some("This removes its saved history from Bootty. Archive keeps it recoverable instead. Close the session before deleting it."),
                &[gpui_kit::PromptButton::Cancel("Cancel".into()), gpui_kit::PromptButton::Other("Delete permanently".into())],
                window, cx,
            );
            let owner = owner.clone();
            let intent = intent.clone();
            cx.spawn(async move |cx| {
                if receiver.await == Ok(1) {
                    _ = owner.update(cx, |_, cx| cx.emit(intent));
                }
            }).detach();
        } else {
            _ = owner.update(cx, |_, cx| cx.emit(intent.clone()));
        }
    });
    if row.starts_group {
        menu.separator().item(item)
    } else {
        menu.item(item)
    }
}

fn menu_icon(intent: &ChromeIntent) -> Option<gpui_kit::assets::IconName> {
    use gpui_kit::assets::IconName as I;
    match intent {
        ChromeIntent::RenameSavedSession(_)
        | ChromeIntent::SessionContext {
            action: super::SessionContextAction::Rename,
            ..
        } => Some(I::Pencil),
        ChromeIntent::SessionContext {
            action: super::SessionContextAction::MoveToSpace,
            ..
        } => Some(I::FolderInput),
        ChromeIntent::Command(invocation) => match invocation.command.as_str() {
            "session.pin" => Some(I::Pin),
            "session.unpin" => Some(I::PinOff),
            "session.settle" | "session.activate" => Some(I::CircleCheck),
            "session.snooze" | "session.unsnooze" => Some(I::Clock),
            "session.archive" => Some(I::Archive),
            "session.unarchive" => Some(I::ArchiveRestore),
            "session.delete" => Some(I::Trash),
            "session.restore" => Some(I::Undo2),
            "session.show" => Some(I::Eye),
            "session.hide" => Some(I::EyeOff),
            _ => None,
        },
        _ => None,
    }
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

pub(super) fn navigation_hint(
    hints: &[(String, gpui_kit::Keystroke)],
    modifiers: gpui_kit::Modifiers,
    action: &str,
    index: usize,
) -> Option<gpui_kit::AnyElement> {
    let name = format!("{action}:{index}");
    let (_, key) = hints.iter().find(|(action, key)| {
        action == &name
            && key.modifiers == modifiers
            && key.modifiers != gpui_kit::Modifiers::default()
    })?;
    Some(gpui_kit::component::kbd::Kbd::new(key.clone()).into_any_element())
}
