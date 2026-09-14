#![cfg(test)]

use bootty_ui::gpui::{
    ModuleIntegrationSnapshot, ModuleIntegrationStatus, ModuleIntegrationsSnapshot,
    ModuleSourceIntent, SettingsCategory, SettingsIntent, SettingsPage, SettingsPageItem,
    SettingsRow, UiPalette, init_theme,
};
use gpui_kit::{Context, Modifiers, TestAppContext, point, px};
use settings_support::GpuiSettingsSnapshot;

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as IntegrationsProbe;

impl IntegrationsProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(snapshot(), cx)
    }
}

#[gpui_kit::test]
fn native_integration_controls_preserve_install_intent_without_source_controls(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| IntegrationsProbe::new(cx));

    assert!(
        cx.debug_bounds("module-integration-agents.pi-extension")
            .is_some()
    );
    assert!(cx.debug_bounds("module-source-editor").is_none());
    assert!(cx.debug_bounds("module-preview-agents.pi").is_none());
    assert!(cx.debug_bounds("settings-module-new").is_none());

    let button = cx
        .debug_bounds("module-integration-agents.pi-extension")
        .expect("integration action");
    let left: f32 = button.origin.x.into();
    let top: f32 = button.origin.y.into();
    let width: f32 = button.size.width.into();
    let height: f32 = button.size.height.into();
    cx.simulate_click(
        point(px(left + width / 2.0), px(top + height / 2.0)),
        Modifiers::none(),
    );

    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            SettingsIntent::Module(ModuleSourceIntent::InstallIntegration {
                identity,
                module,
                id,
            }) if identity == "agents.pi" && module == "agents.pi" && id == "extension"
        )));
    });
}

fn snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Advanced,
        pages: vec![SettingsPage {
            category: SettingsCategory::Advanced,
            title: "Advanced".to_owned(),
            search_terms: "integrations".to_owned(),
            items: vec![SettingsPageItem::Setting(SettingsRow::ModuleIntegrations(
                ModuleIntegrationsSnapshot {
                    identity: "agents.pi".to_owned(),
                    error: None,
                    integrations: vec![ModuleIntegrationSnapshot {
                        module: "agents.pi".to_owned(),
                        id: "extension".to_owned(),
                        title: "Pi extension".to_owned(),
                        summary: "Install the Pi adapter.".to_owned(),
                        status: ModuleIntegrationStatus::Missing,
                    }],
                },
            ))],
        }],
        search: String::new(),
        write_error: None,
    }
}
