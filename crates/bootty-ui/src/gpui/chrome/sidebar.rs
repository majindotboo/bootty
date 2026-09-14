use num_traits::ToPrimitive as _;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::OnceLock,
    time::Instant,
};

use gpui_kit::component::{
    Collapsible, Side,
    menu::ContextMenuExt,
    shimmer::ShimmerText,
    sidebar::{Sidebar, SidebarItem},
    v_flex,
};
use gpui_kit::{
    App, Bounds, Context, Div, Empty, IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Render, SharedString, Stateful, Styled, WeakEntity, Window, canvas, div,
    prelude::*, px, relative,
};

use super::{
    ChromeIntent, ChromeLayout, ChromePalette, ContextMenu, GpuiChrome, MenuRow, Rgba,
    SessionContextAction, SessionContextSnapshot, SessionTarget, SidebarDiffSummary,
    SidebarPosition, SidebarRow, SidebarRowKind, SidebarSnapshot, SpaceSnapshot, SpaceTransition,
    TitlebarSnapshot, UsageMeterSnapshot, color, space_switcher,
};
use crate::gpui::theme::readable_color;

const ROW_HEIGHT: f32 = 28.0;
const GROUP_ROW_HEIGHT: f32 = 31.0;
pub(super) const SPACE_SWITCHER_HEIGHT: f32 = 36.0;
const RESIZE_HANDLE_WIDTH: f32 = 6.0;
const TRAFFIC_LIGHT_PADDING: f32 = 78.0;
const ROW_PAD_X: f32 = 8.0;
const ROW_INDENT: f32 = 8.0;
const TREE_GUIDE_WIDTH: f32 = 1.0;
const CURRENT_RAIL_WIDTH: f32 = 4.0;

#[derive(Clone)]
pub(super) struct DraggedSidebar;

#[derive(Clone)]
pub(super) struct DraggedSidebarRow {
    pub(super) source: String,
    pub(super) sessions: Vec<SessionTarget>,
}

#[derive(Clone)]
struct DraggedSidebarPreview {
    label: String,
    detail: Option<String>,
    foreground: Rgba,
    background: Rgba,
    border: Rgba,
}

impl Render for DraggedSidebarPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(gpui_kit::rems(14.0))
            .debug_selector(|| "sidebar-drag-preview".to_owned())
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(color(self.background))
            .border_1()
            .border_color(color(self.border))
            .text_sm()
            .text_color(color(self.foreground))
            .child(div().min_w_0().truncate().child(self.label.clone()))
            .when_some(self.detail.clone(), |preview, detail| {
                preview.child(div().text_xs().min_w_0().truncate().child(detail))
            })
    }
}

type SidebarContentRenderer = dyn Fn(&mut Window, &mut App) -> gpui_kit::AnyElement;

pub(super) type SidebarRowBounds = Rc<RefCell<Vec<(SessionTarget, Bounds<Pixels>)>>>;

#[derive(Clone)]
struct BoottySidebarContent {
    render: Rc<SidebarContentRenderer>,
    collapsed: bool,
}

impl Collapsible for BoottySidebarContent {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl SidebarItem for BoottySidebarContent {
    fn render(
        self,
        _: impl Into<gpui_kit::ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        (self.render)(window, cx)
    }
}

// Keep independent snapshot owners explicit until the chrome snapshot owns this projection.
#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    snapshot: &SidebarSnapshot,
    title: &TitlebarSnapshot,
    pointer_hovered_session: Option<&SessionTarget>,
    spaces: &[SpaceSnapshot],
    transition: Option<SpaceTransition>,
    layout: &ChromeLayout,
    header_height: f32,
    docked: bool,
    colors: ChromePalette,
    sidebar_row_bounds: &SidebarRowBounds,
    reveal_current: &Rc<Cell<bool>>,
    reconcile_hover: bool,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    let width = layout.effective_sidebar_width();
    let position = layout.sidebar_position;
    let content = SidebarRows {
        snapshot: snapshot.clone(),
        pointer_hovered_session: pointer_hovered_session.cloned(),
        colors,
        owner: cx.weak_entity(),
        row_bounds: sidebar_row_bounds.clone(),
        reveal_current: reveal_current.clone(),
        reconcile_hover,
    }
    .content(width, docked);
    let status_footer = (!docked)
        .then(|| render_codexbar(snapshot, colors))
        .flatten();

    let resize_handle = resize_handle(position, cx);

