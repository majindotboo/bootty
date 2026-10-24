#![cfg(test)]
//! Speech runs on the Codex sign-in already on this machine. Without one, voice doesn't show and
//! dictation shows only where it can run on device.

use assert_fs::prelude::*;
use bootty_agents::{AgentKind, NativeSessionConfig, NativeSessionRecord};
use bootty_control::{AppCommandReceiver, Caller, CommandOutcome};
use bootty_ui::{
    NativeAgentSessionView,
    gpui::{UiPalette, init_theme},
};
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::{Arc, Mutex};

fn record(account: &std::path::Path) -> NativeSessionRecord {
    let mut config = NativeSessionConfig::new(AgentKind::Codex, "/tmp/project");
    config.account_directory = Some(account.to_string_lossy().into_owned());
    NativeSessionRecord {
        restart: bootty_agents::NativeRestart::default(),
        id: "speech".to_owned(),
        binding_id: "binding".to_owned(),
        task_identity: Some("task".to_owned()),
        title: "Conversation".to_owned(),
        pending_initial_message: None,
        permissions_pending: false,
        generation: 1,
        config,
        snapshot: serde_json::from_value(serde_json::json!({
            "provider": "codex", "session_id": "thread", "turn_id": null, "status": "idle",
            "requests": [], "usage": null, "error": null, "revision": 1, "transcript": []
        }))
        .unwrap(),
        side_chat: None,
        spawn_parent: None,
        queued_messages: Vec::new(),
        attachments: Vec::new(),
        draft: None,
        seen_turns: 0,
    }
}

/// Whether dictation and voice show in a Codex conversation whose account is `signed_in`.
fn speech_buttons(signed_in: bool, cx: &mut TestAppContext) -> (bool, bool) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let account = assert_fs::TempDir::new().unwrap();
    if signed_in {
        account
            .child("auth.json")
            .write_str(r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-test"}"#)
            .unwrap();
    }
    let record = record(account.path());
    // Every command the view sends is answered, as the app would.
    let inbox = Arc::new(Mutex::new(None::<AppCommandReceiver>));
    let queued = inbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        16,
        Arc::new(move || {
            let inbox = queued.lock().unwrap();
            while let Ok(request) = inbox.as_ref().unwrap().try_recv() {
                request
                    .response
                    .send(CommandOutcome::Success {
                        value: serde_json::Value::Null,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *inbox.lock().unwrap() = Some(receiver);
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NativeAgentSessionView::new(
                record,
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            )
        });
        gpui_kit::component::Root::new(view, window, cx)
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    (
        visual.debug_bounds("composer-dictation").is_some(),
        visual.debug_bounds("composer-voice").is_some(),
    )
}

/// A Codex conversation can dictate and hold a voice call on its own sign-in.
#[gpui_kit::test]
fn a_codex_sign_in_shows_dictation_and_voice(cx: &mut TestAppContext) {
    assert_eq!(speech_buttons(true, cx), (true, true));
}

#[gpui_kit::test]
fn without_a_codex_sign_in_only_on_device_dictation_shows(cx: &mut TestAppContext) {
    let on_device = bootty_agents::voice::availability(None).on_device;
    assert_eq!(speech_buttons(false, cx), (on_device, false));
}
