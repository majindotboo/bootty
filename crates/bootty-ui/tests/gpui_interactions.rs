#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use std::{
    cell::RefCell,
    ops::{Add as _, Div as _, Sub as _},
    rc::Rc,
};

use bootty_ui::gpui::{
    ChromeIntent, ChromeLayout, ChromePalette, ChromeSnapshot, DialogAction, DialogId,
    DialogIntent, DialogRole, DialogRow, DialogSpec, DialogView, FindDirection, GpuiChrome,
    GpuiPaneColors, GpuiPaneDividerSnapshot, GpuiPaneIntent, GpuiPaneSnapshot, GpuiPaneWorkspace,
    GpuiPaneWorkspaceSnapshot, NativeChromeAction, PaneRect, PaneSplitDirection, Rgba,
    SessionContextSnapshot, SessionTarget, SidebarAgent, SidebarFooterItem, SidebarPosition,
    SidebarProject, SidebarRow, SidebarRowKind, SidebarSnapshot, SidebarTask, SpaceKey,
    SpaceSnapshot, StatusAlignment, StatusBarSnapshot, StatusIntent, StatusItemSnapshot,
    StatusSegmentSnapshot, TabContextSnapshot, UiPalette, UsageMeterSnapshot, init_theme,
};
use bootty_ui::usage::{UsageProvider, UsageWindow};
use gpui_kit::component::{Root, WindowExt as _};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, Modifiers, MouseButton,
    MouseUpEvent, Render, ScrollDelta, ScrollWheelEvent, Styled, Subscription, TestAppContext,
    TouchPhase, VisualTestContext, Window, div, point, prelude::*, px,
};
use pretty_assertions::{assert_eq, assert_ne};

const fn color(red: u8, green: u8, blue: u8) -> Rgba {
    Rgba::rgb(red, green, blue)
}

fn palette() -> ChromePalette {
    ChromePalette {
        mantle: color(20, 20, 24),
        base: color(25, 25, 30),
        tab_bar: color(30, 30, 36),
        pane: color(35, 35, 42),
        surface: color(45, 45, 52),
        hover: color(60, 60, 70),
        border: color(80, 80, 90),
        border_variant: color(55, 55, 64),
        text: color(240, 240, 245),
        subtext: color(180, 180, 190),
        muted: color(130, 130, 140),
        accent: color(100, 160, 240),
        tab_accent: color(100, 160, 240),
    }
}

fn target() -> SessionTarget {
    SessionTarget {
        scope: SpaceKey(1),
        session_id: "session-1".to_owned(),
    }
}

fn chrome_snapshot() -> ChromeSnapshot {
    let session = target();
    ChromeSnapshot {
        palette: palette(),
        layout: chrome_layout(),
        titlebar: bootty_gpui::TitlebarSnapshot {
            title: "Bootty".to_owned(),
            icon: None,
            session_count: 1,
            reserve_window_controls: false,
        },
        sidebar: Some(sidebar_snapshot(session)),
        spaces: vec![
            SpaceSnapshot {
                key: SpaceKey(1),
                name: "Main".to_owned(),
                icon: "terminal".to_owned(),
                color: color(100, 200, 140),
                active: true,
                error: None,
                accepts_moves: true,
                can_close: true,
            },
            SpaceSnapshot {
                key: SpaceKey(2),
                name: "Work".to_owned(),
                icon: "briefcase".to_owned(),
                color: color(120, 160, 240),
                active: false,
                error: None,
                accepts_moves: true,
                can_close: true,
            },
        ],
        space_transition: None,
        top_status: Some(top_status_snapshot()),
        bottom_status: None,
        window_focused: true,
    }
}

fn chrome_layout() -> ChromeLayout {
    ChromeLayout {
        left_dock_toggle: true,
        right_dock_toggle: true,
        panel_tab_style: bootty_config::config::PanelTabStyle::default(),
        panel_tabs: bootty_config::config::PanelTabs::default(),
        tabs: bootty_config::config::ChromeConfig::default().tabs,
        width: 900.0,
        height: 600.0,
        sidebar_position: SidebarPosition::Left,
        sidebar_width: 240.0,
        gap: 1.0,
        top_inset: 0.0,
        notch_span: None,
        wrap_tabs_at_notch: true,
        titlebar_height: 32.0,
        status_height: 28.0,
        sidebar_visible: true,
        titlebar_visible: true,
        fullscreen: false,
    }
}

fn sidebar_snapshot(target: SessionTarget) -> SidebarSnapshot {
    SidebarSnapshot {
        projects: Vec::new(),
        project_target: None,
        animate_working: true,
        now_utc: 1_000,
        rows: vec![
            SidebarRow {
                key: "session".to_owned(),
                text: "Session one".to_owned(),
                secondary: None,
                project: None,
                project_path: None,
                branch: None,
                agents: Vec::new(),
                trailing: None,
                trailing_icon: None,
                trailing_color: None,
                working: false,
                needs_attention: false,
                number: Some(1),
                indent: 0,
                tree: None,
                artwork: None,
                icon: None,
                diff: None,
                color: color(220, 220, 230),
                dim_color: color(150, 150, 160),
                kind: SidebarRowKind::Session,
                active: false,
                current: true,
                selectable: true,
                target: Some(target.clone()),
                reorder_anchor: Some("session-1".to_owned()),
                task: None,
                context: Some(SessionContextSnapshot {
                    can_rename: true,
                    can_activate: true,
                    can_move_up: true,
                    can_move_down: true,
                    can_navigate: true,
                    can_return_to_last: true,
                }),
            },
            SidebarRow {
                key: "session:cwd".to_owned(),
                text: "/Users/luan/src/bootty".to_owned(),
                secondary: None,
                project: None,
                project_path: None,
                branch: None,
                agents: Vec::new(),
                trailing: None,
                trailing_icon: None,
                trailing_color: None,
                working: false,
                needs_attention: false,
                number: None,
                indent: 2,
                tree: None,
                artwork: None,
                icon: Some("folder".to_owned()),
                diff: None,
                color: color(180, 180, 190),
                dim_color: color(130, 130, 140),
                kind: SidebarRowKind::Detail,
                active: false,
                current: true,
                selectable: true,
                target: Some(target),
                reorder_anchor: None,
                context: None,
                task: None,
            },
        ],
        footer: Vec::new(),
        title_visible: true,
        group_by_project: true,
        sort_order: bootty_config::config::SidebarSortOrder::Manual,
        focused: true,
        hovered_session: None,
        dim_when_unfocused: 0.0,
        tint: color(25, 25, 30),
        foreground: color(235, 235, 240),
        hover: color(60, 60, 70),
        current: color(45, 45, 52),
        border: color(80, 80, 90),
    }
}

fn top_status_snapshot() -> StatusBarSnapshot {
    StatusBarSnapshot {
        key: "top".to_owned(),
        rows: 1,
        background: color(25, 25, 30),
        segments: vec![
            StatusSegmentSnapshot {
                align: StatusAlignment::Left,
                source_slot: 0,
                surface: "clock".to_owned(),
                items: vec![StatusItemSnapshot {
                    key: "clock".to_owned(),
                    text: "12:00".to_owned(),
                    icon: None,
                    gauge: None,
                    pad_left: 0.0,
                    pad_right: 0.0,
                    progress: None,
                    foreground: None,
                    background: None,
                    active: false,
                    action: Some(NativeChromeAction::ToggleKeepAwake),
                    reorder_anchor: None,
                    tab_context: None,
                }],
            },
            StatusSegmentSnapshot {
                align: StatusAlignment::Left,
                source_slot: 1,
                surface: "windows".to_owned(),
                items: vec![StatusItemSnapshot {
                    key: "tab-one".to_owned(),
                    text: "One".to_owned(),
                    icon: None,
                    gauge: None,
                    pad_left: 0.0,
                    pad_right: 0.0,
                    progress: None,
                    foreground: None,
                    background: None,
                    active: true,
                    action: None,
                    reorder_anchor: Some("tab-one".to_owned()),
                    tab_context: Some(TabContextSnapshot {
                        pane_actions: Vec::new(),
                        session_id: "session-1".to_owned(),
                        window_id: "window-1".to_owned(),
                        can_activate: true,
                        can_move_left: true,
                        can_move_right: true,
                        can_navigate: true,
                        can_close_pane: true,
                    }),
                }],
            },
        ],
    }
}

struct ChromeProbe {
    chrome: Entity<GpuiChrome>,
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    _subscription: Subscription,
}

impl ChromeProbe {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(chrome_snapshot(), window, cx)
    }

    fn with_snapshot(
        snapshot: ChromeSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let chrome = cx.new(|cx| GpuiChrome::new(snapshot, window, cx));
        let intents = Rc::new(RefCell::new(Vec::new()));
        let received = Rc::clone(&intents);
        let subscription = cx.subscribe(&chrome, move |_, _, intent: &ChromeIntent, _| {
            received.borrow_mut().push(intent.clone());
        });
        Self {
            chrome,
            intents,
            _subscription: subscription,
        }
    }
}

impl Render for ChromeProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.chrome.clone()
    }
}

fn center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    bounds.center()
}

#[gpui_kit::test]
fn sidebar_tab_space_and_status_clicks_emit_their_typed_actions(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let session = cx
        .debug_bounds("sidebar-row-hitbox-session")
        .expect("session row hitbox");
    let shell = cx
        .debug_bounds("bootty-gpui-sidebar-shell")
        .expect("sidebar shell");
    assert!(session.left() > shell.left());
    assert!(session.right() < shell.right());
    // Exercise the edge too: valid layout bounds alone do not catch an inset scroll clip.
    cx.simulate_click(
        point(session.left().add(px(1.0)), session.center().y),
        Modifiers::none(),
    );
    let space = cx.debug_bounds("space-2").expect("target space");
    cx.simulate_click(center(space), Modifiers::none());
    let create = cx
        .debug_bounds("space-create")
        .expect("create space button");
    cx.simulate_click(center(create), Modifiers::none());
    let status = cx
        .debug_bounds("status-item-0-clock")
        .expect("status action");
    cx.simulate_click(center(status), Modifiers::none());

    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [
                ChromeIntent::ActivateSession(target()),
                ChromeIntent::ActivateSpace(SpaceKey(2)),
                ChromeIntent::CreateSpace,
                ChromeIntent::Status(StatusIntent::Action(NativeChromeAction::ToggleKeepAwake)),
            ]
        );
    });
}

fn fill_sidebar_sessions(sidebar: &mut SidebarSnapshot) {
    let template = sidebar.rows.first().expect("session row").clone();
    let detail_template = sidebar.rows.get(1).expect("session detail row").clone();
    sidebar.rows = (0..40)
        .flat_map(|index| {
            let mut row = template.clone();
            row.key = format!("session-{index}");
            row.target = Some(SessionTarget {
                scope: SpaceKey(1),
                session_id: row.key.clone(),
            });
            row.text.clone_from(&row.key);
            row.current = index == 0;
            row.reorder_anchor = Some(row.key.clone());
            let target = row.target.clone();
            let mut cwd = detail_template.clone();
            cwd.key = format!("session-{index}:cwd");
            cwd.text = format!("/workspaces/session-{index}");
            cwd.target.clone_from(&target);
            cwd.current = row.current;
            cwd.reorder_anchor.clone_from(&row.reorder_anchor);
            let mut branch = detail_template.clone();
            branch.key = format!("session-{index}:branch");
            branch.text = format!("main-{index}");
            branch.target = target;
            branch.current = row.current;
            branch.reorder_anchor.clone_from(&row.reorder_anchor);
            [row, cwd, branch]
        })
        .collect();
}

#[gpui_kit::test]
fn dropping_on_task_title_reorders_the_whole_session(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    let sidebar = snapshot.sidebar.as_mut().expect("sidebar");
    fill_sidebar_sessions(sidebar);
    sidebar.rows.truncate(6);
    let (probe, cx) =
        cx.add_window_view(move |window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let start = center(
        cx.debug_bounds("sidebar-row-hitbox-session-0")
            .expect("source session"),
    );
    let end = center(
        cx.debug_bounds("sidebar-row-hitbox-session-1")
            .expect("destination task"),
    );
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(start.x.add(px(15.0)), start.y.add(px(15.0))),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_event(MouseUpEvent {
        position: end,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [ChromeIntent::ReorderSession {
                source: "session-0".to_owned(),
                before: Some("session-1".to_owned()),
            }]
        );
    });
}

