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
    KeyDownEvent, MouseButton, MouseUpEvent, ParentElement, Pixels, Render, SharedString, Styled,
    Window, div, prelude::*, px,
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
    pub(super) sidebar_settled_shelf: sidebar::SettledShelf,
    pub(super) sidebar_reconcile_hover: bool,
    pub(super) sidebar_reveal_current: Rc<Cell<bool>>,
    pub(super) sidebar_row_bounds: sidebar::SidebarRowBounds,
    pub(super) sidebar_scroll: gpui_kit::ScrollHandle,
    pub(super) sidebar_rename: Option<SidebarRename>,
    /// Saved session rows chosen together for one action, by row key, last chosen last.
    pub(super) sidebar_selection: Vec<String>,
    /// Saved sessions whose conversations mention the search, by identity, with the line.
    pub(super) sidebar_conversation_matches: Vec<(String, String)>,
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
    Draft {
        identity: String,
        binding: bootty_control::CommandTarget,
    },
    Session {
        target: SessionTarget,
        options: SessionContextSnapshot,
        task: Option<SidebarTask>,
        now: i64,
        place: SessionPlace,
        working: bool,
    },
    DetachedSession {
        target: SessionTarget,
        task: Option<SidebarTask>,
        now: i64,
        place: SessionPlace,
    },
    /// Several saved sessions chosen together, each with whether its terminal is live.
    Sessions {
        tasks: Vec<(SidebarTask, bool)>,
        now: i64,
    },
    Space(SpaceSnapshot),
    Tab(TabContextSnapshot),
}

/// A session title being edited in its sidebar row. Enter or leaving the field saves it; Escape
/// keeps the old title.
pub(super) struct SidebarRename {
    pub title: String,
    pub task: SidebarTask,
    pub input: gpui_kit::Entity<InputState>,
    _events: gpui_kit::Subscription,
}

/// Where a session works: its project directory and branch, and whether they're on this
/// computer.
#[derive(Clone, Default)]
pub(super) struct SessionPlace {
    pub project: Option<String>,
    pub branch: Option<String>,
    pub local: bool,
    /// The session, and its project's scripts it can run in a new tab.
    pub scripts: Option<(SessionTarget, Vec<String>)>,
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
        cx.subscribe(&sidebar_search, |this, search, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.sidebar_reveal_current.set(false);
                let query = search.read(cx).value().trim().to_owned();
                cx.emit(ChromeIntent::SearchConversations(query));
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
            sidebar_settled_shelf: sidebar::SettledShelf::default(),
            sidebar_reconcile_hover: false,
            sidebar_reveal_current: Rc::new(Cell::new(true)),
            sidebar_row_bounds: Rc::new(RefCell::new(Vec::new())),
            sidebar_scroll: gpui_kit::ScrollHandle::new(),
            sidebar_rename: None,
            sidebar_selection: Vec::new(),
            sidebar_conversation_matches: Vec::new(),
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
        leading: Vec<gpui_kit::AnyElement>,
        trailing: Vec<gpui_kit::AnyElement>,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let mut status =
            self.snapshot
                .top_status
                .clone()
                .unwrap_or_else(|| super::StatusBarSnapshot {
                    key: "top".to_owned(),
                    rows: 1,
                    background: self.snapshot.palette.base,
                    segments: Vec::new(),
                });
        status
            .segments
            .retain(|segment| segment.surface != "windows");
        status_bar::render(
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
                leading,
                trailing,
                colors: self.snapshot.palette,
                tab_bounds: self.tab_bounds.clone(),
                insertion_target: None,
                tab_focus_handles: &self.status_tab_focus_handles,
                navigation_hints: &self.navigation_hints,
                hint_modifiers: self.hint_modifiers,
            },
            cx,
        )
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
                    self.sidebar_settled_shelf,
                    self.sidebar_drag_order.as_ref(),
                    self.sidebar_rename.as_ref(),
                    &self.sidebar_selection,
                    &self.sidebar_conversation_matches,
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

    pub(crate) fn window_controls_inset(&self, window: &Window) -> f32 {
        let native =
            if self.snapshot.titlebar.reserve_window_controls && !self.snapshot.layout.fullscreen {
                76.0
            } else {
                0.0
            };
        native + f32::from(crate::gpui::UI_HEADER_EDGE_PADDING.to_pixels(window.rem_size()))
    }

