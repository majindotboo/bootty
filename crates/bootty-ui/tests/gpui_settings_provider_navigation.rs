#![cfg(test)]

use bootty_ui::gpui::{
    ScalarValue, SettingsCategory, SettingsControl, SettingsPage, SettingsPageItem, SettingsRow,
    ToggleFocusNav, UiPalette, init_theme,
};
use gpui_kit::{Action as _, Modifiers, TestAppContext, VisualTestContext, point, px};
use settings_support::{GpuiSettingsSnapshot, SettingsProbe};

#[path = "support/settings.rs"]
mod settings_support;

const PROVIDER_NAVIGATION_CLICK_CASES: [(&str, &str, &str, &str, &str); 2] = [
    (
        "claude",
        "settings-section-providers:claude",
        "settings-item-agents.claude.program",
        "settings-item-agents.pi.program",
        "settings-active-section-providers:claude",
    ),
    (
        "codex",
        "settings-section-providers:codex",
        "settings-item-agents.codex.program",
        "settings-item-agents.claude.program",
        "settings-active-section-providers:codex",
    ),
];

fn init_settings_ui(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}

#[gpui_kit::test]
fn provider_subsections_select_and_expand_their_matching_cards(cx: &mut TestAppContext) {
    init_settings_ui(cx);
    let (probe, cx) =
        cx.add_window_view(|_, cx| SettingsProbe::with_snapshot(provider_snapshot(), cx));
    cx.update(|window, app| {
        probe.update(app, |probe, app| {
            probe
                .settings
                .update(app, |settings, app| settings.focus(window, app));
        });
    });
    cx.update(|window, app| window.dispatch_action(ToggleFocusNav.boxed_clone(), app));
    for _ in 0..3 {
        cx.refresh().expect("focus settings navigation");
    }

    assert_provider_root_selected(cx);

    let disclosure = "settings-disclosure-settings-category-providers";
    let bounds = cx.debug_bounds(disclosure).expect("Providers disclosure");
    cx.simulate_click(bounds_center(bounds), Modifiers::none());
    cx.refresh().expect("collapse provider subsections");
    assert!(cx.debug_bounds("settings-section-providers:pi").is_none());
    assert!(
        cx.debug_bounds("settings-active-category-providers")
            .is_some()
    );
    let bounds = cx.debug_bounds(disclosure).expect("collapsed disclosure");
    cx.simulate_click(bounds_center(bounds), Modifiers::none());
    cx.refresh().expect("expand provider subsections");
    assert_provider_root_selected(cx);

    // The providers follow their real page order: Pi, Codex, then Claude.
    cx.simulate_keystrokes("down enter");
    cx.refresh().expect("render expanded Pi settings");
    assert!(cx.debug_bounds("settings-item-agents.pi.program").is_some());
    assert!(
        cx.debug_bounds("settings-item-agents.codex.program")
            .is_none()
    );
    assert!(
        cx.debug_bounds("settings-active-section-providers:pi")
            .is_some()
    );
    assert!(
        cx.debug_bounds("settings-active-category-providers")
            .is_none(),
        "selecting a provider subsection moves selection off the Providers root"
    );

    for (provider, selector, program, prior_program, active) in PROVIDER_NAVIGATION_CLICK_CASES {
        let subsection = cx.debug_bounds(selector).expect("provider subsection");
        cx.simulate_click(bounds_center(subsection), Modifiers::none());
        cx.refresh().expect("render selected provider settings");

        assert!(
            cx.debug_bounds(program).is_some(),
            "the selected {provider} card is expanded"
        );
        assert!(
            cx.debug_bounds(prior_program).is_some(),
            "selecting {provider} keeps the previously expanded card open"
        );
        assert!(
            cx.debug_bounds(active).is_some(),
            "the selected provider subsection is visible"
        );
    }
}

fn assert_provider_root_selected(cx: &mut VisualTestContext) {
    for (provider, selector) in [
        ("pi", "settings-section-providers:pi"),
        ("codex", "settings-section-providers:codex"),
        ("claude", "settings-section-providers:claude"),
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "the {provider} provider has a sidebar subsection"
        );
    }
    for (provider, active) in [
        ("pi", "settings-active-section-providers:pi"),
        ("codex", "settings-active-section-providers:codex"),
        ("claude", "settings-active-section-providers:claude"),
    ] {
        assert!(
            cx.debug_bounds(active).is_none(),
            "the {provider} subsection is not selected at the category root"
        );
    }
    assert!(
        cx.debug_bounds("settings-active-category-providers")
            .is_some()
    );
    if let Some(tree) = cx.update(|window, _| window.debug_a11y_tree_json()) {
        for label in ["Pi", "Codex", "Claude"] {
            assert!(tree.contains(&format!(r#""label": "{label}""#)));
        }
    }
}

fn bounds_center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(width.mul_add(0.5, left)), px(height.mul_add(0.5, top)))
}

fn provider_snapshot() -> GpuiSettingsSnapshot {
    let items = bootty_agents::AgentKind::ALL
        .into_iter()
        .map(|provider| {
            let id = provider.to_string();
            let label = match provider {
                bootty_agents::AgentKind::Pi => "Pi",
                bootty_agents::AgentKind::Codex => "Codex",
                bootty_agents::AgentKind::Claude => "Claude",
            };
            SettingsPageItem::Dependent {
                parent: SettingsRow::Value {
                    id: format!("agents.{id}.enabled"),
                    label: label.to_owned(),
                    help: "Installed · Account not verified".to_owned(),
                    value: ScalarValue::Bool(true),
                    control: SettingsControl::Toggle,
                    enabled: true,
                },
                children: vec![SettingsRow::Value {
                    id: format!("agents.{id}.program"),
                    label: "Executable".to_owned(),
                    help: "Provider executable".to_owned(),
                    value: ScalarValue::Text(String::new()),
                    control: SettingsControl::Text {
                        placeholder: id,
                        optional: true,
                    },
                    enabled: true,
                }],
            }
        })
        .collect();

    GpuiSettingsSnapshot {
        category: SettingsCategory::Providers,
        pages: vec![SettingsPage {
            category: SettingsCategory::Providers,
            title: "Providers".to_owned(),
            search_terms: "agents accounts".to_owned(),
            items,
        }],
        search: String::new(),
        write_error: None,
    }
}
