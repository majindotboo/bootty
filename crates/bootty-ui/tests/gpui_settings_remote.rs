#![cfg(test)]

use settings_support::GpuiSettingsSnapshot;
use std::{cell::RefCell, rc::Rc};

use bootty_ui::gpui::{
    RemoteEditorSnapshot, RemoteProfileFieldSnapshot, RemoteProfileSnapshot, RemoteTestState,
    SettingsCategory, SettingsIntent, SettingsPage, SettingsPageItem, SettingsRow, UiPalette,
    init_theme,
};
use gpui_kit::component::{Root, WindowExt as _};
use gpui_kit::{AppContext as _, Context, Modifiers, TestAppContext, point};

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as RemoteProbe;

impl RemoteProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(snapshot(), cx)
    }
}

#[gpui_kit::test]
fn remote_text_fields_use_retained_inputs_and_restore_focus_after_editing(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let probe_slot = Rc::new(RefCell::new(None));
    let opened_probe = Rc::clone(&probe_slot);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), move |window, cx| {
            let probe = cx.new(RemoteProbe::new);
            opened_probe.replace(Some(probe.clone()));
            cx.new(|cx| Root::new(probe, window, cx).bordered(false))
        })
        .expect("open rooted remote settings window")
    });
    let probe = probe_slot
        .borrow_mut()
        .take()
        .expect("capture remote probe");
    let mut cx = gpui_kit::VisualTestContext::from_window(window.into(), cx);

    for selector in [
        "remote-field-profile-host",
        "remote-field-profile-user",
        "remote-field-profile-port",
        "remote-field-profile-program",
        "remote-argument-profile-0",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "missing {selector}");
    }

    click(&mut cx, "remote-field-profile-host");
    replace_focused_text(&mut cx, "gateway.example");
    probe.update(&mut cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().last(),
            Some(SettingsIntent::SetRemoteField {
                profile_id,
                field_id,
                value,
            }) if profile_id == "profile" && field_id == "host" && value == "gateway.example"
        ));
    });

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        !focused_input(&mut cx),
        "Enter restores focus to the remote trigger"
    );
    click(&mut cx, "remote-argument-profile-0");
    replace_focused_text(&mut cx, "-o BatchMode=yes");
    probe.update(&mut cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().last(),
            Some(SettingsIntent::SetRemoteField {
                profile_id,
                field_id,
                value,
            }) if profile_id == "profile" && field_id == "args.0" && value == "-o BatchMode=yes"
        ));
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        !focused_input(&mut cx),
        "Escape restores focus to the remote trigger"
    );
}

fn snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Remotes,
        pages: vec![SettingsPage {
            category: SettingsCategory::Remotes,
            title: "Remotes".to_owned(),
            search_terms: "remote ssh".to_owned(),
            items: vec![SettingsPageItem::Setting(SettingsRow::Remote(
                RemoteEditorSnapshot {
                    id: "remote".to_owned(),
                    label: "Remote profile".to_owned(),
                    detail: "profile@gateway.example".to_owned(),
                    error: None,
                    profile: Some(RemoteProfileSnapshot {
                        id: "profile".to_owned(),
                        fields: vec![
                            field("name", "Name", "Work"),
                            field("host", "Host", "old.example"),
                            field("user", "User", "luan"),
                            field("port", "Port", "22"),
                            field("identity-file", "Identity file", "~/.ssh/id_ed25519"),
                            field("proxy-jump", "Proxy / jump host", ""),
                            field("program", "SSH client", "ssh"),
                        ],
                        arguments: vec!["-o BatchMode=no".to_owned()],
                        test: None,
                    }),
                    test_state: RemoteTestState::Idle,
                    actions: Vec::new(),
                },
            ))],
        }],
        search: String::new(),
        write_error: None,
    }
}

fn field(id: &str, label: &str, value: &str) -> RemoteProfileFieldSnapshot {
    RemoteProfileFieldSnapshot {
        id: id.to_owned(),
        label: label.to_owned(),
        value: value.to_owned(),
        options: Vec::new(),
    }
}

fn click(cx: &mut gpui_kit::VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).expect("remote control exists");
    cx.simulate_click(center(bounds), Modifiers::none());
    cx.run_until_parked();
}

fn replace_focused_text(cx: &mut gpui_kit::VisualTestContext, text: &str) {
    let command = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.simulate_keystrokes(&format!("{command}-a"));
    cx.simulate_input(text);
}

fn focused_input(cx: &mut gpui_kit::VisualTestContext) -> bool {
    cx.update(|window, cx| window.focused_input(cx).is_some())
}

fn center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(
        gpui_kit::px(width.mul_add(0.5, left)),
        gpui_kit::px(height.mul_add(0.5, top)),
    )
}
