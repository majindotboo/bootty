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
    ChromeIntent, ChromeLayout, ChromePalette, Confirmation, ContextMenu, GpuiChrome, MenuRow,
    Rgba, SessionContextAction, SessionContextSnapshot, SessionTarget, SidebarPosition, SidebarRow,
    SidebarRowKind, SidebarSnapshot, SidebarTask, SpaceKey, SpaceSnapshot, SpaceTransition,
    TaskView, TitlebarSnapshot, UsageMeterSnapshot, color, space_switcher,
};
use crate::gpui::theme::{mix, readable_color};

const ROW_HEIGHT: f32 = 1.75;
const SESSION_ROW_HEIGHT: f32 = 5.0;
const GROUPED_SESSION_ROW_HEIGHT: f32 = 3.625;
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
    /// The rest of a dragged project's sessions, which move right after `source`.
    pub(super) following: Vec<String>,
    pub(super) sessions: Vec<SessionTarget>,
}

impl DraggedSidebarRow {
    /// Moves the dragged sessions, in order, before `before`; the end when it is `None`. Sorted
    /// by recent activity, the order `shown` becomes the manual order first, and the sidebar
    /// switches to it, so the drop lands where it was made.
    fn reorder(
        &self,
        before: Option<&str>,
        shown: Option<&[String]>,
        cx: &mut Context<GpuiChrome>,
    ) {
        if before.is_some_and(|before| {
            before == self.source || self.following.iter().any(|source| source == before)
        }) {
            return;
        }
        let moved = || std::iter::once(&self.source).chain(&self.following);
        let kept = shown
            .into_iter()
            .flatten()
            .filter(|source| moved().all(|moving| moving != *source))
            .map(|source| (source, None));
        let placed = moved().map(|source| (source, before.map(str::to_owned)));
        for (source, before) in kept.chain(placed) {
            cx.emit(ChromeIntent::ReorderSession {
                source: source.clone(),
                before,
            });
        }
        if shown.is_some() {
            cx.emit(ChromeIntent::Command(
                bootty_control::CommandInvocation::from_action(
                    "ui.sidebar.sort_manual",
                    bootty_control::Caller::Internal,
                ),
            ));
        }
    }
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
            .p_2()
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

/// Settled sessions the shelf shows before Show more.
const SETTLED_PAGE: usize = 10;

/// How much of the settled shelf shows; whether it is open is saved in the configuration.
#[derive(Clone, Copy)]
pub(super) struct SettledShelf {
    pub shown: usize,
}
impl Default for SettledShelf {
    fn default() -> Self {
        Self {
            shown: SETTLED_PAGE,
        }
    }
}

/// The sidebar's own state in the chrome: search, filters, selection, renaming and scrolling.
#[derive(Clone, Copy)]
pub(super) struct SidebarView<'a> {
    pub search: &'a Entity<InputState>,
    pub project_filter: Option<&'a str>,
    pub task_view: TaskView,
    pub attention_only: bool,
    pub settled_shelf: SettledShelf,
    pub drag_order: Option<&'a (String, Option<String>)>,
    pub rename: Option<&'a super::view::SidebarRename>,
    pub selection: &'a [String],
    pub conversation_matches: &'a [(String, String)],
    pub row_bounds: &'a SidebarRowBounds,
    pub scroll: &'a gpui_kit::ScrollHandle,
    pub reveal_current: &'a Rc<Cell<bool>>,
    pub reconcile_hover: bool,
}

/// What the sidebar shows from the rest of the chrome, and where it sits. Each snapshot owner
/// stays explicit until the chrome snapshot owns this projection.
#[derive(Clone, Copy)]
pub(super) struct SidebarChrome<'a> {
    pub title: &'a TitlebarSnapshot,
    pub pointer_hovered_session: Option<&'a SessionTarget>,
    pub spaces: &'a [SpaceSnapshot],
    pub transition: Option<SpaceTransition>,
    pub layout: &'a ChromeLayout,
    pub header_height: f32,
    pub docked: bool,
    pub colors: ChromePalette,
    pub navigation_hints: &'a [(String, gpui_kit::Keystroke)],
    pub hint_modifiers: gpui_kit::Modifiers,
}

pub(super) fn render(
    snapshot: &SidebarSnapshot,
    view: SidebarView<'_>,
    chrome: SidebarChrome<'_>,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    let SidebarChrome {
        title,
        layout,
        header_height,
        docked,
        colors,
        ..
    } = chrome;
    let scroll = view.scroll;
    let width = layout.effective_sidebar_width();
    let position = layout.sidebar_position;
    let rows = sidebar_rows(snapshot, view, chrome, cx);
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
        .child(space_switcher::render(chrome, snapshot.tint, cx));
    let sidebar = v_flex()
        .id("bootty-gpui-sidebar")
        .size_full()
        .bg(color(snapshot.tint))
        .text_color(color(snapshot.foreground))
        .child(
            v_flex()
                .flex_none()
                .w_full()
                .children(header)
                .child(toolbar),
        )
        .child(content)
        .child(component_footer);

    shell(width, docked)
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if !hovered && this.pointer_hovered_session.take().is_some() {
                cx.notify();
            }
        }))
        .child(sidebar)
        .when(!layout.fullscreen, |element| {
            element.child(edge(position, snapshot.border))
        })
        .when(!docked, |element| element.child(resize_handle))
        .into_any_element()
}

/// The rows the sidebar lists for its search, filters and task view.
fn sidebar_rows(
    snapshot: &SidebarSnapshot,
    view: SidebarView<'_>,
    chrome: SidebarChrome<'_>,
    cx: &Context<GpuiChrome>,
) -> SidebarRows {
    let SidebarView {
        search,
        project_filter,
        task_view,
        attention_only,
        settled_shelf,
        drag_order,
        rename,
        selection,
        conversation_matches,
        row_bounds: sidebar_row_bounds,
        scroll,
        reveal_current,
        reconcile_hover,
    } = view;
    let SidebarChrome {
        pointer_hovered_session,
        spaces,
        layout,
        colors,
        navigation_hints,
        hint_modifiers,
        ..
    } = chrome;
    let width = layout.effective_sidebar_width();
    let query = search.read(cx).value().trim().to_lowercase();
    let words = Search {
        query: &query,
        conversations: conversation_matches,
    };
    let views = task_view_counts(snapshot, &words, project_filter);
    SidebarRows {
        snapshot: task_view_snapshot(snapshot, &words, project_filter, task_view),
        search: search.clone(),
        project_filter: project_filter.map(str::to_owned),
        projects: project_paths(&snapshot.rows),
        searching: !query.is_empty(),
        attention_only,
        settled_shelf,
        drag_order: drag_order.cloned(),
        rename: rename.map(|rename| (rename.task.identity.clone(), rename.input.clone())),
        selection: selection.to_vec(),
        visible_sessions: Rc::default(),
        conversation_matches: words.found().to_vec(),
        shown_anchors: Rc::default(),
        scope: spaces
            .iter()
            .find(|space| space.active)
            .map(|space| space.key),
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
    }
}