#[gpui_kit::test]
fn switching_sessions_reveals_the_row_without_capturing_manual_scroll(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    fill_sidebar_sessions(snapshot.sidebar.as_mut().expect("sidebar"));
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    for (selected, selectors) in [
        (39, ["sidebar-row-session-39"]),
        (0, ["sidebar-row-session-0"]),
        (25, ["sidebar-row-session-25"]),
    ] {
        let selected_key = format!("session-{selected}");
        for row in &mut snapshot.sidebar.as_mut().expect("sidebar").rows {
            row.current =
                row.key == selected_key || row.key.starts_with(&format!("{selected_key}:"));
        }
        cx.update(|window, cx| {
            probe
                .read(cx)
                .chrome
                .clone()
                .update(cx, |chrome, cx| chrome.update(&snapshot, window, cx));
        });
        cx.run_until_parked();
        let shell = cx
            .debug_bounds("bootty-gpui-sidebar-shell")
            .expect("sidebar shell");
        let footer = cx
            .debug_bounds("bootty-gpui-space-switcher")
            .expect("space switcher");
        for selector in selectors {
            let row = cx
                .debug_bounds(selector)
                .expect("selected session entry row");
            assert!(
                row.top() >= shell.top(),
                "selected session entry row must be inside the sidebar: {selector}"
            );
            assert!(
                row.bottom() <= footer.top(),
                "selected session entry row must be above the footer: {selector}, {row:?}, {footer:?}"
            );
        }
    }
    let selected_before = cx
        .debug_bounds("sidebar-row-session-25")
        .expect("selected row");
    cx.simulate_event(ScrollWheelEvent {
        position: selected_before.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(200.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();
    let scrolled = cx
        .debug_bounds("sidebar-row-session-25")
        .expect("selected row after manual scroll");
    assert!(
        scrolled.top() > selected_before.top(),
        "manual scrolling must remain available"
    );
    snapshot.titlebar.title = "Updated title".to_owned();
    cx.update(|window, cx| {
        probe
            .read(cx)
            .chrome
            .clone()
            .update(cx, |chrome, cx| chrome.update(&snapshot, window, cx));
    });
    cx.run_until_parked();
    assert_eq!(cx.debug_bounds("sidebar-row-session-25"), Some(scrolled));
}

#[gpui_kit::test]
fn compact_task_rows_keep_inset_targets_and_centered_space_controls(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (_, cx) = cx.add_window_view(ChromeProbe::new);

    let row = cx.debug_bounds("sidebar-row-session").expect("session row");
    let hitbox = cx
        .debug_bounds("sidebar-row-hitbox-session")
        .expect("session row hitbox");
    let block = cx
        .debug_bounds("sidebar-session-session")
        .expect("session block");
    let shell = cx
        .debug_bounds("bootty-gpui-sidebar-shell")
        .expect("sidebar shell");
    assert!(row.left() > shell.left());
    assert!(row.right() < shell.right());
    assert_eq!(hitbox.origin.x, row.origin.x);
    assert_eq!(hitbox.size.width, row.size.width);
    assert_eq!(block.origin.x, row.origin.x);
    assert_eq!(block.size.width, row.size.width);

    assert_eq!(cx.debug_bounds("sidebar-row-session:cwd"), None);
    assert_eq!(block.size.height, row.size.height);

    let first_space = cx.debug_bounds("space-1").expect("first space");
    let second_space = cx.debug_bounds("space-2").expect("second space");
    let create_space = cx.debug_bounds("space-create").expect("create space");
    let switcher = cx
        .debug_bounds("bootty-gpui-space-switcher")
        .expect("space switcher");
    assert!(
        (first_space
            .left()
            .add(create_space.right())
            .div(2.0)
            .sub(switcher.center().x))
        .abs()
            <= px(0.5),
        "space controls center within half a layout pixel"
    );
    assert!(first_space.right() < second_space.left());
    assert!(second_space.right() < create_space.left());
}

const TASK_ROW_SELECTORS: [(&str, &str, &str, &str, &str); 6] = [
    (
        "sidebar-row-task-0",
        "sidebar-title-task-0",
        "sidebar-status-task-0",
        "sidebar-metadata-task-0",
        "sidebar-status-icon-task-0",
    ),
    (
        "sidebar-row-task-1",
        "sidebar-title-task-1",
        "sidebar-status-task-1",
        "sidebar-metadata-task-1",
        "sidebar-status-icon-task-1",
    ),
    (
        "sidebar-row-task-2",
        "sidebar-title-task-2",
        "sidebar-status-task-2",
        "sidebar-metadata-task-2",
        "sidebar-status-icon-task-2",
    ),
    (
        "sidebar-row-task-3",
        "sidebar-title-task-3",
        "sidebar-status-task-3",
        "sidebar-metadata-task-3",
        "sidebar-status-icon-task-3",
    ),
    (
        "sidebar-row-task-4",
        "sidebar-title-task-4",
        "sidebar-status-task-4",
        "sidebar-metadata-task-4",
        "sidebar-status-icon-task-4",
    ),
    (
        "sidebar-row-task-5",
        "sidebar-title-task-5",
        "sidebar-status-task-5",
        "sidebar-metadata-task-5",
        "sidebar-status-icon-task-5",
    ),
];

fn task_row_snapshot(grouped: bool) -> ChromeSnapshot {
    let mut snapshot = chrome_snapshot();
    let sidebar = snapshot.sidebar.as_mut().expect("sidebar");
    sidebar.group_by_project = grouped;
    let mut template = sidebar.rows.first().expect("task title").clone();
    template.project = (!grouped).then(|| SidebarProject {
        name: "Bootty".to_owned(),
        artwork: None,
    });
    template.project_path = Some("/projects/bootty".to_owned());
    template.branch = Some("session-specific-long-worktree-branch".to_owned());
    sidebar.rows = [
        ("Working 7s", "circle-dashed"),
        ("Input", "message-circle-question-mark"),
        ("Waiting", "clock"),
        ("Failed", "circle-alert"),
        ("Approval", "shield-question-mark"),
        ("Finished", "circle-check"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (status, icon))| {
        assert!(bootty_gpui::has_icon(icon), "status glyph must render");
        let mut row = template.clone();
        row.key = format!("task-{index}");
        row.text = "A long task title describing the actual work without terminal process paths"
            .to_owned();
        row.trailing = Some(status.to_owned());
        row.trailing_icon = Some(icon.to_owned());
        row.agents = ["openai", "claude", "pi"]
            .into_iter()
            .enumerate()
            .map(|(agent, icon)| SidebarAgent {
                key: format!("{}-{agent}", row.key),
                icon: icon.to_owned(),
                description: format!("{icon} · {status}"),
            })
            .collect();
        row.current = index == 0;
        row.target = Some(SessionTarget {
            scope: SpaceKey(1),
            session_id: row.key.clone(),
        });
        row
    })
    .collect();
    let mut shell = template;
    shell.key = "plain-shell".to_owned();
    shell.current = false;
    shell.agents.clear();
    shell.trailing = None;
    shell.trailing_icon = None;
    sidebar.rows.push(shell);
    snapshot
}

#[gpui_kit::test]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Compare measured GPUI pixel geometry within a row's padding"
)]
fn task_rows_keep_unselected_status_and_equal_heights_at_ui_scale(cx: &mut TestAppContext) {
    for (font_size, grouped) in [12.0, 16.0, 20.0]
        .into_iter()
        .flat_map(|font| [(font, true), (font, false)])
    {
        cx.update(|cx| {
            init_theme(UiPalette::default(), cx);
            bootty_gpui::update_ui_font_size(font_size, cx);
        });
        let mut snapshot = task_row_snapshot(grouped);
        let (probe, cx) = cx.add_window_view(|window, cx| {
            window.set_rem_size(px(font_size));
            ChromeProbe::with_snapshot(snapshot.clone(), window, cx)
        });
        let selected = cx
            .debug_bounds("sidebar-row-task-0")
            .expect("selected task");
        let shell = cx
            .debug_bounds("sidebar-row-plain-shell")
            .expect("pure terminal");
        assert_eq!(shell.size.height, selected.size.height);
        for (row_id, title_id, status_id, metadata_id, icon_id) in TASK_ROW_SELECTORS {
            let row = cx.debug_bounds(row_id).expect("task");
            let title = cx.debug_bounds(title_id).expect("task title");
            let status = cx.debug_bounds(status_id).expect("unselected status");
            let metadata = cx
                .debug_bounds(metadata_id)
                .expect("reserved metadata strip");
            let icon = cx.debug_bounds(icon_id).expect("status glyph");
            assert_eq!(row.size, selected.size);
            if grouped {
                assert!((status.center().y - title.center().y).abs() <= px(0.5));
                assert!(title.right() < status.left());
                assert!(title.top() - row.top() < px(font_size));
                assert!(row.size.height <= px(font_size * 3.0));
                assert!(cx.debug_bounds("sidebar-project-task-0").is_none());
            } else {
                assert!(status.bottom() <= title.top());
            }
            assert!(status.right() <= row.right());
            assert!(
                row.right() - status.right() <= px(font_size),
                "status must align to the row’s right padding: {status:?} in {row:?}"
            );
            assert!(status.size.width >= px(font_size));
            if grouped {
                // Title and status share a line; both remain readable at the largest UI scale.
                assert!(title.size.width >= px(font_size * 4.0));
            } else {
                assert!(title.size.width > row.size.width.div(2.0));
            }
            assert!(metadata.top() >= title.bottom());
            assert!(metadata.bottom() <= row.bottom());
            assert!(icon.left() >= status.left());
            assert!(icon.right() <= status.right());
        }
        assert_task_agent_stack(cx);
        if !grouped {
            let project = cx
                .debug_bounds("sidebar-project-task-0")
                .expect("flat project context");
            assert!(project.bottom() <= cx.debug_bounds("sidebar-title-task-0").unwrap().top());
        }
        let sidebar = snapshot.sidebar.as_mut().expect("sidebar");
        for row in &mut sidebar.rows {
            row.current = row.key == "task-1";
            if row.key == "task-0" {
                row.agents.clear();
                row.trailing = None;
                row.trailing_icon = None;
            }
        }
        cx.update(|window, cx| {
            probe
                .read(cx)
                .chrome
                .clone()
                .update(cx, |chrome, cx| chrome.update(&snapshot, window, cx));
        });
        cx.run_until_parked();
        assert_eq!(
            cx.debug_bounds("sidebar-row-task-0").unwrap().size.height,
            selected.size.height
        );
        assert_eq!(
            cx.debug_bounds("sidebar-row-plain-shell")
                .unwrap()
                .size
                .height,
            shell.size.height
        );
        assert_eq!(
            cx.debug_bounds("sidebar-row-task-1").unwrap().size.height,
            selected.size.height
        );
        assert_eq!(cx.debug_bounds("sidebar-agent-task-0-0"), None);
        assert_eq!(cx.debug_bounds("sidebar-status-task-0"), None);
        assert_eq!(
            cx.debug_bounds("sidebar-title-task-0").unwrap().left(),
            cx.debug_bounds("sidebar-title-task-1").unwrap().left()
        );
    }
}

fn assert_task_agent_stack(cx: &mut VisualTestContext) {
    let branch = cx
        .debug_bounds("sidebar-branch-task-0")
        .expect("session branch");
    let stack = cx
        .debug_bounds("sidebar-agents-task-0")
        .expect("agent stack");
    assert!(branch.right() < stack.left());
    let first = cx
        .debug_bounds("sidebar-agent-task-0-0")
        .expect("first agent");
    let second = cx
        .debug_bounds("sidebar-agent-task-0-1")
        .expect("second agent");
    let third = cx
        .debug_bounds("sidebar-agent-task-0-2")
        .expect("third agent");
    assert!(first.left() < second.left() && second.left() < third.left());
    assert!(
        second.left() < first.right() && third.left() < second.right(),
        "agent marks overlap"
    );
}

#[gpui_kit::test]
fn sidebar_search_filters_tasks_without_changing_selection_and_survives_grouping(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = task_row_snapshot(false);
    snapshot.sidebar.as_mut().unwrap().rows[1].text = "Fix rendering".to_owned();
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    let input = cx.debug_bounds("sidebar-search").expect("session search");
    let create = cx.debug_bounds("sidebar-new-session").expect("create icon");
    let menu = cx.debug_bounds("sidebar-view").expect("grouping menu icon");
    let add = cx
        .debug_bounds("sidebar-add-project")
        .expect("add project icon");
    assert!(
        input.right() <= menu.left() && menu.right() <= add.left() && add.right() <= create.left()
    );
    cx.simulate_click(center(input), Modifiers::none());
    cx.simulate_input("RENDER");
    cx.run_until_parked();
    assert!(cx.debug_bounds("sidebar-row-task-1").is_some());
    assert!(cx.debug_bounds("sidebar-row-task-0").is_none());
    snapshot.sidebar.as_mut().unwrap().group_by_project = true;
    cx.update(|window, cx| {
        probe
            .read(cx)
            .chrome
            .clone()
            .update(cx, |chrome, cx| chrome.update(&snapshot, window, cx));
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("sidebar-row-task-1").is_some());
    assert!(cx.debug_bounds("sidebar-row-task-0").is_none());
    probe.update(cx, |probe, _| {
        assert!(
            probe.intents.borrow().is_empty(),
            "search never activates a session"
        );
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a backspace"
    } else {
        "ctrl-a backspace"
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("sidebar-row-task-0").is_some());
    cx.simulate_click(center(create), Modifiers::none());
    probe.update(cx, |probe, _| assert!(matches!(probe.intents.borrow().as_slice(), [ChromeIntent::Command(invocation)] if invocation.command == "new_mux_session")));
}

#[gpui_kit::test]
fn sidebar_grouping_menu_uses_the_shared_command_and_restores_focus(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let button = cx.debug_bounds("sidebar-view").expect("session options");
    cx.simulate_click(center(button), Modifiers::none());
    cx.run_until_parked();
    draw_chrome(cx);
    cx.simulate_keystrokes("down right");
    cx.run_until_parked();
    draw_chrome(cx);
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    probe.update(cx, |probe, _| assert!(matches!(probe.intents.borrow().as_slice(), [ChromeIntent::Command(invocation)] if invocation.command == "ui.sidebar.toggle_grouping")));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    draw_chrome(cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    probe.update(cx, |probe, _| assert_eq!(probe.intents.borrow().len(), 1));
}

#[gpui_kit::test]
fn fullscreen_top_status_keeps_controls_clear_of_notch(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    snapshot.layout.notch_span = Some((380.0, 600.0));
    let mut center_item = snapshot.top_status.as_ref().unwrap().segments[0].items[0].clone();
    center_item.key = "center".to_owned();
    center_item.text = "center".to_owned();
    snapshot
        .top_status
        .as_mut()
        .unwrap()
        .segments
        .push(StatusSegmentSnapshot {
            align: StatusAlignment::Center,
            source_slot: 2,
            surface: "custom".to_owned(),
            items: vec![center_item],
        });
    let (_, view) =
        cx.add_window_view(move |window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let tabs = view.debug_bounds("status-tabs-top-1").expect("tab region");
    let center = view
        .debug_bounds("status-item-2-center")
        .expect("center status");
    assert!(tabs.right() <= px(380.0), "tabs enter notch: {tabs:?}");
    assert!(
        center.left() >= px(600.0),
        "status enters notch: {center:?}"
    );
}

#[gpui_kit::test]
fn new_tab_button_follows_the_last_tab_and_opens_the_shared_chooser(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(chrome_snapshot(), window, cx));
    draw_chrome(cx);
    let button = cx
        .debug_bounds("status-new-tab-top-1")
        .expect("new tab button");
    let last = cx
        .debug_bounds("status-tab-top-1-tab-one")
        .expect("last tab");
    assert!(
        button.left() >= last.right(),
        "new tab button must follow the last tab: {button:?} {last:?}"
    );
    cx.simulate_click(button.center(), Modifiers::none());
    cx.run_until_parked();
    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        let [ChromeIntent::Command(invocation)] = intents.as_slice() else {
            panic!("new tab must emit exactly one shared command: {intents:?}");
        };
        assert_eq!(invocation.command, "new_tab");
        assert_eq!(invocation.arguments, Vec::<String>::new());
    });
}

#[gpui_kit::test]
fn quota_rows_keep_quota_meter_pacing_and_reset_inline(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    snapshot.sidebar.as_mut().expect("sidebar").footer = ["5h", "7d"]
        .into_iter()
        .map(|label| SidebarFooterItem {
            key: format!("codex:{label}"),
            text: format!("codex {label}"),
            icon: Some("openai".to_owned()),
            color: palette().text,
            meter: Some(UsageMeterSnapshot {
                provider: UsageProvider::Codex,
                label: format!("{label} 23%"),
                reset_at: Some("Oct 6 14:00".to_owned()),
                description: "Codex remaining quota and estimated pace".to_owned(),
                fill: palette().accent,
                marker: palette().accent,
                pace: palette().text,
                track: palette().border,
                meter: UsageWindow {
                    label,
                    used_percent: 77.0,
                    duration_secs: 604_800.0,
                    resets_at: Some(352_000),
                }
                .meter(0),
            }),
        })
        .collect();
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    for font_size in [12.0, 16.0, 20.0] {
        cx.update(|window, cx| {
            gpui_kit::component::Theme::global_mut(cx).font_size = px(font_size);
            window.set_rem_size(px(font_size));
            window.refresh();
        });
        for width in [160.0, 240.0, 320.0, 480.0] {
            snapshot.layout.sidebar_width = width;
            cx.update(|window, cx| {
                probe.read(cx).chrome.clone().update(cx, |chrome, cx| {
                    chrome.update(&snapshot, window, cx);
                });
            });
            cx.run_until_parked();
            let mut previous_bottom = px(0.0);
            for (row, labels, track, pace, reset) in [
                (
                    "sidebar-footer-codex:5h",
                    "sidebar-footer-codex:5h-labels",
                    "sidebar-footer-codex:5h-track",
                    "sidebar-footer-codex:5h-pace",
                    "sidebar-footer-codex:5h-reset",
                ),
                (
                    "sidebar-footer-codex:7d",
                    "sidebar-footer-codex:7d-labels",
                    "sidebar-footer-codex:7d-track",
                    "sidebar-footer-codex:7d-pace",
                    "sidebar-footer-codex:7d-reset",
                ),
            ] {
                let row = cx.debug_bounds(row).expect("quota row");
                let labels = cx.debug_bounds(labels).expect("quota labels");
                let track = cx.debug_bounds(track).expect("quota track");
                let pace = cx.debug_bounds(pace).expect("pacing delta");
                let reset = cx.debug_bounds(reset).expect("reset countdown");
                assert!(labels.right() <= track.left());
                assert!(track.right() <= pace.left());
                assert!(pace.right() <= reset.left());
                assert!(reset.right() <= row.right());
                assert!(
                    track.size.width > px(0.0),
                    "meter stays visible at every scale"
                );
                assert_eq!(track.size.height, px(font_size * 0.375));
                for part in [labels, track, pace, reset] {
                    assert!(part.top() >= row.top() && part.bottom() <= row.bottom());
                    assert!(
                        (f32::from(part.center().y) - f32::from(row.center().y)).abs() <= 0.5,
                        "quota information shares one horizontal center line"
                    );
                }
                assert!(row.size.height <= px(font_size * 1.25));
                assert!(row.top() >= previous_bottom);
                previous_bottom = row.bottom();
            }
        }
    }
}

#[gpui_kit::test]
fn crowded_sidebar_keeps_all_footer_items_and_spaces_reachable(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    snapshot.layout.sidebar_width = 120.0;
    snapshot.sidebar.as_mut().expect("sidebar").footer = (0..6)
        .map(|index| SidebarFooterItem {
            key: format!("footer-{index}"),
            text: format!("Footer {index}"),
            icon: None,
            meter: None,
            color: color(220, 220, 230),
        })
        .collect();
    snapshot.spaces = (1..=8)
        .map(|key| SpaceSnapshot {
            key: SpaceKey(key),
            name: format!("Space {key}"),
            icon: "terminal".to_owned(),
            color: color(100, 200, 140),
            active: key == 1,
            error: None,
            accepts_moves: true,
            can_close: true,
        })
        .collect();
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));

    for selector in [
        "sidebar-footer-footer-0",
        "sidebar-footer-footer-1",
        "sidebar-footer-footer-2",
        "sidebar-footer-footer-3",
        "sidebar-footer-footer-4",
        "sidebar-footer-footer-5",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} remains reachable"
        );
    }
    for selector in [
        "space-1", "space-2", "space-3", "space-4", "space-5", "space-6", "space-7", "space-8",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} remains reachable"
        );
    }
    assert!(cx.debug_bounds("space-create").is_some());

    let footer = cx
        .debug_bounds("bootty-gpui-sidebar-footer")
        .expect("footer surface");
    let last_footer = cx
        .debug_bounds("sidebar-footer-footer-5")
        .expect("last footer remains visible");
    assert!(last_footer.left() >= footer.left());
    assert!(last_footer.right() <= footer.right());
    assert!(last_footer.bottom() <= footer.bottom());

    let switcher = cx
        .debug_bounds("bootty-gpui-space-switcher")
        .expect("space switcher");
    cx.simulate_event(ScrollWheelEvent {
        position: switcher.center(),
        delta: ScrollDelta::Pixels(point(px(-1000.0), px(0.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();
    let last_space = cx
        .debug_bounds("space-8")
        .expect("last space remains scrollable");
    let create_space = cx
        .debug_bounds("space-create")
        .expect("create space remains scrollable");
    assert!(last_space.left() >= switcher.left());
    assert!(create_space.right() <= switcher.right());
    cx.simulate_click(center(last_space), Modifiers::none());
    cx.simulate_click(center(create_space), Modifiers::none());

    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [
                ChromeIntent::ActivateSpace(SpaceKey(8)),
                ChromeIntent::CreateSpace,
            ]
        );
    });
}

#[gpui_kit::test]
fn top_status_inset_stays_clear_of_status_items_and_tabs(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    snapshot.layout.top_inset = 24.0;
    let (_, cx) = cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));

    let status = cx.debug_bounds("status-top").expect("top status row");
    let item = cx.debug_bounds("status-item-0-clock").expect("status item");
    let tab = cx
        .debug_bounds("status-tab-top-1-tab-one")
        .expect("status tab");
    let inset_bottom = status.origin.y.add(px(24.0));

    assert!(item.origin.y >= inset_bottom);
    assert!(tab.origin.y >= inset_bottom);
    // Metrics reach the content boundary; the compact pill tab keeps its bottom margin.
    assert_eq!(item.bottom(), status.bottom());
    assert!(tab.bottom().add(px(2.0)) <= status.bottom());
}

