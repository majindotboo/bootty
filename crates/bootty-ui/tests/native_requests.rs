#![cfg(test)]
//! Provider requests in a conversation: approvals and their reusable choices.

use bootty_agents::{AgentKind, NativeSessionConfig, NativeSessionRecord};
use bootty_control::{AppCommandReceiver, Caller, CommandInvocation, CommandOutcome};
use bootty_ui::{
    NativeAgentSessionView,
    gpui::{UiPalette, init_theme},
};
use gpui_kit::{AppContext as _, Modifiers, TestAppContext};
use pretty_assertions::assert_eq;
use std::sync::{Arc, Mutex};

/// A Claude approval offers every choice Claude suggested, and a reusable one goes through the
/// shared approve command with its scope.
#[gpui_kit::test]
fn claude_approvals_offer_the_choices_claude_suggested(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let record = NativeSessionRecord {
        id: "approvals".to_owned(),
        binding_id: "binding".to_owned(),
        task_identity: Some("task".to_owned()),
        title: "Conversation".to_owned(),
        pending_initial_message: None,
        permissions_pending: false,
        generation: 1,
        config: NativeSessionConfig::new(AgentKind::Claude, "/tmp/project"),
        snapshot: serde_json::from_value(serde_json::json!({
            "provider": "claude", "session_id": "session", "turn_id": null, "status": "waiting",
            "usage": null, "error": null, "revision": 1, "transcript": [],
            "requests": [{"id": "claude-permission-1", "method": "claude.permission", "parameters": {
                "subtype": "can_use_tool", "tool_name": "Bash", "tool_use_id": "tool-1",
                "input": {"command": "npm view left-pad version"},
                "description": "Show latest left-pad version from npm",
                "permission_suggestions": [{"type": "addRules", "behavior": "allow",
                    "rules": [{"toolName": "Bash", "ruleContent": "npm view *"}],
                    "destination": "localSettings"}]
            }}]
        }))
        .unwrap(),
        side_chat: None,
        spawn_parent: None,
        queued_messages: Vec::new(),
        attachments: Vec::new(),
        draft: None,
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
    let choices = [
        "deny:claude-permission-1",
        "allow:claude-permission-1",
        "allow-session:claude-permission-1",
        "always-allow:claude-permission-1",
    ];
    let offered = choices
        .into_iter()
        .filter(|choice| visual.debug_bounds(choice).is_some())
        .collect::<Vec<_>>();
    assert_eq!(offered, choices);
    let session = visual
        .debug_bounds("allow-session:claude-permission-1")
        .unwrap();
    visual.simulate_click(session.center(), Modifiers::none());
    visual.run_until_parked();
    let approve = commands
        .lock()
        .unwrap()
        .iter()
        .find(|invocation| invocation.command == "agents.native.approve")
        .map(|invocation| invocation.arguments[2..].to_vec());
    assert_eq!(
        approve,
        Some(vec![
            "claude-permission-1".to_owned(),
            "allow-session".to_owned()
        ])
    );
}

/// Opens a conversation view for `record` whose commands are all answered.
fn open(record: NativeSessionRecord, cx: &mut TestAppContext) -> &mut gpui_kit::VisualTestContext {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
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
    visual
}

fn record(provider: AgentKind, snapshot: serde_json::Value) -> NativeSessionRecord {
    NativeSessionRecord {
        id: "approvals".to_owned(),
        binding_id: "binding".to_owned(),
        task_identity: Some("task".to_owned()),
        title: "Conversation".to_owned(),
        pending_initial_message: None,
        permissions_pending: false,
        generation: 1,
        config: NativeSessionConfig::new(provider, "/tmp/project"),
        snapshot: serde_json::from_value(snapshot).unwrap(),
        side_chat: None,
        spawn_parent: None,
        queued_messages: Vec::new(),
        attachments: Vec::new(),
        draft: None,
        seen_turns: 0,
    }
}

/// A Claude edit's approval shows the change it would make.
#[gpui_kit::test]
fn claude_edit_approvals_show_the_change(cx: &mut TestAppContext) {
    let visual = open(
        record(
            AgentKind::Claude,
            serde_json::json!({
                "provider": "claude", "session_id": "session", "turn_id": null, "status": "waiting",
                "usage": null, "error": null, "revision": 1, "transcript": [],
                "requests": [{"id": "edit-1", "method": "claude.permission", "parameters": {
                    "subtype": "can_use_tool", "tool_name": "Edit", "tool_use_id": "tool-1",
                    "input": {"file_path": "/tmp/project/notes.txt",
                        "old_string": "line one\nline two\nline three",
                        "new_string": "line one\nline 2\nline three"}
                }}]
            }),
        ),
        cx,
    );
    assert!(visual.debug_bounds("approval-diff:edit-1").is_some());
}

/// A Codex file-change approval shows the changes of the item it asks about.
#[gpui_kit::test]
fn codex_file_change_approvals_show_the_change(cx: &mut TestAppContext) {
    let visual = open(
        record(
            AgentKind::Codex,
            serde_json::json!({
                "provider": "codex", "session_id": "thread", "turn_id": "turn", "status": "waiting",
                "usage": null, "error": null, "revision": 1,
                "transcript": [{"id": "change", "role": "tool", "text": "", "complete": false,
                    "tool": {"name": "fileChange", "status": "running",
                        "input": "[{\"path\":\"/tmp/project/src/main.rs\",\"diff\":\"@@ -1 +1 @@\\n-old\\n+new\\n\"}]"}}],
                "requests": [{"id": "approve-1", "method": "item/fileChange/requestApproval",
                    "parameters": {"threadId": "thread", "turnId": "turn", "itemId": "change"}}]
            }),
        ),
        cx,
    );
    assert!(visual.debug_bounds("approval-diff:approve-1").is_some());
}
