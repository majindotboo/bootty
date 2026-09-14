#![cfg(test)]

use settings_support::{GpuiSettingsSnapshot, SettingsProbe as DependentProbe};
#[path = "support/settings.rs"]
mod settings_support;
use bootty_ui::gpui::{
    ScalarValue, SettingsCategory, SettingsControl, SettingsPage, SettingsPageItem, SettingsRow,
    UiPalette, init_theme,
};
use gpui_kit::TestAppContext;

#[gpui_kit::test]
fn dependent_rows_keep_zeds_parent_and_indented_child_group(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (_, cx) = cx.add_window_view(|_, cx| DependentProbe::with_snapshot(snapshot(), cx));

    let parent = cx
        .debug_bounds("settings-dependent-parent-dependent.parent")
        .expect("dependent parent uses the normal settings-row layout");
    let child = cx
        .debug_bounds("settings-dependent-child-dependent.parent-dependent.child")
        .expect("dependent child uses Zed's nested setting group");
    let parent_left: f32 = parent.origin.x.into();
    let child_left: f32 = child.origin.x.into();
    let parent_width: f32 = parent.size.width.into();
    let child_width: f32 = child.size.width.into();
    let parent_right = parent_width.mul_add(1.0, parent_left);
    let child_right = child_width.mul_add(1.0, child_left);
    let left_inset = child_left - parent_left;
    let right_inset = parent_right - child_right;

    assert!(
        (left_inset - 32.0).abs() <= 1.0,
        "Zed's mx_8 gives the child group a 32px left inset, got {left_inset}px"
    );
    assert!(
        (right_inset - 32.0).abs() <= 1.0,
        "Zed's mx_8 gives the child group a 32px right inset, got {right_inset}px"
    );
    assert!(
        (left_inset - right_inset).abs() <= 1.0,
        "dependent children keep symmetric parent-relative margins"
    );
    assert!(
        cx.debug_bounds("settings-toggle-dependent.parent")
            .is_some()
            && cx.debug_bounds("settings-toggle-dependent.child").is_some(),
        "parent and dependent child controls both remain interactive"
    );
}

fn snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::General,
        pages: vec![SettingsPage {
            category: SettingsCategory::General,
            title: "General".to_owned(),
            search_terms: "general".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "general".to_owned(),
                    title: "General".to_owned(),
                    search_terms: "general".to_owned(),
                },
                SettingsPageItem::Dependent {
                    parent: toggle("dependent.parent", "Enable dependent settings"),
                    children: vec![toggle("dependent.child", "Child setting")],
                },
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}

fn toggle(id: &str, label: &str) -> SettingsRow {
    SettingsRow::Value {
        id: id.to_owned(),
        label: label.to_owned(),
        help: format!("Help for {label}."),
        value: ScalarValue::Bool(false),
        control: SettingsControl::Toggle,
        enabled: true,
    }
}
