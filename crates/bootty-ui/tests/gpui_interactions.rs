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
    SessionContextSnapshot, SessionTarget, SidebarFooterItem, SidebarPosition, SidebarRow,
    SidebarRowKind, SidebarSnapshot, SpaceKey, SpaceSnapshot, StatusAlignment, StatusBarSnapshot,
    StatusIntent, StatusItemSnapshot, StatusSegmentSnapshot, TabContextSnapshot, UiPalette,
    UsageMeterSnapshot, init_theme,
};
use bootty_ui::usage::{UsageProvider, UsageWindow};
use gpui_kit::component::{Root, WindowExt as _};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, Modifiers, MouseButton,
    MouseUpEvent, Render, ScrollDelta, ScrollWheelEvent, Styled, Subscription, TestAppContext,
    TouchPhase, VisualTestContext, Window, div, point, prelude::*, px,
};
use pretty_assertions::assert_eq;

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
        dock_tabs: bootty_config::config::ChromeConfig::default().dock_tabs,
        terminal_tabs: bootty_config::config::ChromeConfig::default().terminal_tabs,
        width: 900.0,
        height: 600.0,
        sidebar_position: SidebarPosition::Left,
        sidebar_width: 240.0,
        gap: 1.0,
        top_inset: 0.0,
        titlebar_height: 32.0,
        status_height: 28.0,
        sidebar_visible: true,
        titlebar_visible: true,
        fullscreen: false,
    }
}

fn sidebar_snapshot(target: SessionTarget) -> SidebarSnapshot {
    SidebarSnapshot {
        rows: vec![
            SidebarRow {
                key: "session".to_owned(),
                text: "Session one".to_owned(),
                trailing: None,
                trailing_color: None,
                trailing_shimmer: false,
                number: Some(1),
                indent: 0,
                tree: None,
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
                context: Some(SessionContextSnapshot {
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
                trailing: None,
                trailing_color: None,
                trailing_shimmer: false,
                number: None,
                indent: 2,
                tree: None,
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
            },
        ],
        footer: Vec::new(),
        title_visible: true,
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
    assert_eq!(session.size.width, px(240.0));
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
            let target = row.target.clone();
            let mut cwd = detail_template.clone();
            cwd.key = format!("session-{index}:cwd");
            cwd.text = format!("/workspaces/session-{index}");
            cwd.target.clone_from(&target);
            cwd.current = row.current;
            let mut branch = detail_template.clone();
            branch.key = format!("session-{index}:branch");
            branch.text = format!("main-{index}");
            branch.target = target;
            branch.current = row.current;
            [row, cwd, branch]
        })
        .collect();
}

#[gpui_kit::test]
fn switching_sessions_reveals_the_row_without_capturing_manual_scroll(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let mut snapshot = chrome_snapshot();
    fill_sidebar_sessions(snapshot.sidebar.as_mut().expect("sidebar"));
    let (probe, cx) =
        cx.add_window_view(|window, cx| ChromeProbe::with_snapshot(snapshot.clone(), window, cx));
    for (selected, selectors) in [
        (
            39,
            [
                "sidebar-row-session-39",
                "sidebar-row-session-39:cwd",
                "sidebar-row-session-39:branch",
            ],
        ),
        (
            0,
            [
                "sidebar-row-session-0",
                "sidebar-row-session-0:cwd",
                "sidebar-row-session-0:branch",
            ],
        ),
        (
            25,
            [
                "sidebar-row-session-25",
                "sidebar-row-session-25:cwd",
                "sidebar-row-session-25:branch",
            ],
        ),
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
                "selected session entry row must be above the footer: {selector}"
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
fn sidebar_rows_and_space_switcher_use_the_full_centered_surface(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (_, cx) = cx.add_window_view(ChromeProbe::new);

    let row = cx.debug_bounds("sidebar-row-session").expect("session row");
    let hitbox = cx
        .debug_bounds("sidebar-row-hitbox-session")
        .expect("session row hitbox");
    let rail = cx
        .debug_bounds("sidebar-current-rail-session")
        .expect("current session rail");
    assert_eq!(row.origin.x, px(0.0));
    assert_eq!(row.size.width, px(240.0));
    assert_eq!(hitbox.origin.x, row.origin.x);
    assert_eq!(hitbox.size.width, row.size.width);
    assert_eq!(rail.origin.x, row.origin.x);
    assert_eq!(rail.size.width, px(4.0));
    assert_eq!(rail.size.height, row.size.height);

    let detail = cx
        .debug_bounds("sidebar-row-session:cwd")
        .expect("session detail row");
    let detail_rail = cx
        .debug_bounds("sidebar-current-rail-session:cwd")
        .expect("current session detail rail");
    assert_eq!(detail_rail.origin.x, detail.origin.x);
    assert_eq!(detail_rail.size.width, px(4.0));
    assert_eq!(detail_rail.size.height, detail.size.height);

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

#[gpui_kit::test]
fn quota_rows_keep_labels_above_full_width_meters(cx: &mut TestAppContext) {
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
                label: format!("{label} 23% left"),
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
            for (row, labels, track) in [
                (
                    "sidebar-footer-codex:5h",
                    "sidebar-footer-codex:5h-labels",
                    "sidebar-footer-codex:5h-track",
                ),
                (
                    "sidebar-footer-codex:7d",
                    "sidebar-footer-codex:7d-labels",
                    "sidebar-footer-codex:7d-track",
                ),
            ] {
                let row = cx.debug_bounds(row).expect("quota row");
                let labels = cx.debug_bounds(labels).expect("quota labels");
                let track = cx.debug_bounds(track).expect("quota track");
                assert_eq!(track.left(), row.left());
                assert_eq!(track.right(), row.right());
                assert!(track.top() >= labels.bottom());
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
    // The row surfaces reach the content boundary: metrics paint their bottom edge while the
    // active tab leaves it open. A parent border must not consume a pixel beneath every child.
    assert_eq!(item.bottom(), status.bottom());
    assert_eq!(tab.bottom(), status.bottom());
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
    // The standard menu owns keyboard selection; Rename is the seventh enabled row.
    cx.simulate_keystrokes("down down down down down down down enter");
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
        .can_activate = false;
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
        GpuiPaneWorkspace::new(
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
        )
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layer = Root::render_dialog_layer(window, cx);
        div()
            .size_full()
            .track_focus(&self.background_focus)
            .children(layer)
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
            cx.new(|cx| Root::new(surface, window, cx).bordered(false))
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
        busy: false,
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
        busy: false,
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
        busy: false,
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
                snapshot.layout.terminal_tabs = TabConfig {
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
        snapshot.layout.terminal_tabs.appearance = appearance;
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
