use num_traits::ToPrimitive as _;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use std::{
    cell::{Cell, RefCell},
    fmt::Write as _,
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Side, Sizable as _,
    input::{Input, InputState},
    menu::{ContextMenuExt, DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
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
    SidebarPosition, SidebarRow, SidebarRowKind, SidebarSnapshot, SidebarTask, SpaceSnapshot,
    SpaceTransition, TaskView, TitlebarSnapshot, UsageMeterSnapshot, color, space_switcher,
};
use crate::gpui::theme::{mix, readable_color};

const ROW_HEIGHT: f32 = 1.75;
const SESSION_ROW_HEIGHT: f32 = 4.25;
const GROUPED_SESSION_ROW_HEIGHT: f32 = 3.0;
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
    rows: SidebarRows,
    row: SidebarRow,
}

impl Render for DraggedSidebarPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "sidebar-drag-preview".to_owned())
            .w(px((self.rows.width - 16.0).max(0.0)))
            .h(gpui_kit::rems(if self.rows.snapshot.group_by_project {
                GROUPED_SESSION_ROW_HEIGHT
            } else {
                SESSION_ROW_HEIGHT
            }))
            .px_4()
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(self.rows.radius)
            .bg(color(self.rows.background(&self.row)))
            .shadow_md()
            .child(self.rows.session_label(&self.row))
    }
}

pub(super) type SidebarRowBounds = Rc<RefCell<Vec<(SessionTarget, Bounds<Pixels>)>>>;