    let header = sidebar_header(snapshot, title, layout, header_height, docked, colors, cx);
    let component_footer = v_flex()
        .w(px(width))
        .when(docked, gpui_kit::Styled::w_full)
        .mb_neg_3()
        .when_some(status_footer, ParentElement::child)
        .child(space_switcher::render(
            spaces,
            transition,
            SPACE_SWITCHER_HEIGHT,
            snapshot.tint,
            colors,
            cx,
        ));
    let side = match position {
        SidebarPosition::Left => Side::Left,
        SidebarPosition::Right => Side::Right,
    };
    let sidebar = Sidebar::new("bootty-gpui-sidebar")
        .side(side)
        .collapsible(false)
        // Expand the component around its px_3 content inset, not its rows: the internal list
        // clips at that inset. The shell keeps the visible width and the panel-edge border.
        .w_auto()
        .flex_1()
        .mx_neg_3()
        .h_full()
        .bg(color(snapshot.tint))
        .text_color(color(snapshot.foreground))
        .border_l_0()
        .border_r_0()
        .when_some(header, gpui_kit::component::sidebar::Sidebar::header)
        .child(content)
        .footer(component_footer);

    div()
        // Empty chrome must not transfer focus to its dock panel. Child controls
        // still handle their own clicks and keyboard focus.
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .id("bootty-gpui-sidebar-shell")
        .debug_selector(|| "bootty-gpui-sidebar-shell".to_owned())
        .relative()
        .flex()
        .w(px(width))
        .when(docked, gpui_kit::Styled::w_full)
        .h_full()
        .overflow_hidden()
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if !hovered && this.pointer_hovered_session.take().is_some() {
                cx.notify();
            }
        }))
        .child(sidebar)
        .when(!layout.fullscreen, |element| {
            element.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w(px(1.0))
                    .when(position == SidebarPosition::Left, gpui_kit::Styled::right_0)
                    .when(position == SidebarPosition::Right, gpui_kit::Styled::left_0)
                    .bg(color(snapshot.border)),
            )
        })
        .when(!docked, |element| element.child(resize_handle))
        .into_any_element()
}

struct SidebarRows {
    snapshot: SidebarSnapshot,
    pointer_hovered_session: Option<SessionTarget>,
    colors: ChromePalette,
    owner: WeakEntity<GpuiChrome>,
    row_bounds: SidebarRowBounds,
    reveal_current: Rc<Cell<bool>>,
    reconcile_hover: bool,
}

impl SidebarRows {
    fn content(self, width: f32, docked: bool) -> BoottySidebarContent {
        BoottySidebarContent {
            render: Rc::new(move |_, _| self.render_content(width, docked)),
            collapsed: false,
        }
    }