#[gpui_kit::test]
fn focusable_chrome_actions_activate_from_enter(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let chrome_focus = cx.update(|_, app| {
        let chrome = probe.read(app).chrome.clone();
        chrome.read(app).focus_handle(app)
    });
    cx.update(|window, app| chrome_focus.focus(window, app));
    cx.update(|window, app| {
        _ = window.draw(app);
    });

    for _ in 0..12 {
        cx.update(gpui_kit::Window::focus_next);
        cx.simulate_keystrokes("enter");
        cx.simulate_event(gpui_kit::KeyUpEvent {
            keystroke: gpui_kit::Keystroke::parse("enter").expect("Enter key"),
        });
    }

    probe.update(cx, |probe, _| {
        assert!(
            probe.intents.borrow().iter().any(|intent| matches!(
                intent,
                ChromeIntent::Status(StatusIntent::Action(NativeChromeAction::ToggleKeepAwake))
            )),
            "keyboard traversal should activate the status action: {:?}",
            probe.intents.borrow()
        );
    });
}

#[gpui_kit::test]
fn context_menu_escape_and_action_restore_focus_and_emit(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let chrome_focus = cx.update(|_, app| {
        let chrome = probe.read(app).chrome.clone();
        chrome.read(app).focus_handle(app)
    });
    cx.update(|window, app| chrome_focus.focus(window, app));
    let row = center(cx.debug_bounds("sidebar-row-session").expect("session row"));
    cx.simulate_mouse_down(row, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|window, _| chrome_focus.is_focused(window)));

    cx.simulate_mouse_down(row, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_click(point(px(880.0), px(580.0)), Modifiers::none());

    cx.simulate_mouse_down(row, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_keystrokes("down enter");
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [ChromeIntent::SessionContext {
                target: target(),
                action: bootty_gpui::SessionContextAction::Rename,
            }]
        );
    });
}

#[gpui_kit::test]
fn disabled_session_context_action_is_a_keyboard_noop(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    snapshot.sidebar.as_mut().expect("sidebar").rows[0]
        .context
        .as_mut()
        .expect("session context")
        .can_rename = false;
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let row = center(cx.debug_bounds("sidebar-row-session").expect("session row"));
    cx.simulate_mouse_down(row, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    // The first row is disabled; selecting it and confirming must not emit its intent.
    cx.simulate_keystrokes("down enter");
    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().is_empty());
    });
}

#[gpui_kit::test]
fn session_menu_keeps_only_rename_and_move_before_saved_lifecycle_actions(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let row = cx.debug_bounds("sidebar-row-session").unwrap();
    for (keys, action) in [
        ("down enter", bootty_gpui::SessionContextAction::Rename),
        (
            "down down enter",
            bootty_gpui::SessionContextAction::MoveToSpace,
        ),
        (
            "down down down enter",
            bootty_gpui::SessionContextAction::Rename,
        ),
    ] {
        cx.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::none());
        draw_chrome(cx);
        cx.simulate_keystrokes(keys);
        draw_chrome(cx);
        probe.update(cx, |probe, _| {
            assert_eq!(
                probe.intents.borrow().as_slice(),
                [ChromeIntent::SessionContext {
                    target: target(),
                    action,
                }]
            );
            probe.intents.borrow_mut().clear();
        });
    }
}

#[gpui_kit::test]
fn sidebar_resize_double_click_emits_reset(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let handle = cx
        .debug_bounds("bootty-sidebar-resize")
        .expect("sidebar resize handle");
    let position = point(handle.origin.x.add(px(1.0)), handle.center().y);
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
    cx.simulate_event(MouseUpEvent {
        position,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
    });
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [ChromeIntent::SidebarResizeReset]
        );
    });
}

#[gpui_kit::test]
fn tab_click_and_context_menu_emit_status_intents(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let tab = cx
        .debug_bounds("status-tab-top-1-tab-one")
        .expect("tab surface");
    cx.simulate_click(center(tab), Modifiers::none());

    cx.simulate_mouse_down(center(tab), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_keystrokes("down down down down down down down down down enter");

    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [
                ChromeIntent::Status(StatusIntent::Context {
                    session_id: "session-1".to_owned(),
                    window_id: "window-1".to_owned(),
                    action: bootty_gpui::TabContextAction::Activate,
                }),
                ChromeIntent::Status(StatusIntent::Context {
                    session_id: "session-1".to_owned(),
                    window_id: "window-1".to_owned(),
                    action: bootty_gpui::TabContextAction::ClosePane,
                }),
            ]
        );
    });
}

#[gpui_kit::test]
fn middle_click_on_tab_emits_close_intent(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let tab = cx
        .debug_bounds("status-tab-top-1-tab-one")
        .expect("tab surface");

    cx.simulate_mouse_down(center(tab), MouseButton::Middle, Modifiers::none());

    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [ChromeIntent::Status(StatusIntent::Context {
                session_id: "session-1".to_owned(),
                window_id: "window-1".to_owned(),
                action: bootty_gpui::TabContextAction::ClosePane,
            })]
        );
    });
}

#[gpui_kit::test]
fn dragging_sidebar_session_to_space_emits_move_intent(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let row = cx.debug_bounds("sidebar-row-session").expect("session row");
    let space = cx.debug_bounds("space-2").expect("target space");
    let start = center(row);
    let end = center(space);

    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_event(MouseUpEvent {
        position: end,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });

    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [ChromeIntent::MoveSessionsToSpace {
                sessions: vec![target()],
                to: SpaceKey(2),
            }]
        );
    });
}

struct PaneProbe {
    arrangement_target: Option<bootty_control::CommandTarget>,
    intents: Rc<RefCell<Vec<GpuiPaneIntent>>>,
}

impl Render for PaneProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let intents = Rc::clone(&self.intents);
        div()
            .w(px(400.0))
            .h(px(200.0))
            .child(GpuiPaneWorkspace::new(
                GpuiPaneWorkspaceSnapshot {
                    arrangement_target: self.arrangement_target.clone(),
                    area: PaneRect::new(0.0, 0.0, 400.0, 200.0),
                    panes: vec![
                        GpuiPaneSnapshot {
                            id: "left".to_owned(),
                            rect: PaneRect::new(0.0, 0.0, 200.0, 200.0),
                            terminal: gpui_kit::div().size_full().into_any_element(),
                            focused: true,
                            progress: None,
                        },
                        GpuiPaneSnapshot {
                            id: "right".to_owned(),
                            rect: PaneRect::new(200.0, 0.0, 200.0, 200.0),
                            terminal: gpui_kit::div().size_full().into_any_element(),
                            focused: false,
                            progress: None,
                        },
                    ],
                    dividers: vec![GpuiPaneDividerSnapshot {
                        path: vec![],
                        direction: PaneSplitDirection::Right,
                        rect: PaneRect::new(198.0, 0.0, 4.0, 200.0),
                        area: PaneRect::new(0.0, 0.0, 400.0, 200.0),
                    }],
                    gap: 4.0,
                    corner_radius: 0.0,
                    focus_border_width: 2.0,
                    inactive_dim: 0.0,
                    window_dim: 0.0,
                    animation_seconds: 0.0,
                    colors: GpuiPaneColors {
                        background: gpui_kit::black(),
                        divider: gpui_kit::white(),
                        divider_hover: gpui_kit::white(),
                        focus_border: gpui_kit::white(),
                        progress_track: gpui_kit::black(),
                        progress_normal: gpui_kit::white(),
                        progress_error: gpui_kit::white(),
                        progress_warning: gpui_kit::white(),
                        empty_text: gpui_kit::white(),
                    },
                    empty_message: None,
                },
                move |intent, _, _| intents.borrow_mut().push(intent),
            ))
    }
}

