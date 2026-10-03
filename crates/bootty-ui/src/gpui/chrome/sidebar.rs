use num_traits::ToPrimitive as _;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use std::{
    cell::{Cell, RefCell},
    fmt::Write as _,
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, Collapsible, Side, Sizable as _,
    input::{Input, InputState},
    menu::{ContextMenuExt, DropdownMenu as _, PopupMenuItem},
    shimmer::ShimmerText,
    sidebar::{Sidebar, SidebarItem},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{
    App, Bounds, Context, Div, Empty, Entity, FontWeight, IntoElement, MouseButton, MouseUpEvent,
    ParentElement, Pixels, Render, SharedString, Stateful, Styled, WeakEntity, Window, canvas, div,
    img, prelude::*, px, relative,
};

use super::{
    ChromeIntent, ChromeLayout, ChromePalette, ContextMenu, GpuiChrome, MenuRow, Rgba,
    SessionContextAction, SessionContextSnapshot, SessionTarget, SidebarDiffSummary,
    SidebarPosition, SidebarRow, SidebarRowKind, SidebarSnapshot, SpaceSnapshot, SpaceTransition,
    TitlebarSnapshot, UsageMeterSnapshot, color, space_switcher,
};
use crate::gpui::theme::readable_color;

const ROW_HEIGHT: f32 = 1.75;
const SESSION_ROW_HEIGHT: f32 = 2.75;
const PROJECT_HEADER_HEIGHT: f32 = 1.25;
const GROUP_ROW_HEIGHT: f32 = 2.0;
pub(super) const SPACE_SWITCHER_HEIGHT: f32 = 36.0;
const RESIZE_HANDLE_WIDTH: f32 = 6.0;
const TRAFFIC_LIGHT_PADDING: f32 = 78.0;

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
    search: &Entity<InputState>,
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
    let query = search.read(cx).value().trim().to_lowercase();
    let rows = SidebarRows {
        snapshot: SidebarSnapshot {
            rows: filtered_rows(&snapshot.rows, &query),
            ..snapshot.clone()
        },
        search: search.clone(),
        searching: !query.is_empty(),
        pointer_hovered_session: pointer_hovered_session.cloned(),
        colors,
        radius: cx.theme().radius_tokens().xl,
        icon_size: f32::from(cx.theme().font_size) * 0.875,
        owner: cx.weak_entity(),
        row_bounds: sidebar_row_bounds.clone(),
        reveal_current: reveal_current.clone(),
        reconcile_hover,
    };
    let toolbar = rows.search_toolbar();
    let content = rows.content(width, docked);
    let status_footer = render_codexbar(snapshot, colors);

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
        .header(
            v_flex()
                .w(px(width))
                .when(docked, gpui_kit::Styled::w_full)
                .when(header.is_none(), gpui_kit::Styled::mt_neg_3)
                .when_some(header, ParentElement::child)
                .child(toolbar),
        )
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

fn filtered_rows(rows: &[SidebarRow], query: &str) -> Vec<SidebarRow> {
    if query.is_empty() {
        return rows.to_vec();
    }
    let mut filtered = Vec::new();
    let mut group = None;
    let mut group_added = false;
    for row in rows {
        if matches!(row.kind, SidebarRowKind::Group) {
            group = Some(row);
            group_added = false;
            continue;
        }
        let matches = std::iter::once(row.text.as_str())
            .chain(row.project.iter().map(|project| project.name.as_str()))
            .chain(group.iter().map(|group| group.text.as_str()))
            .chain(row.branch.as_deref())
            .chain(row.agents.iter().map(|agent| agent.description.as_str()))
            .any(|text| text.to_lowercase().contains(query));
        if matches {
            if !group_added && let Some(group) = group {
                filtered.push(group.clone());
                group_added = true;
            }
            filtered.push(row.clone());
        }
    }
    filtered
}

struct SidebarRows {
    snapshot: SidebarSnapshot,
    search: Entity<InputState>,
    searching: bool,
    pointer_hovered_session: Option<SessionTarget>,
    colors: ChromePalette,
    radius: Pixels,
    icon_size: f32,
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
                    v_flex()
                        .w_full()
                        .p_4()
                        .gap_2()
                        .text_sm()
                        .text_color(color(colors.muted))
                        .child(if self.searching {
                            "No matching sessions"
                        } else {
                            "No sessions"
                        })
                        .child(
                            Button::new("sidebar-create-session")
                                .debug_selector(|| "sidebar-create-session".to_owned())
                                .label("New session")
                                .ghost()
                                .on_click({
                                    let owner = self.owner.clone();
                                    move |_, _, cx| {
                                        _ = owner.update(cx, |_, cx| {
                                            cx.emit(ChromeIntent::Command(
                                                bootty_control::CommandInvocation::from_action(
                                                    "new_mux_session",
                                                    bootty_control::Caller::Internal,
                                                ),
                                            ));
                                        });
                                    }
                                }),
                        ),
                )
            })
            .children(self.render_session_blocks())
            .child(self.reorder_end())
            .child(self.hover_reconciliation())
            .into_any_element()
    }

    fn search_toolbar(&self) -> gpui_kit::AnyElement {
        let grouped = self.snapshot.group_by_project;
        let create_owner = self.owner.clone();
        let menu_owner = self.owner.clone();
        div()
            .debug_selector(|| "sidebar-search-toolbar".to_owned())
            .w_full()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .child(
                div().flex_1().min_w_0().child(
                    crate::gpui::focus_input(
                        &self.search,
                        Input::new(&self.search)
                            .aria_label("Search sessions")
                            .role(gpui_kit::Role::SearchInput)
                            .appearance(false)
                            .small()
                            .prefix(crate::gpui::sized_icon(
                                "search",
                                crate::gpui::IconSize::Small,
                                color(self.colors.muted),
                            ))
                            .cleanable(true),
                    )
                    .debug_selector(|| "sidebar-search".to_owned()),
                ),
            )
            .child(
                Button::new("sidebar-new-session")
                    .debug_selector(|| "sidebar-new-session".to_owned())
                    .ghost()
                    .small()
                    .size_6()
                    .child(crate::gpui::sized_icon(
                        "square-pen",
                        crate::gpui::IconSize::Small,
                        color(self.colors.subtext),
                    ))
                    .accessibility_label("New session")
                    .tooltip("New session")
                    .on_click(move |_, _, cx| {
                        _ = create_owner.update(cx, |_, cx| {
                            cx.emit(ChromeIntent::Command(
                                bootty_control::CommandInvocation::from_action(
                                    "new_mux_session",
                                    bootty_control::Caller::Internal,
                                ),
                            ));
                        });
                    }),
            )
            .child(
                Button::new("sidebar-view")
                    .debug_selector(|| "sidebar-view".to_owned())
                    .ghost()
                    .small()
                    .size_6()
                    .child(crate::gpui::sized_icon(
                        "ellipsis",
                        crate::gpui::IconSize::Small,
                        color(self.colors.subtext),
                    ))
                    .accessibility_label("Session list options")
                    .tooltip("Session list options")
                    .dropdown_menu(move |menu, _, _| {
                        [("Group by project", true), ("Flat session list", false)]
                            .into_iter()
                            .fold(menu, |menu, (label, desired)| {
                                let owner = menu_owner.clone();
                                menu.item(
                                    PopupMenuItem::new(label)
                                        .checked(grouped == desired)
                                        .on_click(move |_, _, cx| {
                                            if grouped != desired {
                                                _ = owner.update(cx, |_, cx| {
                                                    cx.emit(ChromeIntent::Command(
                                                    bootty_control::CommandInvocation::from_action(
                                                        "ui.sidebar.toggle_grouping",
                                                        bootty_control::Caller::Internal,
                                                    ),
                                                ));
                                                });
                                            }
                                        }),
                                )
                            })
                    }),
            )
            .into_any_element()
    }

    fn render_session_blocks(&self) -> Vec<gpui_kit::AnyElement> {
        let mut blocks = Vec::new();
        let mut rows = self.snapshot.rows.iter().peekable();
        while let Some(row) = rows.next() {
            if matches!(
                row.kind,
                SidebarRowKind::Detail | SidebarRowKind::Progress { .. } | SidebarRowKind::Ports(_)
            ) {
                continue;
            }
            if !matches!(row.kind, SidebarRowKind::Session) {
                blocks.push(self.render_row(row, false));
                continue;
            }
            while rows.peek().is_some_and(|next| {
                !matches!(next.kind, SidebarRowKind::Group | SidebarRowKind::Session)
                    && next.target == row.target
            }) {
                rows.next();
            }
            let block = div()
                .id(SharedString::from(format!("sidebar-session-{}", row.key)))
                .debug_selector({
                    let key = row.key.clone();
                    move || format!("sidebar-session-{key}")
                })
                .relative()
                .w_full()
                .flex()
                .flex_col()
                .rounded(self.radius)
                .overflow_hidden()
                .bg(color(self.background(row)))
                .child(self.render_row(row, true))
                .when_some(row.target.clone(), |block, target| {
                    let owner = self.owner.clone();
                    block.on_hover(move |hovered: &bool, _, cx| {
                        _ = owner.update(cx, |this, cx| {
                            if *hovered && this.pointer_hovered_session.as_ref() != Some(&target) {
                                this.pointer_hovered_session = Some(target.clone());
                                cx.notify();
                            } else if !*hovered
                                && this.pointer_hovered_session.as_ref() == Some(&target)
                            {
                                this.pointer_hovered_session = None;
                                cx.notify();
                            }
                        });
                    })
                });
            blocks.push(
                div()
                    .px_2()
                    .py_0p5()
                    .child(self.drop_target(block, row))
                    .into_any_element(),
            );
        }
        blocks
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
    _colors: ChromePalette,
    cx: &Context<GpuiChrome>,
) -> Option<gpui_kit::AnyElement> {
    let width = layout.effective_sidebar_width();
    let reserve_window_controls =
        title.reserve_window_controls && layout.sidebar_position == SidebarPosition::Left;
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
            .into_any_element()
    })
}