    fn render_content(&self, width: f32, docked: bool) -> gpui_kit::AnyElement {
        let colors = self.colors;
        let current_rail_color = self
            .snapshot
            .rows
            .iter()
            .find(|row| row.current && matches!(&row.kind, SidebarRowKind::Session))
            .map(|row| row.color);
        v_flex()
            .w(px(width))
            .when(docked, gpui_kit::Styled::w_full)
            .my_neg_3()
            .child(
                canvas(
                    {
                        let row_bounds = self.row_bounds.clone();
                        move |_, _, _| row_bounds.borrow_mut().clear()
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .when(self.snapshot.rows.is_empty(), |element| {
                element.child(
                    div()
                        .h(px(ROW_HEIGHT * 3.0))
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(color(colors.muted))
                        .child("no sessions"),
                )
            })
            .children(
                self.snapshot
                    .rows
                    .iter()
                    .map(|row| self.render_row(row, current_rail_color)),
            )
            .child(self.reorder_end())
            .child(self.hover_reconciliation())
            .into_any_element()
    }

    fn hover_reconciliation(&self) -> gpui_kit::AnyElement {
        canvas(move |bounds, _, _| bounds, {
            let reconcile_hover = self.reconcile_hover;
            let owner = self.owner.clone();
            let row_bounds = self.row_bounds.clone();
            move |_, _, window, cx| {
                if !reconcile_hover {
                    return;
                }
                let pointer = window.mouse_position();
                cx.defer(move |cx| {
                    _ = owner.update(cx, |this, cx| {
                        if !this.sidebar_reconcile_hover {
                            return;
                        }
                        let hovered = row_bounds
                            .borrow()
                            .iter()
                            .find(|(_, bounds)| bounds.contains(&pointer))
                            .map(|(target, _)| target.clone());
                        this.pointer_hovered_session = hovered;
                        this.sidebar_reconcile_hover = false;
                        cx.notify();
                    });
                });
            }
        })
        .absolute()
        .inset_0()
        .into_any_element()
    }

    fn reorder_end(&self) -> gpui_kit::AnyElement {
        let colors = self.colors;
        div()
            .id("sidebar-reorder-end")
            .h(px(12.0))
            .drag_over::<DraggedSidebarRow>(move |element, _, _, _| {
                element.border_t_2().border_color(color(colors.accent))
            })
            .on_drop({
                let owner = self.owner.clone();
                move |dragged: &DraggedSidebarRow, _, cx| {
                    _ = owner.update(cx, |this, cx| {
                        let changed =
                            this.sidebar_dragging || this.pointer_hovered_session.take().is_some();
                        this.sidebar_dragging = false;
                        this.sidebar_reconcile_hover = true;
                        this.pointer_hovered_session = None;
                        cx.emit(ChromeIntent::ReorderSession {
                            source: dragged.source.clone(),
                            before: None,
                        });
                        if changed {
                            cx.notify();
                        }
                    });
                }
            })
            .into_any_element()
    }
}

fn resize_handle(position: SidebarPosition, cx: &Context<GpuiChrome>) -> gpui_kit::AnyElement {
    div()
        .id("bootty-sidebar-resize")
        .debug_selector(|| "bootty-sidebar-resize".to_owned())
        .absolute()
        .top_0()
        .bottom_0()
        .w(px(RESIZE_HANDLE_WIDTH))
        .when(position == SidebarPosition::Left, |element| {
            element.right(px(-RESIZE_HANDLE_WIDTH / 2.0))
        })
        .when(position == SidebarPosition::Right, |element| {
            element.left(px(-RESIZE_HANDLE_WIDTH / 2.0))
        })
        .cursor(gpui_kit::CursorStyle::ResizeLeftRight)
        .on_drag(DraggedSidebar, |_, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| Empty)
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|_, event: &MouseUpEvent, _, cx| {
                if event.click_count >= 2 {
                    cx.emit(ChromeIntent::SidebarResizeReset);
                    cx.stop_propagation();
                } else {
                    cx.emit(ChromeIntent::SidebarResizePersist);
                }
            }),
        )
        .occlude()
        .into_any_element()
}

fn sidebar_header(
    snapshot: &SidebarSnapshot,
    title: &TitlebarSnapshot,
    layout: &ChromeLayout,
    header_height: f32,
    docked: bool,
    colors: ChromePalette,
    cx: &Context<GpuiChrome>,
) -> Option<gpui_kit::AnyElement> {
    let width = layout.effective_sidebar_width();
    let reserve_window_controls =
        title.reserve_window_controls && layout.sidebar_position == SidebarPosition::Left;
    let title = title.clone();
    let title_has_icon = title.icon.is_some();
    (snapshot.title_visible && !docked).then(|| {
        div()
            .id("bootty-gpui-sidebar-header")
            .w(px(width))
            .when(docked, gpui_kit::Styled::w_full)
            .mt_neg_3()
            .h(px(header_height))
            .pl(px(6.0))
            .pr(px(6.0))
            .pt(px(layout.top_inset))
            .when(reserve_window_controls, |element| {
                element.pl(px(TRAFFIC_LIGHT_PADDING))
            })
            .flex()
            .items_center()
            .gap_1()
            .overflow_hidden()
            .window_control_area(gpui_kit::WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.emit(ChromeIntent::StartWindowDrag);
                }),
            )
            .when_some(title.icon, |element, icon| {
                element.child(crate::gpui::icon(&icon, 14.0, color(colors.muted)))
            })
            .when(!title_has_icon, |element| {
                element.child(div().size(px(14.0)))
            })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .text_sm()
                    .text_color(color(snapshot.foreground))
                    .truncate()
                    .child(title.title),
            )
            .child(
                div()
                    .flex_none()
                    .pl_1()
                    .text_xs()
                    .text_color(color(colors.muted))
                    .child(title.session_count.to_string()),
            )
            .into_any_element()
    })
}