#[gpui_kit::test]
fn pane_click_focuses_and_double_click_divider_resets_ratio(cx: &mut TestAppContext) {
    let intents = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view({
        let intents = Rc::clone(&intents);
        move |_, _| PaneProbe {
            intents,
            arrangement_target: None,
        }
    });
    let right = cx
        .debug_bounds("terminal-pane-surface-right")
        .expect("right pane");
    let right_center = center(right);
    cx.simulate_mouse_down(right_center, MouseButton::Left, Modifiers::none());
    assert_eq!(
        intents.borrow().as_slice(),
        [GpuiPaneIntent::Focus("right".to_owned())]
    );
    cx.simulate_event(MouseUpEvent {
        position: right_center,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });
    let divider = cx
        .debug_bounds("terminal-divider-visual-root")
        .expect("root divider");
    let position = center(divider);
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
    cx.simulate_event(MouseUpEvent {
        position,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
    });
    assert_eq!(
        intents.borrow().as_slice(),
        [
            GpuiPaneIntent::Focus("right".to_owned()),
            GpuiPaneIntent::Resize {
                path: vec![],
                ratio: 0.5,
                min_fraction: 0.05,
            },
        ]
    );
}

#[gpui_kit::test]
fn pane_divider_drag_emits_a_clamped_ratio(cx: &mut TestAppContext) {
    let intents = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view({
        let intents = Rc::clone(&intents);
        move |_, _| PaneProbe {
            intents,
            arrangement_target: None,
        }
    });
    let divider = cx
        .debug_bounds("terminal-divider-visual-root")
        .expect("root divider");
    let start = center(divider);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(px(12.0), start.y),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    cx.simulate_mouse_move(
        point(px(80.0), start.y),
        Some(MouseButton::Left),
        Modifiers::none(),
    );

    assert!(intents.borrow().iter().any(|intent| matches!(
        intent,
        GpuiPaneIntent::Resize {
            path,
            ratio,
            min_fraction,
        } if path.is_empty() && *ratio >= *min_fraction && *ratio < 0.5
    )));
}

struct DialogProbe {
    dialog: Entity<DialogView>,
    intents: Rc<RefCell<Vec<DialogIntent>>>,
    _subscription: Subscription,
}

impl DialogProbe {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dialog = cx.new(|cx| DialogView::new(window, cx));
        let intents = Rc::new(RefCell::new(Vec::new()));
        let received = Rc::clone(&intents);
        let subscription = cx.subscribe(&dialog, move |_, _, intent: &DialogIntent, _| {
            received.borrow_mut().push(intent.clone());
        });
        Self {
            dialog,
            intents,
            _subscription: subscription,
        }
    }
}

impl Render for DialogProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.dialog.clone()
    }
}

struct RootDialogSurface {
    background_focus: FocusHandle,
}

impl Render for RootDialogSurface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().track_focus(&self.background_focus)
    }
}

fn rooted_dialog_window(
    cx: &TestAppContext,
    role: DialogRole,
    mut spec: DialogSpec,
) -> (Entity<DialogProbe>, VisualTestContext) {
    spec.role = role;
    let dialog_id = spec.id.clone();
    let probe_slot = Rc::new(RefCell::new(None));
    let opened_probe = Rc::clone(&probe_slot);
    let focus_slot = Rc::new(RefCell::new(None));
    let opened_focus = Rc::clone(&focus_slot);
    let window = cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        cx.open_window(gpui_kit::WindowOptions::default(), move |window, cx| {
            let probe = cx.new(|cx| {
                let probe = DialogProbe::new(window, cx);
                probe.dialog.update(cx, |dialog, cx| {
                    dialog.present(Some(spec), window, cx);
                });
                probe
            });
            opened_probe.replace(Some(probe));
            let surface = cx.new(|cx| RootDialogSurface {
                background_focus: cx.focus_handle(),
            });
            opened_focus.replace(Some(surface.read(cx).background_focus.clone()));
            cx.new(|cx| Root::new(surface, window, cx))
        })
        .expect("open rooted dialog")
    });
    let probe = probe_slot
        .borrow_mut()
        .take()
        .expect("capture rooted dialog probe");
    let background_focus = focus_slot
        .borrow_mut()
        .take()
        .expect("capture rooted dialog focus");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.refresh().expect("render rooted dialog");
    visual.update(|window, cx| {
        background_focus.focus(window, cx);
        let view = probe.read(cx).dialog.clone();
        let root_title = view.read(cx).root_title();
        let show_root_chrome = root_title.is_some();
        let cancel_view = view.clone();
        let cancel_id = dialog_id.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let cancel_view = cancel_view.clone();
            let cancel_id = cancel_id.clone();
            let content_view = view.clone();
            let dialog = dialog.close_button(show_root_chrome);
            let dialog = match root_title.clone() {
                Some(title) => dialog.title(title),
                None => dialog,
            };
            dialog
                .on_cancel(move |_, _, cx| {
                    cancel_view.update(cx, |_, cx| {
                        cx.emit(DialogIntent::Dismiss {
                            dialog: cancel_id.clone(),
                        });
                    });
                    true
                })
                .content(move |content, _, _| content.p_0().child(content_view.clone()))
        });
    });
    visual.run_until_parked();
    visual.refresh().expect("render active rooted dialog");
    (probe, visual)
}

fn dialog_window(
    cx: &mut TestAppContext,
    role: DialogRole,
    spec: DialogSpec,
) -> (Entity<DialogProbe>, &mut VisualTestContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    cx.add_window_view(move |window, cx| {
        let probe = DialogProbe::new(window, cx);
        probe.dialog.update(cx, |dialog, cx| {
            let mut spec = spec;
            spec.role = role;
            dialog.present(Some(spec), window, cx);
        });
        probe
    })
}

fn focus_dialog(probe: &Entity<DialogProbe>, cx: &mut VisualTestContext) {
    let focus = cx.update(|_, app| {
        let dialog = probe.read(app).dialog.clone();
        dialog.read(app).focus_handle(app)
    });
    cx.update(|window, app| focus.focus(window, app));
}

#[gpui_kit::test]
fn worktree_options_keep_the_composer_fixed_and_emit_edits(cx: &mut TestAppContext) {
    use bootty_gpui::{DialogField, DialogFieldKind};

    let mut spec = DialogSpec::prompt(
        bootty_ui::presentation::dialogs::NEW_SESSION_ID,
        "New session",
        "Keep the draft",
        "Prompt",
        DialogAction::new("start-session"),
    );
    spec.multiline = true;
    spec.fields = [
        (
            "provider",
            "Provider",
            "Codex",
            DialogFieldKind::Choice(vec!["Codex".into()]),
        ),
        (
            "isolation",
            "Checkout",
            "New worktree",
            DialogFieldKind::Choice(vec!["Current checkout".into(), "New worktree".into()]),
        ),
        ("start-ref", "Start from", "HEAD", DialogFieldKind::Text),
        ("branch", "Branch", "", DialogFieldKind::Text),
        ("folder", "Folder", "", DialogFieldKind::Text),
    ]
    .into_iter()
    .map(|(id, label, value, kind)| DialogField {
        id: id.into(),
        label: label.into(),
        value: value.into(),
        placeholder: String::new(),
        kind,
    })
    .collect();
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let slot = Rc::new(RefCell::new(None));
    let opened = Rc::clone(&slot);
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let probe = cx.new(|cx| DialogProbe::new(window, cx));
        probe.read(cx).dialog.clone().update(cx, |dialog, cx| {
            dialog.present(Some(spec), window, cx);
        });
        opened.replace(Some(probe.clone()));
        Root::new(probe, window, cx)
    });
    let probe = slot.borrow_mut().take().expect("creation view");
    draw_chrome(cx);
    let composer = cx.debug_bounds("dialog-prompt-input").expect("composer");
    let trigger = cx
        .debug_bounds("dialog-worktree-options-trigger")
        .expect("worktree options");
    cx.simulate_click(trigger.center(), Modifiers::none());
    draw_chrome(cx);
    assert_eq!(cx.debug_bounds("dialog-prompt-input"), Some(composer));
    for (field, selector, value) in [
        ("start-ref", "dialog-field-input-start-ref", "baseline"),
        ("branch", "dialog-field-input-branch", "luan/custom"),
        ("folder", "dialog-field-input-folder", "custom-checkout"),
    ] {
        let input = cx.debug_bounds(selector).expect("editable worktree field");
        cx.simulate_click(input.center(), Modifiers::none());
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        });
        cx.simulate_input(value);
        draw_chrome(cx);
        probe.update(cx, |probe, _| assert!(probe.intents.borrow().iter().any(|intent| matches!(intent,
            DialogIntent::FieldChanged { field: changed, value: accepted, .. } if changed == field && accepted == value
        ))));
    }
    cx.simulate_keystrokes("escape");
    draw_chrome(cx);
    assert!(cx.debug_bounds("dialog-field-input-start-ref").is_none());
    assert_eq!(cx.debug_bounds("dialog-prompt-input"), Some(composer));
    probe.update(cx, |probe, _| {
        assert!(
            !probe
                .intents
                .borrow()
                .iter()
                .any(|intent| matches!(intent, DialogIntent::Dismiss { .. }))
        );
    });
}

#[gpui_kit::test]
fn new_session_composer_routes_start_keys_and_preserves_shift_enter(cx: &mut TestAppContext) {
    let mut spec = DialogSpec::prompt(
        bootty_ui::presentation::dialogs::NEW_SESSION_ID,
        "New session",
        "draft",
        "Prompt",
        DialogAction::new("start-session"),
    );
    spec.multiline = true;
    let (probe, cx) = dialog_window(cx, DialogRole::Prompt, spec);
    focus_dialog(&probe, cx);
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("second line");
    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(intents.iter().any(|intent| matches!(intent,
            DialogIntent::TextChanged { value, .. } if value.contains('\n') && value.contains("second line")
        )));
        assert!(!intents.iter().any(|intent| matches!(intent, DialogIntent::Activate { .. })));
    });
    cx.simulate_keystrokes("enter cmd-enter");
    probe.update(cx, |probe, _| {
        let actions = probe
            .intents
            .borrow()
            .iter()
            .filter_map(|intent| match intent {
                DialogIntent::Activate { dialog, action, .. } => {
                    assert_eq!(dialog.0, bootty_ui::presentation::dialogs::NEW_SESSION_ID);
                    Some(action.0.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(actions, ["enter-session", "start-session-background"]);
    });
}

#[gpui_kit::test]
fn prompt_confirm_and_close_buttons_emit_dialog_intents(cx: &TestAppContext) {
    let (probe, mut cx) = rooted_dialog_window(
        cx,
        DialogRole::Prompt,
        DialogSpec::prompt(
            "prompt",
            "Rename",
            "old",
            "name",
            DialogAction::new("submit").with_payload("new"),
        ),
    );
    focus_dialog(&probe, &mut cx);
    cx.simulate_keystrokes("enter");
    probe.update(&mut cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(intents.contains(&DialogIntent::Activate {
            dialog: DialogId::new("prompt"),
            row: bootty_gpui::RowId::new("submit"),
            action: bootty_gpui::ActionId::new("submit"),
            payload: bootty_gpui::DialogPayload::text("new"),
        }));
    });
}

#[gpui_kit::test]
fn confirm_dialog_activates_only_the_enabled_row(cx: &mut TestAppContext) {
    let spec = DialogSpec {
        id: DialogId::new("confirm"),
        role: DialogRole::Confirm,
        title: "Delete session?".to_owned(),
        icon: None,
        hint: Some("Enter confirm   Esc close".to_owned()),
        footer: None,
        text: None,
        text_label: None,
        fields: Vec::new(),
        spaces: Vec::new(),
        attachments: Vec::new(),
        applications: Vec::new(),
        completion: None,
        models: Vec::new(),
        projects: Vec::new(),
        project_labels: std::collections::BTreeMap::new(),
        selected_project: None,
        models_loading: false,
        model_error: None,
        selected_model: None,
        busy: false,
        multiline: false,
        text_hint: None,
        rows: vec![
            DialogRow {
                enabled: false,
                ..DialogRow::action("delete", "Delete", DialogAction::new("delete"))
            },
            DialogRow::action("cancel", "Cancel", DialogAction::new("cancel")),
        ],
        empty_text: String::new(),
        placement: bootty_gpui::DialogPlacement::Center,
    };
    let (probe, cx) = dialog_window(cx, DialogRole::Confirm, spec);
    // Command skips disabled rows when selecting with the keyboard.
    focus_dialog(&probe, cx);
    cx.simulate_keystrokes("enter");
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [DialogIntent::Activate {
                dialog: DialogId::new("confirm"),
                row: bootty_gpui::RowId::new("cancel"),
                action: bootty_gpui::ActionId::new("cancel"),
                payload: bootty_gpui::DialogPayload::default(),
            }]
        );
    });
}

#[gpui_kit::test]
fn terminal_find_buttons_and_shift_enter_keep_direction_typed(cx: &mut TestAppContext) {
    let spec = DialogSpec {
        id: DialogId::new("find"),
        role: DialogRole::TerminalFind,
        title: "Find".to_owned(),
        icon: None,
        hint: None,
        footer: Some("1 / 2".to_owned()),
        text: Some("needle".to_owned()),
        text_label: None,
        fields: Vec::new(),
        spaces: Vec::new(),
        attachments: Vec::new(),
        applications: Vec::new(),
        completion: None,
        models: Vec::new(),
        projects: Vec::new(),
        project_labels: std::collections::BTreeMap::new(),
        selected_project: None,
        models_loading: false,
        model_error: None,
        selected_model: None,
        busy: false,
        multiline: false,
        text_hint: Some("find".to_owned()),
        rows: vec![
            DialogRow::action("previous", "Previous", DialogAction::new("previous")),
            DialogRow::action("next", "Next", DialogAction::new("next")),
        ],
        empty_text: String::new(),
        placement: bootty_gpui::DialogPlacement::TopRight,
    };
    let (probe, cx) = dialog_window(cx, DialogRole::TerminalFind, spec);
    focus_dialog(&probe, cx);
    cx.simulate_keystrokes("shift-enter");
    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().contains(&DialogIntent::Find {
            dialog: DialogId::new("find"),
            query: "needle".to_owned(),
            direction: FindDirection::Previous,
        }));
    });
}

