#![cfg(test)]
//! `@` mentions in a conversation composer.

use bootty_agents::{AgentKind, NativeSessionConfig, NativeSessionRecord};
use bootty_control::{
    AppCommandReceiver, Caller, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use bootty_ui::{
    NativeAgentSessionView,
    gpui::{CommandAction, UiPalette, init_theme},
};
use gpui_kit::{AppContext as _, Modifiers, TestAppContext};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::sync::{Arc, Mutex};

fn idle_record() -> NativeSessionRecord {
    NativeSessionRecord {
        id: "mentions".to_owned(),
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
    }
}

/// A file picked from `@` is named by its project path, so the agent reads and edits the real
/// file. Nothing is copied or attached.
#[rstest]
#[case::plain("@main", "Review @src/main.rs ")]
#[case::with_spaces("@Notes", "Review @\"docs/My Notes.md\" ")]
fn a_mentioned_file_is_its_project_path(#[case] typed: &str, #[case] draft: &str) {
    let cx = &mut TestAppContext::single();
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let commands = Arc::new(Mutex::new(Vec::<CommandInvocation>::new()));
    let observed = commands.clone();
    let inbox = Arc::new(Mutex::new(None::<AppCommandReceiver>));
    let queued = inbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        16,
        Arc::new(move || {
            let inbox = queued.lock().unwrap();
            while let Ok(request) = inbox.as_ref().unwrap().try_recv() {
                let value = match request.invocation.command.as_str() {
                    "files.complete" => serde_json::json!({
                        "kind": "completions",
                        "root": "/tmp/project",
                        "files": ["src/main.rs", "docs/My Notes.md"],
                        "omitted": 0
                    }),
                    "agents.native.completions" => serde_json::json!({"options": []}),
                    _ => serde_json::Value::Null,
                };
                observed.lock().unwrap().push(request.invocation.clone());
                request
                    .response
                    .send(CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    })
                    .unwrap();
            }
        }),
    );
    *inbox.lock().unwrap() = Some(receiver);
    let mut session = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = NativeAgentSessionView::new(
                idle_record(),
                sender.for_caller(Caller::Internal),
                true,
                window,
                cx,
            );
            view.set_completion_binding(
                Some(CommandTarget {
                    kind: ResourceKind::Binding,
                    handle: "binding".into(),
                    generation: 1,
                }),
                window,
                cx,
            );
            view
        });
        session = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    let session = session.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let editor = visual.debug_bounds("native-composer").expect("composer");
    visual.simulate_click(editor.center(), Modifiers::none());
    visual.simulate_input(&format!("Review {typed}"));
    visual.run_until_parked();
    // File search waits out typing before it asks the host.
    visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(100));
    visual.run_until_parked();
    visual.update(|window, cx| {
        session.update(cx, |view, cx| {
            view.perform_completion(CommandAction::Confirm, window, cx);
        });
    });
    visual.run_until_parked();
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let sent = commands.lock().unwrap().clone();
    let prompt = sent
        .iter()
        .find(|invocation| invocation.command == "agents.native.prompt")
        .unwrap_or_else(|| panic!("the draft is sent: {sent:?}"));
    assert_eq!(
        (prompt.arguments[2].as_str(), prompt.arguments[4].as_str()),
        (draft, "[]"),
        "the prompt names the project file and attaches nothing: {:?}",
        sent.iter()
            .map(|invocation| &invocation.command)
            .collect::<Vec<_>>()
    );
    assert!(
        sent.iter().all(|invocation| !matches!(
            invocation.command.as_str(),
            "files.source" | "agents.native.import"
        )),
        "a mention copies nothing: {sent:?}"
    );
}