impl SidebarRows {
    fn label(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        let snapshot = &self.snapshot;
        let selected = row.current || row.active;
        let current = row.current;
        let is_group = matches!(row.kind, SidebarRowKind::Group);
        let row_text = if row.text.trim().is_empty() {
            match &row.kind {
                SidebarRowKind::Progress {
                    label: Some(label), ..
                } => label.clone(),
                _ => row.text.clone(),
            }
        } else {
            row.text.clone()
        };

        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            // Keep sidebar labels aligned along the same scan lane.
            .text_left()
            .gap_1()
            .when_some(row.number, |element, number| {
                element.child(self.number_badge(row, number))
            })
            .when_some(row.icon.clone(), |element, icon| {
                element.child(div().flex_none().child(crate::gpui::icon(
                    &icon,
                    14.0,
                    color(if selected { row.color } else { row.dim_color }),
                )))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(is_group, gpui_kit::Styled::text_xs)
                    .text_color(if matches!(&row.kind, SidebarRowKind::Session) {
                        color(if current {
                            readable_color(snapshot.current, row.color)
                        } else {
                            row.dim_color
                        })
                    } else if matches!(&row.kind, SidebarRowKind::Group) {
                        color(if current { row.color } else { row.dim_color })
                    } else if row.active {
                        color(row.color)
                    } else {
                        color(snapshot.foreground)
                    })
                    .child(row_text),
            )
            .children(self.trailing_label(row))
            .into_any_element()
    }

    fn number_badge(&self, row: &SidebarRow, number: usize) -> gpui_kit::AnyElement {
        let snapshot = &self.snapshot;
        div()
            .flex_none()
            .w(px(14.0))
            .h(px(14.0))
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .rounded(px(3.0))
            .when(row.active, |badge| {
                badge
                    .bg(color(row.color))
                    .text_color(color(readable_color(row.color, snapshot.foreground)))
            })
            .when(!row.active, |badge| {
                badge
                    .border_1()
                    .border_color(color(row.dim_color))
                    .text_color(color(row.dim_color))
            })
            .child((number % 100).to_string())
            .into_any_element()
    }

    fn trailing_label(&self, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
        let trailing = row.trailing.clone()?;
        let colors = self.colors;
        let trailing_color = color(row.trailing_color.unwrap_or(colors.muted));
        div()
            .flex_none()
            .max_w(px(96.0))
            .truncate()
            .text_xs()
            .text_color(trailing_color)
            .map(|element| {
                if row.trailing_shimmer {
                    element.child(
                        ShimmerText::new(trailing)
                            .id(SharedString::from(format!("shimmer-{}", row.key))),
                    )
                } else {
                    element.child(trailing)
                }
            })
            .into_any_element()
            .into()
    }

    fn current_rail(row: &SidebarRow, current_rail_color: Option<Rgba>) -> gpui_kit::AnyElement {
        div()
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-current-rail-{key}")
            })
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .w(px(CURRENT_RAIL_WIDTH))
            .bg(color(current_rail_color.unwrap_or(row.color)))
            .into_any_element()
    }

    fn keyboard_rail(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        let current = row.current;
        let colors = self.colors;
        let keyboard_focus_key = row.key.clone();
        div()
            .debug_selector(move || format!("sidebar-keyboard-focus-{keyboard_focus_key}"))
            .absolute()
            // Keep the focus rail visible alongside the current-session rail.
            .left(px(if current { CURRENT_RAIL_WIDTH } else { 0.0 }))
            .top(px(3.0))
            .bottom(px(3.0))
            .w(px(2.0))
            .rounded(px(1.0))
            .bg(color(colors.accent))
            .into_any_element()
    }

    fn bounds_probe(&self, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
        let target = row.target.clone()?;
        let row_bounds = self.row_bounds.clone();
        let reveal_current = self.reveal_current.clone();
        let reveal_row = row.current && matches!(row.kind, SidebarRowKind::Session);
        let reveal_detail_height = reveal_row.then(|| self.session_detail_height(row));
        canvas(
            move |bounds, window, _| {
                if reveal_row && reveal_current.replace(false) {
                    let reveal_bounds = reveal_detail_height.map_or(bounds, |detail_height| {
                        Self::session_reveal_bounds(bounds, detail_height)
                    });
                    window.request_autoscroll(reveal_bounds);
                }
                row_bounds.borrow_mut().push((target.clone(), bounds));
                bounds
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
        .into_any_element()
        .into()
    }

    fn session_detail_height(&self, row: &SidebarRow) -> f32 {
        self.snapshot
            .rows
            .iter()
            .position(|candidate| candidate.key == row.key)
            .map_or(0.0, |index| {
                self.snapshot
                    .rows
                    .iter()
                    .skip(index.saturating_add(1))
                    .take_while(|candidate| {
                        candidate.current && matches!(candidate.kind, SidebarRowKind::Detail)
                    })
                    .fold(0.0, |height, _| height + ROW_HEIGHT)
            })
    }

    fn session_reveal_bounds(bounds: Bounds<Pixels>, detail_height: f32) -> Bounds<Pixels> {
        let bottom: f32 = bounds.bottom().into();
        Bounds::from_corners(
            gpui_kit::point(bounds.left(), bounds.top()),
            gpui_kit::point(bounds.right(), px(bottom + detail_height)),
        )
    }

    fn diff_button(&self, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
        let diff = row.diff?;
        let owner = self.owner.clone();
        let target = row.target.clone();
        div()
            .id(format!("git-diff-{}", row.key))
            .cursor_pointer()
            .child(sidebar_diff(diff))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                if let Some(target) = &target {
                    _ = owner.update(cx, |_, cx| {
                        cx.emit(ChromeIntent::OpenGitChanges(target.clone()));
                    });
                }
            })
            .into_any_element()
            .into()
    }

    fn drag_row(&self, element: Stateful<Div>, row: &SidebarRow) -> Stateful<Div> {
        let Some(source) = row.reorder_anchor.clone() else {
            return element;
        };
        let snapshot = &self.snapshot;
        let colors = self.colors;
        let drag = DraggedSidebarRow {
            source: source.clone(),
            sessions: row.target.clone().into_iter().collect(),
        };
        let session_row = snapshot
            .rows
            .iter()
            .find(|candidate| {
                candidate.reorder_anchor.as_ref() == Some(&source)
                    && matches!(
                        candidate.kind,
                        SidebarRowKind::Session | SidebarRowKind::Group
                    )
            })
            .unwrap_or(row);
        let preview = DraggedSidebarPreview {
            label: session_row.text.clone(),
            detail: snapshot
                .rows
                .iter()
                .find(|candidate| {
                    candidate.reorder_anchor.as_ref() == Some(&source)
                        && matches!(candidate.kind, SidebarRowKind::Detail)
                })
                .map(|candidate| candidate.text.clone()),
            foreground: readable_color(snapshot.current, row.color),
            background: snapshot.current,
            border: colors.border_variant,
        };
        let drop_target = source.clone();
        let drag_owner = self.owner.clone();
        element
            .cursor_move()
            .drag_over::<DraggedSidebarRow>(move |element, dragged, _, _| {
                if dragged.source == drop_target {
                    element
                } else {
                    element.border_t_2().border_color(color(colors.accent))
                }
            })
            .on_drag(drag, move |_, _, _, cx| {
                _ = drag_owner.update(cx, |this, cx| {
                    this.sidebar_dragging = true;
                    cx.notify();
                });
                cx.new(|_| preview.clone())
            })
            .on_drop({
                let owner = self.owner.clone();
                move |dragged: &DraggedSidebarRow, _, cx| {
                    _ = owner.update(cx, |this, cx| {
                        let changed =
                            this.sidebar_dragging || this.pointer_hovered_session.take().is_some();
                        this.sidebar_dragging = false;
                        this.sidebar_reconcile_hover = true;
                        if dragged.source != source {
                            cx.emit(ChromeIntent::ReorderSession {
                                source: dragged.source.clone(),
                                before: Some(source.clone()),
                            });
                        }
                        if changed {
                            cx.notify();
                        }
                    });
                }
            })
    }

    fn row_control(
        &self,
        element: Stateful<Div>,
        row: &SidebarRow,
        row_height: f32,
    ) -> gpui_kit::AnyElement {
        let accessible_label = if row.text.trim().is_empty() {
            row.key.clone()
        } else {
            row.text.clone()
        };
        let element = if row.selectable {
            element.cursor_pointer()
        } else {
            element
        };
        let context_owner = self.owner.clone();
        let element = if let (Some(target), Some(context)) = (row.target.clone(), row.context) {
            element
                .context_menu(move |menu, _, _| {
                    super::popup_menu(
                        menu,
                        &ContextMenu::Session {
                            target: target.clone(),
                            options: context,
                        },
                        &context_owner,
                    )
                })
                .into_any_element()
        } else {
            element.into_any_element()
        };
        if row.selectable {
            let owner = self.owner.clone();
            let activation_target = row.target.clone();
            let activation_kind = row.kind.clone();
            let button = Button::new(SharedString::from(format!(
                "sidebar-row-button-{}",
                row.key
            )))
            .ghost()
            .p_0()
            .w_full()
            .h(px(row_height))
            .tab_index(0_isize)
            .accessibility_label(accessible_label)
            .child(element);
            let activated =
                super::button::activated_button(div().w_full().h_full(), button, move |_, app| {
                    _ = owner.update(app, |_, cx| {
                        if let Some(target) = activation_target.clone() {
                            let intent = if matches!(
                                &activation_kind,
                                SidebarRowKind::Other(kind) if kind == "unassigned"
                            ) {
                                ChromeIntent::AdoptSession(target)
                            } else {
                                ChromeIntent::ActivateSession(target)
                            };
                            cx.emit(intent);
                        }
                    });
                });
            let hitbox_selector = format!("sidebar-row-hitbox-{}", row.key);
            let hitbox_debug_selector = format!("sidebar-row-hitbox-{}", row.key);
            div()
                .id(SharedString::from(hitbox_selector))
                .debug_selector(move || hitbox_debug_selector)
                .w_full()
                .h(px(row_height))
                .child(activated)
                .into_any_element()
        } else {
            element
        }
    }

    fn render_row(
        &self,
        row: &SidebarRow,
        current_rail_color: Option<Rgba>,
    ) -> gpui_kit::AnyElement {
        let snapshot = &self.snapshot;
        let current = row.current;
        let selected = row.current || row.active;
        let row_height = if matches!(row.kind, SidebarRowKind::Group) {
            GROUP_ROW_HEIGHT
        } else {
            ROW_HEIGHT
        };
        let row_indent = f32::from(row.indent) * 8.0;
        let pointer_hovered = row
            .target
            .as_ref()
            .is_some_and(|target| self.pointer_hovered_session.as_ref() == Some(target));
        let keyboard_focused = row.target.as_ref().is_some_and(|target| {
            snapshot.focused && snapshot.hovered_session.as_ref() == Some(target)
        });
        let element = div()
            .id(SharedString::from(format!("sidebar-row-{}", row.key)))
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-row-{key}")
            })
            .w_full()
            .relative()
            .h(px(row_height))
            .pl(px(ROW_PAD_X + row_indent))
            .pr_2()
            .flex()
            .items_center()
            .text_sm()
            .text_left()
            .overflow_hidden()
            .when(current, |element| {
                element.child(Self::current_rail(row, current_rail_color))
            })
            .when(keyboard_focused, |element| {
                element.child(self.keyboard_rail(row))
            })
            .when(!snapshot.focused, |row| {
                row.opacity(1.0 - snapshot.dim_when_unfocused.clamp(0.0, 1.0))
            })
            .bg(if pointer_hovered {
                color(snapshot.hover)
            } else if selected {
                color(snapshot.current)
            } else {
                color(snapshot.tint)
            })
            // Full-width rows touch both shell edges; rounded corners expose the sidebar/terminal
            // backing surfaces at either edge.
            .rounded(px(0.0))
            .when_some(tree_guide(row, row_height), ParentElement::child)
            .child(self.label(row))
            .children(self.bounds_probe(row))
            .children(self.diff_button(row))
            .when_some(progress(&row.kind, row), ParentElement::child)
            .when_some(row.target.clone(), |element, target| {
                let owner = self.owner.clone();
                element.on_mouse_move(move |_: &MouseMoveEvent, _, cx| {
                    _ = owner.update(cx, |this, cx| {
                        if this.pointer_hovered_session.as_ref() != Some(&target) {
                            this.pointer_hovered_session = Some(target.clone());
                            cx.notify();
                        }
                    });
                })
            });
        let element = self.drag_row(element, row);
        self.row_control(element, row, row_height)
    }
}