#[gpui_kit::test]
fn theme_picker_scope_toggle_and_tab_emit_cycle_scope(cx: &TestAppContext) {
    let spec = DialogSpec {
        id: DialogId::new("themes"),
        role: DialogRole::ThemePicker,
        title: "Themes".to_owned(),
        icon: None,
        hint: Some("Tab cycle scope".to_owned()),
        footer: Some("2 themes · All".to_owned()),
        text: Some(String::new()),
        text_label: None,
        fields: Vec::new(),
        spaces: Vec::new(),
        attachments: Vec::new(),
        applications: Vec::new(),
        completion: None,
        models: Vec::new(),
        projects: Vec::new(),
        project_labels: std::collections::BTreeMap::new(),
        selected_project: None,
        models_loading: false,
        model_error: None,
        selected_model: None,
        busy: false,
        multiline: false,
        text_hint: Some("filter".to_owned()),
        rows: vec![DialogRow::action("one", "One", DialogAction::new("one"))],
        empty_text: "none".to_owned(),
        placement: bootty_gpui::DialogPlacement::Center,
    };
    let (probe, mut cx) = rooted_dialog_window(cx, DialogRole::ThemePicker, spec);
    focus_dialog(&probe, &mut cx);
    cx.simulate_keystrokes("tab");
    probe.update(&mut cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [DialogIntent::CycleScope {
                dialog: DialogId::new("themes"),
            }]
        );
    });
}

#[gpui_kit::test]
fn pane_grip_drag_emits_scoped_swap_or_edge_move(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let target = bootty_control::CommandTarget {
        kind: bootty_control::ResourceKind::Session,
        handle: "captured-session".to_owned(),
        generation: 7,
    };
    let intents = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view({
        let intents = Rc::clone(&intents);
        let target = target.clone();
        move |_, _| PaneProbe {
            intents,
            arrangement_target: Some(target),
        }
    });
    let grip = center(cx.debug_bounds("pane-grip-left").expect("drag grip"));
    for (x, y, direction) in [
        (300., 100., None),
        (210., 100., Some("left")),
        (390., 100., Some("right")),
        (300., 10., Some("up")),
        (300., 190., Some("down")),
    ] {
        intents.borrow_mut().clear();
        let end = point(px(x), px(y));
        cx.simulate_mouse_down(grip, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(grip.x.sub(px(15.)), grip.y.add(px(15.))),
            Some(MouseButton::Left),
            Modifiers::none(),
        );
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_event(MouseUpEvent {
            position: end,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
        });
        let mut args = vec!["left".to_owned(), "right".to_owned()];
        if let Some(direction) = direction {
            args.push(direction.to_owned());
        }
        let mut expected = bootty_control::CommandInvocation::new(
            if direction.is_some() {
                "pane.move"
            } else {
                "pane.swap"
            },
            args,
            bootty_control::Caller::Keybinding,
        );
        expected.target = Some(target.clone());
        assert_eq!(
            intents
                .borrow()
                .iter()
                .filter(|intent| matches!(intent, GpuiPaneIntent::Command(_)))
                .cloned()
                .collect::<Vec<_>>(),
            vec![GpuiPaneIntent::Command(expected)]
        );
    }
}

#[gpui_kit::test]
fn status_only_bar_does_not_cancel_a_tab_reorder(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    let top = snapshot.top_status.as_mut().expect("top status");
    let mut bottom = top.clone();
    bottom.key = "bottom".to_owned();
    bottom
        .segments
        .retain(|segment| segment.surface != "windows");
    snapshot.bottom_status = Some(bottom);
    let tabs = &mut top.segments[1].items;
    let mut second = tabs[0].clone();
    second.key = "tab-two".to_owned();
    second.text = "Two".to_owned();
    second.reorder_anchor = Some("tab-two".to_owned());
    second.active = false;
    second.tab_context = None;
    tabs.push(second);
    let (probe, cx) =
        cx.add_window_view(move |window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let first = cx
        .debug_bounds("status-tab-top-1-tab-one")
        .expect("first tab");
    let second = cx
        .debug_bounds("status-tab-top-1-tab-two")
        .expect("second tab");
    let end = point(first.left().add(px(8.0)), first.center().y);
    cx.simulate_mouse_down(center(second), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_event(MouseUpEvent {
        position: end,
        button: MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 1,
    });
    probe.update(cx, |probe, _| {
        assert!(
            probe
                .intents
                .borrow()
                .contains(&ChromeIntent::Status(StatusIntent::Reorder {
                    source: "tab-two".to_owned(),
                    before: Some("tab-one".to_owned()),
                }))
        );
    });
}

#[gpui_kit::test]
fn pane_divider_commits_the_drop_position_after_one_motion(cx: &mut TestAppContext) {
    let intents = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view({
        let intents = Rc::clone(&intents);
        move |_, _| PaneProbe {
            intents,
            arrangement_target: None,
        }
    });
    let start = center(
        cx.debug_bounds("terminal-divider-visual-root")
            .expect("divider"),
    );
    let end = point(px(80.0), start.y);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    // The first motion starts GPUI's drag; there need not be another move before release.
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_event(MouseUpEvent {
        position: end,
        button: MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 1,
    });
    assert!(
        intents
            .borrow()
            .iter()
            .any(|intent| matches!(intent, GpuiPaneIntent::Resize { ratio, .. } if *ratio < 0.5))
    );
}

#[gpui_kit::test]
fn terminal_tab_variants_keep_close_buttons_in_padding(cx: &mut TestAppContext) {
    use bootty_config::config::{TabAppearance, TabCloseButton, TabClosePosition, TabConfig};
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    for appearance in [
        TabAppearance::Classic,
        TabAppearance::Underline,
        TabAppearance::Pill,
        TabAppearance::Outline,
        TabAppearance::Segmented,
    ] {
        for close_position in [TabClosePosition::Left, TabClosePosition::Right] {
            let mut shown_width = None;
            for close_button in [
                TabCloseButton::Always,
                TabCloseButton::Hover,
                TabCloseButton::Hidden,
            ] {
                let mut snapshot = chrome_snapshot();
                snapshot.layout.tabs = TabConfig {
                    appearance,
                    close_position,
                    close_button,
                };
                let (probe, view) = cx.add_window_view(move |window, cx| {
                    ChromeProbe::with_snapshot(snapshot, window, cx)
                });
                let tab = view.debug_bounds("status-tab-top-1-tab-one").unwrap();
                if close_button == TabCloseButton::Hidden {
                    assert!(
                        view.debug_bounds("status-tab-close-top-1-tab-one")
                            .is_none()
                    );
                    continue;
                }
                if let Some(width) = shown_width {
                    assert_eq!(tab.size.width, width, "hover must not change tab width");
                } else {
                    shown_width = Some(tab.size.width);
                }
                view.simulate_mouse_move(center(tab), None, Modifiers::none());
                view.update(|window, cx| {
                    _ = window.draw(cx);
                });
                let close = view.debug_bounds("status-tab-close-top-1-tab-one").unwrap();
                let label = view.debug_bounds("status-item-1-tab-one").unwrap();
                assert!(
                    close.left() >= tab.left() && close.right() <= tab.right(),
                    "{appearance:?} {close_position:?}: close {close:?} tab {tab:?}"
                );
                match close_position {
                    TabClosePosition::Left => assert!(close.right() <= label.left()),
                    TabClosePosition::Right => assert!(close.left() >= label.right()),
                }
                view.simulate_click(center(close), Modifiers::none());
                probe.update(view, |probe, _| {
                    assert_eq!(
                        probe.intents.borrow().as_slice(),
                        [ChromeIntent::Status(StatusIntent::Context {
                            session_id: "session-1".to_owned(),
                            window_id: "window-1".to_owned(),
                            action: bootty_gpui::TabContextAction::ClosePane,
                        })]
                    );
                });
            }
        }
    }
}

#[gpui_kit::test]
fn terminal_tab_progress_stays_below_label(cx: &mut TestAppContext) {
    use bootty_config::config::TabAppearance;
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    for appearance in [
        TabAppearance::Classic,
        TabAppearance::Underline,
        TabAppearance::Pill,
        TabAppearance::Outline,
        TabAppearance::Segmented,
    ] {
        let mut snapshot = chrome_snapshot();
        snapshot.layout.tabs.appearance = appearance;
        let item = snapshot
            .top_status
            .as_mut()
            .unwrap()
            .segments
            .iter_mut()
            .flat_map(|segment| &mut segment.items)
            .find(|item| item.key == "tab-one")
            .unwrap();
        item.progress = Some(bootty_gpui::StatusProgress {
            value: Some(50),
            color: color(200, 180, 80),
        });
        let (_, view) =
            cx.add_window_view(move |window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
        let label = view.debug_bounds("status-label-1-tab-one").unwrap();
        let progress = view.debug_bounds("status-progress-1-tab-one").unwrap();
        assert!(
            progress.top() >= label.bottom(),
            "{appearance:?}: {progress:?} overlaps {label:?}"
        );
    }
}

#[gpui_kit::test]
fn empty_chrome_click_preserves_keyboard_focus(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (_probe, cx) = cx.add_window_view(ChromeProbe::new);
    let previous = cx.update(|window, app| {
        let focus = app.focus_handle();
        focus.focus(window, app);
        focus
    });
    let sidebar = cx
        .debug_bounds("bootty-gpui-sidebar-shell")
        .expect("sidebar");
    for position in [
        point(
            sidebar.origin.x.add(px(100.0)),
            sidebar.origin.y.add(px(300.0)),
        ),
        point(px(500.0), px(300.0)),
    ] {
        cx.simulate_click(position, Modifiers::none());
        assert!(cx.update(|window, _| previous.is_focused(window)));
    }
}

fn overflowing_tabs() -> ChromeSnapshot {
    let mut snapshot = chrome_snapshot();
    let segment = snapshot
        .top_status
        .as_mut()
        .unwrap()
        .segments
        .iter_mut()
        .find(|segment| segment.surface == "windows")
        .unwrap();
    let template = segment.items[0].clone();
    segment.items = (0..16)
        .map(|ix| {
            let mut item = template.clone();
            item.key = format!("tab-{ix}");
            item.text = format!("{ix} shell");
            item.reorder_anchor = Some(format!("tab-{ix}"));
            item.tab_context.as_mut().unwrap().window_id = format!("window-{ix}");
            item.active = ix == 0;
            item
        })
        .collect();
    snapshot
}

fn draw_chrome(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
}

fn update_chrome(
    probe: &Entity<ChromeProbe>,
    snapshot: &ChromeSnapshot,
    cx: &mut VisualTestContext,
) {
    cx.update(|window, cx| {
        probe
            .read(cx)
            .chrome
            .clone()
            .update(cx, |chrome, cx| chrome.update(snapshot, window, cx));
    });
    draw_chrome(cx);
}

#[gpui_kit::test]
fn tab_strip_reveals_selection_and_preserves_manual_scrolling(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = overflowing_tabs();
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    draw_chrome(cx);
    assert!(cx.debug_bounds("mux-tabs-top-1-scroll-left").is_none());
    assert!(cx.debug_bounds("mux-tabs-top-1-scroll-right").is_some());
    let region = cx.debug_bounds("status-tabs-top-1").unwrap();
    for (selected, selector) in [
        (15, "status-tab-top-1-tab-15"),
        (0, "status-tab-top-1-tab-0"),
        (9, "status-tab-top-1-tab-9"),
    ] {
        for (ix, item) in snapshot.top_status.as_mut().unwrap().segments[1]
            .items
            .iter_mut()
            .enumerate()
        {
            item.active = ix == selected;
        }
        update_chrome(&probe, &snapshot, cx);
        let tab = cx.debug_bounds(selector).unwrap();
        assert!(
            tab.left() >= region.left(),
            "selected tab is clipped on left: {tab:?}"
        );
        assert!(
            tab.right() <= region.right(),
            "selected tab is clipped on right: {tab:?}"
        );
    }
    let before = cx.debug_bounds("status-tab-top-1-tab-9").unwrap();
    cx.simulate_event(ScrollWheelEvent {
        position: region.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(150.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    draw_chrome(cx);
    let after = cx.debug_bounds("status-tab-top-1-tab-9").unwrap();
    assert!(
        after.left() > before.left(),
        "vertical wheel must scroll tabs horizontally"
    );
    snapshot.titlebar.title = "Unrelated update".into();
    update_chrome(&probe, &snapshot, cx);
    assert_eq!(cx.debug_bounds("status-tab-top-1-tab-9"), Some(after));
    let next = cx.debug_bounds("mux-tabs-top-1-scroll-right").unwrap();
    cx.simulate_click(next.center(), Modifiers::none());
    draw_chrome(cx);
    assert!(cx.debug_bounds("status-tab-top-1-tab-9").unwrap().left() < after.left());
}

#[gpui_kit::test]
fn overflowing_tabs_keep_the_new_tab_button_clickable_after_scrolling(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(overflowing_tabs(), window, cx));
    draw_chrome(cx);
    let region = cx.debug_bounds("status-tabs-top-1").expect("tab viewport");
    for _ in 0..16 {
        let Some(next) = cx.debug_bounds("mux-tabs-top-1-scroll-right") else {
            break;
        };
        cx.simulate_click(next.center(), Modifiers::none());
        draw_chrome(cx);
    }
    assert!(cx.debug_bounds("mux-tabs-top-1-scroll-right").is_none());
    let button = cx
        .debug_bounds("status-new-tab-top-1")
        .expect("new tab button");
    assert!(
        button.size.width >= px(24.0)
            && button.left() >= region.left()
            && button.right() <= region.right(),
        "new tab must stay visible and clickable at the end: {button:?}, {region:?}"
    );
    cx.simulate_click(button.center(), Modifiers::none());
    draw_chrome(cx);
    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(matches!(intents.as_slice(), [ChromeIntent::Command(invocation)] if invocation.command == "new_tab"), "visible new tab button must emit its command: {intents:?}");
    });
}

#[gpui_kit::test]
fn keyboard_focus_reveals_offscreen_tabs(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (_, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(overflowing_tabs(), window, cx));
    draw_chrome(cx);
    let first = cx.debug_bounds("status-tab-top-1-tab-0").unwrap().center();
    cx.simulate_click(first, Modifiers::none());
    cx.simulate_keystrokes("end");
    draw_chrome(cx);
    let region = cx.debug_bounds("status-tabs-top-1").unwrap();
    let last = cx.debug_bounds("status-tab-top-1-tab-15").unwrap();
    assert!(
        last.left() >= region.left() && last.right() <= region.right(),
        "focused tab must be visible: {last:?}, {region:?}"
    );
    cx.simulate_keystrokes("home");
    draw_chrome(cx);
    let first = cx.debug_bounds("status-tab-top-1-tab-0").unwrap();
    assert!(first.left() >= region.left() && first.right() <= region.right());
}

#[gpui_kit::test]
fn tab_width_waits_for_a_stable_title_before_shrinking(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = overflowing_tabs();
    snapshot.top_status.as_mut().unwrap().segments[1].items[0].text =
        "A considerably longer terminal task title".into();
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    draw_chrome(cx);
    let initial = cx
        .debug_bounds("status-tab-top-1-tab-0")
        .unwrap()
        .size
        .width;
    snapshot.top_status.as_mut().unwrap().segments[1].items[0].text = "shell".into();
    update_chrome(&probe, &snapshot, cx);
    assert_eq!(
        cx.debug_bounds("status-tab-top-1-tab-0")
            .unwrap()
            .size
            .width,
        initial
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(750));
    snapshot.top_status.as_mut().unwrap().segments[1].items[0].text = "build".into();
    update_chrome(&probe, &snapshot, cx);
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(750));
    draw_chrome(cx);
    assert_eq!(
        cx.debug_bounds("status-tab-top-1-tab-0")
            .unwrap()
            .size
            .width,
        initial
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(300));
    draw_chrome(cx);
    assert!(
        cx.debug_bounds("status-tab-top-1-tab-0")
            .unwrap()
            .size
            .width
            < initial.div(2.0),
        "a short title should settle to a compact tab"
    );
}

#[gpui_kit::test]
fn empty_session_sidebar_opens_the_shared_creation_command(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    snapshot.sidebar.as_mut().expect("sidebar").rows.clear();
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let create = cx
        .debug_bounds("sidebar-create-session")
        .expect("visible creation action");
    cx.simulate_click(center(create), Modifiers::none());
    probe.update(cx, |probe, _| {
        assert!(matches!(probe.intents.borrow().as_slice(), [ChromeIntent::Command(invocation)]
            if invocation.command == "new_mux_session" && invocation.caller == bootty_control::Caller::Internal));
    });
}

#[gpui_kit::test]
fn detached_session_row_reopens_and_edits_metadata_with_stable_height(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    let row = &mut snapshot.sidebar.as_mut().expect("sidebar").rows[0];
    row.kind = SidebarRowKind::DetachedSession;
    row.text = "Review keyboard input".to_owned();
    row.trailing = None;
    row.trailing_icon = None;
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    let detached = cx.debug_bounds("sidebar-row-session").expect("saved row");
    cx.simulate_click(center(detached), Modifiers::none());
    cx.run_until_parked();
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [ChromeIntent::ReopenSession(target())]
        );
        probe.intents.borrow_mut().clear();
    });
    cx.simulate_mouse_down(center(detached), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [ChromeIntent::RenameSavedSession(target())]
        );
    });
    let row = &mut snapshot.sidebar.as_mut().expect("sidebar").rows[0];
    row.kind = SidebarRowKind::Session;
    row.trailing = None;
    row.trailing_icon = None;
    cx.update(|window, cx| {
        probe
            .read(cx)
            .chrome
            .clone()
            .update(cx, |chrome, cx| chrome.update(&snapshot, window, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("sidebar-row-session").unwrap().size.height,
        detached.size.height
    );
}

#[gpui_kit::test]
fn project_filter_retains_search_and_does_not_activate_a_hidden_session(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = task_row_snapshot(false);
    snapshot.sidebar.as_mut().unwrap().rows[1].project_path = Some("/projects/agents".to_owned());
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let options = cx.debug_bounds("sidebar-view").unwrap();
    cx.simulate_click(options.center(), Modifiers::none());
    cx.run_until_parked();
    draw_chrome(cx);
    cx.simulate_keystrokes("down down right");
    cx.run_until_parked();
    draw_chrome(cx);
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    draw_chrome(cx);
    assert!(cx.debug_bounds("sidebar-row-task-1").is_some());
    assert!(cx.debug_bounds("sidebar-row-task-0").is_none());
    probe.update(cx, |probe, _| {
        assert!(
            probe.intents.borrow().is_empty(),
            "filter never activates or closes sessions"
        );
    });
    cx.simulate_click(options.center(), Modifiers::none());
    cx.run_until_parked();
    draw_chrome(cx);
    cx.simulate_keystrokes("down down right");
    cx.run_until_parked();
    draw_chrome(cx);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    draw_chrome(cx);
    assert!(cx.debug_bounds("sidebar-row-task-0").is_some());
}

fn saved_task(state: bootty_mux::session_membership::SessionState) -> SidebarTask {
    SidebarTask {
        native_conversation: None,
        identity: "saved-identity".to_owned(),
        state,
        binding: Some(bootty_control::CommandTarget {
            kind: bootty_control::ResourceKind::Binding,
            handle: "captured-binding".to_owned(),
            generation: 42,
        }),
        pending: false,
    }
}

fn saved_task_snapshot(state: bootty_mux::session_membership::SessionState) -> ChromeSnapshot {
    let mut snapshot = chrome_snapshot();
    let sidebar = snapshot.sidebar.as_mut().expect("sidebar");
    sidebar.rows.truncate(1);
    let row = &mut sidebar.rows[0];
    row.kind = SidebarRowKind::DetachedSession;
    row.task = Some(saved_task(state));
    snapshot
}

fn choose_task_view(index: usize, cx: &mut VisualTestContext) {
    let options = cx.debug_bounds("sidebar-view").expect("view controls");
    cx.simulate_click(options.center(), Modifiers::none());
    draw_chrome(cx);
    cx.simulate_keystrokes("down down down down right");
    draw_chrome(cx);
    for _ in 0..index.saturating_sub(1) {
        cx.simulate_keystrokes("down");
    }
    cx.simulate_keystrokes("enter");
    draw_chrome(cx);
}

#[gpui_kit::test]
fn selected_task_settle_button_uses_its_saved_binding(cx: &mut TestAppContext) {
    use bootty_mux::session_membership::SessionState;
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = saved_task_snapshot(SessionState::default());
    let row = &mut snapshot.sidebar.as_mut().unwrap().rows[0];
    row.current = true;
    row.trailing = None;
    row.trailing_icon = None;
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    let bounds = cx.debug_bounds("sidebar-row-session").unwrap();
    assert_eq!(cx.debug_bounds("sidebar-settle-session"), None);
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    draw_chrome(cx);
    let settle = cx
        .debug_bounds("sidebar-settle-session")
        .expect("hovered Settle");
    cx.simulate_click(settle.center(), Modifiers::none());
    draw_chrome(cx);
    let mut expected = bootty_control::CommandInvocation::new(
        "session.settle",
        vec!["saved-identity".to_owned()],
        bootty_control::Caller::Internal,
    );
    expected.target = saved_task(SessionState::default()).binding;
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [ChromeIntent::Command(expected)]
        );
    });

    let row = &mut snapshot.sidebar.as_mut().unwrap().rows[0];
    row.trailing = Some("Working".to_owned());
    row.trailing_icon = Some("circle-dashed".to_owned());
    cx.update(|window, cx| {
        probe
            .read(cx)
            .chrome
            .clone()
            .update(cx, |chrome, cx| chrome.update(&snapshot, window, cx));
    });
    draw_chrome(cx);
    assert_eq!(cx.debug_bounds("sidebar-settle-session"), None);
    assert!(cx.debug_bounds("sidebar-status-session").is_some());
    assert_eq!(
        cx.debug_bounds("sidebar-row-session").unwrap().size.height,
        bounds.size.height
    );
}