    /// Whether Back and Forward have a session to go to.
    pub(crate) const fn navigation(&self) -> (bool, bool) {
        (self.snapshot.titlebar.back, self.snapshot.titlebar.forward)
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
            ContextMenu::Draft { identity, binding } => [
                ("Open draft", "session.draft.open"),
                ("Discard draft", "session.draft.discard"),
            ]
            .into_iter()
            .map(|(label, command)| {
                let mut invocation = bootty_control::CommandInvocation::new(
                    command,
                    vec![identity.clone()],
                    bootty_control::Caller::Internal,
                );
                invocation.target = Some(binding.clone());
                MenuRow {
                    label: label.into(),
                    enabled: true,
                    destructive: command == "session.draft.discard",
                    starts_group: false,
                    confirm: None,
                    intent: ChromeIntent::Command(invocation),
                }
            })
            .collect(),
            ContextMenu::Session {
                target,
                options,
                task,
                now,
                place,
                working,
            } => {
                let mut rows = sidebar::session_menu(target, *options);
                rows.extend(sidebar::place_menu(target.scope, place));
                if let Some(task) = task {
                    rows.extend(sidebar::saved_session_menu(task, *now, *working));
                }
                rows
            }
            ContextMenu::DetachedSession {
                target,
                task,
                now,
                place,
            } => {
                let mut rows = sidebar::detached_session_menu(target);
                rows.extend(sidebar::place_menu(target.scope, place));
                if let Some(task) = task {
                    rows.extend(sidebar::saved_session_menu(task, *now, false));
                }
                rows
            }
            ContextMenu::Sessions { tasks, now } => sidebar::sessions_menu(tasks, *now),
            ContextMenu::Space(space) => space_switcher::space_menu(space),
            ContextMenu::Tab(tab) => status_bar::tab_menu(tab.clone()),
        }
    }

    /// Edits `target`'s title in its sidebar row. False when the row has no saved work to rename.
    pub(super) fn begin_sidebar_rename(
        &mut self,
        target: &SessionTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((title, task)) = self.snapshot.sidebar.as_ref().and_then(|sidebar| {
            let row = sidebar
                .rows
                .iter()
                .find(|row| row.target.as_ref() == Some(target))?;
            let task = row
                .task
                .clone()
                .filter(|task| task.binding.is_some() && !task.pending)?;
            Some((row.text.clone(), task))
        }) else {
            return false;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title.clone()));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.finish_sidebar_rename(true, cx);
            }
        });
        // Focus once the field is drawn, after the menu or row that started it lets go.
        let focused = input.clone();
        cx.defer_in(window, move |_, window, cx| {
            focused.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        });
        self.sidebar_rename = Some(SidebarRename {
            title,
            task,
            input,
            _events: events,
        });
        cx.notify();
        true
    }

    /// Ends the in-place rename, saving a changed, nonempty title when `save` is set.
    pub(super) fn finish_sidebar_rename(&mut self, save: bool, cx: &mut Context<Self>) {
        let Some(rename) = self.sidebar_rename.take() else {
            return;
        };
        let title = rename.input.read(cx).value().trim().to_owned();
        if save
            && !title.is_empty()
            && title != rename.title
            && let Some(invocation) =
                sidebar::saved_state_invocation(&rename.task, "session.set_title", vec![title])
        {
            cx.emit(ChromeIntent::Command(invocation));
        }
        cx.notify();
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
            } else if !self.sidebar_selection.is_empty() {
                self.clear_sidebar_selection(cx);
                cx.stop_propagation();
            }
        }
    }

    /// Shows the sessions whose conversations mention the sidebar's search.
    pub fn set_conversation_matches(
        &mut self,
        matches: Vec<bootty_agents::NativeSearchMatch>,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_conversation_matches = matches
            .into_iter()
            .filter_map(|found| Some((found.task_identity?, found.snippet)))
            .collect();
        cx.notify();
    }

    /// Lets go of the sessions chosen together.
    pub(crate) fn clear_sidebar_selection(&mut self, cx: &mut Context<Self>) {
        if !self.sidebar_selection.is_empty() {
            self.sidebar_selection.clear();
            cx.notify();
        }
    }

    /// Adds or removes `key` from the sessions chosen together; `range` instead chooses every
    /// row in `visible` between the last chosen one and `key`.
    pub(super) fn choose_sidebar_row(
        &mut self,
        key: &str,
        range: bool,
        visible: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Escape lets go of the choice, so the chrome takes the keyboard.
        self.focus.focus(window, cx);
        let anchor = self
            .sidebar_selection
            .last()
            .and_then(|last| visible.iter().position(|row| row == last));
        let clicked = visible.iter().position(|row| row == key);
        if range && let (Some(anchor), Some(clicked)) = (anchor, clicked) {
            let (first, last) = (anchor.min(clicked), anchor.max(clicked));
            for row in visible.get(first..=last).unwrap_or_default() {
                if !self.sidebar_selection.contains(row) {
                    self.sidebar_selection.push(row.clone());
                }
            }
            // The clicked row anchors the next range.
            self.sidebar_selection.retain(|row| row != key);
            self.sidebar_selection.push(key.to_owned());
        } else if let Some(index) = self.sidebar_selection.iter().position(|row| row == key) {
            self.sidebar_selection.remove(index);
        } else {
            self.sidebar_selection.push(key.to_owned());
        }
        cx.notify();
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
                            leading: Vec::new(),
                            trailing: Vec::new(),
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
                        leading: Vec::new(),
                        trailing: Vec::new(),
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
        let render_sidebar = |sidebar: &super::SidebarSnapshot, cx: &Context<Self>| {
            sidebar::render(
                sidebar,
                &self.sidebar_search,
                self.sidebar_project.as_deref(),
                self.sidebar_task_view,
                self.sidebar_attention_only,
                self.sidebar_settled_shelf,
                self.sidebar_drag_order.as_ref(),
                self.sidebar_rename.as_ref(),
                &self.sidebar_selection,
                &self.sidebar_conversation_matches,
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
            )
        };
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
                |element, sidebar| element.child(render_sidebar(sidebar, cx)),
            )
            .child(content)
            .when_some(
                sidebar
                    .as_ref()
                    .filter(|_| layout.sidebar_position == SidebarPosition::Right),
                |element, sidebar| element.child(render_sidebar(sidebar, cx)),
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
    /// Asked before the row runs.
    pub confirm: Option<Confirmation>,
}