fn tree_guide(row: &SidebarRow, row_height: f32) -> Option<gpui_kit::AnyElement> {
    let tree = row.tree.as_deref()?;
    let (top, bottom, connector) = match tree {
        "middle" => (0.0, 0.0, true),
        "last" => (0.0, row_height * 0.5, true),
        "pipe" => (0.0, 0.0, false),
        _ => return None,
    };
    let vertical_left = ROW_PAD_X + ROW_INDENT;
    let connector_left = f32::mul_add(f32::from(row.indent), ROW_INDENT, ROW_PAD_X) - ROW_INDENT;
    let vertical = div()
        .absolute()
        .left(px(vertical_left))
        .top(px(top))
        .bottom(px(bottom))
        .w(px(TREE_GUIDE_WIDTH))
        .bg(color(row.dim_color));
    let mut guide = div().absolute().inset_0().child(vertical);
    if connector {
        guide = guide.child(
            div()
                .absolute()
                .left(px(connector_left))
                .top(px((row_height - TREE_GUIDE_WIDTH) * 0.5))
                .w(px(ROW_INDENT))
                .h(px(TREE_GUIDE_WIDTH))
                .bg(color(row.dim_color)),
        );
    }
    Some(guide.into_any_element())
}

fn sidebar_diff(diff: SidebarDiffSummary) -> gpui_kit::AnyElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .when(diff.added > 0, |element| {
            element.child(
                div()
                    .text_color(color(diff.added_color))
                    .child(format!("+{}", diff.added)),
            )
        })
        .when(diff.removed > 0, |element| {
            element.child(
                div()
                    .text_color(color(diff.removed_color))
                    .child(format!("-{}", diff.removed)),
            )
        })
        .into_any_element()
}