// Keep independent snapshot owners explicit until the chrome snapshot owns this projection.
#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    snapshot: &SidebarSnapshot,
    search: &Entity<InputState>,
    project_filter: Option<&str>,
    task_view: TaskView,
    attention_only: bool,
    drag_order: Option<&(String, Option<String>)>,
    title: &TitlebarSnapshot,
    pointer_hovered_session: Option<&SessionTarget>,
    spaces: &[SpaceSnapshot],
    transition: Option<SpaceTransition>,
    layout: &ChromeLayout,
    header_height: f32,
    docked: bool,
    colors: ChromePalette,
    sidebar_row_bounds: &SidebarRowBounds,
    scroll: &gpui_kit::ScrollHandle,
    reveal_current: &Rc<Cell<bool>>,
    reconcile_hover: bool,
    navigation_hints: &[(String, gpui_kit::Keystroke)],
    hint_modifiers: gpui_kit::Modifiers,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    let width = layout.effective_sidebar_width();
    let position = layout.sidebar_position;
    let query = search.read(cx).value().trim().to_lowercase();
    let views = task_view_counts(snapshot, &query, project_filter);
    let rows = SidebarRows {
        snapshot: task_view_snapshot(snapshot, &query, project_filter, task_view),
        search: search.clone(),
        project_filter: project_filter.map(str::to_owned),
        projects: project_paths(&snapshot.rows),
        searching: !query.is_empty(),
        attention_only,
        drag_order: drag_order.cloned(),
        width,
        task_view,
        views,
        pointer_hovered_session: pointer_hovered_session.cloned(),
        colors,
        radius: cx.theme().radius_tokens().md,
        icon_size: f32::from(cx.theme().font_size) * 0.875,
        owner: cx.weak_entity(),
        row_bounds: sidebar_row_bounds.clone(),
        scroll: scroll.clone(),
        reveal_current: reveal_current.clone(),
        reconcile_hover,
        session_ordinal: Rc::new(Cell::new(0)),
        navigation_hints: navigation_hints.to_vec(),
        hint_modifiers,
    };
    let toolbar = rows.search_toolbar();
    let content = v_flex()
        .id("bootty-sidebar-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .child(rows.render_content(width, docked).flex_none().min_h_full())
        .vertical_scrollbar(scroll);
    let status_footer = render_codexbar(snapshot, colors);

    let resize_handle = resize_handle(position, cx);

    let header = sidebar_header(snapshot, title, layout, header_height, docked, colors, cx);
    let component_footer = v_flex()
        .w(px(width))
        .when(docked, gpui_kit::Styled::w_full)
        .when_some(status_footer, ParentElement::child)
        .child(space_switcher::render(
            spaces,
            transition,
            SPACE_SWITCHER_HEIGHT,
            snapshot.tint,
            colors,
            navigation_hints,
            hint_modifiers,
            cx,
        ));
    let sidebar = v_flex()
        .id("bootty-gpui-sidebar")
        .size_full()
        .bg(color(snapshot.tint))
        .text_color(color(snapshot.foreground))
        .child(
            v_flex()
                .flex_none()
                .w_full()
                .when_some(header, ParentElement::child)
                .child(toolbar),
        )
        .child(content)
        .child(component_footer);

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

fn task_view_counts(
    snapshot: &SidebarSnapshot,
    query: &str,
    project_filter: Option<&str>,
) -> [(TaskView, usize); 5] {
    let view_rows = filtered_rows(
        &snapshot.rows,
        query,
        project_filter,
        None,
        snapshot.now_utc,
    );
    TASK_VIEWS.map(|view| {
        (
            view,
            view_rows
                .iter()
                .filter(|row| row.kind.is_session() && row_view(row, snapshot.now_utc) == view)
                .count(),
        )
    })
}

fn task_view_snapshot(
    snapshot: &SidebarSnapshot,
    query: &str,
    project_filter: Option<&str>,
    task_view: TaskView,
) -> SidebarSnapshot {
    SidebarSnapshot {
        rows: filtered_rows(
            &snapshot.rows,
            query,
            project_filter,
            Some(task_view),
            snapshot.now_utc,
        ),
        ..snapshot.clone()
    }
}

fn filtered_rows(
    rows: &[SidebarRow],
    query: &str,
    project_filter: Option<&str>,
    task_view: Option<TaskView>,
    now: i64,
) -> Vec<SidebarRow> {
    if query.is_empty() && project_filter.is_none() && task_view.is_none() {
        return rows.to_vec();
    }
    let mut filtered = Vec::new();
    let mut group = None;
    let mut group_added = false;
    let mut parent_view = TaskView::Active;
    for row in rows {
        if matches!(row.kind, SidebarRowKind::Group) {
            group = Some(row);
            group_added = false;
            parent_view = TaskView::Active;
            continue;
        }
        if row.kind.is_session() {
            parent_view = row_view(row, now);
        }
        let view = if matches!(
            row.kind,
            SidebarRowKind::Detail | SidebarRowKind::Progress { .. } | SidebarRowKind::Ports(_)
        ) {
            parent_view
        } else {
            row_view(row, now)
        };
        if task_view.is_some_and(|selected| {
            selected != view && !(selected == TaskView::Active && view == TaskView::Settled)
        }) {
            continue;
        }
        let matches = std::iter::once(row.text.as_str())
            .chain(row.project.iter().map(|project| project.name.as_str()))
            .chain(group.iter().map(|group| group.text.as_str()))
            .chain(row.branch.as_deref())
            .chain(row.agents.iter().map(|agent| agent.description.as_str()))
            .any(|text| text.to_lowercase().contains(query));
        if matches && project_filter.is_none_or(|path| row.project_path.as_deref() == Some(path)) {
            if !group_added && let Some(group) = group {
                filtered.push(group.clone());
                group_added = true;
            }
            filtered.push(row.clone());
        }
    }
    if task_view.is_none_or(|view| view == TaskView::Active) {
        for row in rows
            .iter()
            .filter(|row| matches!(row.kind, SidebarRowKind::Group) && row.project_path.is_some())
        {
            if !rows.iter().any(|session| {
                session.kind.is_session() && session.project_path == row.project_path
            }) && row.text.to_lowercase().contains(query)
                && project_filter.is_none_or(|path| row.project_path.as_deref() == Some(path))
            {
                filtered.push(row.clone());
            }
        }
    }
    filtered
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SessionSection {
    Pinned,
    Attention,
    Working,
    Active,
    Settled,
}

#[derive(Clone)]
struct SidebarRows {
    snapshot: SidebarSnapshot,
    search: Entity<InputState>,
    project_filter: Option<String>,
    projects: Vec<ProjectChoice>,
    searching: bool,
    attention_only: bool,
    drag_order: Option<(String, Option<String>)>,
    width: f32,
    task_view: TaskView,
    views: [(TaskView, usize); 5],
    pointer_hovered_session: Option<SessionTarget>,
    colors: ChromePalette,
    radius: Pixels,
    icon_size: f32,
    owner: WeakEntity<GpuiChrome>,
    row_bounds: SidebarRowBounds,
    scroll: gpui_kit::ScrollHandle,
    reveal_current: Rc<Cell<bool>>,
    reconcile_hover: bool,
    session_ordinal: Rc<Cell<usize>>,
    navigation_hints: Vec<(String, gpui_kit::Keystroke)>,
    hint_modifiers: gpui_kit::Modifiers,
}

impl SidebarRows {
    fn render_content(&self, width: f32, docked: bool) -> Div {
        let colors = self.colors;
        let blocks = self.render_session_blocks();
        let empty = blocks.is_empty();
        v_flex()
            .w(px(width))
            .when(docked, gpui_kit::Styled::w_full)
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
            .when(empty, |element| {
                element.child(
                    v_flex()
                        .debug_selector(|| "sidebar-empty-state".to_owned())
                        .w_full()
                        .p_4()
                        .gap_2()
                        .text_sm()
                        .text_color(color(colors.muted))
                        .child(if self.attention_only {
                            "No sessions need attention"
                        } else if self.searching {
                            "No matching sessions"
                        } else {
                            "No sessions in this view"
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
            .children(blocks)
            .child(self.reorder_end())
            .child(self.hover_reconciliation())
    }

    fn search_toolbar(&self) -> gpui_kit::AnyElement {
        let create_owner = self.owner.clone();
        let add_owner = self.owner.clone();
        let rows = self.clone();
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
                Button::new("sidebar-view")
                    .debug_selector(|| "sidebar-view".to_owned())
                    .ghost()
                    .small()
                    .size_7()
                    .icon(gpui_kit::assets::IconName::ListFilter)
                    .text_color(color(self.colors.subtext))
                    .accessibility_label("Filter and sort sessions")
                    .tooltip("Filter and sort sessions")
                    .dropdown_menu(move |menu, window, cx| rows.options_menu(menu, window, cx)),
            )
            .child(
                Button::new("sidebar-add-project")
                    .debug_selector(|| "sidebar-add-project".to_owned())
                    .ghost()
                    .small()
                    .size_7()
                    .icon(gpui_kit::assets::IconName::FolderPlus)
                    .text_color(color(self.colors.subtext))
                    .accessibility_label("Add project")
                    .tooltip("Add project")
                    .on_click(move |_, _, cx| emit_sidebar_command(&add_owner, "add_project", cx)),
            )
            .child(
                Button::new("sidebar-new-session")
                    .debug_selector(|| "sidebar-new-session".to_owned())
                    .ghost()
                    .small()
                    .size_7()
                    .icon(gpui_kit::assets::IconName::SquarePen)
                    .text_color(color(self.colors.subtext))
                    .accessibility_label("New session")
                    .tooltip("New session")
                    .on_click(move |_, _, cx| {
                        emit_sidebar_command(&create_owner, "new_mux_session", cx);
                    }),
            )
            .into_any_element()
    }

    fn options_menu(
        &self,
        menu: gpui_kit::component::menu::PopupMenu,
        window: &mut Window,
        cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
    ) -> gpui_kit::component::menu::PopupMenu {
        use gpui_kit::{assets::IconName as I, component::Icon};
        let grouped = self.snapshot.group_by_project;
        let group_rows = self.clone();
        let project_rows = self.clone();
        let sort_owner = self.owner.clone();
        let order = self.snapshot.sort_order;
        let view_rows = self.clone();
        let menu = super::view::popup_focus_context(menu, &self.owner, cx);
        menu.submenu_with_icon(
            Some(Icon::new(I::LayoutDashboard)),
            format!("Group by · {}", if grouped { "Project" } else { "None" }),
            window,
            cx,
            move |menu, _, cx| {
                group_rows.group_menu(super::view::popup_focus_context(
                    menu,
                    &group_rows.owner,
                    cx,
                ))
            },
        )
        .submenu_with_icon(
            Some(Icon::new(I::Folder)),
            format!(
                "Project · {}",
                self.project_filter.as_deref().map_or("All", project_name)
            ),
            window,
            cx,
            move |menu, _, cx| {
                project_rows.project_menu(super::view::popup_focus_context(
                    menu,
                    &project_rows.owner,
                    cx,
                ))
            },
        )
        .submenu_with_icon(
            Some(Icon::new(I::SortDescending)),
            format!(
                "Sort · {}",
                if order == bootty_config::config::SidebarSortOrder::Manual {
                    "Manual"
                } else {
                    "Recent activity"
                }
            ),
            window,
            cx,
            move |menu, _, cx| {
                let menu = super::view::popup_focus_context(menu, &sort_owner, cx);
                [
                    (
                        bootty_config::config::SidebarSortOrder::Manual,
                        "Manual",
                        "ui.sidebar.sort_manual",
                    ),
                    (
                        bootty_config::config::SidebarSortOrder::RecentActivity,
                        "Recent activity",
                        "ui.sidebar.sort_recent_activity",
                    ),
                ]
                .into_iter()
                .fold(menu, |menu, (desired, label, command)| {
                    let owner = sort_owner.clone();
                    menu.item(
                        PopupMenuItem::new(label)
                            .checked(order == desired)
                            .on_click(move |_, _, cx| {
                                if order != desired {
                                    emit_sidebar_command(&owner, command, cx);
                                }
                            }),
                    )
                })
            },
        )
        .submenu_with_icon(
            Some(Icon::new(I::ListFilter)),
            format!(
                "Show · {}",
                if self.attention_only {
                    "Needs attention"
                } else if self.task_view == TaskView::Active {
                    "Sessions"
                } else {
                    task_view_label(self.task_view)
                }
            ),
            window,
            cx,
            move |menu, _, cx| {
                view_rows.views_menu(super::view::popup_focus_context(menu, &view_rows.owner, cx))
            },
        )
    }

    fn group_menu(
        &self,
        menu: gpui_kit::component::menu::PopupMenu,
    ) -> gpui_kit::component::menu::PopupMenu {
        [("Project", true), ("None", false)]
            .into_iter()
            .fold(menu, |menu, (label, desired)| {
                let owner = self.owner.clone();
                let grouped = self.snapshot.group_by_project;
                menu.item(
                    PopupMenuItem::new(label)
                        .checked(grouped == desired)
                        .on_click(move |_, _, cx| {
                            if grouped != desired {
                                emit_sidebar_command(&owner, "ui.sidebar.toggle_grouping", cx);
                            }
                        }),
                )
            })
    }

    fn project_menu(
        &self,
        menu: gpui_kit::component::menu::PopupMenu,
    ) -> gpui_kit::component::menu::PopupMenu {
        let all_owner = self.owner.clone();
        let menu = menu.check_side(Side::Right).item(
            project_menu_item("all-projects".to_owned(), "All projects".to_owned(), None)
                .checked(self.project_filter.is_none())
                .on_click(move |_, _, cx| {
                    _ = all_owner.update(cx, |chrome, cx| {
                        chrome.sidebar_project = None;
                        chrome.sidebar_reveal_current.set(false);
                        cx.notify();
                    });
                }),
        );
        self.projects.iter().fold(menu, |menu, project| {
            let owner = self.owner.clone();
            let path = project.path.clone();
            menu.item(
                project_menu_item(
                    path.clone(),
                    project_name(&path).to_owned(),
                    project.artwork.clone(),
                )
                .checked(self.project_filter.as_deref() == Some(&path))
                .on_click(move |_, _, cx| {
                    _ = owner.update(cx, |chrome, cx| {
                        chrome.sidebar_project = Some(path.clone());
                        chrome.sidebar_reveal_current.set(false);
                        cx.notify();
                    });
                }),
            )
        })
    }

    fn views_menu(
        &self,
        menu: gpui_kit::component::menu::PopupMenu,
    ) -> gpui_kit::component::menu::PopupMenu {
        let selected = self.task_view;
        let attention = self.attention_only;
        let menu = self
            .views
            .into_iter()
            .filter(|(view, _)| *view != TaskView::Settled)
            .fold(menu.check_side(Side::Right), |menu, (view, count)| {
                let owner = self.owner.clone();
                menu.item(
                    PopupMenuItem::new(if view == TaskView::Active {
                        format!(
                            "Sessions ({})",
                            self.views
                                .iter()
                                .filter(|(view, _)| matches!(
                                    view,
                                    TaskView::Active | TaskView::Settled
                                ))
                                .map(|(_, count)| count)
                                .sum::<usize>()
                        )
                    } else {
                        format!("{} ({count})", task_view_label(view))
                    })
                    .icon(gpui_kit::component::Icon::new(task_view_icon(view)))
                    .checked(selected == view && !attention)
                    .on_click(move |_, _, cx| {
                        _ = owner.update(cx, |chrome, cx| {
                            chrome.sidebar_task_view = view;
                            chrome.sidebar_attention_only = false;
                            chrome.sidebar_reveal_current.set(false);
                            cx.notify();
                        });
                    }),
                )
            });
        let owner = self.owner.clone();
        menu.separator().item(
            PopupMenuItem::new("Needs attention")
                .icon(gpui_kit::component::Icon::new(
                    gpui_kit::assets::IconName::CircleAlert,
                ))
                .checked(attention)
                .on_click(move |_, _, cx| {
                    _ = owner.update(cx, |chrome, cx| {
                        chrome.sidebar_task_view = TaskView::Active;
                        chrome.sidebar_attention_only = true;
                        chrome.sidebar_reveal_current.set(false);
                        cx.notify();
                    });
                }),
        )
    }

    fn presentation_rows(&self) -> Vec<(SidebarRow, SessionSection)> {
        let mut group = None;
        let mut sessions = Vec::new();
        for row in &self.snapshot.rows {
            if matches!(row.kind, SidebarRowKind::Group) {
                group = Some(row.clone());
            }
            if row.kind.is_session() && (!self.attention_only || needs_attention(row)) {
                sessions.push((row.clone(), group.clone()));
            }
        }
        if self.snapshot.sort_order == bootty_config::config::SidebarSortOrder::RecentActivity {
            sessions.sort_by_key(|(row, _)| {
                std::cmp::Reverse(
                    row.task
                        .as_ref()
                        .and_then(|task| task.state.last_activity_at),
                )
            });
        }
        if let Some((source, before)) = &self.drag_order
            && let Some(index) = sessions
                .iter()
                .position(|(row, _)| row.reorder_anchor.as_ref() == Some(source))
        {
            let dragged = sessions.remove(index);
            let target = before
                .as_ref()
                .and_then(|before| {
                    sessions
                        .iter()
                        .position(|(row, _)| row.reorder_anchor.as_ref() == Some(before))
                })
                .unwrap_or(sessions.len());
            sessions.insert(target, dragged);
        }
        // Pinning changes presentation only; membership remains the one manual order owner.
        sessions.sort_by_key(|(row, _)| self.section(row));
        if self.snapshot.group_by_project {
            let mut group_order = std::collections::BTreeMap::new();
            for (row, group) in &sessions {
                let key = (
                    self.section(row),
                    group.as_ref().map(|group| group.key.clone()),
                );
                let next = group_order.len();
                group_order.entry(key).or_insert(next);
            }
            sessions.sort_by_key(|(row, group)| {
                let key = (
                    self.section(row),
                    group.as_ref().map(|group| group.key.clone()),
                );
                group_order.get(&key).copied()
            });
        }
        let mut result = Vec::new();
        let mut previous_group = None;
        let mut previous_section = None;
        for (row, group) in sessions {
            let section = self.section(&row);
            if previous_section != Some(section) {
                previous_group = None;
            }
            if self.snapshot.group_by_project
                && let Some(group) = group
                && previous_group.as_ref() != Some(&group.key)
            {
                previous_group = Some(group.key.clone());
                let mut group = group;
                group.key = match section {
                    SessionSection::Pinned => format!("pinned-{}", group.key),
                    SessionSection::Settled => format!("settled-{}", group.key),
                    SessionSection::Attention => format!("attention-{}", group.key),
                    SessionSection::Working => format!("working-{}", group.key),
                    SessionSection::Active => group.key,
                };
                result.push((group, section));
            }
            previous_section = Some(section);
            if !self.project_collapsed(row.project_path.as_deref())
                || !self.snapshot.group_by_project
            {
                result.push((row, section));
            }
        }
        self.append_empty_projects(&mut result);
        let tail = result
            .iter()
            .position(|(_, section)| *section == SessionSection::Settled)
            .unwrap_or(result.len());
        result.splice(tail..tail, self.unassigned_rows());
        result
    }

    fn append_empty_projects(&self, result: &mut Vec<(SidebarRow, SessionSection)>) {
        if self.snapshot.group_by_project
            && self.task_view == TaskView::Active
            && !self.attention_only
        {
            for group in self.snapshot.rows.iter().filter(|row| {
                matches!(row.kind, SidebarRowKind::Group) && row.project_path.is_some()
            }) {
                if !result.iter().any(|(row, _)| {
                    matches!(row.kind, SidebarRowKind::Group)
                        && row.project_path == group.project_path
                }) {
                    let tail = result
                        .iter()
                        .position(|(_, section)| *section == SessionSection::Settled)
                        .unwrap_or(result.len());
                    result.insert(tail, (group.clone(), SessionSection::Active));
                }
            }
        }
    }

    fn project_collapsed(&self, path: Option<&str>) -> bool {
        path.is_some_and(|path| {
            self.snapshot
                .projects
                .iter()
                .any(|project| project.cwd == path && project.collapsed)
        })
    }

    fn unassigned_rows(&self) -> Vec<(SidebarRow, SessionSection)> {
        if self.task_view != TaskView::Active || self.attention_only {
            return Vec::new();
        }
        let mut group = None;
        let mut rows = Vec::new();
        for row in &self.snapshot.rows {
            if matches!(row.kind, SidebarRowKind::Group) {
                group = Some(row.clone());
            } else if matches!(&row.kind, SidebarRowKind::Other(kind) if kind == "unassigned") {
                rows.extend(group.take().map(|group| (group, SessionSection::Active)));
                rows.push((row.clone(), SessionSection::Active));
            }
        }
        rows
    }

    fn section(&self, row: &SidebarRow) -> SessionSection {
        if row_is_pinned(row, self.snapshot.now_utc) {
            SessionSection::Pinned
        } else if row_view(row, self.snapshot.now_utc) == TaskView::Settled {
            SessionSection::Settled
        } else if row.needs_attention {
            SessionSection::Attention
        } else if row.working {
            SessionSection::Working
        } else {
            SessionSection::Active
        }
    }

    fn render_session_blocks(&self) -> Vec<gpui_kit::AnyElement> {
        let mut blocks = Vec::new();
        let mut previous_section = None;
        for (row, section) in self.presentation_rows() {
            if previous_section != Some(section) {
                let (selector, label) = match section {
                    SessionSection::Pinned => ("sidebar-pinned-section", "Pinned"),
                    SessionSection::Attention => ("sidebar-attention-section", "Needs attention"),
                    SessionSection::Working => ("sidebar-working-section", "Working"),
                    SessionSection::Active => ("sidebar-sessions-section", "Sessions"),
                    SessionSection::Settled => ("sidebar-settled-section", "Settled"),
                };
                blocks.push(
                    div()
                        .debug_selector(move || selector.to_owned())
                        .when(section == SessionSection::Settled, Styled::mt_auto)
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(color(readable_color(self.snapshot.tint, self.colors.muted)))
                        .child(label)
                        .into_any_element(),
                );
            }
            previous_section = Some(section);
            blocks.push(if row.kind.is_session() {
                self.session_block(&row)
            } else {
                self.render_row(&row, false)
            });
        }
        blocks
    }

    fn session_block(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
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
        div()
            .px_2()
            .py_0p5()
            .child(self.drop_target(block, row))
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
        if self.snapshot.sort_order != bootty_config::config::SidebarSortOrder::Manual {
            return Empty.into_any_element();
        }
        div()
            .id("sidebar-reorder-end")
            .h(px(12.0))
            .on_drag_move::<DraggedSidebarRow>({
                let owner = self.owner.clone();
                move |event, _, cx| {
                    if !event.bounds.contains(&event.event.position) {
                        return;
                    }
                    let source = event.drag(cx).source.clone();
                    _ = owner.update(cx, |chrome, cx| {
                        let desired = Some((source, None));
                        if chrome.sidebar_drag_order != desired {
                            chrome.sidebar_drag_order = desired;
                            cx.notify();
                        }
                    });
                }
            })
            .on_drop({
                let owner = self.owner.clone();
                move |dragged: &DraggedSidebarRow, window, cx| {
                    _ = owner.update(cx, |this, cx| {
                        this.finish_sidebar_drag(window, cx, true);
                        cx.emit(ChromeIntent::ReorderSession {
                            source: dragged.source.clone(),
                            before: None,
                        });
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
    fn text_color(&self, row: &SidebarRow, preferred: Rgba) -> Rgba {
        let background = self.background(row);
        let preferred = if self.snapshot.focused {
            preferred
        } else {
            mix(preferred, background, self.snapshot.dim_when_unfocused)
        };
        // Dim the preferred text before enforcing contrast, never the painted row afterward.
        readable_color(background, preferred)
    }

    fn background(&self, row: &SidebarRow) -> Rgba {
        let base = if row.current {
            self.snapshot.current
        } else if row
            .target
            .as_ref()
            .is_some_and(|target| self.pointer_hovered_session.as_ref() == Some(target))
        {
            self.snapshot.hover
        } else {
            self.snapshot.tint
        };
        if row.kind.is_session() {
            mix(base, row.color, if row.current { 0.18 } else { 0.055 })
        } else {
            base
        }
    }

    fn label(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        let snapshot = &self.snapshot;
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
            .when(
                row.kind.is_session() && snapshot.group_by_project,
                |label| label.min_w(gpui_kit::rems(4.0)),
            )
            .flex()
            .items_center()
            // Keep sidebar labels aligned along the same scan lane.
            .text_left()
            .gap_1()
            .when(!row.kind.is_session(), |label| {
                label
                    .children(self.project_disclosure(row))
                    .child(self.leading_icon(row))
            })
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
                            .text_color(color(self.text_color(
                                row,
                                if row.kind.is_session() {
                                    if self.section(row) == SessionSection::Settled && !row.current
                                    {
                                        self.colors.muted
                                    } else {
                                        snapshot.foreground
                                    }
                                } else if is_group {
                                    if current { row.color } else { row.dim_color }
                                } else if row.active {
                                    row.color
                                } else {
                                    snapshot.foreground
                                },
                            )))
                            .child(row_text),
                    )
                    .when_some(
                        row.secondary.clone().filter(|_| !row.kind.is_session()),
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
            .when(!row.kind.is_session(), |label| {
                label.children(self.trailing_label(row))
            })
            .into_any_element()
    }

    fn project_disclosure(&self, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
        (matches!(row.kind, SidebarRowKind::Group) && row.project_path.is_some()).then(|| {
            crate::gpui::icon(
                if self.project_collapsed(row.project_path.as_deref()) {
                    "chevron-right"
                } else {
                    "chevron-down"
                },
                self.icon_size,
                color(row.dim_color),
            )
            .into_any_element()
        })
    }

    fn leading_icon(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        let slot = div()
            .size_4()
            .flex_none()
            .flex()
            .items_center()
            .justify_center();
        slot.map(|slot| {
            if let Some(artwork) = &row.artwork {
                slot.child(img(artwork.clone()).size_full())
            } else if let Some(icon) = &row.icon {
                slot.child(crate::gpui::icon(
                    icon,
                    self.icon_size,
                    color(if row.current || row.active {
                        row.color
                    } else {
                        row.dim_color
                    }),
                ))
            } else {
                slot
            }
        })
        .into_any_element()
    }

    fn session_label(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap_0p5()
            .when(!self.snapshot.group_by_project, |label| {
                label.child(self.session_header(row))
            })
            .child(
                div()
                    .h(gpui_kit::rems(1.375))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(self.label(row))
                    .when(self.snapshot.group_by_project, |title| {
                        title.children(self.trailing_label(row))
                    }),
            )
            .child(self.session_metadata(row))
            .into_any_element()
    }

    fn session_header(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        div()
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-project-{key}")
            })
            .h(gpui_kit::rems(PROJECT_HEADER_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap_1p5()
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
                    .gap_1p5()
                    .when_some(row.project.as_ref(), |header, project| {
                        header
                            .child(div().size_4().flex_none().map(|slot| {
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
                            .child(div().min_w_0().truncate().child(project.name.clone()))
                    }),
            )
            .children(self.trailing_label(row))
            .into_any_element()
    }

    fn session_metadata(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        div()
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-metadata-{key}")
            })
            .w_full()
            .h(gpui_kit::rems(1.125))
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
                                color(self.text_color(
                                    row,
                                    if self.section(row) == SessionSection::Settled {
                                        self.colors.muted
                                    } else {
                                        self.snapshot.foreground
                                    },
                                )),
                            ))
                    }),
            )
            .into_any_element()
    }

    fn trailing_label(&self, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
        if let Some(button) = self.settle_button(row) {
            return Some(button);
        }
        let lifecycle = row.task.as_ref().and_then(|task| {
            if task.state.is_overdue(self.snapshot.now_utc) {
                Some(("Woke", "alarm-clock", self.colors.accent))
            } else {
                match task.state.view(self.snapshot.now_utc) {
                    TaskView::Active => None,
                    TaskView::Settled => Some(("Settled", "check", self.colors.muted)),
                    TaskView::Archived => Some(("Archived", "archive", self.colors.muted)),
                    TaskView::Snoozed => Some(("Snoozed", "clock", self.colors.muted)),
                    TaskView::Hidden => Some(("Hidden", "eye-off", self.colors.muted)),
                    TaskView::Deleted => Some(("Deleted", "trash", self.colors.muted)),
                }
            }
        });
        let (trailing, icon, tone) = if let Some((text, icon, tone)) = lifecycle
            && (row.trailing.is_none() || row.trailing.as_deref() == Some("Finished"))
        {
            (text.to_owned(), Some(icon), tone)
        } else if let Some(text) = &row.trailing {
            (
                text.clone(),
                row.trailing_icon.as_deref(),
                row.trailing_color.unwrap_or(self.colors.muted),
            )
        } else {
            return None;
        };
        let trailing_color = color(readable_color(self.background(row), tone));
        div()
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-status-{key}")
            })
            .flex_initial()
            .min_w(gpui_kit::rems(1.0))
            .flex()
            .items_center()
            .gap_1()
            .max_w(gpui_kit::rems(9.0))
            .truncate()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .when(row.working, |element| {
                element.font_features(gpui_kit::FontFeatures(std::sync::Arc::new(vec![(
                    "tnum".to_owned(),
                    1,
                )])))
            })
            .text_color(trailing_color)
            .when_some(icon, |element, icon| {
                element.child(
                    div()
                        .debug_selector({
                            let key = row.key.clone();
                            move || format!("sidebar-status-icon-{key}")
                        })
                        .size_3()
                        .flex_none()
                        .child(if icon == "circle-dashed" {
                            self.working_indicator(trailing_color)
                        } else {
                            crate::gpui::icon(icon, self.icon_size * 0.85, trailing_color)
                        }),
                )
            })
            .child(div().min_w_0().truncate().child(trailing))
            .into_any_element()
            .into()
    }

    fn settle_button(&self, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
        let task = row.task.as_ref()?;
        if self.pointer_hovered_session.as_ref() != row.target.as_ref()
            || task.state.is_overdue(self.snapshot.now_utc)
            || task.state.view(self.snapshot.now_utc) != TaskView::Active
            || row
                .trailing
                .as_deref()
                .is_some_and(|status| status != "Finished")
        {
            return None;
        }
        let invocation = saved_state_invocation(task, "session.settle", Vec::new())?;
        let owner = self.owner.clone();
        let foreground = color(self.colors.text);
        let button = Button::new(SharedString::from(format!("settle-{}", row.key)))
            .ghost()
            .xsmall()
            .p_0()
            .child(
                div()
                    .id("settle-content")
                    .size_full()
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(color(self.colors.muted))
                    .when(!task.pending, |content| {
                        content.hover(move |style| style.text_color(foreground))
                    })
                    .child(
                        gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Check).xsmall(),
                    )
                    .child("Settle"),
            )
            .accessibility_label(format!("Settle {}", row.text))
            .tooltip("Settle this session")
            .disabled(task.pending);
        Some(super::button::activated_button(
            div()
                .flex_none()
                .debug_selector({
                    let key = row.key.clone();
                    move || format!("sidebar-settle-{key}")
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
            button,
            move |_, cx| {
                _ = owner.update(cx, |_, cx| {
                    cx.emit(ChromeIntent::Command(invocation.clone()));
                });
            },
        ))
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Paint the rotating status ring in its measured icon box"
    )]
    fn working_indicator(&self, tint: gpui_kit::Hsla) -> gpui_kit::AnyElement {
        let owner = self.owner.clone();
        let animate = self.snapshot.animate_working;
        canvas(
            |bounds, _, _| bounds,
            move |bounds, _, window, cx| {
                if !bounds.intersects(&window.content_mask().bounds) {
                    return;
                }
                let animated = animate && window.is_window_active() && !cx.reduce_motion();
                let phase = owner.upgrade().map_or(0.0, |owner| {
                    if animated {
                        owner
                            .read(cx)
                            .sidebar_animation_epoch
                            .elapsed()
                            .as_secs_f32()
                    } else {
                        0.0
                    }
                });
                let radius = f32::from(bounds.size.width.min(bounds.size.height)) * 0.4;
                let center = bounds.center();
                for dash in 0..8_u8 {
                    let angle = f32::from(dash)
                        .mul_add(std::f32::consts::FRAC_PI_4, phase * std::f32::consts::TAU);
                    let position = |angle: f32| {
                        gpui_kit::point(
                            center.x + px(radius * angle.cos()),
                            center.y + px(radius * angle.sin()),
                        )
                    };
                    let mut path = gpui_kit::PathBuilder::stroke(px(radius * 0.22));
                    path.move_to(position(angle));
                    path.arc_to(
                        gpui_kit::point(px(radius), px(radius)),
                        px(0.0),
                        false,
                        true,
                        position(angle + 0.45),
                    );
                    if let Ok(path) = path.build() {
                        window.paint_path(path, tint);
                    }
                }
                if animated {
                    _ = owner.update(cx, |this, cx| {
                        if this.sidebar_animation_task.is_some() {
                            return;
                        }
                        // One 30 Hz wakeup per sidebar; clipped, inactive and reduced-motion rings park.
                        this.sidebar_animation_task = Some(cx.spawn(async move |owner, cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(33))
                                .await;
                            _ = owner.update(cx, |this, cx| {
                                this.sidebar_animation_task = None;
                                cx.notify();
                            });
                        }));
                    });
                }
            },
        )
        .size_full()
        .into_any_element()
    }

    fn focus_outline(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        let keyboard_focus_key = row.key.clone();
        div()
            .debug_selector(move || format!("sidebar-keyboard-focus-{keyboard_focus_key}"))
            .absolute()
            .inset_0()
            .border_1()
            .rounded(self.radius)
            .border_color(color(mix(self.background(row), self.colors.accent, 0.45)))
            .into_any_element()
    }

    fn bounds_probe(&self, row: &SidebarRow) -> Option<gpui_kit::AnyElement> {
        let target = row.target.clone()?;
        let row_bounds = self.row_bounds.clone();
        let reveal_current = self.reveal_current.clone();
        let reveal_row = row.current && row.kind.is_session();
        let scroll = self.scroll.clone();
        let owner = self.owner.clone();
        canvas(
            move |bounds, _, cx| {
                if reveal_row && reveal_current.replace(false) {
                    let scroll = scroll.clone();
                    let owner = owner.clone();
                    cx.defer(move |cx| {
                        let viewport = scroll.bounds();
                        let adjustment = if bounds.top() < viewport.top() {
                            std::ops::Sub::sub(viewport.top(), bounds.top())
                        } else if bounds.bottom() > viewport.bottom() {
                            std::ops::Sub::sub(viewport.bottom(), bounds.bottom())
                        } else {
                            px(0.0)
                        };
                        if adjustment != px(0.0) {
                            let mut offset = scroll.offset();
                            offset.y = std::ops::Add::add(offset.y, adjustment);
                            scroll.set_offset(offset);
                            _ = owner.update(cx, |_, cx| cx.notify());
                        }
                    });
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
        if self.snapshot.sort_order != bootty_config::config::SidebarSortOrder::Manual {
            return element;
        }
        let Some(source) = row.reorder_anchor.clone() else {
            return element;
        };
        let drag = DraggedSidebarRow {
            source,
            sessions: row.target.clone().into_iter().collect(),
        };
        let preview = DraggedSidebarPreview {
            rows: self.clone(),
            row: row.clone(),
        };
        let drag_owner = self.owner.clone();
        element.on_drag(drag, move |_, _, window, cx| {
            _ = drag_owner.update(cx, |this, cx| this.begin_sidebar_drag(window, cx));
            cx.new(|_| preview.clone())
        })
    }

    fn drop_target(&self, element: Stateful<Div>, row: &SidebarRow) -> Stateful<Div> {
        if self.snapshot.sort_order != bootty_config::config::SidebarSortOrder::Manual {
            return element;
        }
        let Some(source) = row.reorder_anchor.clone() else {
            return element;
        };
        element
            .on_drag_move::<DraggedSidebarRow>({
                let source = source.clone();
                let owner = self.owner.clone();
                move |event, _, cx| {
                    if !event.bounds.contains(&event.event.position) {
                        return;
                    }
                    let dragged = event.drag(cx).source.clone();
                    if dragged == source {
                        return;
                    }
                    _ = owner.update(cx, |chrome, cx| {
                        let desired = Some((dragged, Some(source.clone())));
                        if chrome.sidebar_drag_order != desired {
                            chrome.sidebar_drag_order = desired;
                            cx.notify();
                        }
                    });
                }
            })
            .on_drop({
                let owner = self.owner.clone();
                move |dragged: &DraggedSidebarRow, window, cx| {
                    _ = owner.update(cx, |this, cx| {
                        let before = this
                            .sidebar_drag_order
                            .take()
                            .filter(|(from, _)| from == &dragged.source)
                            .map_or_else(|| Some(source.clone()), |(_, before)| before);
                        this.finish_sidebar_drag(window, cx, true);
                        if before.as_ref() != Some(&dragged.source) {
                            cx.emit(ChromeIntent::ReorderSession {
                                source: dragged.source.clone(),
                                before,
                            });
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
        let accessible_label = row_accessible_label(row, self.snapshot.now_utc);
        let context_owner = self.owner.clone();
        let context = self.row_menu(row);
        let element = if let Some(context) = context {
            element
                .context_menu(move |menu, window, cx| {
                    super::view::popup_session_menu(menu, &context, &context_owner, window, cx)
                })
                .into_any_element()
        } else {
            element.into_any_element()
        };
        if matches!(row.kind, SidebarRowKind::Group)
            && let Some(path) = row.project_path.clone()
            && let Some(target) = self.snapshot.project_target.clone()
        {
            let owner = self.owner.clone();
            let collapsed = self.project_collapsed(Some(&path));
            let button = Button::new(SharedString::from(format!("sidebar-project-{}", row.key)))
                .text()
                .p_0()
                .w_full()
                .h(gpui_kit::rems(row_height))
                .tab_index(0_isize)
                .accessibility_label(format!(
                    "{} {}",
                    if collapsed { "Expand" } else { "Collapse" },
                    row.text
                ))
                .child(element);
            let disclosure = super::button::activated_button(
                div().w_full().h(gpui_kit::rems(row_height)),
                button,
                move |_, app| {
                    _ = owner.update(app, |_, cx| {
                        let mut invocation = bootty_control::CommandInvocation::new(
                            "project.toggle_collapsed",
                            vec![path.clone()],
                            bootty_control::Caller::Internal,
                        );
                        invocation.target = Some(target.clone());
                        cx.emit(ChromeIntent::Command(invocation));
                    });
                },
            )
            .into_any_element();
            gpui_kit::component::h_flex()
                .w_full()
                .min_w_0()
                .child(div().flex_1().min_w_0().child(disclosure))
                .into_any_element()
        } else if row.selectable {
            self.row_activation(element, row, accessible_label, row_height)
        } else {
            element
        }
    }

    fn row_menu(&self, row: &SidebarRow) -> Option<ContextMenu> {
        if row.task.as_ref().is_some_and(|task| task.pending) {
            return None;
        }
        let now = self.snapshot.now_utc;
        if row.task.as_ref().is_some_and(|task| task.state.deleted) {
            return row
                .task
                .clone()
                .map(|task| ContextMenu::SavedSession { task, now });
        }
        let target = row.target.clone()?;
        let options = row.context?;
        Some(if matches!(row.kind, SidebarRowKind::DetachedSession) {
            ContextMenu::DetachedSession {
                target,
                task: row.task.clone(),
                now,
            }
        } else {
            ContextMenu::Session {
                target,
                options,
                task: row.task.clone(),
                now,
            }
        })
    }

    fn row_activation(
        &self,
        element: gpui_kit::AnyElement,
        row: &SidebarRow,
        accessible_label: String,
        row_height: f32,
    ) -> gpui_kit::AnyElement {
        let deleted = row.task.as_ref().is_some_and(|task| task.state.deleted);

        let owner = self.owner.clone();
        let activation_target = row.target.clone();
        let activation_kind = row.kind.clone();
        let native_conversation = row
            .task
            .as_ref()
            .and_then(|task| task.native_conversation.clone());
        let restore = row
            .task
            .as_ref()
            .and_then(|task| saved_state_invocation(task, "session.restore", Vec::new()));
        let restore_disabled = deleted
            && row
                .task
                .as_ref()
                .is_none_or(|task| task.pending || task.binding.is_none());
        let button = Button::new(SharedString::from(format!(
            "sidebar-row-button-{}",
            row.key
        )))
        .text()
        .p_0()
        .w_full()
        .h(gpui_kit::rems(row_height))
        .tab_index(0_isize)
        .disabled(restore_disabled)
        .accessibility_label(if deleted {
            format!("Restore {accessible_label}")
        } else if native_conversation.is_some() {
            format!("Open conversation for {accessible_label}")
        } else {
            accessible_label
        })
        .when(deleted, |button| {
            button.tooltip(
                "Restore saved work with its previous state; never start or stop a terminal",
            )
        })
        .child(element);
        let activated =
            super::button::activated_button(div().w_full().h_full(), button, move |_, app| {
                _ = owner.update(app, |_, cx| {
                    if deleted {
                        if !restore_disabled && let Some(invocation) = restore.clone() {
                            cx.emit(ChromeIntent::Command(invocation));
                        }
                    } else if let Some(target) = native_conversation.clone() {
                        let mut invocation = bootty_control::CommandInvocation::from_action(
                            "agents.native.focus",
                            bootty_control::Caller::Internal,
                        );
                        invocation.target = Some(target);
                        cx.emit(ChromeIntent::Command(invocation));
                    } else if let Some(target) = activation_target.clone() {
                        let intent = if matches!(
                            &activation_kind,
                            SidebarRowKind::Other(kind) if kind == "unassigned"
                        ) {
                            ChromeIntent::AdoptSession(target)
                        } else if matches!(activation_kind, SidebarRowKind::DetachedSession) {
                            ChromeIntent::ReopenSession(target)
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
    }

    fn render_row(&self, row: &SidebarRow, in_session_block: bool) -> gpui_kit::AnyElement {
        let shortcut = if row.kind.is_session() && row.target.is_some() {
            let ordinal = self.session_ordinal.get().saturating_add(1);
            self.session_ordinal.set(ordinal);
            let action = format!("select_session:{ordinal}");
            self.navigation_hints
                .iter()
                .find(|(name, key)| {
                    name == &action
                        && key.modifiers == self.hint_modifiers
                        && key.modifiers != gpui_kit::Modifiers::default()
                })
                .map(|(_, key)| {
                    div()
                        .absolute()
                        .right_2()
                        .bottom_1()
                        .child(gpui_kit::component::kbd::Kbd::new(key.clone()))
                })
        } else {
            None
        };
        let snapshot = &self.snapshot;
        let row_height = match row.kind {
            SidebarRowKind::Group => GROUP_ROW_HEIGHT,
            SidebarRowKind::Session | SidebarRowKind::DetachedSession => {
                if self.snapshot.group_by_project {
                    GROUPED_SESSION_ROW_HEIGHT
                } else {
                    SESSION_ROW_HEIGHT
                }
            }
            _ => ROW_HEIGHT,
        };
        let keyboard_focused = !row.current
            && row.target.as_ref().is_some_and(|target| {
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
            .when(row.kind.is_session(), gpui_kit::Styled::px_4)
            .flex()
            .items_center()
            .text_sm()
            .text_left()
            .overflow_hidden()
            .when(keyboard_focused, |element| {
                element.child(self.focus_outline(row))
            })
            .bg(if in_session_block {
                gpui_kit::Hsla::transparent_black()
            } else {
                color(self.background(row))
            })
            .rounded(self.radius)
            .when(
                matches!(row.kind, SidebarRowKind::Group) && row.project_path.is_some(),
                |element| element.hover(|element| element.bg(color(self.snapshot.hover))),
            )
            .child(if row.kind.is_session() {
                self.session_label(row)
            } else {
                self.label(row)
            })
            .children(shortcut)
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

pub(super) fn detached_session_menu(target: &SessionTarget) -> Vec<MenuRow> {
    [
        ("Rename…", ChromeIntent::RenameSavedSession(target.clone())),
        (
            "Move to Space…",
            ChromeIntent::SessionContext {
                target: target.clone(),
                action: SessionContextAction::MoveToSpace,
            },
        ),
    ]
    .into_iter()
    .map(|(label, intent)| MenuRow {
        label: label.to_owned(),
        enabled: true,
        destructive: false,
        starts_group: false,
        intent,
    })
    .collect()
}

pub(super) fn session_menu(
    target: &SessionTarget,
    options: SessionContextSnapshot,
) -> Vec<MenuRow> {
    [
        ("Rename…", options.can_rename, SessionContextAction::Rename),
        ("Move to Space…", true, SessionContextAction::MoveToSpace),
    ]
    .into_iter()
    .map(|(label, enabled, action)| MenuRow {
        label: label.to_owned(),
        enabled,
        destructive: false,
        starts_group: false,
        intent: ChromeIntent::SessionContext {
            target: target.clone(),
            action,
        },
    })
    .collect()
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

fn project_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

#[derive(Clone)]
struct ProjectChoice {
    path: String,
    artwork: Option<std::sync::Arc<gpui_kit::RenderImage>>,
}

fn project_paths(rows: &[SidebarRow]) -> Vec<ProjectChoice> {
    let mut projects = std::collections::BTreeMap::new();
    let mut group_artwork = None;
    for row in rows {
        if matches!(row.kind, SidebarRowKind::Group) {
            group_artwork.clone_from(&row.artwork);
        }
        if (row.kind.is_session() || matches!(row.kind, SidebarRowKind::Group))
            && let Some(path) = &row.project_path
        {
            projects
                .entry(path.clone())
                .or_insert_with(|| ProjectChoice {
                    path: path.clone(),
                    artwork: row
                        .project
                        .as_ref()
                        .and_then(|project| project.artwork.clone())
                        .or_else(|| group_artwork.clone()),
                });
        }
    }
    projects.into_values().collect()
}

const fn needs_attention(row: &SidebarRow) -> bool {
    row.needs_attention
}

fn row_accessible_label(row: &SidebarRow, now: i64) -> String {
    let mut label = if row.text.trim().is_empty() {
        row.key.clone()
    } else {
        row.text.clone()
    };
    for detail in row.secondary.iter().chain(row.trailing.iter()) {
        _ = write!(label, " · {detail}");
    }
    if let Some(project) = &row.project {
        _ = write!(label, " · {}", project.name);
    }
    if let Some(branch) = &row.branch {
        _ = write!(label, " · {branch}");
    }
    for agent in &row.agents {
        _ = write!(label, " · {}", agent.description);
    }
    if let Some(task) = &row.task {
        let view = task.state.view(now);
        if view != TaskView::Active {
            _ = write!(label, " · {}", task_view_label(view));
        }
        if task.state.is_overdue(now) {
            label.push_str(" · Woke");
        }
    }
    label
}

const TASK_VIEWS: [TaskView; 5] = [
    TaskView::Active,
    TaskView::Settled,
    TaskView::Archived,
    TaskView::Snoozed,
    TaskView::Hidden,
];

const fn task_view_label(view: TaskView) -> &'static str {
    match view {
        TaskView::Active => "Active",
        TaskView::Settled => "Settled",
        TaskView::Archived => "Archived",
        TaskView::Snoozed => "Snoozed",
        TaskView::Hidden => "Hidden",
        TaskView::Deleted => "Deleted",
    }
}

fn row_view(row: &SidebarRow, now: i64) -> TaskView {
    row.task
        .as_ref()
        .map_or(TaskView::Active, |task| task.state.view(now))
}

fn saved_state_invocation(
    task: &SidebarTask,
    command: &str,
    extra: Vec<String>,
) -> Option<bootty_control::CommandInvocation> {
    let mut arguments = vec![task.identity.clone()];
    arguments.extend(extra);
    let mut invocation = bootty_control::CommandInvocation::new(
        command,
        arguments,
        bootty_control::Caller::Internal,
    );
    invocation.target = Some(task.binding.clone()?);
    Some(invocation)
}

pub(super) fn saved_session_menu(task: &SidebarTask, now: i64) -> Vec<MenuRow> {
    use bootty_mux::session_membership::SessionLifecycle;
    let row = |label: &str, command, starts_group| {
        saved_state_invocation(task, command, Vec::new()).map(|invocation| MenuRow {
            label: label.to_owned(),
            enabled: !task.pending,
            destructive: command == "session.delete",
            starts_group,
            intent: ChromeIntent::Command(invocation),
        })
    };
    if task.state.deleted {
        return row("Restore", "session.restore", false)
            .into_iter()
            .collect();
    }
    let mut rows = Vec::new();
    if !task.state.archived {
        rows.extend(row(
            if task.state.pinned { "Unpin" } else { "Pin" },
            if task.state.pinned {
                "session.unpin"
            } else {
                "session.pin"
            },
            true,
        ));
        let settled = task.state.lifecycle == SessionLifecycle::Settled;
        rows.extend(row(
            if settled { "Un-settle" } else { "Settle" },
            if settled {
                "session.activate"
            } else {
                "session.settle"
            },
            false,
        ));
        if task.state.snoozed_until.is_some_and(|until| until > now) {
            rows.extend(row("Wake", "session.unsnooze", false));
        }
        rows.extend(row(
            if task.state.hidden {
                "Show session"
            } else {
                "Hide session"
            },
            if task.state.hidden {
                "session.show"
            } else {
                "session.hide"
            },
            false,
        ));
    }
    rows.extend(row(
        if task.state.archived {
            "Unarchive"
        } else {
            "Archive"
        },
        if task.state.archived {
            "session.unarchive"
        } else {
            "session.archive"
        },
        true,
    ));
    rows.extend(row("Delete…", "session.delete", false));
    rows
}

pub(super) fn saved_snooze_menu(task: &SidebarTask, now: i64) -> Vec<MenuRow> {
    if task.state.archived
        || task.state.deleted
        || task.state.snoozed_until.is_some_and(|until| until > now)
    {
        return Vec::new();
    }
    [("1 hour", 3_600), ("1 day", 86_400), ("1 week", 604_800)]
        .into_iter()
        .filter_map(|(label, seconds)| {
            let until = now.checked_add(seconds)?;
            let date = chrono::DateTime::from_timestamp(until, 0)?;
            let invocation =
                saved_state_invocation(task, "session.snooze", vec![until.to_string()])?;
            Some(MenuRow {
                label: format!("{label} ({})", date.format("%b %-d, %H:%M UTC")),
                enabled: !task.pending,
                destructive: false,
                starts_group: false,
                intent: ChromeIntent::Command(invocation),
            })
        })
        .collect()
}

fn emit_sidebar_command(owner: &WeakEntity<GpuiChrome>, command: &str, cx: &mut App) {
    _ = owner.update(cx, |_, cx| {
        cx.emit(ChromeIntent::Command(
            bootty_control::CommandInvocation::from_action(
                command,
                bootty_control::Caller::Internal,
            ),
        ));
    });
}

fn project_menu_item(
    key: String,
    label: String,
    artwork: Option<std::sync::Arc<gpui_kit::RenderImage>>,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        div()
            .id(SharedString::from(format!("project-menu-{key}")))
            .role(gpui_kit::Role::MenuItem)
            .aria_label(label.clone())
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .size_4()
                    .flex_none()
                    .child(artwork.as_ref().map_or_else(
                        || {
                            crate::gpui::sized_icon(
                                "folder",
                                crate::gpui::IconSize::Small,
                                cx.theme().muted_foreground,
                            )
                        },
                        |artwork| img(artwork.clone()).size_full().into_any_element(),
                    )),
            )
            .child(div().min_w_0().truncate().child(label.clone()))
            .into_any_element()
    })
}

const fn task_view_icon(view: TaskView) -> gpui_kit::assets::IconName {
    use gpui_kit::assets::IconName as I;
    match view {
        TaskView::Active => I::List,
        TaskView::Settled => I::CircleCheck,
        TaskView::Archived => I::Archive,
        TaskView::Snoozed => I::Clock,
        TaskView::Hidden => I::EyeOff,
        TaskView::Deleted => I::Trash,
    }
}

fn row_is_pinned(row: &SidebarRow, now: i64) -> bool {
    row.task
        .as_ref()
        .is_some_and(|task| task.state.pinned && task.state.view(now) == TaskView::Active)
}
