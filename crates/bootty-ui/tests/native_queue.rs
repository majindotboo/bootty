#![cfg(test)]
//! Queued follow-up messages in a conversation's composer drawer.

use bootty_agents::{AgentKind, NativeSessionConfig, NativeSessionRecord};
use bootty_control::{AppCommandReceiver, Caller, CommandInvocation, CommandOutcome};
use bootty_ui::{
    NativeAgentSessionView,
    gpui::{UiPalette, init_theme},
};
use gpui_kit::{AppContext as _, Modifiers, TestAppContext};
use std::sync::{Arc, Mutex};

/// A queued message offers steer, remove and more; Edit message moves it back to the composer
/// and out of the queue.
#[gpui_kit::test]
fn editing_a_queued_message_moves_it_back_to_the_composer(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = NativeSessionRecord {
        id: "queue".to_owned(),
        binding_id: "binding".to_owned(),
        task_identity: Some("task".to_owned()),
        title: "Conversation".to_owned(),
        pending_initial_message: None,
        permissions_pending: false,
        generation: 1,
        config: NativeSessionConfig::new(AgentKind::Codex, "/tmp/project"),
        snapshot: serde_json::from_value(serde_json::json!({
            "provider": "codex", "session_id": "thread", "turn_id": "turn", "status": "working",
            "requests": [], "usage": null, "error": null, "revision": 1, "transcript": []
        }))
        .unwrap(),
        side_chat: None,
        spawn_parent: None,
        queued_messages: serde_json::from_value(serde_json::json!([{
            "id": "next", "text": "Check the logs too", "preview_text": "Check the logs too",
            "attachments": [], "status": "queued"
        }]))
        .unwrap(),
        attachments: Vec::new(),
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
    for control in [
        "native-queue-steer:next",
        "native-queue-cancel:next",
        "native-queued-menu:next",
    ] {
        assert!(visual.debug_bounds(control).is_some(), "{control}");
    }
    let menu = visual.debug_bounds("native-queued-menu:next").unwrap();
    visual.simulate_click(menu.center(), Modifiers::none());
    visual.run_until_parked();
    visual.simulate_keystrokes("down enter");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let composer = visual.debug_bounds("native-composer").unwrap();
    visual.simulate_click(composer.center(), Modifiers::none());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let sent = commands.lock().unwrap().clone();
    assert!(
        sent.iter().any(
            |invocation| invocation.command == "agents.native.queue-cancel"
                && invocation.arguments.last().is_some_and(|id| id == "next")
        ),
        "the edited message leaves the queue: {sent:?}"
    );
    assert!(
        sent.iter().any(|invocation| matches!(
            invocation.command.as_str(),
            "agents.native.prompt" | "agents.native.queue"
        ) && invocation
            .arguments
            .get(2)
            .is_some_and(|text| text == "Check the logs too")),
        "its text is back in the composer and sends: {sent:?}"
    );
}