fn usage_meter(
    snapshot: &UsageMeterSnapshot,
    item: &super::SidebarFooterItem,
    colors: ChromePalette,
) -> gpui_kit::AnyElement {
    let fill_width = (snapshot.meter.remaining_percent.clamp(0.0, 100.0) / 100.0)
        .to_f32()
        .unwrap_or(0.0);
    let marker = snapshot
        .meter
        .expected_remaining_percent
        .and_then(|value| (value.clamp(0.0, 100.0) / 100.0).to_f32());
    let selector = |part: &str| {
        let name = format!("sidebar-footer-{}-{part}", item.key);
        move || name.clone()
    };
    v_flex()
        .w_full()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .id(SharedString::from(format!("usage-labels-{}", item.key)))
                .debug_selector(selector("labels"))
                .w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_1()
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(color(item.color))
                        .when_some(item.icon.as_deref(), |element, icon| {
                            element.child(crate::gpui::sized_icon(
                                icon,
                                crate::gpui::IconSize::Small,
                                color(snapshot.fill),
                            ))
                        })
                        .child(snapshot.label.clone()),
                )
                .when(!snapshot.meter.pace.is_empty(), |element| {
                    element.child(
                        div()
                            .flex_none()
                            .text_color(color(snapshot.pace))
                            .child(snapshot.meter.pace.clone()),
                    )
                })
                .when(!snapshot.meter.reset.is_empty(), |element| {
                    element.child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_color(color(colors.muted))
                            .child(crate::gpui::sized_icon(
                                "rotate-ccw",
                                crate::gpui::IconSize::XSmall,
                                color(colors.muted),
                            ))
                            .child(snapshot.meter.reset.clone()),
                    )
                }),
        )
        .child(
            div()
                .id(SharedString::from(format!("usage-track-{}", item.key)))
                .debug_selector(selector("track"))
                .relative()
                .w_full()
                .h_1()
                .rounded_full()
                .bg(color(snapshot.track))
                .child(
                    div()
                        .h_full()
                        .w(relative(fill_width))
                        .rounded_full()
                        .bg(color(snapshot.fill)),
                )
                .when_some(marker, |element, marker| {
                    element.child(
                        div()
                            .absolute()
                            .left(relative(marker))
                            .top_neg_1()
                            .bottom_neg_1()
                            .w_0p5()
                            .rounded_full()
                            .bg(color(snapshot.marker)),
                    )
                }),
        )
        .into_any_element()
}