#[derive(Clone)]
pub(super) struct Confirmation {
    pub question: String,
    pub detail: &'static str,
    pub action: &'static str,
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
    let (task, snooze) = match context {
        ContextMenu::Session { task, now, .. } | ContextMenu::DetachedSession { task, now, .. } => {
            (
                task.as_ref(),
                task.as_ref().map_or_else(Vec::new, |task| {
                    sidebar::snooze_menu(std::slice::from_ref(task), *now)
                }),
            )
        }
        ContextMenu::Sessions { tasks, now } => {
            let tasks: Vec<SidebarTask> = tasks.iter().map(|(task, _)| task.clone()).collect();
            (None, sidebar::snooze_menu(&tasks, *now))
        }
        _ => return popup_menu(menu, context, owner),
    };
    let mut project = Vec::new();
    let mut more = Vec::new();
    let mut destructive = Vec::new();
    for row in GpuiChrome::menu_rows(context) {
        match &row.intent {
            ChromeIntent::NewSessionInProject { .. }
            | ChromeIntent::RevealPath { .. }
            | ChromeIntent::CopyText(_)
            | ChromeIntent::RunProjectScript { .. } => project.push(row),
            ChromeIntent::SessionContext {
                action: super::SessionContextAction::MoveToSpace,
                ..
            }
            | ChromeIntent::ForkConversation(_) => more.push(row),
            ChromeIntent::Command(invocation)
                if invocation.command.starts_with("agents.native.") =>
            {
                more.push(row);
            }
            ChromeIntent::Command(invocation)
                if matches!(
                    invocation.command.as_str(),
                    "session.archive" | "session.unarchive" | "session.delete"
                ) =>
            {
                destructive.push(row);
            }
            ChromeIntent::Commands(invocations)
                if invocations.first().is_some_and(|invocation| {
                    matches!(
                        invocation.command.as_str(),
                        "session.archive" | "session.delete"
                    )
                }) =>
            {
                destructive.push(row);
            }
            _ => menu = append_menu_row(menu, row, owner),
        }
    }
    if !snooze.is_empty() {
        let owner = owner.clone();
        menu = menu.submenu_with_icon(
            Some(gpui_kit::component::Icon::new(
                gpui_kit::assets::IconName::Clock,
            )),
            "Snooze",
            window,
            cx,
            move |menu, _, _| append_menu_rows(menu, &snooze, &owner),
        );
    }
    if !project.is_empty() {
        let owner = owner.clone();
        menu = menu
            .separator()
            .submenu("Project", window, cx, move |menu, window, cx| {
                popup_project_actions(menu, &project, &owner, window, cx)
            });
    }
    let auto_settle = task.filter(|task| !task.state.deleted).cloned();
    if !more.is_empty() || auto_settle.is_some() {
        let owner = owner.clone();
        menu = menu.submenu("More", window, cx, move |menu, window, cx| {
            let menu = append_menu_rows(menu, &more, &owner);
            let Some(task) = &auto_settle else {
                return menu;
            };
            popup_auto_settle(menu.separator(), task, &owner, window, cx)
        });
    }
    for row in destructive {
        menu = append_menu_row(menu, row, owner);
    }
    menu
}

