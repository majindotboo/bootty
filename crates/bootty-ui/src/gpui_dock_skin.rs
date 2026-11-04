//! Workspace header composition over Kit's Dock appearance and behavior.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Instant,
};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _,
    button::{Button, ButtonVariants as _},
    dock::{
        BasePanelView, DockArea, DockAreaRenderer, DockContext, DockEvent, DockPlacement, DockSkin,
        DragPanel, DropIndicator, NodeId, PanelState, TabGroupContext, TabGroupRenderer,
    },
};
use gpui_kit::{
    AnyElement, App, AsKeystroke as _, Axis, Context, Div, Entity, IntoElement, ParentElement,
    Pixels, Render, Stateful, Styled, WeakEntity, Window, WindowControlArea, canvas, deferred, div,
    prelude::*, px,
};

use crate::{
    gpui::chrome::GpuiChrome,
    gpui_dock::{WorkspaceDock, motion::DockMotion},
};

const DOCK_RESIZE_GUTTER: f32 = 8.0;

pub struct WorkspaceDockSkin {
    kit: Rc<DockSkin>,
    area: WeakEntity<DockArea>,
    owner: WeakEntity<WorkspaceDock>,
    chrome: Entity<GpuiChrome>,
    motion: Rc<RefCell<DockMotion>>,
}

impl WorkspaceDockSkin {
    pub(crate) fn new(
        owner: WeakEntity<WorkspaceDock>,
        chrome: Entity<GpuiChrome>,
        motion: Rc<RefCell<DockMotion>>,
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
            motion,
        })
    }

    /// Asks for the next frame while a dock slides, and closes in Kit each dock that has slid
    /// shut.
    fn settle_slides(&self, now: Instant, window: &Window, cx: &mut App) {
        let mut motion = self.motion.borrow_mut();
        if !motion.moving() {
            return;
        }
        window.request_animation_frame();
        for placement in motion.finish(now) {
            let (area, motion) = (self.area.clone(), self.motion.clone());
            window.defer(cx, move |window, cx| {
                motion.borrow_mut().clear(placement);
                _ = area.update(cx, |area, cx| {
                    if area.is_dock_open(placement) {
                        area.toggle_dock(placement, window, cx);
                    }
                    cx.notify();
                });
            });
        }
    }
}

#[derive(Clone)]
struct DraggedDockResize(DockContext);