impl SidebarRows {
    fn background(&self, row: &SidebarRow) -> Rgba {
        if row
            .target
            .as_ref()
            .is_some_and(|target| self.pointer_hovered_session.as_ref() == Some(target))
        {
            self.snapshot.hover
        } else if row.current || row.active {
            self.snapshot.current
        } else {
            self.snapshot.tint
        }
    }

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
            .child(
                div()
                    .size_4()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|slot| {
                        if let Some(artwork) = &row.artwork {
                            slot.child(img(artwork.clone()).size_full())
                        } else if let Some(icon) = &row.icon {
                            slot.child(crate::gpui::icon(
                                icon,
                                self.icon_size,
                                color(if selected { row.color } else { row.dim_color }),
                            ))
                        } else {
                            slot
                        }
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .gap_1p5()
                    .child(
                        div()
                            .debug_selector({
                                let key = row.key.clone();
                                move || format!("sidebar-title-{key}")
                            })
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(is_group, gpui_kit::Styled::text_xs)
                            .when(!is_group, |title| title.font_weight(FontWeight::MEDIUM))
                            .text_color(if matches!(&row.kind, SidebarRowKind::Session) {
                                color(snapshot.foreground)
                            } else if is_group {
                                color(if current { row.color } else { row.dim_color })
                            } else if row.active {
                                color(row.color)
                            } else {
                                color(snapshot.foreground)
                            })
                            .child(row_text),
                    )
                    .when_some(
                        row.secondary
                            .clone()
                            .filter(|_| !matches!(row.kind, SidebarRowKind::Session)),
                        |column, secondary| {
                            column.child(
                                div()
                                    .debug_selector({
                                        let key = row.key.clone();
                                        move || format!("sidebar-secondary-{key}")
                                    })
                                    .min_w_0()
                                    .max_w(relative(0.5))
                                    .truncate()
                                    .text_xs()
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(color(readable_color(
                                        self.background(row),
                                        self.colors.muted,
                                    )))
                                    .child(secondary),
                            )
                        },
                    ),
            )
            .children(self.trailing_label(row))
            .into_any_element()
    }