#[gpui_kit::test]
fn saved_task_without_a_terminal_has_no_connection_status(cx: &mut TestAppContext) {
    use bootty_mux::session_membership::{SessionLifecycle, SessionState};
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = saved_task_snapshot(SessionState::default());
    let row = &mut snapshot.sidebar.as_mut().unwrap().rows[0];
    row.trailing = None;
    row.trailing_icon = None;
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    let active = cx.debug_bounds("sidebar-row-session").unwrap();
    assert_eq!(cx.debug_bounds("sidebar-status-session"), None);
    assert_eq!(cx.debug_bounds("sidebar-lifecycle-session"), None);

    snapshot.sidebar.as_mut().unwrap().rows[0].task = Some(saved_task(SessionState {
        lifecycle: SessionLifecycle::Settled,
        ..SessionState::default()
    }));
    cx.update(|window, cx| {
        probe.read(cx).chrome.clone().update(cx, |chrome, cx| {
            chrome.update(&snapshot, window, cx);
        });
    });
    draw_chrome(cx);
    assert!(cx.debug_bounds("sidebar-settled-section").is_some());
    let settled = cx.debug_bounds("sidebar-row-session").unwrap();
    let status = cx.debug_bounds("sidebar-status-session").unwrap();
    let title = cx.debug_bounds("sidebar-title-session").unwrap();
    assert_eq!(settled.size.height, active.size.height);
    assert!(status.center().y.sub(title.center().y).abs() <= px(0.5));
    assert!(title.right() < status.left());
    assert!(cx.debug_bounds("sidebar-status-icon-session").is_some());
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn saved_task_views_filter_without_backend_actions_and_keep_row_heights(cx: &mut TestAppContext) {
    use bootty_mux::session_membership::{SessionLifecycle, SessionState};
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = saved_task_snapshot(SessionState::default());
    let sidebar = snapshot.sidebar.as_mut().expect("sidebar");
    let template = sidebar.rows[0].clone();
    let states = [
        SessionState::default(),
        SessionState {
            lifecycle: SessionLifecycle::Settled,
            ..SessionState::default()
        },
        SessionState {
            archived: true,
            ..SessionState::default()
        },
        SessionState {
            snoozed_until: Some(1_001),
            ..SessionState::default()
        },
        SessionState {
            hidden: true,
            ..SessionState::default()
        },
        SessionState {
            deleted: true,
            ..SessionState::default()
        },
    ];
    sidebar.rows = states
        .into_iter()
        .enumerate()
        .map(|(index, state)| {
            let mut row = template.clone();
            row.key = format!("saved-{index}");
            row.text = format!("Saved task {index}");
            row.task = Some(saved_task(state));
            row
        })
        .collect();
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let active_height = cx
        .debug_bounds("sidebar-row-saved-0")
        .expect("default Active view")
        .size
        .height;
    let row_selectors = [
        "sidebar-row-saved-0",
        "sidebar-row-saved-1",
        "sidebar-row-saved-2",
        "sidebar-row-saved-3",
        "sidebar-row-saved-4",
        "sidebar-row-saved-5",
    ];
    for selected in [0, 2, 3, 4] {
        choose_task_view(selected, cx);
        for (index, selector) in row_selectors.into_iter().enumerate() {
            let bounds = cx.debug_bounds(selector);
            assert_eq!(
                bounds.is_some(),
                index == selected || (selected == 0 && index == 1),
                "view {selected}, row {index}"
            );
            if let Some(bounds) = bounds {
                assert_eq!(bounds.size.height, active_height);
            }
        }
    }
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn saved_task_keyboard_menu_captures_binding_identity_and_absolute_snooze(cx: &mut TestAppContext) {
    use bootty_mux::session_membership::SessionState;
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = saved_task_snapshot(SessionState::default());
    snapshot.sidebar.as_mut().unwrap().rows[0].kind = SidebarRowKind::Session;
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let row = cx.debug_bounds("sidebar-row-session").expect("saved row");
    for (position, preset, command, deadline) in [
        (3, None, "session.pin", None),
        (4, None, "session.settle", None),
        (6, None, "session.hide", None),
        (7, None, "session.archive", None),
        (5, Some(0), "session.snooze", Some("4600")),
        (5, Some(1), "session.snooze", Some("87400")),
        (5, Some(2), "session.snooze", Some("605800")),
    ] {
        cx.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::none());
        draw_chrome(cx);
        for _ in 0..position {
            cx.simulate_keystrokes("down");
        }
        if let Some(preset) = preset {
            cx.simulate_keystrokes("right");
            draw_chrome(cx);
            for _ in 0..preset {
                cx.simulate_keystrokes("down");
            }
        }
        cx.simulate_keystrokes("enter");
        draw_chrome(cx);
        let mut expected = bootty_control::CommandInvocation::new(
            command,
            std::iter::once("saved-identity".to_owned())
                .chain(deadline.map(str::to_owned))
                .collect(),
            bootty_control::Caller::Internal,
        );
        expected.target = saved_task(SessionState::default()).binding;
        probe.update(cx, |probe, _| {
            assert_eq!(
                probe.intents.borrow().as_slice(),
                [ChromeIntent::Command(expected)]
            );
            probe.intents.borrow_mut().clear();
        });
    }
}

#[gpui_kit::test]
fn pending_saved_task_menu_is_a_keyboard_noop(cx: &mut TestAppContext) {
    use bootty_mux::session_membership::SessionState;
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = saved_task_snapshot(SessionState {
        archived: true,
        ..SessionState::default()
    });
    snapshot.sidebar.as_mut().unwrap().rows[0]
        .task
        .as_mut()
        .unwrap()
        .pending = true;
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    choose_task_view(2, cx);
    let row = cx.debug_bounds("sidebar-row-session").unwrap();
    cx.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::none());
    draw_chrome(cx);
    cx.simulate_keystrokes("down enter");
    draw_chrome(cx);
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn saved_task_menu_offers_the_exact_lifecycle_recovery_for_its_current_state(
    cx: &mut TestAppContext,
) {
    use bootty_mux::session_membership::{SessionLifecycle, SessionState};
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    for (state, view, position, command) in [
        (
            SessionState {
                lifecycle: SessionLifecycle::Settled,
                ..SessionState::default()
            },
            0,
            4,
            "session.activate",
        ),
        (
            SessionState {
                archived: true,
                ..SessionState::default()
            },
            2,
            3,
            "session.unarchive",
        ),
        (
            SessionState {
                snoozed_until: Some(1_100),
                ..SessionState::default()
            },
            3,
            5,
            "session.unsnooze",
        ),
        (
            SessionState {
                hidden: true,
                ..SessionState::default()
            },
            4,
            6,
            "session.show",
        ),
    ] {
        let snapshot = saved_task_snapshot(state);
        let (probe, cx) =
            cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
        choose_task_view(view, cx);
        let row = cx.debug_bounds("sidebar-row-session").unwrap();
        cx.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::none());
        draw_chrome(cx);
        for _ in 0..position {
            cx.simulate_keystrokes("down");
        }
        cx.simulate_keystrokes("enter");
        draw_chrome(cx);
        let mut invocation = bootty_control::CommandInvocation::new(
            command,
            vec!["saved-identity".to_owned()],
            bootty_control::Caller::Internal,
        );
        invocation.target = saved_task(state).binding;
        probe.update(cx, |probe, _| {
            assert_eq!(
                probe.intents.borrow().as_slice(),
                [ChromeIntent::Command(invocation)]
            );
        });
    }
}

