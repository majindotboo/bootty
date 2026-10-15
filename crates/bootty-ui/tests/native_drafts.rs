#![cfg(test)]
//! A conversation's unsent draft comes back when its view opens, and typing saves it again.

use bootty_agents::{AgentKind, NativeSessionConfig, NativeSessionRecord};
use bootty_control::{AppCommandReceiver, Caller, CommandInvocation, CommandOutcome};
use bootty_ui::{
    NativeAgentSessionView,
    gpui::{UiPalette, init_theme},
};
use gpui_kit::{AppContext as _, Modifiers, TestAppContext};
use pretty_assertions::assert_eq;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[gpui_kit::test]
fn a_saved_draft_returns_and_typing_saves_it_again(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = NativeSessionRecord {
        id: "drafts".to_owned(),
        binding_id: "binding".to_owned(),
        task_identity: Some("task".to_owned()),
        title: "Conversation".to_owned(),
        pending_initial_message: None,
        permissions_pending: false,
        generation: 1,
        config: NativeSessionConfig::new(AgentKind::Codex, "/tmp/project"),
        snapshot: serde_json::from_value(serde_json::json!({
            "provider": "codex", "session_id": "thread", "turn_id": null, "status": "idle",
            "requests": [], "usage": null, "error": null, "revision": 1, "transcript": []
        }))
        .unwrap(),
        side_chat: None,
        spawn_parent: None,
        queued_messages: Vec::new(),
        attachments: Vec::new(),
        draft: Some(bootty_agents::NativeDraft {
            text: "Unsent idea".to_owned(),
            attachments: Vec::new(),
        }),
        seen_turns: 0,
    };
    let commands = Arc::new(Mutex::new(Vec::<CommandInvocation>::new()));
    let observed = commands.clone();
    let inbox = Arc::new(Mutex::new(None::<AppCommandReceiver>));
    let queued = inbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        16,
        Arc::new(move || {
            let inbox = queued.lock().unwrap();
            while let Ok(request) = inbox.as_ref().unwrap().try_recv() {
                observed.lock().unwrap().push(request.invocation.clone());
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
    let composer = visual.debug_bounds("native-composer").unwrap();
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_keystrokes("end space a g a i n");
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_secs(1));
    visual.run_until_parked();
    let saved = commands
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|invocation| invocation.command == "agents.native.draft")
        .and_then(|invocation| invocation.arguments.get(2).cloned())
        .map(|draft| serde_json::from_str::<bootty_agents::NativeDraft>(&draft).unwrap());
    assert_eq!(
        saved,
        Some(bootty_agents::NativeDraft {
            text: "Unsent idea again".to_owned(),
            attachments: Vec::new(),
        })
    );
}