impl DockAreaRenderer for WorkspaceDockSkin {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        let area = self.area.clone();
        self.kit
            .frame(window, cx)
            .relative()
            .bg(self.chrome.read(cx).panel_background())
            // Window controls keep one identity while the full-height docks slide beneath.
            .when_some(self.owner.upgrade(), |frame, owner| {
                frame.child(deferred(owner.read(cx).navigation.clone()))
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
        let now = cx.background_executor().now();
        let (left, right) = {
            let motion = self.motion.borrow();
            (
                motion.hidden(DockPlacement::Left, now),
                motion.hidden(DockPlacement::Right, now),
            )
        };
        self.settle_slides(now, window, cx);
        self.kit
            .center_frame(window, cx)
            .relative()
            // The centre covers what a sliding dock leaves.
            .ml(px(-left))
            .mr(px(-right))
            // Bottom segments belong below the terminals, within the side docks.
            .pb(self.chrome.read(cx).dock_bottom_height())
            .child(
                // Chrome moves between the startup frame and the dock; its text and
                // accessibility nodes must belong to the current frame.
                div().absolute().size_full().child(self.chrome.clone()),
            )
            .when_some(self.owner.upgrade(), |frame, owner| {
                frame.child(owner.read(cx).titlebar.clone())
            })
    }

    fn render_split_handle(
        &self,
        handle: &gpui_kit::base::ResizeHandleContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let _ = window;
        let (width, color) = self
            .owner
            .upgrade()
            .map_or((1., None), |owner| owner.read(cx).pane_divider);
        let color = if handle.is_active() {
            cx.theme().primary
        } else {
            color.unwrap_or(cx.theme().border)
        };
        Some(
            div()
                .flex_none()
                .bg(color)
                .when(handle.axis() == Axis::Horizontal, |line| {
                    line.w(px(width)).h_full()
                })
                .when(handle.axis() == Axis::Vertical, |line| {
                    line.h(px(width)).w_full()
                })
                .into_any_element(),
        )
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
        let hidden = self
            .motion
            .borrow()
            .hidden(placement, cx.background_executor().now());
        let handle = dock_resize_handle(dock, cx);
        div()
            .relative()
            // A sliding dock's content moves out toward the window edge.
            .left(px(if placement.is_left() { -hidden } else { hidden }))
            .flex()
            .size_full()
            .flex_col()
            .bg(self.chrome.read(cx).panel_background())
            .when(placement.is_left(), |dock| {
                dock.child(
                    div()
                        .id("sidebar-window-drag-region")
                        .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
                        .flex_none()
                        .window_control_area(WindowControlArea::Drag)
                        .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            window.start_window_move();
                        }),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    // Native child views paint and receive input above GPUI hitboxes.
                    .when(placement.is_right(), |body| body.pl(px(DOCK_RESIZE_GUTTER)))
                    .child(content),
            )
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
                .w(px(DOCK_RESIZE_GUTTER))
                .cursor_col_resize()
        })
        .when(placement.is_right(), |handle| {
            handle
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(DOCK_RESIZE_GUTTER))
                .cursor_col_resize()
        })
        .when(placement.is_bottom(), |handle| {
            handle
                .top_0()
                .left_0()
                .right_0()
                .h(px(DOCK_RESIZE_GUTTER))
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
    chrome: Entity<GpuiChrome>,
}

pub struct WorkspaceTitleBar {
    chrome: Entity<GpuiChrome>,
    dock: WeakEntity<WorkspaceDock>,
    area: WeakEntity<DockArea>,
    measured_height: Rc<Cell<Pixels>>,
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
            measured_height: Rc::new(Cell::new(px(crate::gpui::UI_TAB_BAR_HEIGHT))),
        }
    }

    pub(crate) fn height(&self) -> Pixels {
        self.measured_height.get()
    }

    fn measure_height(&self) -> impl IntoElement {
        let height = self.measured_height.clone();
        let dock = self.dock.clone();
        let area = self.area.clone();
        canvas(
            move |bounds, _, cx| {
                if height.replace(bounds.size.height) != bounds.size.height {
                    cx.defer(move |cx| {
                        _ = dock.update(cx, |_, cx| cx.notify());
                        _ = area.update(cx, |_, cx| cx.notify());
                    });
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
    }
}

impl Render for WorkspaceTitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let area = self.area.upgrade();
        let left_width = area
            .as_ref()
            .filter(|area| area.read(cx).is_dock_open(DockPlacement::Left))
            .and_then(|area| area.read(cx).dock_size(DockPlacement::Left))
            .map_or(0.0, f32::from);
        let hidden = self.dock.upgrade().map_or(0.0, |dock| {
            dock.read(cx)
                .motion
                .borrow()
                .hidden(DockPlacement::Left, cx.background_executor().now())
        });
        let left_shown = (left_width - hidden).max(0.0);
        let (show_left, _, _) = self.chrome.read(cx).dock_presentation();
        let inset = self.chrome.read(cx).window_controls_inset(window);
        let leading = (inset + f32::from(leading_width(show_left).to_pixels(window.rem_size()))
            - left_shown)
            .max(0.0);
        let notch = self.chrome.read(cx).notch_span().map(|(left, right)| {
            crate::gpui::tabs::NotchTabLayout {
                width: (left - left_shown - leading).max(0.0),
                inset: leading,
                height: crate::gpui::UI_TAB_BAR_HEIGHT,
                right_edge: (right - left_shown).max(0.0),
                wrap: self.chrome.read(cx).wrap_tabs_at_notch(),
            }
        });
        let tabs = self
            .chrome
            .update(cx, |chrome, cx| chrome.dock_tabs(notch, cx));
        let status = self.chrome.update(cx, |chrome, cx| {
            chrome.dock_status(Vec::new(), Vec::new(), cx)
        });
        let controls = div()
            .min_w_0()
            .flex_initial()
            .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
            .flex()
            .items_center()
            .when_some(notch, |controls, notch| {
                controls
                    .absolute()
                    .top_0()
                    .right_0()
                    .left(px(notch.right_edge))
                    .justify_end()
            })
            .when(notch.is_some(), |controls| {
                controls.pr(crate::gpui::UI_HEADER_EDGE_PADDING)
            })
            .child(status);
        div()
            .id("workspace-titlebar")
            .debug_selector(|| "workspace-titlebar".into())
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
                    .pl(px(leading))
                    .pr(crate::gpui::UI_HEADER_EDGE_PADDING)
            })
            .child(
                div()
                    .min_w_0()
                    .when(notch.is_none(), |tabs| tabs.flex_1().h_full().min_w_32())
                    .children(tabs),
            )
            .child(controls)
            .child(self.measure_height())
    }
}

/// Window navigation is rendered after `DockArea` releases its update borrow.
/// It keeps one identity while the left dock slides beneath it.
pub struct WorkspaceNavigation {
    owner: WeakEntity<WorkspaceDock>,
    area: WeakEntity<DockArea>,
    chrome: Entity<GpuiChrome>,
}

impl WorkspaceNavigation {
    pub(crate) fn new(
        owner: WeakEntity<WorkspaceDock>,
        area: &Entity<DockArea>,
        chrome: Entity<GpuiChrome>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(area, |_, _, cx| cx.notify()).detach();
        cx.observe(&chrome, |_, _, cx| cx.notify()).detach();
        Self {
            owner,
            area: area.downgrade(),
            chrome,
        }
    }
}