    fn session_label(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        v_flex()
            .w_full()
            .min_w_0()
            .when_some(row.project.as_ref(), |column, project| {
                column.child(
                    div()
                        .debug_selector({
                            let key = row.key.clone();
                            move || format!("sidebar-project-{key}")
                        })
                        .h(gpui_kit::rems(PROJECT_HEADER_HEIGHT))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(color(readable_color(
                            self.background(row),
                            self.colors.muted,
                        )))
                        .child(div().size_3p5().flex_none().map(|slot| {
                            if let Some(artwork) = &project.artwork {
                                slot.child(img(artwork.clone()).size_full())
                            } else {
                                slot.child(crate::gpui::icon(
                                    "folder",
                                    self.icon_size,
                                    color(self.colors.muted),
                                ))
                            }
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(project.name.clone()),
                        ),
                )
            })
            .child(
                div()
                    .h(gpui_kit::rems(1.5))
                    .w_full()
                    .flex()
                    .items_center()
                    .child(self.label(row)),
            )
            .child(self.session_metadata(row))
            .into_any_element()
    }

    fn session_metadata(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        div()
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-metadata-{key}")
            })
            .w_full()
            .h(gpui_kit::rems(1.25))
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(color(readable_color(
                self.background(row),
                self.colors.muted,
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when_some(row.branch.as_ref(), |branch, name| {
                        branch
                            .child(crate::gpui::icon(
                                "git-branch",
                                self.icon_size * 0.85,
                                color(self.colors.muted),
                            ))
                            .child(
                                div()
                                    .debug_selector({
                                        let key = row.key.clone();
                                        move || format!("sidebar-branch-{key}")
                                    })
                                    .min_w_0()
                                    .truncate()
                                    .child(name.clone()),
                            )
                    }),
            )
            .child(self.agent_stack(row))
            .into_any_element()
    }

    fn agent_stack(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        // Bound horizontal space; the tooltip retains every observed agent when the stack overflows.
        let visible = row.agents.len().min(4);
        let width = visible
            .saturating_sub(1)
            .to_f32()
            .unwrap_or_default()
            .mul_add(0.75, 1.125);
        let description = row
            .agents
            .iter()
            .map(|agent| agent.description.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        div()
            .id(SharedString::from(format!("sidebar-agents-{}", row.key)))
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-agents-{key}")
            })
            .flex_none()
            .relative()
            .h(gpui_kit::rems(1.125))
            .w(gpui_kit::rems(if visible == 0 { 0.0 } else { width }))
            .tooltip(move |window, cx| Tooltip::new(description.clone()).build(window, cx))
            .children(
                row.agents
                    .iter()
                    .take(visible)
                    .enumerate()
                    .map(|(index, agent)| {
                        div()
                            .id(SharedString::from(format!("sidebar-agent-{}", agent.key)))
                            .debug_selector({
                                let key = agent.key.clone();
                                move || format!("sidebar-agent-{key}")
                            })
                            .absolute()
                            .left(gpui_kit::rems(index.to_f32().unwrap_or_default() * 0.75))
                            .size(gpui_kit::rems(1.125))
                            .rounded_full()
                            .bg(color(self.background(row)))
                            .border_1()
                            .border_color(color(self.background(row)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::gpui::icon(
                                &agent.icon,
                                self.icon_size,
                                color(self.snapshot.foreground),
                            ))
                    }),
            )
            .into_any_element()
    }

    fn trailing_label(&self, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
        let trailing = row.trailing.clone()?;
        let colors = self.colors;
        let trailing_color = color(readable_color(
            self.background(row),
            row.trailing_color.unwrap_or(colors.muted),
        ));
        div()
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-status-{key}")
            })
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .max_w(gpui_kit::rems(7.5))
            .truncate()
            .text_xs()
            .text_color(trailing_color)
            .when_some(row.trailing_icon.as_ref(), |element, icon| {
                element.child(
                    div()
                        .debug_selector({
                            let key = row.key.clone();
                            move || format!("sidebar-status-icon-{key}")
                        })
                        .size_3()
                        .flex_none()
                        .child(crate::gpui::icon(
                            icon,
                            self.icon_size * 0.85,
                            trailing_color,
                        )),
                )
            })
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

    fn focus_outline(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        let keyboard_focus_key = row.key.clone();
        div()
            .debug_selector(move || format!("sidebar-keyboard-focus-{keyboard_focus_key}"))
            .absolute()
            .inset_0()
            .border_1()
            .rounded(self.radius)
            .border_color(color(self.colors.accent))
            .into_any_element()
    }

    fn bounds_probe(&self, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
        let target = row.target.clone()?;
        let row_bounds = self.row_bounds.clone();
        let reveal_current = self.reveal_current.clone();
        let reveal_row = row.current && matches!(row.kind, SidebarRowKind::Session);
        canvas(
            move |bounds, window, _| {
                if reveal_row && reveal_current.replace(false) {
                    window.request_autoscroll(bounds);
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
        let drag_owner = self.owner.clone();
        element.cursor_move().on_drag(drag, move |_, _, _, cx| {
            _ = drag_owner.update(cx, |this, cx| {
                this.sidebar_dragging = true;
                cx.notify();
            });
            cx.new(|_| preview.clone())
        })
    }

    fn drop_target(&self, element: Stateful<Div>, row: &SidebarRow) -> Stateful<Div> {
        let Some(source) = row.reorder_anchor.clone() else {
            return element;
        };
        let colors = self.colors;
        element
            .drag_over::<DraggedSidebarRow>({
                let source = source.clone();
                move |element, dragged, _, _| {
                    if dragged.source == source {
                        element
                    } else {
                        element.border_1().border_color(color(colors.accent))
                    }
                }
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
        let label = if row.text.trim().is_empty() {
            row.key.clone()
        } else {
            row.text.clone()
        };
        let label = row.secondary.as_ref().map_or_else(
            || label.clone(),
            |secondary| format!("{label} · {secondary}"),
        );
        let mut accessible_label = row
            .trailing
            .as_ref()
            .map_or_else(|| label.clone(), |status| format!("{label} · {status}"));
        if let Some(project) = &row.project {
            _ = write!(accessible_label, " · {}", project.name);
        }
        if let Some(branch) = &row.branch {
            _ = write!(accessible_label, " · {branch}");
        }
        for agent in &row.agents {
            _ = write!(accessible_label, " · {}", agent.description);
        }
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
            .text()
            .p_0()
            .w_full()
            .h(gpui_kit::rems(row_height))
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
                .h(gpui_kit::rems(row_height))
                .child(activated)
                .into_any_element()
        } else {
            element
        }
    }

    fn render_row(&self, row: &SidebarRow, in_session_block: bool) -> gpui_kit::AnyElement {
        let snapshot = &self.snapshot;
        let row_height = match row.kind {
            SidebarRowKind::Group => GROUP_ROW_HEIGHT,
            SidebarRowKind::Session => {
                SESSION_ROW_HEIGHT
                    + if row.project.is_some() {
                        PROJECT_HEADER_HEIGHT
                    } else {
                        0.0
                    }
            }
            _ => ROW_HEIGHT,
        };
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
            .h(gpui_kit::rems(row_height))
            .pl_2()
            .pr_2()
            .flex()
            .items_center()
            .text_sm()
            .text_left()
            .overflow_hidden()
            .when(keyboard_focused, |element| {
                element.child(self.focus_outline(row))
            })
            .when(!snapshot.focused, |row| {
                row.opacity(1.0 - snapshot.dim_when_unfocused.clamp(0.0, 1.0))
            })
            .bg(if in_session_block {
                gpui_kit::Hsla::transparent_black()
            } else {
                color(self.background(row))
            })
            .rounded(self.radius)
            .child(if matches!(row.kind, SidebarRowKind::Session) {
                self.session_label(row)
            } else {
                self.label(row)
            })
            .children(self.bounds_probe(row))
            .children(self.diff_button(row));
        let element = self.drag_row(element, row);
        let element = if !in_session_block && matches!(row.kind, SidebarRowKind::Group) {
            self.drop_target(element, row)
        } else {
            element
        };
        self.row_control(element, row, row_height)
    }
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
    let pace = snapshot
        .meter
        .expected_remaining_percent
        .map(|expected| format!("{:+.0}%", snapshot.meter.remaining_percent - expected));
    let selector = |part: &str| {
        let name = format!("sidebar-footer-{}-{part}", item.key);
        move || name.clone()
    };
    div()
        .id(SharedString::from(format!("usage-meter-{}", item.key)))
        .tooltip({
            let description = snapshot.description.clone();
            move |window, cx| Tooltip::new(description.clone()).build(window, cx)
        })
        .w_full()
        .min_w_0()
        .h(gpui_kit::rems(1.25))
        .flex()
        .items_center()
        .gap_1()
        .when_some(item.icon.as_deref(), |element, icon| {
            element.child(crate::gpui::sized_icon(
                icon,
                crate::gpui::IconSize::Small,
                color(snapshot.fill),
            ))
        })
        .child(
            div()
                .debug_selector(selector("labels"))
                .flex_none()
                .text_color(color(item.color))
                .child(snapshot.label.clone()),
        )
        .child(
            div()
                .id(SharedString::from(format!("usage-track-{}", item.key)))
                .debug_selector(selector("track"))
                .relative()
                .flex_1()
                .min_w_0()
                .h(gpui_kit::rems(0.375))
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
        .when_some(pace, |element, pace| {
            element.child(
                div()
                    .debug_selector(selector("pace"))
                    .flex_none()
                    .text_color(color(snapshot.pace))
                    .child(pace),
            )
        })
        .when(!snapshot.meter.reset.is_empty(), |element| {
            element.child(
                div()
                    .debug_selector(selector("reset"))
                    .flex_none()
                    .text_color(color(colors.muted))
                    .child(format!("↻ {}", snapshot.meter.reset.replace(' ', ""))),
            )
        })
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
                .px_2()
                .py_0p5()
                .gap_1()
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