#[gpui_kit::test]
fn working_ring_parks_when_disabled_reduced_or_inactive(cx: &mut TestAppContext) {
    use std::{cell::Cell, time::Duration};
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    let row = &mut snapshot.sidebar.as_mut().unwrap().rows[0];
    row.trailing = Some("Working 7s".to_owned());
    row.trailing_icon = Some("circle-dashed".to_owned());
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    let notifications = Rc::new(Cell::new(0_usize));
    let observed = notifications.clone();
    let _subscription = probe.update(cx, |probe, cx| {
        cx.observe(&probe.chrome, move |_, _, _| {
            observed.set(observed.get().saturating_add(1));
        })
    });
    cx.update(|window, _| window.activate_window());
    draw_chrome(cx);
    notifications.set(0);
    cx.executor().advance_clock(Duration::from_millis(34));
    draw_chrome(cx);
    assert!(notifications.get() > 0, "visible Working ring rotates");

    for (animate, reduced, active) in [
        (false, false, true),
        (true, true, true),
        (true, false, false),
    ] {
        snapshot.sidebar.as_mut().unwrap().animate_working = animate;
        cx.update(|_, cx| cx.set_reduce_motion(reduced));
        if !active {
            cx.deactivate_window();
        }
        update_chrome(&probe, &snapshot, cx);
        // Let the already scheduled frame retire, then check the quiet steady state.
        cx.executor().advance_clock(Duration::from_millis(34));
        draw_chrome(cx);
        notifications.set(0);
        cx.executor().advance_clock(Duration::from_secs(1));
        draw_chrome(cx);
        assert_eq!(notifications.get(), 0);
        assert!(cx.debug_bounds("sidebar-status-session").is_some());
        assert!(cx.debug_bounds("sidebar-status-icon-session").is_some());
    }
}

#[gpui_kit::test]
fn expired_snooze_returns_to_lifecycle_view_with_injected_clock(cx: &mut TestAppContext) {
    use bootty_mux::session_membership::SessionState;
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = saved_task_snapshot(SessionState {
        snoozed_until: Some(1_001),
        ..SessionState::default()
    });
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    assert!(cx.debug_bounds("sidebar-row-session").is_none());
    snapshot.sidebar.as_mut().unwrap().now_utc = 1_001;
    update_chrome(&probe, &snapshot, cx);
    assert!(cx.debug_bounds("sidebar-row-session").is_some());
    assert!(cx.debug_bounds("sidebar-status-session").is_some());
    assert!(cx.debug_bounds("sidebar-status-icon-session").is_some());
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn native_tab_keeps_its_exact_target_across_selection_updates(cx: &mut TestAppContext) {
    use bootty_agents::{AgentKind, NativeSessionConfig, NativeSessionRecord};
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let record = NativeSessionRecord {
        id: "native:codex:owned-thread".into(),
        binding_id: "binding-one".into(),
        task_identity: Some("task-one".into()),
        title: "Review the project".into(),
        pending_initial_message: None,
        permissions_pending: false,
        generation: 7,
        config: NativeSessionConfig::new(AgentKind::Codex, "/project"),
        snapshot: serde_json::from_value(serde_json::json!({
            "provider":"codex", "session_id":"owned-thread", "turn_id":null,
            "status":"idle", "transcript":[], "requests":[], "usage":null,
            "error":null, "revision":1
        }))
        .expect("provider snapshot"),
        side_chat: None,
        spawn_parent: None,
        attachments: Vec::new(),
    };
    let mut snapshot = chrome_snapshot();
    let selector = "status-tab-top-1-native:codex:owned-thread:conversation";
    let segment = &mut snapshot.top_status.as_mut().expect("top strip").segments[1];
    let mut native_item = segment.items.first().expect("terminal tab style").clone();
    native_item.key = format!("{}:conversation", record.id);
    native_item.text.clone_from(&record.title);
    native_item.icon = Some("openai".to_owned());
    native_item.action = Some(NativeChromeAction::FocusConversation(record.target()));
    native_item.reorder_anchor = None;
    native_item.tab_context = None;
    native_item.active = false;
    segment.items.push(native_item);
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    for selected in [true, false, true] {
        let items = &mut snapshot.top_status.as_mut().expect("strip").segments[1].items;
        items.first_mut().expect("terminal tab").active = !selected;
        items
            .first_mut()
            .expect("terminal tab")
            .tab_context
            .as_mut()
            .expect("terminal context")
            .can_activate = selected;
        items.last_mut().expect("native tab").active = selected;
        cx.update(|window, cx| {
            probe.read(cx).chrome.clone().update(cx, |chrome, cx| {
                chrome.update(&snapshot, window, cx);
            });
        });
        draw_chrome(cx);
        let bounds = cx
            .debug_bounds(selector)
            .expect("native tab survives selection changes");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }
    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().iter().all(|intent| {
            *intent
                == ChromeIntent::Status(StatusIntent::Action(
                    NativeChromeAction::FocusConversation(record.target()),
                ))
        }));
        assert_eq!(probe.intents.borrow().len(), 3);
    });
    let terminal = cx
        .debug_bounds("status-tab-top-1-tab-one")
        .expect("terminal tab retained beside conversation");
    cx.simulate_click(terminal.center(), Modifiers::none());
    cx.run_until_parked();
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().last(),
            Some(&ChromeIntent::Status(StatusIntent::Context {
                session_id: "session-1".to_owned(),
                window_id: "window-1".to_owned(),
                action: bootty_gpui::TabContextAction::Activate,
            }))
        );
    });
}

#[gpui_kit::test]
fn pinned_partition_and_recent_receipts_preserve_manual_ties_and_row_heights(
    cx: &mut TestAppContext,
) {
    use bootty_config::config::SidebarSortOrder;
    use bootty_mux::session_membership::SessionState;
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    for (order, expected) in [
        (SidebarSortOrder::Manual, [3, 0, 1, 2]),
        (SidebarSortOrder::RecentActivity, [3, 1, 2, 0]),
    ] {
        let mut snapshot = chrome_snapshot();
        let sidebar = snapshot.sidebar.as_mut().unwrap();
        fill_sidebar_sessions(sidebar);
        sidebar.rows.truncate(12);
        sidebar.sort_order = order;
        for (index, row) in sidebar
            .rows
            .iter_mut()
            .filter(|row| row.kind.is_session())
            .enumerate()
        {
            row.task = Some(saved_task(SessionState {
                pinned: index == 3,
                last_activity_at: matches!(index, 1 | 2).then_some(100),
                ..SessionState::default()
            }));
        }
        let (probe, cx) =
            cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
        let selectors = [
            "sidebar-row-session-0",
            "sidebar-row-session-1",
            "sidebar-row-session-2",
            "sidebar-row-session-3",
        ];
        let bounds = selectors.map(|selector| cx.debug_bounds(selector).unwrap());
        for pair in expected.windows(2) {
            assert!(bounds[pair[0]].origin.y < bounds[pair[1]].origin.y);
            assert_eq!(bounds[pair[0]].size.height, bounds[pair[1]].size.height);
        }
        assert!(cx.debug_bounds("sidebar-pinned-section").is_some());
        assert!(cx.debug_bounds("sidebar-sessions-section").is_some());
        probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
    }
}

#[gpui_kit::test]
fn attention_filter_uses_observed_attention_and_keeps_other_states_out(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    let sidebar = snapshot.sidebar.as_mut().unwrap();
    fill_sidebar_sessions(sidebar);
    sidebar.rows.truncate(21);
    for (row, (status, attention)) in sidebar
        .rows
        .iter_mut()
        .filter(|row| row.kind.is_session())
        .zip([
            ("Working 0:10", false),
            ("Approval", true),
            ("Input", true),
            ("Failed", true),
            ("Finished", false),
            ("Woke", false),
            ("Waiting", false),
        ])
    {
        row.trailing = Some(status.to_owned());
        row.needs_attention = attention;
    }
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    choose_task_view(5, cx);
    for (index, selector) in [
        "sidebar-row-session-0",
        "sidebar-row-session-1",
        "sidebar-row-session-2",
        "sidebar-row-session-3",
        "sidebar-row-session-4",
        "sidebar-row-session-5",
        "sidebar-row-session-6",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            cx.debug_bounds(selector).is_some(),
            matches!(index, 1..=3),
            "row {index}"
        );
    }
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn sort_submenu_emits_the_shared_sort_command(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(ChromeProbe::new);
    let options = cx.debug_bounds("sidebar-view").unwrap();
    cx.simulate_click(options.center(), Modifiers::none());
    draw_chrome(cx);
    cx.simulate_keystrokes("down down down right");
    draw_chrome(cx);
    cx.simulate_keystrokes("down enter");
    draw_chrome(cx);
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.intents.borrow().as_slice(),
            [ChromeIntent::Command(
                bootty_control::CommandInvocation::from_action(
                    "ui.sidebar.sort_recent_activity",
                    bootty_control::Caller::Internal
                )
            )]
        );
    });
}