/// What the sidebar looks for: words in a row's own labels, and saved sessions whose
/// conversations mention them, by identity.
struct Search<'a> {
    query: &'a str,
    conversations: &'a [(String, String)],
}

impl Search<'_> {
    /// The conversation matches that apply: none once the search is cleared.
    const fn found(&self) -> &[(String, String)] {
        if self.query.is_empty() {
            &[]
        } else {
            self.conversations
        }
    }

    fn in_conversation(&self, row: &SidebarRow) -> bool {
        row.task.as_ref().is_some_and(|task| {
            self.conversations
                .iter()
                .any(|(identity, _)| *identity == task.identity)
        })
    }
}

fn task_view_counts(
    snapshot: &SidebarSnapshot,
    search: &Search<'_>,
    project_filter: Option<&str>,
) -> [(TaskView, usize); 4] {
    let view_rows = filtered_rows(
        &snapshot.rows,
        search,
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
    search: &Search<'_>,
    project_filter: Option<&str>,
    task_view: TaskView,
) -> SidebarSnapshot {
    SidebarSnapshot {
        rows: filtered_rows(
            &snapshot.rows,
            search,
            project_filter,
            Some(task_view),
            snapshot.now_utc,
        ),
        ..snapshot.clone()
    }
}

fn filtered_rows(
    rows: &[SidebarRow],
    search: &Search<'_>,
    project_filter: Option<&str>,
    task_view: Option<TaskView>,
    now: i64,
) -> Vec<SidebarRow> {
    let query = search.query;
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
            .chain(row.task.iter().map(|task| task.name.as_str()))
            .any(|text| text.to_lowercase().contains(query))
            || (!query.is_empty() && search.in_conversation(row));
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
/// Sections a person chooses. Status (working, waiting for an answer) shows on the row, so a
/// row and its numbered shortcut stay put while its agent works.
enum SessionSection {
    Pinned,
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
    settled_shelf: SettledShelf,
    drag_order: Option<(String, Option<String>)>,
    /// The saved identity whose title is being edited in place, and its field.
    rename: Option<(String, Entity<InputState>)>,
    /// The Space whose sessions the sidebar lists.
    scope: Option<SpaceKey>,
    /// Saved session rows chosen together, by key.
    selection: Vec<String>,
    /// Saved session rows in the order this render shows them, for choosing a range.
    visible_sessions: Rc<std::cell::RefCell<Vec<String>>>,
    /// While searching, saved sessions whose conversations mention the search, with the line.
    conversation_matches: Vec<(String, String)>,
    /// Session reorder anchors in the order this render shows them.
    shown_anchors: Rc<std::cell::RefCell<Vec<String>>>,
    width: f32,
    task_view: TaskView,
    views: [(TaskView, usize); 4],
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
        let resume_owner = self.owner.clone();
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
                            .context_menu(crate::gpui::text_menu(&self.search))
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
            // Grouped sessions start from their project header instead.
            .when(!self.snapshot.group_by_project, |toolbar| {
                toolbar.child(
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
            })
            .child(
                Button::new("sidebar-resume-session")
                    .ghost()
                    .small()
                    .size_7()
                    .child(crate::gpui::sized_icon(
                        "history",
                        crate::gpui::IconSize::Small,
                        color(self.colors.subtext),
                    ))
                    .text_color(color(self.colors.subtext))
                    .accessibility_label("History")
                    .tooltip("Browse conversation history")
                    .on_click(move |_, _, cx| {
                        emit_sidebar_command(&resume_owner, "agent_history", cx);
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
                        chrome.filter_sidebar_project(None, cx);
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
                        chrome.filter_sidebar_project(Some(path.clone()), cx);
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
        self.sort_settled_shelf(&mut sessions);
        let mut result = Vec::new();
        let mut previous_group = None;
        let mut previous_section = None;
        for (row, group) in sessions {
            let section = self.section(&row);
            if previous_section != Some(section) {
                previous_group = None;
            }
            if self.snapshot.group_by_project
                && section != SessionSection::Settled
                && let Some(group) = group
                && previous_group.as_ref() != Some(&group.key)
            {
                previous_group = Some(group.key.clone());
                let mut group = group;
                group.key = match section {
                    SessionSection::Pinned => format!("pinned-{}", group.key),
                    SessionSection::Settled => format!("settled-{}", group.key),
                    SessionSection::Active => group.key,
                };
                result.push((group, section));
            }
            previous_section = Some(section);
            if section == SessionSection::Settled
                || !self.project_collapsed(row.project_path.as_deref())
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

    /// The settled shelf has no project headers; most recently active first keeps its ages in
    /// order. `sessions` are already in section order, so the shelf is their tail.
    fn sort_settled_shelf(&self, sessions: &mut [(SidebarRow, Option<SidebarRow>)]) {
        let first_settled = sessions
            .iter()
            .position(|(row, _)| self.section(row) == SessionSection::Settled)
            .unwrap_or(sessions.len());
        if let Some(settled) = sessions.get_mut(first_settled..) {
            settled.sort_by_key(|(row, _)| {
                std::cmp::Reverse(
                    row.task
                        .as_ref()
                        .and_then(|task| task.state.last_activity_at),
                )
            });
        }
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
        } else if row_view(row, self.snapshot.now_utc) == TaskView::Settled
            // An approval, question or failure brings a settled session back.
            && !row.needs_attention
            && !row
                .task
                .as_ref()
                .is_some_and(|task| task.state.is_overdue(self.snapshot.now_utc))
        {
            SessionSection::Settled
        } else {
            SessionSection::Active
        }
    }

    fn render_session_blocks(&self) -> Vec<gpui_kit::AnyElement> {
        let mut blocks = Vec::new();
        let mut previous_section = None;
        let rows = self.presentation_rows();
        let settled_count = rows
            .iter()
            .filter(|(_, section)| *section == SessionSection::Settled)
            .count();
        let collapsed = self.snapshot.settled_collapsed;
        let mut settled_shown = 0_usize;
        for (row, section) in rows {
            if previous_section != Some(section) {
                let (selector, label) = match section {
                    SessionSection::Pinned => ("sidebar-pinned-section", "Pinned"),
                    SessionSection::Active => ("sidebar-sessions-section", "Sessions"),
                    SessionSection::Settled => ("sidebar-settled-section", "Settled"),
                };
                if section == SessionSection::Settled {
                    blocks.push(self.settled_header(settled_count));
                } else {
                    blocks.push(
                        div()
                            .debug_selector(move || selector.to_owned())
                            .when(section == SessionSection::Settled, Styled::mt_auto)
                            .px_3()
                            .py_1()
                            .text_xs()
                            .text_color(color(readable_color(
                                self.snapshot.tint,
                                self.colors.muted,
                            )))
                            .child(label)
                            .into_any_element(),
                    );
                }
            }
            previous_section = Some(section);
            if section == SessionSection::Settled {
                if collapsed || settled_shown >= self.settled_shelf.shown {
                    continue;
                }
                settled_shown = settled_shown.saturating_add(1);
            }
            blocks.push(if row.kind.is_session() {
                self.session_block(&row)
            } else {
                self.render_row(&row, false)
            });
        }
        if !collapsed && settled_count > settled_shown {
            blocks.push(self.settled_more(settled_count.saturating_sub(settled_shown)));
        }
        blocks
    }

    /// Shows the next page of the settled shelf.
    fn settled_more(&self, hidden: usize) -> gpui_kit::AnyElement {
        let owner = self.owner.clone();
        let next = hidden.min(SETTLED_PAGE);
        div()
            .flex()
            .px_1()
            .child(
                Button::new("sidebar-settled-more")
                    .debug_selector(move || format!("sidebar-settled-more:{next}"))
                    .ghost()
                    .small()
                    .text_color(color(self.colors.muted))
                    .label(format!("Show {next} more"))
                    .on_click(move |_, _, cx| {
                        _ = owner.update(cx, |this, cx| {
                            this.sidebar_settled_shelf.shown = this
                                .sidebar_settled_shelf
                                .shown
                                .saturating_add(SETTLED_PAGE);
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }

    fn settled_header(&self, count: usize) -> gpui_kit::AnyElement {
        let owner = self.owner.clone();
        // Button content centers in its own width, so a row keeps the heading at the start.
        div()
            .flex()
            .mt_auto()
            .child(
                Button::new("sidebar-settled-toggle")
                    .debug_selector(|| "sidebar-settled-section".into())
                    .ghost()
                    .small()
                    .text_color(color(self.colors.muted))
                    .child(crate::gpui::icon(
                        if self.snapshot.settled_collapsed {
                            "chevron-right"
                        } else {
                            "chevron-down"
                        },
                        self.icon_size,
                        color(self.colors.muted),
                    ))
                    .label(format!("Settled ({count})"))
                    .accessibility_label(if self.snapshot.settled_collapsed {
                        "Expand settled sessions"
                    } else {
                        "Collapse settled sessions"
                    })
                    .on_click(move |_, _, cx| {
                        emit_sidebar_command(&owner, "ui.sidebar.toggle_settled", cx);
                    }),
            )
            .into_any_element()
    }

    /// How long ago a saved session was last active, such as "4d".
    fn activity_age(&self, row: &SidebarRow) -> Option<String> {
        row.task
            .as_ref()
            .and_then(|task| task.state.last_activity_at)
            .map(|last| {
                let seconds = self.snapshot.now_utc.saturating_sub(last).max(0);
                crate::clock::format_age(std::time::Duration::from_secs(
                    u64::try_from(seconds).unwrap_or_default(),
                ))
            })
    }

    /// The settled shelf has no project headers, so each row names its branch, or its project
    /// without one, to tell same-titled sessions apart.
    fn settled_label(&self, row: &SidebarRow) -> gpui_kit::AnyElement {
        let age = self.activity_age(row);
        let context = row
            .branch
            .clone()
            .filter(|branch| !branch.is_empty() && *branch != row.text)
            .or_else(|| row.project.as_ref().map(|project| project.name.clone()));
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .text_color(color(readable_color(
                self.background(row),
                self.colors.muted,
            )))
            .child(div().size_4().flex_none().map(|slot| {
                if let Some(artwork) = row
                    .project
                    .as_ref()
                    .and_then(|project| project.artwork.as_ref())
                {
                    slot.child(img(artwork.clone()).size_full())
                } else {
                    slot.child(crate::gpui::icon(
                        "folder",
                        self.icon_size,
                        color(self.colors.muted),
                    ))
                }
            }))
            .child(self.title_or_rename(
                row,
                div().flex_1().min_w_0().truncate().child(row.text.clone()),
            ))
            .children(context.map(|context| {
                let key = row.key.clone();
                div()
                    .debug_selector(move || format!("sidebar-settled-context-{key}"))
                    .flex_shrink(1.0)
                    .min_w_0()
                    .max_w(gpui_kit::relative(0.4))
                    .truncate()
                    .text_xs()
                    .child(context)
            }))
            .children(self.settle_button(row).or_else(|| {
                age.map(|age| div().flex_none().text_xs().child(age).into_any_element())
            }))
            .into_any_element()
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
            .hover(|style| style.bg(color(self.row_background(row, true))))
            .tooltip({
                let row = row.clone();
                let age = self.activity_age(&row);
                move |window, cx| session_tooltip(&row, age.clone()).build(window, cx)
            })
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

    /// The order to make manual before a drop: the shown one, unless it already is manual.
    fn shown_order(&self) -> Option<Rc<std::cell::RefCell<Vec<String>>>> {
        (self.snapshot.sort_order != bootty_config::config::SidebarSortOrder::Manual)
            .then(|| self.shown_anchors.clone())
    }

    fn reorder_end(&self) -> gpui_kit::AnyElement {
        let shown = self.shown_order();
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
                        let shown = shown.as_ref().map(|shown| shown.borrow().clone());
                        dragged.reorder(None, shown.as_deref(), cx);
                    });
                }
            })
            .into_any_element()
    }
}

/// The sidebar's outer frame: its own width, or the dock's when docked.
fn shell(width: f32, docked: bool) -> Stateful<Div> {
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
}

/// The line between the sidebar and the workspace.
fn edge(position: SidebarPosition, border: Rgba) -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .w(px(1.0))
        .when(position == SidebarPosition::Left, gpui_kit::Styled::right_0)
        .when(position == SidebarPosition::Right, gpui_kit::Styled::left_0)
        .bg(color(border))
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
        self.row_background(
            row,
            row.target
                .as_ref()
                .is_some_and(|target| self.pointer_hovered_session.as_ref() == Some(target)),
        )
    }

    fn row_background(&self, row: &SidebarRow, hovered: bool) -> Rgba {
        let current = row.current || self.selection.contains(&row.key);
        let base = if current && hovered {
            mix(self.snapshot.current, self.snapshot.hover, 0.5)
        } else if current {
            self.snapshot.current
        } else if hovered {
            self.snapshot.hover
        } else {
            self.snapshot.tint
        };
        if row.kind.is_session() {
            mix(base, row.color, if current { 0.18 } else { 0.055 })
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
                        self.title_or_rename(
                            row,
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
                                        if (self.section(row) == SessionSection::Settled
                                            || row.kind == SidebarRowKind::DetachedSession)
                                            && !row.current
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
                        ),
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

    /// The line that matched the search in this session's conversation.
    fn conversation_line(&self, row: &SidebarRow) -> Option<String> {
        let identity = &row.task.as_ref()?.identity;
        self.conversation_matches
            .iter()
            .find(|(matched, _)| matched == identity)
            .map(|(_, line)| line.clone())
    }

    fn rename_input(&self, row: &SidebarRow) -> Option<&Entity<InputState>> {
        let (identity, input) = self.rename.as_ref()?;
        (row.task.as_ref()?.identity == *identity).then_some(input)
    }

    /// The row's title, or the field editing it in place.
    fn title_or_rename(&self, row: &SidebarRow, title: Div) -> gpui_kit::AnyElement {
        let Some(input) = self.rename_input(row) else {
            return title.into_any_element();
        };
        let owner = self.owner.clone();
        div()
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-rename-{key}")
            })
            .flex_1()
            .min_w_0()
            // Enter saves the title rather than opening the row, and Escape keeps the old title
            // even while the field holds a selection.
            .on_action(|_: &gpui_kit::component::input::Enter, _, cx| cx.stop_propagation())
            .capture_action(move |_: &gpui_kit::component::input::Escape, _, cx| {
                _ = owner.update(cx, |chrome, cx| chrome.finish_sidebar_rename(false, cx));
                cx.stop_propagation();
            })
            .child(
                gpui_kit::component::input::Input::new(input)
                    .context_menu(crate::gpui::text_menu(input))
                    .xsmall()
                    .aria_label("Session title"),
            )
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
            .debug_selector({
                let key = row.key.clone();
                move || format!("sidebar-session-content-{key}")
            })
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
                    // While searching, a session found by its conversation shows the line.
                    .when_some(self.conversation_line(row), |line, text| {
                        line.child(crate::gpui::icon(
                            "message-square",
                            self.icon_size * 0.85,
                            color(self.colors.muted),
                        ))
                        .child(
                            div()
                                .debug_selector({
                                    let key = row.key.clone();
                                    move || format!("sidebar-match-{key}")
                                })
                                .min_w_0()
                                .truncate()
                                .child(text),
                        )
                    })
                    // A session titled with its branch doesn't repeat it here.
                    .when_some(
                        row.branch
                            .as_ref()
                            .filter(|branch| **branch != row.text)
                            .filter(|_| self.conversation_line(row).is_none()),
                        |branch, name| {
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
                        },
                    )
                    .when_some(row.pull_request.as_ref(), |line, pull_request| {
                        line.child(
                            div()
                                .debug_selector({
                                    let key = row.key.clone();
                                    move || format!("sidebar-pull-request-{key}")
                                })
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_0p5()
                                .text_color(color(pull_request.color))
                                .child(crate::gpui::icon(
                                    "git-pull-request",
                                    self.icon_size * 0.85,
                                    color(pull_request.color),
                                ))
                                .child(format!("#{}", pull_request.number)),
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
        let lifecycle =
            row.task.as_ref().and_then(|task| {
                if task.state.is_overdue(self.snapshot.now_utc) {
                    Some(("Woke", "alarm-clock", self.colors.accent))
                } else {
                    match task.state.view(self.snapshot.now_utc) {
                        TaskView::Active => (row.kind == SidebarRowKind::DetachedSession)
                            .then_some(("Closed", "power-off", self.colors.muted)),
                        TaskView::Settled => Some(("Settled", "check", self.colors.muted)),
                        TaskView::Archived => Some(("Archived", "archive", self.colors.muted)),
                        TaskView::Snoozed => Some(("Snoozed", "clock", self.colors.muted)),
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
            // An idle session says how long it has been idle.
            (self.activity_age(row)?, None, self.colors.muted)
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
        let settled = self.section(row) == SessionSection::Settled;
        if self.pointer_hovered_session.as_ref() != row.target.as_ref()
            || (task.state.view(self.snapshot.now_utc) != TaskView::Active
                && !task.state.is_overdue(self.snapshot.now_utc)
                && !settled)
            || row.trailing.as_deref().is_some_and(|status| {
                status != "Finished"
                    && !(status == "Woke" && task.state.is_overdue(self.snapshot.now_utc))
            })
        {
            return None;
        }
        let label = if settled { "Unsettle" } else { "Settle" };
        let invocation = saved_state_invocation(
            task,
            if settled {
                "session.activate"
            } else {
                "session.settle"
            },
            Vec::new(),
        )?;
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
                        gpui_kit::component::Icon::new(if settled {
                            gpui_kit::assets::IconName::Undo2
                        } else {
                            gpui_kit::assets::IconName::Check
                        })
                        .xsmall(),
                    )
                    .child(label),
            )
            .accessibility_label(format!("{label} {}", row.text))
            .tooltip(format!("{label} this session"))
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

    fn working_indicator(&self, tint: gpui_kit::Hsla) -> gpui_kit::AnyElement {
        let owner = self.owner.clone();
        let animate = self.snapshot.animate_working;
        canvas(
            |bounds, _, _| bounds,
            move |bounds, _, window, cx| {
                if !bounds.intersects(&window.content_mask().bounds) {
                    return;
                }
                let animated = animate && window.is_visible() && !cx.reduce_motion();
                let phase = if animated {
                    crate::motion::phase(std::time::Duration::from_secs(1))
                } else {
                    0.0
                };
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
                // Clipped, inactive and reduced-motion rings park.
                if animated && let Some(owner) = owner.upgrade() {
                    crate::motion::request_frame(owner.entity_id(), cx);
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

    fn drag_row(&self, element: Stateful<Div>, row: &SidebarRow) -> Stateful<Div> {
        if self.rename_input(row).is_some() {
            return element;
        }
        // A project header carries every session it groups, in order.
        let members: Vec<&SidebarRow> = if row.kind == SidebarRowKind::Group {
            self.snapshot
                .rows
                .iter()
                .filter(|member| {
                    member.kind.is_session()
                        && member.reorder_anchor.is_some()
                        && member.project_path == row.project_path
                })
                .collect()
        } else {
            vec![row]
        };
        let mut anchors = members
            .iter()
            .filter_map(|member| member.reorder_anchor.clone());
        let Some(source) = anchors.next() else {
            return element;
        };
        let drag = DraggedSidebarRow {
            source,
            following: anchors.collect(),
            sessions: members
                .iter()
                .filter_map(|member| member.target.clone())
                .collect(),
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
        let Some(source) = row.reorder_anchor.clone() else {
            return element;
        };
        let shown = self.shown_order();
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
                        let shown = shown.as_ref().map(|shown| shown.borrow().clone());
                        dragged.reorder(before.as_deref(), shown.as_deref(), cx);
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
            let group = project_header_group(&row.key);
            let new_session = self.project_new_session(row, &path);
            let actions = self.project_actions(row, &path, &target, group.clone());
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
                .group(group)
                .w_full()
                .min_w_0()
                .child(div().flex_1().min_w_0().child(disclosure))
                .children(new_session)
                .child(actions)
                .into_any_element()
        } else if row.selectable {
            self.row_activation(element, row, accessible_label, row_height)
        } else {
            element
        }
    }

    /// Opens the new-session form for this project's Space with the project's path filled in.
    fn project_new_session(&self, row: &SidebarRow, path: &str) -> Option<gpui_kit::AnyElement> {
        let scope = self.scope?;
        let path = path.to_owned();
        let owner = self.owner.clone();
        Some(
            Button::new(SharedString::from(format!(
                "project-new-session-{}",
                row.key
            )))
            .debug_selector({
                let key = row.key.clone();
                move || format!("project-new-session-{key}")
            })
            .ghost()
            .icon(gpui_kit::assets::IconName::SquarePen)
            .text_color(color(self.colors.muted))
            .tooltip("New session in project")
            .accessibility_label(format!("New session in {}", row.text))
            .on_click(move |_, _, cx| {
                _ = owner.update(cx, |_, cx| {
                    cx.emit(ChromeIntent::NewSessionInProject {
                        scope,
                        path: path.clone(),
                    });
                });
            })
            .into_any_element(),
        )
    }

    /// A project header's actions: start a session there, reveal or copy its path, and open its
    /// settings.
    fn project_actions(
        &self,
        row: &SidebarRow,
        path: &str,
        target: &bootty_control::CommandTarget,
        group: SharedString,
    ) -> ProjectActions {
        let mut rows = self.scope.map_or_else(Vec::new, |scope| {
            place_menu(
                scope,
                &super::view::SessionPlace {
                    project: Some(path.to_owned()),
                    project_name: Some(row.text.clone()),
                    cwd: Some(path.to_owned()),
                    branch: None,
                    local: row.host.is_none(),
                    scripts: None,
                },
            )
        });
        for row in &mut rows {
            if matches!(row.intent, ChromeIntent::CopyText(_)) {
                row.label = format!("Copy {}", row.label);
            }
        }
        rows.push(project_settings_row(path, target));
        ProjectActions {
            key: row.key.clone(),
            name: row.text.clone(),
            group,
            tint: self.colors.muted,
            rows,
            owner: self.owner.clone(),
        }
    }

    fn row_menu(&self, row: &SidebarRow) -> Option<ContextMenu> {
        if let SidebarRowKind::Draft { identity, binding } = &row.kind {
            return Some(ContextMenu::Draft {
                identity: identity.clone(),
                binding: binding.clone(),
                place: super::view::SessionPlace {
                    project: row.project_path.clone(),
                    project_name: row.project.as_ref().map(|project| project.name.clone()),
                    cwd: row.cwd.clone(),
                    branch: row.branch.clone(),
                    ..Default::default()
                },
            });
        }
        if row.task.as_ref().is_some_and(|task| task.pending) {
            return None;
        }
        let now = self.snapshot.now_utc;
        if self.selection.len() > 1 && self.selection.contains(&row.key) {
            let tasks = self
                .snapshot
                .rows
                .iter()
                .filter(|chosen| self.selection.contains(&chosen.key))
                .filter_map(|chosen| {
                    let mut task = chosen.task.clone()?;
                    task.title.clone_from(&chosen.text);
                    Some((task, chosen.kind == SidebarRowKind::Session))
                })
                .collect();
            return Some(ContextMenu::Sessions { tasks, now });
        }
        // Confirmations name the session as its row shows it.
        let task = row.task.clone().map(|mut task| {
            task.title.clone_from(&row.text);
            task
        });
        let target = row.target.clone()?;
        let options = row.context?;
        let place = super::view::SessionPlace {
            project: row.project_path.clone(),
            project_name: row.project.as_ref().map(|project| project.name.clone()),
            cwd: row.cwd.clone(),
            branch: row.branch.clone(),
            local: row.host.is_none(),
            scripts: Some((target.clone(), row.scripts.clone())),
        };
        Some(if matches!(row.kind, SidebarRowKind::DetachedSession) {
            ContextMenu::DetachedSession {
                target,
                task,
                now,
                place,
            }
        } else {
            ContextMenu::Session {
                target,
                options,
                task,
                now,
                place,
                working: row.working,
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
        let hitbox_selector = format!("sidebar-row-hitbox-{}", row.key);
        let hitbox_debug_selector = format!("sidebar-row-hitbox-{}", row.key);
        let hitbox = div()
            .id(SharedString::from(hitbox_selector))
            .debug_selector(move || hitbox_debug_selector)
            .w_full()
            .h(gpui_kit::rems(row_height));
        // The title field takes the row's clicks and keys while it edits.
        if self.rename_input(row).is_some() {
            return hitbox.child(element).into_any_element();
        }
        let owner = self.owner.clone();
        let activation_target = row.target.clone();
        let activation_kind = row.kind.clone();
        let native_conversation = row
            .task
            .as_ref()
            .and_then(|task| task.native_conversation.clone());
        let button = Button::new(SharedString::from(format!(
            "sidebar-row-button-{}",
            row.key
        )))
        .text()
        .p_0()
        .w_full()
        .h(gpui_kit::rems(row_height))
        .tab_index(0_isize)
        .accessibility_label(if native_conversation.is_some() {
            format!("Open conversation for {accessible_label}")
        } else {
            accessible_label
        })
        .child(element);
        let renames = row.task.is_some();
        let (key, visible) = (row.key.clone(), self.visible_sessions.clone());
        let activated = div().w_full().h_full().child(button.on_click(
            move |event: &gpui_kit::ClickEvent, window, app| {
                let modifiers = event.modifiers();
                // Command-click (Control-click elsewhere) chooses saved work for one action
                // together; Shift-click chooses a range.
                if renames && (modifiers.secondary() || modifiers.shift) {
                    _ = owner.update(app, |chrome, cx| {
                        chrome.choose_sidebar_row(
                            &key,
                            modifiers.shift,
                            &visible.borrow(),
                            window,
                            cx,
                        );
                    });
                    return;
                }
                _ = owner.update(app, GpuiChrome::clear_sidebar_selection);
                // A second click on saved work edits its title.
                if renames
                    && event.click_count() >= 2
                    && let Some(target) = &activation_target
                    && owner
                        .update(app, |chrome, cx| {
                            chrome.begin_sidebar_rename(target, window, cx)
                        })
                        .unwrap_or(false)
                {
                    return;
                }
                _ = owner.update(app, |_, cx| {
                    if let SidebarRowKind::Draft { identity, binding } = &activation_kind {
                        let mut invocation = bootty_control::CommandInvocation::new(
                            "session.draft.open",
                            vec![identity.clone()],
                            bootty_control::Caller::Internal,
                        );
                        invocation.target = Some(binding.clone());
                        cx.emit(ChromeIntent::Command(invocation));
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
            },
        ));
        hitbox.child(activated).into_any_element()
    }

    fn render_row(&self, row: &SidebarRow, in_session_block: bool) -> gpui_kit::AnyElement {
        if row.kind.is_session() && row.task.is_some() {
            self.visible_sessions.borrow_mut().push(row.key.clone());
        }
        if let Some(anchor) = row.reorder_anchor.clone().filter(|_| row.kind.is_session()) {
            self.shown_anchors.borrow_mut().push(anchor);
        }
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
            SidebarRowKind::Session
            | SidebarRowKind::DetachedSession
            | SidebarRowKind::Draft { .. } => {
                if self.section(row) == SessionSection::Settled {
                    ROW_HEIGHT
                } else if self.snapshot.group_by_project {
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
            .when(row.kind.is_session(), gpui_kit::Styled::p_2)
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
            .child(
                if self.section(row) == SessionSection::Settled && row.kind.is_session() {
                    self.settled_label(row)
                } else if row.kind.is_session() {
                    self.session_label(row)
                } else {
                    self.label(row)
                },
            )
            .children(shortcut)
            .children(self.bounds_probe(row));
        let element = self.drag_row(element, row);
        let element = if !in_session_block && matches!(row.kind, SidebarRowKind::Group) {
            self.drop_target(element, row)
        } else {
            element
        };
        self.row_control(element, row, row_height)
    }
}

fn session_tooltip(row: &SidebarRow, age: Option<String>) -> Tooltip {
    let row = row.clone();
    Tooltip::element(move |_, cx| {
        let line = |icon: &str, text: String| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child(crate::gpui::icon(icon, 14.0, cx.theme().muted_foreground))
                .child(text)
        };
        v_flex()
            .max_w(gpui_kit::rems(20.0))
            .text_xs()
            .gap_2()
            .p_2()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(row.text.clone()),
            )
            .when_some(row.project.as_ref(), |content, project| {
                content.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().size_4().map(|slot| {
                            if let Some(artwork) = &project.artwork {
                                slot.child(img(artwork.clone()).size_full())
                            } else {
                                slot.child(crate::gpui::icon(
                                    "folder",
                                    14.0,
                                    cx.theme().muted_foreground,
                                ))
                            }
                        }))
                        .child(project.name.clone()),
                )
            })
            .children(
                row.branch
                    .clone()
                    .filter(|branch| !branch.is_empty())
                    .map(|branch| line("git-branch", branch)),
            )
            .children(row.pull_request.as_ref().map(|pull_request| {
                line(
                    "git-pull-request",
                    format!("#{} · {}", pull_request.number, pull_request.status),
                )
            }))
            .child(line(
                "laptop",
                row.host.clone().unwrap_or_else(|| "This computer".into()),
            ))
            .children(
                age.clone()
                    .map(|age| line("clock", format!("Active {age} ago"))),
            )
            // The name to write the session by, in commands, prompts and the CLI.
            .children(
                row.task
                    .as_ref()
                    .filter(|task| !task.name.is_empty())
                    .map(|task| line("at-sign", task.name.clone())),
            )
            .children(row.agents.iter().map(|agent| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(crate::gpui::icon(
                        &agent.icon,
                        14.0,
                        cx.theme().muted_foreground,
                    ))
                    .child(
                        agent
                            .model
                            .clone()
                            .unwrap_or_else(|| agent.description.clone()),
                    )
            }))
    })
}

/// A quota in two rows: what it is, how much is left and when it resets; then the meter with the
/// pace against an even spend.
fn usage_meter(
    snapshot: &UsageMeterSnapshot,
    item: &super::SidebarFooterItem,
    colors: ChromePalette,
) -> gpui_kit::AnyElement {
    let selector = |part: &str| {
        let name = format!("sidebar-footer-{}-{part}", item.key);
        move || name.clone()
    };
    // The reset moves under the name when both don't fit.
    let words = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_1()
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
                .flex_1()
                .min_w(gpui_kit::rems(5.))
                .text_ellipsis()
                .text_color(color(item.color))
                .child(snapshot.label.clone()),
        )
        .when(!snapshot.meter.reset.is_empty(), |element| {
            element.child(
                div()
                    .debug_selector(selector("reset"))
                    .flex_none()
                    .text_color(color(colors.muted))
                    .child(format!("resets in {}", snapshot.meter.reset)),
            )
        });
    div()
        .id(SharedString::from(format!("usage-meter-{}", item.key)))
        .tooltip({
            let description = snapshot.description.clone();
            move |window, cx| Tooltip::new(description.clone()).build(window, cx)
        })
        .w_full()
        .min_w_0()
        .py_0p5()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(words)
        .child(usage_track(snapshot, &item.key))
        .into_any_element()
}

/// The meter: what is left, a mark where an even spend would be, and the pace against it.
fn usage_track(snapshot: &UsageMeterSnapshot, key: &str) -> gpui_kit::Div {
    let fill_width = (snapshot.meter.remaining_percent.clamp(0.0, 100.0) / 100.0)
        .to_f32()
        .unwrap_or(0.0);
    let marker = snapshot
        .meter
        .expected_remaining_percent
        .and_then(|value| (value.clamp(0.0, 100.0) / 100.0).to_f32());
    let selector = |part: &str| {
        let name = format!("sidebar-footer-{key}-{part}");
        move || name.clone()
    };
    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .id(SharedString::from(format!("usage-track-{key}")))
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
        .when(!snapshot.meter.pace.is_empty(), |element| {
            element.child(
                div()
                    .debug_selector(selector("pace"))
                    .flex_none()
                    .text_color(color(snapshot.pace))
                    .child(snapshot.meter.pace.clone()),
            )
        })
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
        confirm: None,
        label: label.to_owned(),
        enabled: true,
        destructive: false,
        starts_group: false,
        intent,
    })
    .collect()
}

/// What can be done with where a session works: start another session there, show its
/// directory in the file manager or open it in an installed editor when it's on this computer,
/// and copy its path or branch.
pub(super) fn place_menu(scope: SpaceKey, place: &super::view::SessionPlace) -> Vec<MenuRow> {
    let row = |label: &str, starts_group: bool, intent| MenuRow {
        confirm: None,
        label: label.to_owned(),
        enabled: true,
        destructive: false,
        starts_group,
        intent,
    };
    let mut rows = Vec::new();
    if let (Some(path), Some(branch)) = (&place.cwd, &place.branch) {
        rows.push(row(
            &format!("New session on {branch}"),
            false,
            ChromeIntent::NewSessionOnBranch {
                scope,
                path: path.clone(),
            },
        ));
    }
    if let Some(path) = &place.project
        && place.branch.is_none()
    {
        rows.push(row(
            "New Session in Project",
            true,
            ChromeIntent::NewSessionInProject {
                scope,
                path: path.clone(),
            },
        ));
    }
    if let Some(path) = place.cwd.as_ref()
        && place.local
    {
        rows.push(row(
            crate::commands::reveal_label(),
            false,
            ChromeIntent::RevealPath {
                scope,
                path: path.clone(),
            },
        ));
    }
    rows.extend(place_copy_menu(place));
    if let (Some(project), Some((session, scripts))) = (&place.project, &place.scripts) {
        rows.extend(scripts.iter().enumerate().map(|(index, name)| {
            row(
                &format!("Run {name}"),
                index == 0,
                ChromeIntent::RunProjectScript {
                    session: session.clone(),
                    project: project.clone(),
                    name: name.clone(),
                },
            )
        }));
    }

    rows
}

pub(super) fn place_copy_menu(place: &super::view::SessionPlace) -> Vec<MenuRow> {
    [
        ("Path", place.cwd.as_ref()),
        ("Branch", place.branch.as_ref()),
    ]
    .into_iter()
    .filter_map(|(label, value)| {
        value.map(|value| MenuRow {
            label: label.into(),
            enabled: true,
            destructive: false,
            starts_group: false,
            confirm: None,
            intent: ChromeIntent::CopyText(value.clone()),
        })
    })
    .collect()
}

pub(super) fn project_settings_row(path: &str, binding: &bootty_control::CommandTarget) -> MenuRow {
    let mut settings = bootty_control::CommandInvocation::new(
        "project.edit",
        vec![path.to_owned()],
        bootty_control::Caller::Internal,
    );
    settings.target = Some(binding.clone());
    MenuRow {
        label: "Project settings…".into(),
        enabled: true,
        destructive: false,
        starts_group: false,
        confirm: None,
        intent: ChromeIntent::Command(settings),
    }
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
        confirm: None,
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

/// The hover group of one project header, from its stable row key.
fn project_header_group(key: &str) -> SharedString {
    SharedString::from(format!("sidebar-project-header-{key}"))
}

/// A project header's quiet actions menu. Its trigger shows while the header is hovered, while it
/// holds keyboard focus, and while its menu is open.
#[derive(IntoElement)]
struct ProjectActions {
    key: String,
    name: String,
    group: SharedString,
    tint: Rgba,
    rows: Vec<MenuRow>,
    owner: WeakEntity<GpuiChrome>,
}

impl RenderOnce for ProjectActions {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let open = window.use_keyed_state(
            SharedString::from(format!("project-actions-open-{}", self.key)),
            cx,
            |_, _| false,
        );
        let shown = *open.read(cx);
        let (rows, owner) = (self.rows, self.owner);
        let key = self.key.clone();
        Button::new(SharedString::from(format!("project-actions-{}", self.key)))
            .debug_selector(move || format!("project-actions-{key}"))
            .ghost()
            .icon(gpui_kit::component::IconName::Ellipsis)
            .text_color(color(self.tint))
            .opacity(if shown { 1. } else { 0. })
            .group_hover(self.group, |style| style.opacity(1.))
            .focus_visible(|style| style.opacity(1.))
            .tooltip("Project actions")
            .accessibility_label(format!("Project actions for {}", self.name))
            .dropdown_menu(move |menu, _, _| {
                rows.iter().cloned().fold(menu, |menu, row| {
                    super::view::append_menu_row(menu, row, &owner)
                })
            })
            .on_open_change(move |is_open, _, cx| {
                open.update(cx, |open, cx| {
                    if *open != *is_open {
                        *open = *is_open;
                        cx.notify();
                    }
                });
            })
    }
}

pub(super) fn project_name(path: &str) -> &str {
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
    if let Some(branch) = row.branch.as_ref().filter(|branch| **branch != row.text) {
        _ = write!(label, " · {branch}");
    }
    if let Some(pull_request) = &row.pull_request {
        _ = write!(
            label,
            " · Pull request #{}, {}",
            pull_request.number, pull_request.status
        );
    }
    for agent in &row.agents {
        _ = write!(label, " · {}", agent.description);
    }
    if let Some(task) = &row.task {
        let view = task.state.view(now);
        if view != TaskView::Active {
            _ = write!(label, " · {}", task_view_label(view));
        } else if row.kind == SidebarRowKind::DetachedSession {
            label.push_str(" · Closed");
        }
        if task.state.is_overdue(now) {
            label.push_str(" · Woke");
        }
    }
    label
}

const TASK_VIEWS: [TaskView; 4] = [
    TaskView::Active,
    TaskView::Settled,
    TaskView::Archived,
    TaskView::Snoozed,
];

const fn task_view_label(view: TaskView) -> &'static str {
    match view {
        TaskView::Active => "Active",
        TaskView::Settled => "Settled",
        TaskView::Archived => "Archived",
        TaskView::Snoozed => "Snoozed",
        TaskView::Deleted => "Deleted",
    }
}

fn row_view(row: &SidebarRow, now: i64) -> TaskView {
    row.task
        .as_ref()
        .map_or(TaskView::Active, |task| task.state.view(now))
}

pub(super) fn saved_state_invocation(
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

/// Archiving working sessions and deleting saved work ask first.
pub(super) fn saved_session_menu(task: &SidebarTask, now: i64, working: bool) -> Vec<MenuRow> {
    use bootty_mux::session_membership::SessionLifecycle;
    let row = |label: &str, command, starts_group| {
        saved_state_invocation(task, command, Vec::new()).map(|invocation| MenuRow {
            confirm: None,
            label: label.to_owned(),
            enabled: !task.pending,
            destructive: command == "session.delete",
            starts_group,
            intent: ChromeIntent::Command(invocation),
        })
    };
    let mut rows = task
        .native_conversation
        .as_ref()
        .map_or_else(Vec::new, |target| {
            conversation_menu(target, task.pending, task.title_regenerating)
        });
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
        let settled =
            task.state.lifecycle == SessionLifecycle::Settled && !task.state.is_overdue(now);
        rows.extend(row(
            if settled { "Unsettle" } else { "Settle" },
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
    }
    rows.extend(
        row(
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
        )
        .map(|mut archive| {
            if working && !task.state.archived {
                archive.confirm = Some(archive_confirmation(&task.title));
            }
            archive
        }),
    );
    rows.extend(row("Delete…", "session.delete", false).map(|mut delete| {
        delete.confirm = Some(delete_confirmation(&task.title));
        delete
    }));
    rows
}

/// One action on several saved sessions: pin or unpin, settle, mark unread, archive, or delete
/// them after closing their terminals.
pub(super) fn sessions_menu(tasks: &[(SidebarTask, bool)], now: i64) -> Vec<MenuRow> {
    use bootty_mux::session_membership::SessionLifecycle;
    let count = tasks.len();
    let all = |command: &str, chosen: &dyn Fn(&SidebarTask, bool) -> bool| {
        tasks
            .iter()
            .filter(|(task, live)| chosen(task, *live))
            .filter_map(|(task, _)| saved_state_invocation(task, command, Vec::new()))
            .collect::<Vec<_>>()
    };
    let row = |label: String, invocations: Vec<_>, starts_group: bool| MenuRow {
        confirm: None,
        label,
        enabled: !invocations.is_empty() && tasks.iter().all(|(task, _)| !task.pending),
        destructive: false,
        starts_group,
        intent: ChromeIntent::Commands(invocations),
    };
    let pinned = tasks.iter().all(|(task, _)| task.state.pinned);
    let mut rows = vec![
        if pinned {
            row(
                format!("Unpin {count} sessions"),
                all("session.unpin", &|_, _| true),
                false,
            )
        } else {
            row(
                format!("Pin {count} sessions"),
                all("session.pin", &|task, _| {
                    !task.state.pinned && !task.state.archived
                }),
                false,
            )
        },
        row(
            format!("Settle {count} sessions"),
            all("session.settle", &|task, _| {
                task.state.lifecycle != SessionLifecycle::Settled || task.state.is_overdue(now)
            }),
            false,
        ),
    ];
    let unread = conversation_invocations(tasks, "agents.native.mark-unread");
    if !unread.is_empty() {
        rows.push(row(format!("Mark {} unread", unread.len()), unread, false));
    }
    let regenerate = conversation_invocations(tasks, "agents.native.regenerate-title");
    if !regenerate.is_empty() {
        let busy = tasks.iter().any(|(task, _)| task.title_regenerating);
        let mut regenerate = row(
            if busy {
                "Regenerating…".into()
            } else {
                format!("Regenerate {} titles", regenerate.len())
            },
            regenerate,
            false,
        );
        regenerate.enabled &= !busy;
        rows.push(regenerate);
    }
    rows.push(row(
        format!("Archive {count} sessions"),
        all("session.archive", &|task, _| !task.state.archived),
        true,
    ));
    let deletable = all("session.delete", &|_, _| true);
    let mut delete = row(format!("Delete {count} sessions…"), deletable, false);
    delete.destructive = true;
    delete.confirm = Some(Confirmation {
        question: format!("Delete {count} sessions permanently?"),
        detail: "This stops their work, closes their terminals and removes their saved history from Bootty.",
        action: "Delete permanently",
    });
    rows.push(delete);
    rows
}

fn conversation_invocations(
    tasks: &[(SidebarTask, bool)],
    command: &str,
) -> Vec<bootty_control::CommandInvocation> {
    tasks
        .iter()
        .filter_map(|(task, _)| task.native_conversation.as_ref())
        .map(|target| {
            let mut invocation = bootty_control::CommandInvocation::new(
                command,
                vec![target.handle.clone(), target.generation.to_string()],
                bootty_control::Caller::Internal,
            );
            invocation.target = Some(target.clone());
            invocation
        })
        .collect()
}

/// What a session's conversation offers: a new title, marking it unread, and a side chat.
fn conversation_menu(
    target: &bootty_control::CommandTarget,
    pending: bool,
    title_regenerating: bool,
) -> Vec<MenuRow> {
    let mut rows = Vec::new();
    for (label, command) in [
        ("Regenerate title", "agents.native.regenerate-title"),
        ("Mark unread", "agents.native.mark-unread"),
    ] {
        let mut invocation = bootty_control::CommandInvocation::new(
            command,
            vec![target.handle.clone(), target.generation.to_string()],
            bootty_control::Caller::Internal,
        );
        invocation.target = Some(target.clone());
        rows.push(MenuRow {
            confirm: None,
            label: if command == "agents.native.regenerate-title" && title_regenerating {
                "Regenerating…".into()
            } else {
                label.to_owned()
            },
            enabled: !(pending
                || command == "agents.native.regenerate-title" && title_regenerating),
            destructive: false,
            starts_group: false,
            intent: ChromeIntent::Command(invocation),
        });
    }
    rows.push(MenuRow {
        confirm: None,
        label: "Start side chat".to_owned(),
        enabled: !pending,
        destructive: false,
        starts_group: false,
        intent: ChromeIntent::ForkConversation(target.clone()),
    });
    rows
}

fn archive_confirmation(title: &str) -> Confirmation {
    Confirmation {
        question: if title.is_empty() {
            "Archive this session while it works?".to_owned()
        } else {
            format!("Archive “{title}” while it works?")
        },
        detail: "It keeps working, out of the active list.",
        action: "Archive",
    }
}

/// Deleting can't be undone, so the question names the session.
fn delete_confirmation(title: &str) -> Confirmation {
    Confirmation {
        question: if title.is_empty() {
            "Delete this session permanently?".to_owned()
        } else {
            format!("Delete “{title}” permanently?")
        },
        detail: "This stops its work, closes its terminals and removes its saved history from Bootty.",
        action: "Delete permanently",
    }
}

/// When `tasks` can sleep until: presets in the local time zone, then a chosen time. One task's
/// rows carry one command; several tasks' rows carry one per task that can snooze.
pub(super) fn snooze_menu(tasks: &[SidebarTask], now: i64) -> Vec<MenuRow> {
    use chrono::{Datelike as _, TimeZone as _};
    let snoozable: Vec<&SidebarTask> = tasks
        .iter()
        .filter(|task| {
            !task.state.archived
                && !task.state.deleted
                && task.state.snoozed_until.is_none_or(|until| until <= now)
        })
        .collect();
    if snoozable.is_empty() {
        return Vec::new();
    }
    let enabled = snoozable.iter().all(|task| !task.pending);
    let intent =
        |invocations: Vec<bootty_control::CommandInvocation>| match <[_; 1]>::try_from(invocations)
        {
            Ok([one]) => ChromeIntent::Command(one),
            Err(several) => ChromeIntent::Commands(several),
        };
    let local =
        chrono::DateTime::from_timestamp(now, 0).map(|date| date.with_timezone(&chrono::Local));
    let morning = |days: u64| {
        local
            .as_ref()
            .and_then(|date| date.date_naive().checked_add_days(chrono::Days::new(days)))
            .and_then(|date| date.and_hms_opt(9, 0, 0))
            .and_then(|date| chrono::Local.from_local_datetime(&date).earliest())
            .map(|date| date.timestamp())
    };
    let monday = local.as_ref().map_or(7, |date| {
        7u64.saturating_sub(u64::from(date.weekday().num_days_from_monday()))
    });
    let mut rows = [
        ("In 1 hour", now.checked_add(3_600)),
        ("In 3 hours", now.checked_add(10_800)),
        ("Tomorrow", morning(1)),
        ("Next week", morning(monday)),
    ]
    .into_iter()
    .filter_map(|(label, until)| {
        let until = until?;
        let date = chrono::DateTime::from_timestamp(until, 0)?.with_timezone(&chrono::Local);
        let invocations = snoozable
            .iter()
            .map(|task| saved_state_invocation(task, "session.snooze", vec![until.to_string()]))
            .collect::<Option<Vec<_>>>()?;
        Some(MenuRow {
            confirm: None,
            label: format!(
                "{label} ({})",
                crate::clock::local_moment(date, crate::clock::Moment::DayAndTime)
            ),
            enabled,
            destructive: false,
            starts_group: false,
            intent: intent(invocations),
        })
    })
    .collect::<Vec<_>>();
    if let Some(invocations) = snoozable
        .iter()
        .map(|task| saved_state_invocation(task, "session.snooze", Vec::new()))
        .collect::<Option<Vec<_>>>()
    {
        rows.push(MenuRow {
            confirm: None,
            label: "Custom…".into(),
            enabled,
            destructive: false,
            starts_group: true,
            intent: ChromeIntent::CustomSnooze(invocations),
        });
    }
    rows
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
        TaskView::Deleted => I::Trash,
    }
}

fn row_is_pinned(row: &SidebarRow, now: i64) -> bool {
    row.task
        .as_ref()
        .is_some_and(|task| task.state.pinned && task.state.view(now) == TaskView::Active)
}
