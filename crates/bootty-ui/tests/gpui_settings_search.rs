#![cfg(test)]

use settings_support::{GpuiSettingsSnapshot, SettingsProbe};
#[path = "support/settings.rs"]
mod settings_support;
use bootty_ui::gpui::{
    ScalarValue, SettingsCategory, SettingsControl, SettingsPage, SettingsPageItem, SettingsRow,
    UiPalette, init_theme,
};
use gpui_kit::{Modifiers, TestAppContext, point, px};

#[gpui_kit::test]
fn retained_search_input_filters_settings_without_an_activation_shell(cx: &mut TestAppContext) {
    initialize(cx);
    let (_, window) = cx.add_window_view(|_, cx| {
        SettingsProbe::with_snapshot(dependent_snapshot("", active_children()), cx)
    });
    let command = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    let search = window
        .debug_bounds("settings-search")
        .expect("component search input");
    window.simulate_click(bounds_center(search), Modifiers::none());
    window.simulate_input("certificate pinning");
    assert!(
        window
            .debug_bounds("settings-content-section-network")
            .is_none(),
        "an inactive child cannot satisfy a query entered through the retained input"
    );

    window.simulate_keystrokes(&format!("{command}-a"));
    window.simulate_input("bastion");
    assert!(
        window
            .debug_bounds("settings-dependent-child-remotes.transport-remotes.proxy")
            .is_some(),
        "component input changes immediately reproject matching settings"
    );
}

#[gpui_kit::test]
fn dependent_search_indexes_parent_and_active_children_with_page_and_section_context(
    cx: &mut TestAppContext,
) {
    initialize(cx);

    for query in ["discriminator", "bastion", "remotes network bastion"] {
        let (_, window) = cx.add_window_view(|_, cx| {
            SettingsProbe::with_snapshot(dependent_snapshot(query, active_children()), cx)
        });

        for selector in [
            "settings-content-section-network",
            "settings-dependent-remotes.transport",
            "settings-dependent-parent-remotes.transport",
            "settings-dependent-child-remotes.transport-remotes.proxy",
        ] {
            assert!(
                window.debug_bounds(selector).is_some(),
                "query {query:?} keeps the dependent group and its section visible: {selector}"
            );
        }
    }
}

#[gpui_kit::test]
fn dependent_search_does_not_index_inactive_children(cx: &mut TestAppContext) {
    initialize(cx);
    let (_, window) = cx.add_window_view(|_, cx| {
        SettingsProbe::with_snapshot(
            dependent_snapshot("certificate pinning", active_children()),
            cx,
        )
    });

    for selector in [
        "settings-content-section-network",
        "settings-dependent-remotes.transport",
        "settings-dependent-parent-remotes.transport",
        "settings-dependent-child-remotes.transport-remotes.proxy",
    ] {
        assert!(
            window.debug_bounds(selector).is_none(),
            "an unprojected child cannot make the dependent group visible: {selector}"
        );
    }
}

fn initialize(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}

fn bounds_center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + width / 2.0), px(top + height / 2.0))
}

fn dependent_snapshot(search: &str, children: Vec<SettingsRow>) -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Remotes,
        pages: vec![SettingsPage {
            category: SettingsCategory::Remotes,
            title: "Remotes".to_owned(),
            search_terms: "remote connections".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "network".to_owned(),
                    title: "SSH Network".to_owned(),
                    search_terms: "network transport".to_owned(),
                },
                SettingsPageItem::Dependent {
                    parent: toggle(
                        "remotes.transport",
                        "Connection discriminator",
                        "Choose the active transport mode.",
                    ),
                    children,
                },
            ],
        }],
        search: search.to_owned(),
        write_error: None,
    }
}

fn active_children() -> Vec<SettingsRow> {
    vec![toggle(
        "remotes.proxy",
        "Proxy tunnel",
        "Route SSH through a bastion host.",
    )]
}

fn toggle(id: &str, label: &str, help: &str) -> SettingsRow {
    SettingsRow::Value {
        id: id.to_owned(),
        label: label.to_owned(),
        help: help.to_owned(),
        value: ScalarValue::Bool(false),
        control: SettingsControl::Toggle,
        enabled: true,
    }
}