fn popup_auto_settle(
    menu: PopupMenu,
    task: &SidebarTask,
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let owner = owner.clone();
    let task = task.clone();
    menu.submenu("Auto-settle behavior", window, cx, move |mut menu, _, _| {
        for (label, enabled) in [("Use inactivity setting", true), ("Keep active", false)] {
            let mut invocation = bootty_control::CommandInvocation::new(
                "session.auto_settle",
                vec![task.identity.clone(), enabled.to_string()],
                bootty_control::Caller::Internal,
            );
            invocation.target.clone_from(&task.binding);
            let owner = owner.clone();
            menu = menu.item(
                PopupMenuItem::new(label)
                    .checked(enabled != task.state.auto_settle_disabled)
                    .disabled(task.pending)
                    .on_click(move |_, _, cx| {
                        _ = owner.update(cx, |_, cx| {
                            cx.emit(ChromeIntent::Command(invocation.clone()));
                        });
                    }),
            );
        }
        menu
    })
}

fn append_menu_rows(
    menu: PopupMenu,
    rows: &[MenuRow],
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
) -> PopupMenu {
    rows.iter()
        .cloned()
        .enumerate()
        .fold(menu, |menu, (index, mut row)| {
            row.starts_group &= index != 0;
            append_menu_row(menu, row, owner)
        })
}

fn popup_project_actions(
    mut menu: PopupMenu,
    rows: &[MenuRow],
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let mut copy = Vec::new();
    for mut row in rows.iter().cloned() {
        if matches!(row.intent, ChromeIntent::CopyText(_)) {
            copy.push(row);
        } else {
            row.starts_group = false;
            menu = append_menu_row(menu, row, owner);
        }
    }
    if !copy.is_empty() {
        let owner = owner.clone();
        menu = menu.submenu("Copy", window, cx, move |menu, _, _| {
            append_menu_rows(menu, &copy, &owner)
        });
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

pub(super) fn append_menu_row(
    menu: PopupMenu,
    row: MenuRow,
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
) -> PopupMenu {
    let icon = menu_icon(&row.intent);
    let owner = owner.clone();
    let intent = row.intent;
    let confirm = row.confirm;
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
        if !enabled {
            return;
        }
        if let Some(confirm) = &confirm {
            let receiver = crate::gpui::dialogs::prompt(
                &confirm.question,
                Some(confirm.detail),
                &[
                    gpui_kit::PromptButton::Cancel("Cancel".into()),
                    gpui_kit::PromptButton::Other(confirm.action.into()),
                ],
                window,
                cx,
            );
            let owner = owner.clone();
            let intent = intent.clone();
            cx.spawn(async move |cx| {
                if receiver.await == Ok(1) {
                    _ = owner.update(cx, |_, cx| cx.emit(intent));
                }
            })
            .detach();
        } else if !begins_rename(&intent, &owner, window, cx) {
            _ = owner.update(cx, |chrome, cx| {
                // Sessions chosen together are done once their action goes out.
                if matches!(intent, ChromeIntent::Commands(_)) {
                    chrome.sidebar_selection.clear();
                }
                cx.emit(intent.clone());
            });
        }
    });
    if row.starts_group {
        menu.separator().item(item)
    } else {
        menu.item(item)
    }
}

/// Starts renaming in the sidebar row for a rename item; false for other items, and for rows
/// the sidebar can't rename in place, which keep the rename dialog.
fn begins_rename(
    intent: &ChromeIntent,
    owner: &gpui_kit::WeakEntity<GpuiChrome>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let (ChromeIntent::RenameSavedSession(target)
    | ChromeIntent::SessionContext {
        target,
        action: super::SessionContextAction::Rename,
    }) = intent
    else {
        return false;
    };
    owner
        .update(cx, |chrome, cx| {
            chrome.begin_sidebar_rename(target, window, cx)
        })
        .unwrap_or(false)
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
        ChromeIntent::ForkConversation(_) => Some(I::GitFork),
        ChromeIntent::Command(invocation) => command_icon(&invocation.command),
        ChromeIntent::Commands(invocations) => command_icon(&invocations.first()?.command),
        _ => None,
    }
}

fn command_icon(command: &str) -> Option<gpui_kit::assets::IconName> {
    use gpui_kit::assets::IconName as I;
    match command {
        "session.pin" => Some(I::Pin),
        "session.unpin" => Some(I::PinOff),
        "session.settle" | "session.activate" => Some(I::CircleCheck),
        "session.snooze" | "session.unsnooze" => Some(I::Clock),
        "session.archive" => Some(I::Archive),
        "session.unarchive" => Some(I::ArchiveRestore),
        "session.delete" => Some(I::Trash),
        "session.restore" => Some(I::Undo2),
        _ => None,
    }
}

pub(super) fn color(value: Rgba) -> gpui_kit::Hsla {
    value.into()
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