impl Render for WorkspaceNavigation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let area = self.area.upgrade();
        let chrome = &self.chrome;
        let owner = &self.owner;
        let (show_left, _, _) = chrome.read(cx).dock_presentation();
        let open = |placement| {
            area.as_ref()
                .is_some_and(|area| area.read(cx).is_dock_open(placement))
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
            .child(
                div()
                    .absolute()
                    .left(px(chrome.read(cx).window_controls_inset(window)))
                    .child(leading_controls(
                        owner,
                        show_left.then_some(open(DockPlacement::Left)),
                        chrome,
                        cx,
                    )),
            )
    }
}

const fn toggle_width() -> gpui_kit::Rems {
    gpui_kit::rems(crate::gpui::UI_HEADER_CONTROL_SIZE.0 + crate::gpui::UI_HEADER_GAP.0)
}
const fn leading_width(toggle: bool) -> gpui_kit::Rems {
    gpui_kit::rems(toggle_width().0 * if toggle { 3.0 } else { 2.0 })
}

/// The left edge of the window: the left dock's toggle, when shown and `Some(open)`, then Back,
/// Forward. These controls stay in the shared top bar while the docks animate.
fn leading_controls(
    owner: &WeakEntity<WorkspaceDock>,
    toggle: Option<bool>,
    chrome: &Entity<GpuiChrome>,
    cx: &App,
) -> Div {
    let (back, forward) = chrome.read(cx).navigation();
    let button = |label, icon, command, enabled| {
        navigation_button(owner.clone(), label, icon, command, enabled, chrome, cx)
    };
    div()
        .h(px(crate::gpui::UI_TAB_BAR_HEIGHT))
        .w(leading_width(toggle.is_some()))
        .flex_none()
        .flex()
        .items_center()
        .when_some(toggle, |leading, open| {
            leading.child(div().w(toggle_width()).flex_none().child(panel_toggle(
                owner.clone(),
                open,
                chrome,
                cx,
            )))
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::gpui::UI_HEADER_GAP)
                .child(button("Back", IconName::ArrowLeft, "navigate_back", back))
                .child(button(
                    "Forward",
                    IconName::ArrowRight,
                    "navigate_forward",
                    forward,
                )),
        )
}

fn navigation_button(
    owner: WeakEntity<WorkspaceDock>,
    label: &'static str,
    icon: IconName,
    command: &'static str,
    enabled: bool,
    chrome: &Entity<GpuiChrome>,
    cx: &App,
) -> Button {
    Button::new(command)
        .icon(icon)
        .ghost()
        .w(crate::gpui::UI_HEADER_CONTROL_SIZE)
        .h(crate::gpui::UI_HEADER_CONTROL_SIZE)
        .rounded(cx.theme().radius)
        .disabled(!enabled)
        .tooltip_with_action(
            label,
            &crate::gpui_actions::InvokeCommand::new(
                bootty_control::CommandInvocation::from_action(
                    command,
                    bootty_control::Caller::Keybinding,
                ),
            ),
            Some(chrome.read(cx).keymap_context()),
        )
        .accessibility_label(label)
        .on_click(move |_, window, cx| {
            _ = owner.update(cx, |owner, cx| {
                owner.submit_command(
                    bootty_control::CommandInvocation::from_action(
                        command,
                        bootty_control::Caller::Internal,
                    ),
                    window,
                    cx,
                );
            });
        })
}

fn panel_toggle(
    owner: WeakEntity<WorkspaceDock>,
    open: bool,
    chrome: &Entity<GpuiChrome>,
    cx: &App,
) -> Button {
    let label = "Toggle left dock";
    let action = crate::commands::DockAction::ToggleLeft;
    Button::new("toggle-left-dock")
        .icon(IconName::PanelLeft)
        .ghost()
        .w(crate::gpui::UI_HEADER_CONTROL_SIZE)
        .h(crate::gpui::UI_HEADER_CONTROL_SIZE)
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
                owner.invoke_action(action, None, window, cx);
            });
        })
}

impl WorkspaceTabGroup {}

impl WorkspaceTabGroup {}

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

    /// No dock group shows tabs: the left dock holds only Sessions, and the center's mux window
    /// tabs live in the title bar.
    fn render_tab_bar(&self, _: &TabGroupContext, _: &mut Window, _: &mut App) -> AnyElement {
        div().into_any_element()
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
                .flex()
                .flex_col()
                .child(panel)
                .into_any_element()
        } else {
            self.kit.render_active_panel(panel, group, window, cx)
        };
        if group
            .active_panel()
            .is_some_and(|panel| terminal_panel_name(panel.panel_name(cx)))
        {
            // Kit gates both drag types with one lock. Keep host-owned AnyDrag drops,
            // but intercept built-in panel moves before the tab group can apply them.
            return div()
                .id("terminal-pane-content")
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
        self.kit.render_empty(group, window, cx)
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
        EmptyTerminalState::Ready => {
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
                .when_some(shortcut, |content, binding| {
                    content.child(
                        div().flex().gap_1().children(
                            binding.keystrokes().iter().map(|stroke| {
                                crate::gpui::keybinding_element(stroke.as_keystroke())
                            }),
                        ),
                    )
                })
                .into_any_element()
        }
    }
}

fn terminal_panel_name(name: &str) -> bool {
    matches!(name, "bootty.terminal-pane")
}