#[gpui_kit::test]
fn dragging_moves_peer_pills_before_drop_and_escape_restores_without_writes(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    let sidebar = snapshot.sidebar.as_mut().unwrap();
    fill_sidebar_sessions(sidebar);
    sidebar.rows.truncate(9);
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let search = cx.debug_bounds("sidebar-search").unwrap();
    cx.simulate_click(search.center(), Modifiers::none());
    draw_chrome(cx);
    let previous_focus = cx.update(|window, cx| window.focused(cx).expect("focused search input"));
    let peer_before = cx.debug_bounds("sidebar-row-session-0").unwrap();
    let start = cx
        .debug_bounds("sidebar-row-hitbox-session-2")
        .unwrap()
        .center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(start.x.add(px(15.0)), start.y.add(px(15.0))),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    draw_chrome(cx);
    cx.simulate_mouse_move(
        peer_before.center(),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    draw_chrome(cx);
    let peer_after = cx.debug_bounds("sidebar-row-session-0").unwrap();
    assert!(peer_after.origin.y > peer_before.origin.y);
    assert_eq!(peer_after.size.height, peer_before.size.height);
    let preview = cx
        .debug_bounds("sidebar-drag-preview")
        .expect("actual pill preview");
    assert_eq!(preview.size.height, peer_before.size.height);
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
    cx.simulate_keystrokes("escape");
    draw_chrome(cx);
    assert_eq!(
        cx.debug_bounds("sidebar-row-session-0").unwrap(),
        peer_before
    );
    assert!(cx.debug_bounds("sidebar-drag-preview").is_none());
    cx.update(|window, _| {
        assert!(
            previous_focus.is_focused(window),
            "Escape restores the real previous input focus"
        );
    });
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn archived_and_snoozed_rows_keep_latent_pin_without_a_pinned_partition(cx: &mut TestAppContext) {
    use bootty_mux::session_membership::SessionState;
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    for (state, view) in [
        (
            SessionState {
                archived: true,
                pinned: true,
                ..SessionState::default()
            },
            2,
        ),
        (
            SessionState {
                snoozed_until: Some(1_100),
                pinned: true,
                ..SessionState::default()
            },
            3,
        ),
    ] {
        let snapshot = saved_task_snapshot(state);
        assert!(
            snapshot.sidebar.as_ref().unwrap().rows[0]
                .task
                .as_ref()
                .unwrap()
                .state
                .pinned
        );
        let (probe, cx) =
            cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
        choose_task_view(view, cx);
        assert!(cx.debug_bounds("sidebar-row-session").is_some());
        assert!(cx.debug_bounds("sidebar-pinned-section").is_none());
        probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
    }
}

#[gpui_kit::test]
fn attention_filter_without_observed_attention_renders_the_empty_state(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    let sidebar = snapshot.sidebar.as_mut().unwrap();
    sidebar.rows.truncate(1);
    sidebar.rows[0].trailing = Some("Finished".to_owned());
    sidebar.rows[0].needs_attention = false;
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    assert!(cx.debug_bounds("sidebar-row-session").is_some());
    choose_task_view(5, cx);
    assert!(cx.debug_bounds("sidebar-row-session").is_none());
    assert!(cx.debug_bounds("sidebar-empty-state").is_some());
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn first_session_in_a_project_drags_its_status_branch_and_agents(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    let sidebar = snapshot.sidebar.as_mut().unwrap();
    sidebar.rows.truncate(1);
    let mut session = sidebar.rows[0].clone();
    session.key = "grouped-source".to_owned();
    session.text = "Actual task".to_owned();
    session.branch = Some("feature".to_owned());
    session.trailing = Some("Input".to_owned());
    session.trailing_icon = Some("message-circle-question-mark".to_owned());
    session.agents = ["claude", "openai"]
        .into_iter()
        .enumerate()
        .map(|(index, icon)| SidebarAgent {
            key: format!("grouped-agent-{index}"),
            icon: icon.to_owned(),
            description: icon.to_owned(),
        })
        .collect();
    let mut group = session.clone();
    group.key = "project-header".to_owned();
    group.text = "Project".to_owned();
    group.kind = SidebarRowKind::Group;
    group.branch = None;
    group.trailing = None;
    group.agents.clear();
    group.target = None;
    group.current = false;
    group.task = None;
    sidebar.rows = vec![group, session];
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
    let source = cx.debug_bounds("sidebar-row-grouped-source").unwrap();
    let start = cx
        .debug_bounds("sidebar-row-hitbox-grouped-source")
        .unwrap()
        .center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(start.x.add(px(15.0)), start.y.add(px(15.0))),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    draw_chrome(cx);
    let preview = cx.debug_bounds("sidebar-drag-preview").unwrap();
    assert_eq!(preview.size.height, source.size.height);
    for selector in [
        "sidebar-title-grouped-source",
        "sidebar-branch-grouped-source",
        "sidebar-status-grouped-source",
        "sidebar-agent-grouped-agent-0",
        "sidebar-agent-grouped-agent-1",
    ] {
        assert!(
            preview.contains(
                &cx.debug_bounds(selector)
                    .expect("actual pill content")
                    .center()
            ),
            "{selector}"
        );
    }
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
    cx.simulate_keystrokes("escape");
    draw_chrome(cx);
    assert!(cx.debug_bounds("sidebar-drag-preview").is_none());
}

#[gpui_kit::test]
fn detached_pills_keep_distinct_colors_and_unselected_working_geometry(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = task_row_snapshot(false);
    let sidebar = snapshot.sidebar.as_mut().unwrap();
    sidebar.rows.truncate(2);
    for (row, tint) in sidebar
        .rows
        .iter_mut()
        .zip([color(240, 100, 80), color(80, 180, 240)])
    {
        row.current = false;
        row.active = false;
        row.color = tint;
    }
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    draw_chrome(cx);
    let working = cx.debug_bounds("sidebar-row-task-0").unwrap();
    let status = cx.debug_bounds("sidebar-status-task-0").unwrap();
    let agent = cx.debug_bounds("sidebar-agent-task-0-0").unwrap();
    let pill_bounds = [
        cx.debug_bounds("sidebar-session-task-0").unwrap(),
        cx.debug_bounds("sidebar-session-task-1").unwrap(),
    ];
    let attached_colors = cx.update(|window, _| {
        let quads = window.painted_quads();
        pill_bounds.map(|bounds| {
            quads
                .iter()
                .find(|quad| quad.bounds == bounds.scale(window.scale_factor()))
                .map(|quad| quad.background)
        })
    });
    assert!(
        attached_colors.iter().all(Option::is_some),
        "both pills must be painted"
    );
    assert_ne!(
        attached_colors[0], attached_colors[1],
        "unselected pills retain their task colors"
    );
    for row in &mut snapshot.sidebar.as_mut().unwrap().rows {
        row.kind = SidebarRowKind::DetachedSession;
    }
    update_chrome(&probe, &snapshot, cx);
    assert_eq!(cx.debug_bounds("sidebar-row-task-0"), Some(working));
    assert_eq!(cx.debug_bounds("sidebar-status-task-0"), Some(status));
    assert_eq!(cx.debug_bounds("sidebar-agent-task-0-0"), Some(agent));
    let detached_colors = cx.update(|window, _| {
        let quads = window.painted_quads();
        pill_bounds.map(|bounds| {
            quads
                .iter()
                .find(|quad| quad.bounds == bounds.scale(window.scale_factor()))
                .map(|quad| quad.background)
        })
    });
    assert_eq!(detached_colors, attached_colors);
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

#[gpui_kit::test]
fn unassigned_backend_rows_stay_visible_and_adopt_exact_target(cx: &mut TestAppContext) {
    use bootty_config::config::SidebarSortOrder;
    for (grouped, sort_order) in [false, true].into_iter().flat_map(|grouped| {
        [SidebarSortOrder::Manual, SidebarSortOrder::RecentActivity].map(|order| (grouped, order))
    }) {
        cx.update(|cx| init_theme(UiPalette::default(), cx));
        let mut snapshot = chrome_snapshot();
        let sidebar = snapshot.sidebar.as_mut().unwrap();
        sidebar.group_by_project = grouped;
        sidebar.sort_order = sort_order;
        let mut unassigned = sidebar.rows[0].clone();
        unassigned.key = "unassigned-terminal".to_owned();
        unassigned.text = "Existing backend shell".to_owned();
        unassigned.kind = SidebarRowKind::Other("unassigned".to_owned());
        unassigned.reorder_anchor = None;
        unassigned.context = None;
        unassigned.task = None;
        unassigned.current = false;
        unassigned.active = false;
        unassigned.target = Some(target());
        let mut group = unassigned.clone();
        group.key = "unassigned-group".to_owned();
        group.text = "Unassigned".to_owned();
        group.kind = SidebarRowKind::Group;
        group.selectable = false;
        group.target = None;
        sidebar.rows = vec![group, unassigned];
        let (probe, cx) =
            cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
        draw_chrome(cx);
        assert!(cx.debug_bounds("sidebar-empty-state").is_none());
        assert!(cx.debug_bounds("sidebar-row-unassigned-group").is_some());
        assert!(cx.debug_bounds("sidebar-pinned-section").is_none());
        let row = cx.debug_bounds("sidebar-row-unassigned-terminal").unwrap();
        cx.simulate_click(row.center(), Modifiers::none());
        draw_chrome(cx);
        probe.update(cx, |probe, _| {
            assert_eq!(
                probe.intents.borrow().as_slice(),
                [ChromeIntent::AdoptSession(target())]
            );
            probe.intents.borrow_mut().clear();
        });
        choose_task_view(2, cx);
        assert!(cx.debug_bounds("sidebar-row-unassigned-terminal").is_none());
        assert!(cx.debug_bounds("sidebar-row-unassigned-group").is_none());
        assert!(cx.debug_bounds("sidebar-empty-state").is_some());
        probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
    }
}

struct PopupShortcutProbe {
    chrome: Entity<GpuiChrome>,
    palette_requests: Rc<RefCell<usize>>,
}

impl Render for PopupShortcutProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .key_context(bootty_ui::gpui_actions::WORKSPACE_KEY_CONTEXT)
            .on_action(cx.listener(
                |this, action: &bootty_ui::gpui_actions::InvokeCommand, _, cx| {
                    if action.invocation().command == "command_palette" {
                        let mut requests = this.palette_requests.borrow_mut();
                        *requests = requests.saturating_add(1);
                    }
                    cx.stop_propagation();
                },
            ))
            .child(self.chrome.clone())
    }
}

#[gpui_kit::test]
fn sidebar_popup_escape_keeps_palette_shortcuts_and_outside_input_focus(cx: &mut TestAppContext) {
    cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        let input = bootty_config::config::InputConfig {
            keybind: vec![
                "cmd+k=command_palette".into(),
                "ctrl+k=command_palette".into(),
            ],
            ..Default::default()
        };
        cx.bind_keys(bootty_ui::gpui_actions::key_bindings(&input, cx).unwrap());
    });
    let palette_requests = Rc::new(RefCell::new(0));
    let received = Rc::clone(&palette_requests);
    let (probe, cx) = cx.add_window_view(|window, cx| PopupShortcutProbe {
        chrome: cx.new(|cx| GpuiChrome::new(chrome_snapshot(), window, cx)),
        palette_requests: received,
    });
    let focus = cx.update(|window, cx| {
        let focus = probe.read(cx).chrome.focus_handle(cx);
        focus.focus(window, cx);
        focus
    });
    let palette_key = if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    };
    for escape in ["escape", "down right escape"] {
        let button = cx.debug_bounds("sidebar-view").expect("session options");
        cx.simulate_click(center(button), Modifiers::none());
        cx.run_until_parked();
        draw_chrome(cx);
        cx.simulate_keystrokes(escape);
        cx.run_until_parked();
        draw_chrome(cx);
        assert!(cx.update(|window, cx| focus.contains_focused(window, cx)));
        cx.simulate_keystrokes(palette_key);
        cx.simulate_keystrokes(palette_key);
    }
    assert_eq!(*palette_requests.borrow(), 4);

    let button = cx.debug_bounds("sidebar-view").expect("session options");
    cx.simulate_click(center(button), Modifiers::none());
    cx.run_until_parked();
    draw_chrome(cx);
    let search = cx.debug_bounds("sidebar-search").expect("session search");
    cx.simulate_click(center(search), Modifiers::none());
    cx.run_until_parked();
    draw_chrome(cx);
    cx.simulate_input("retain focus");
    cx.run_until_parked();
    assert!(!cx.update(|window, _| focus.is_focused(window)));
    assert!(cx.debug_bounds("sidebar-row-session").is_none());
    cx.simulate_keystrokes(palette_key);
    assert_eq!(*palette_requests.borrow(), 5);
}

#[gpui_kit::test]
fn settled_sessions_remain_visible_below_active_and_pinned_sessions(cx: &mut TestAppContext) {
    use bootty_config::config::SidebarSortOrder;
    use bootty_mux::session_membership::{SessionLifecycle, SessionState};
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    for grouped in [false, true] {
        for order in [SidebarSortOrder::Manual, SidebarSortOrder::RecentActivity] {
            let mut snapshot = chrome_snapshot();
            let sidebar = snapshot.sidebar.as_mut().unwrap();
            fill_sidebar_sessions(sidebar);
            sidebar.rows.truncate(12);
            sidebar.group_by_project = grouped;
            sidebar.sort_order = order;
            for (index, row) in sidebar
                .rows
                .iter_mut()
                .filter(|row| row.kind.is_session())
                .enumerate()
            {
                row.task = Some(saved_task(SessionState {
                    lifecycle: if index == 0 {
                        SessionLifecycle::Settled
                    } else {
                        SessionLifecycle::Active
                    },
                    pinned: index == 2,
                    last_activity_at: Some(100),
                    ..SessionState::default()
                }));
            }
            let (probe, cx) =
                cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot, window, cx));
            let settled = cx
                .debug_bounds("sidebar-row-session-0")
                .expect("settled visible by default");
            let heading = cx.debug_bounds("sidebar-settled-section").unwrap();
            let pinned = cx.debug_bounds("sidebar-row-session-2").unwrap();
            let active = cx.debug_bounds("sidebar-row-session-1").unwrap();
            assert!(pinned.bottom() <= active.top());
            assert!(active.bottom() < heading.top());
            assert!(heading.bottom() <= settled.top());
            let footer = cx.debug_bounds("bootty-gpui-space-switcher").unwrap();
            assert!(
                (footer.top().sub(settled.bottom())).abs() <= px(20.0),
                "settled tail should sit above the footer: {settled:?}, {footer:?}"
            );
            probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
        }
    }
}

#[gpui_kit::test]
fn project_disclosure_hides_sessions_and_empty_projects_remain_operable(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    let sidebar = snapshot.sidebar.as_mut().expect("sidebar");
    sidebar.group_by_project = true;
    let mut session = sidebar.rows[0].clone();
    session.project_path = Some("/work/project".to_owned());
    let mut group = session.clone();
    group.key = "registered-project".to_owned();
    group.text = "project".to_owned();
    group.kind = SidebarRowKind::Group;
    group.target = None;
    group.task = None;
    group.selectable = false;
    group.current = false;
    let mut empty = group.clone();
    empty.key = "empty-project".to_owned();
    empty.text = "empty".to_owned();
    empty.project_path = Some("/work/empty".to_owned());
    sidebar.rows = vec![group, session, empty];
    sidebar.projects = vec![bootty_mux::repository::RegisteredProject {
        scope: bootty_mux::controller::SpaceId::from_persistence(1),
        cwd: "/work/project".to_owned(),
        collapsed: true,
        settings: bootty_mux::repository::ProjectSettings::default(),
    }];
    let target = saved_task(bootty_mux::session_membership::SessionState::default())
        .binding
        .unwrap();
    sidebar.project_target = Some(target.clone());
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    assert!(cx.debug_bounds("sidebar-row-session").is_none());
    let header = cx
        .debug_bounds("sidebar-row-registered-project")
        .expect("collapsed header");
    assert!(cx.debug_bounds("sidebar-row-empty-project").is_some());
    cx.simulate_click(header.center(), Modifiers::none());
    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        let [ChromeIntent::Command(invocation)] = intents.as_slice() else {
            panic!("one project command: {intents:?}")
        };
        assert_eq!(invocation.command, "project.toggle_collapsed");
        assert_eq!(invocation.arguments, ["/work/project"]);
        assert_eq!(invocation.target, Some(target));
    });
    snapshot.sidebar.as_mut().unwrap().projects[0].collapsed = false;
    update_chrome(&probe, &snapshot, cx);
    assert!(cx.debug_bounds("sidebar-row-session").is_some());
}

#[gpui_kit::test]
fn permanent_session_deletion_requires_the_visible_confirmation(cx: &mut TestAppContext) {
    use bootty_mux::session_membership::SessionState;
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let holder = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = holder.clone();
    let (_, visual) = cx.add_window_view(move |window, cx| {
        let probe = cx.new(|cx| {
            ChromeProbe::with_snapshot(saved_task_snapshot(SessionState::default()), window, cx)
        });
        *capture.borrow_mut() = Some(probe.clone());
        gpui_kit::component::Root::new(probe, window, cx)
    });
    let probe = holder.borrow().clone().unwrap();
    let row = visual.debug_bounds("sidebar-row-session").unwrap();
    visual.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::none());
    draw_chrome(visual);
    for _ in 0..8 {
        visual.simulate_keystrokes("down");
    }
    visual.simulate_keystrokes("enter");
    draw_chrome(visual);
    probe.update(visual, |probe, _| {
        assert!(probe.intents.borrow().is_empty());
    });
    let cancel = visual
        .debug_bounds("prompt-answer-Cancel")
        .expect("Cancel confirmation");
    visual.simulate_click(cancel.center(), Modifiers::none());
    draw_chrome(visual);
    probe.update(visual, |probe, _| {
        assert!(probe.intents.borrow().is_empty());
    });
}