fn progress(kind: &SidebarRowKind, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
    match kind {
        SidebarRowKind::Progress { value, .. } => {
            Some(progress_bar(*value, row.color, row.dim_color))
        }
        SidebarRowKind::Ports(ports) => Some(
            div()
                .text_xs()
                .text_color(color(row.dim_color))
                .child(
                    ports
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(" · "),
                )
                .into_any_element(),
        ),
        _ => None,
    }
}

fn progress_bar(value: Option<u8>, fill: Rgba, track: Rgba) -> gpui_kit::AnyElement {
    const WIDTH: f32 = 72.0;
    const HEIGHT: f32 = 6.0;
    const SWEEP_PERIOD: f64 = 1.5;
    static STARTED: OnceLock<Instant> = OnceLock::new();
    let started = *STARTED.get_or_init(Instant::now);
    div()
        .relative()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .flex_none()
        .rounded(px(3.0))
        .bg(color(track))
        .child(
            canvas(
                move |bounds, _, _| bounds,
                move |bounds, _, window, _| {
                    let width = f32::from(bounds.size.width).max(0.0);
                    let (fill_x, fill_width) = value.map_or_else(
                        || {
                            if window.is_window_active() {
                                window.request_animation_frame();
                            }
                            let phase =
                                (started.elapsed().as_secs_f64() % SWEEP_PERIOD) / SWEEP_PERIOD;
                            let phase = if phase < 0.5 {
                                phase * 2.0
                            } else {
                                phase.mul_add(-2.0, 2.0)
                            };
                            let phase = phase.to_f32().unwrap_or(0.0);
                            let fill_width = width * 0.35;
                            (width * 0.65 * phase, fill_width)
                        },
                        |value| (0.0, width * (f32::from(value.min(100)) / 100.0)),
                    );
                    window.paint_quad(gpui_kit::quad(
                        gpui_kit::Bounds {
                            origin: gpui_kit::point(
                                px(f32::from(bounds.origin.x) + fill_x),
                                bounds.origin.y,
                            ),
                            size: gpui_kit::size(px(fill_width), px(HEIGHT)),
                        },
                        gpui_kit::Corners {
                            top_left: px(3.0),
                            top_right: px(3.0),
                            bottom_right: px(3.0),
                            bottom_left: px(3.0),
                        },
                        color(fill),
                        px(0.0),
                        gpui_kit::transparent_black(),
                        gpui_kit::BorderStyle::Solid,
                    ));
                },
            )
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0)),
        )
        .into_any_element()
}

