#![cfg(test)]

use bootty_ui::gpui::{
    EnvironmentVariable, SettingsCategory, SettingsIntent, SettingsPage, SettingsPageItem,
    SettingsRow, UiPalette, init_theme,
};
use gpui_kit::{Context, Modifiers, MouseButton, MouseUpEvent, TestAppContext, point, px};
use settings_support::GpuiSettingsSnapshot;

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as EnvironmentProbe;

impl EnvironmentProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(snapshot(), cx)
    }
}

#[gpui_kit::test]
fn environment_uses_distinct_name_and_value_fields_and_typed_add(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| EnvironmentProbe::new(cx));

    assert!(
        cx.debug_bounds("settings-environment-session.env-0-name")
            .is_some(),
        "environment name is a dedicated field"
    );
    assert!(
        cx.debug_bounds("settings-environment-session.env-0-value")
            .is_some(),
        "environment value is a dedicated field"
    );
    let add = cx
        .debug_bounds("settings-environment-session.env-add")
        .expect("add variable button");
    let left: f32 = add.origin.x.into();
    let top: f32 = add.origin.y.into();
    let width: f32 = add.size.width.into();
    let height: f32 = add.size.height.into();
    cx.simulate_click(
        point(px(left + width / 2.0), px(top + height / 2.0)),
        Modifiers::none(),
    );

    probe.update(cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().as_slice(),
            [SettingsIntent::AddEnvironmentVariable]
        ));
    });
}

#[gpui_kit::test]
fn environment_inputs_emit_typed_edits_without_an_activation_shell(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| EnvironmentProbe::new(cx));
    let command = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    let name = cx
        .debug_bounds("settings-environment-session.env-0-name")
        .expect("retained name input");
    cx.simulate_click(bounds_center(name), Modifiers::none());
    cx.simulate_keystrokes(&format!("{command}-a"));
    cx.simulate_input("BOOTTY_TERM");
    probe.update(cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().last(),
            Some(SettingsIntent::SetEnvironmentName { index: 0, value })
                if value == "BOOTTY_TERM"
        ));
    });

    let value = cx
        .debug_bounds("settings-environment-session.env-0-value")
        .expect("retained value input");
    cx.simulate_click(bounds_center(value), Modifiers::none());
    cx.simulate_keystrokes(&format!("{command}-a"));
    cx.simulate_input("screen-256color");
    probe.update(cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().last(),
            Some(SettingsIntent::SetEnvironmentValue { index: 0, value })
                if value == "screen-256color"
        ));
    });
}

#[gpui_kit::test]
fn environment_reorder_controls_have_accessible_names(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (_, cx) = cx.add_window_view(|_, cx| EnvironmentProbe::new(cx));

    for selector in [
        "settings-environment-session.env-0-drag-handle",
        "settings-environment-session.env-0-remove",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "environment action is rendered: {selector}"
        );
    }

    if let Some(tree) = cx.update(|window, _| window.debug_a11y_tree_json()) {
        for label in [
            "Reorder environment variable 0",
            "Remove environment variable 0",
        ] {
            assert!(
                tree.contains(&format!(r#""label": "{label}""#)),
                "environment action exposes its accessible label: {label}"
            );
        }
    }
}

#[gpui_kit::test]
fn environment_end_drop_uses_shared_drag_target_once(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| EnvironmentProbe::new(cx));
    let source = cx
        .debug_bounds("settings-environment-session.env-0-drag-handle")
        .expect("first environment drag handle");
    let end = cx
        .debug_bounds("settings-environment-session.env-2-after")
        .expect("last environment end drop target");
    let start = bounds_center(source);
    let finish = bounds_center(end);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(finish, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_event(MouseUpEvent {
        position: finish,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });

    probe.read_with(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(
            matches!(
                intents.as_slice(),
                [SettingsIntent::MoveEnvironmentVariable {
                    index: 0,
                    offset: 2,
                }]
            ),
            "shared end target should submit one multi-position move: {intents:?}"
        );
    });
}

#[gpui_kit::test]
fn environment_drop_after_snapshot_shrink_is_ignored(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| EnvironmentProbe::new(cx));
    let source = cx
        .debug_bounds("settings-environment-session.env-2-drag-handle")
        .expect("third environment drag handle");
    let start = bounds_center(source);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    let moved_x: f32 = start.x.into();
    cx.simulate_mouse_move(
        point(px(moved_x + 16.0), start.y),
        Some(MouseButton::Left),
        Modifiers::none(),
    );

    let mut refreshed = snapshot();
    if let SettingsPageItem::Setting(SettingsRow::Environment { items, .. }) =
        &mut refreshed.pages[0].items[1]
    {
        items.truncate(1);
    } else {
        panic!("environment setting remains in the test snapshot");
    }
    probe.update(cx, |probe, cx| {
        probe.settings.update(cx, |settings, cx| {
            settings.set_content(refreshed.into_content(), cx)
        });
    });
    cx.run_until_parked();

    let end = cx
        .debug_bounds("settings-environment-session.env-0-after")
        .expect("remaining environment end drop target");
    let finish = bounds_center(end);
    cx.simulate_mouse_move(finish, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_event(MouseUpEvent {
        position: finish,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });

    probe.read_with(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(
            intents.is_empty(),
            "stale drag must not emit a reorder intent: {intents:?}"
        );
    });
}

fn snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Terminal,
        pages: vec![SettingsPage {
            category: SettingsCategory::Terminal,
            title: "Terminal".to_owned(),
            search_terms: "terminal session environment".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "environment".to_owned(),
                    title: "Environment".to_owned(),
                    search_terms: "session environment variables".to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::Environment {
                    id: "session.env".to_owned(),
                    label: "Environment variables".to_owned(),
                    help: "Variables added to every new terminal session.".to_owned(),
                    items: vec![
                        EnvironmentVariable {
                            name: "TERM".to_owned(),
                            value: "xterm-256color".to_owned(),
                        },
                        EnvironmentVariable {
                            name: "PATH".to_owned(),
                            value: "/usr/bin".to_owned(),
                        },
                        EnvironmentVariable {
                            name: "LANG".to_owned(),
                            value: "en_US.UTF-8".to_owned(),
                        },
                    ],
                    enabled: true,
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}

fn bounds_center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(width.mul_add(0.5, left)), px(height.mul_add(0.5, top)))
}