pub(super) fn session_menu(
    target: &SessionTarget,
    options: SessionContextSnapshot,
) -> Vec<MenuRow> {
    use SessionContextAction as A;
    let row = |label: &str, enabled: bool, destructive: bool, starts_group: bool, action| MenuRow {
        label: label.to_owned(),
        enabled,
        destructive,
        starts_group,
        intent: ChromeIntent::SessionContext {
            target: target.clone(),
            action,
        },
    };
    vec![
        row(
            "Activate Session",
            options.can_activate,
            false,
            false,
            A::Activate,
        ),
        row("New Session…", true, false, true, A::NewSession),
        row("Switch Session…", true, false, false, A::SwitchSession),
        row(
            "Previous Session",
            options.can_navigate,
            false,
            false,
            A::PreviousSession,
        ),
        row(
            "Next Session",
            options.can_navigate,
            false,
            false,
            A::NextSession,
        ),
        row(
            "Last Session",
            options.can_return_to_last,
            false,
            false,
            A::LastSession,
        ),
        row("Rename Session…", true, false, true, A::Rename),
        row(
            "Move Session Up",
            options.can_move_up,
            false,
            false,
            A::MoveUp,
        ),
        row(
            "Move Session Down",
            options.can_move_down,
            false,
            false,
            A::MoveDown,
        ),
        row("Move to Space…", true, false, true, A::MoveToSpace),
        row("Ditch Session…", true, true, false, A::Ditch),
    ]
}

pub(super) fn render_codexbar(
    snapshot: &SidebarSnapshot,
    colors: ChromePalette,
) -> Option<gpui_kit::AnyElement> {
    (!snapshot.footer.is_empty())
        .then(|| {
            v_flex()
                .id("bootty-gpui-sidebar-footer")
                .debug_selector(|| "bootty-gpui-sidebar-footer".to_owned())
                .flex_none()
                .w_full()
                .px_1()
                .py_2()
                .gap_2()
                .border_t_1()
                .border_color(color(snapshot.border))
                .children(snapshot.footer.iter().map(|item| {
                    let row = v_flex()
                        .id(SharedString::from(format!("sidebar-footer-{}", item.key)))
                        .debug_selector({
                            let key = item.key.clone();
                            move || format!("sidebar-footer-{key}")
                        })
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .text_color(color(item.color));
                    if let Some(meter) = &item.meter {
                        row.child(usage_meter(meter, item, colors))
                            .into_any_element()
                    } else {
                        row.child(div().min_w_0().child(item.text.clone()))
                            .into_any_element()
                    }
                }))
        })
        .map(IntoElement::into_any_element)
}
