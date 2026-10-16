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

type Commands = Arc<Mutex<Vec<CommandInvocation>>>;

/// Opens a conversation view for `record` whose commands are all answered and recorded.
fn open(
    record: NativeSessionRecord,
    cx: &mut TestAppContext,
) -> (&mut gpui_kit::VisualTestContext, Commands) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let commands = Commands::default();
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
    (visual, commands)
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
    let (visual, _) = open(
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
    let (visual, _) = open(
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

/// The answers sent through the shared respond command, in order.
fn responses(commands: &Commands) -> Vec<serde_json::Value> {
    commands
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.respond")
        .map(|invocation| serde_json::from_str(&invocation.arguments[3]).unwrap())
        .collect()
}

fn elicitation(id: &str, parameters: serde_json::Value) -> NativeSessionRecord {
    let mut parameters = parameters;
    parameters["threadId"] = "thread".into();
    parameters["turnId"] = "turn".into();
    parameters["serverName"] = "crm".into();
    record(
        AgentKind::Codex,
        serde_json::json!({
            "provider": "codex", "session_id": "thread", "turn_id": "turn", "status": "waiting",
            "usage": null, "error": null, "revision": 1, "transcript": [],
            "requests": [{"id": id, "method": "mcpServer/elicitation/request",
                "parameters": parameters}]
        }),
    )
}

/// An MCP form submits only once its answers satisfy the server's schema, with the server's
/// defaults filled in.
#[gpui_kit::test]
fn mcp_forms_submit_answers_the_schema_accepts(cx: &mut TestAppContext) {
    let (visual, commands) = open(
        elicitation(
            "form-1",
            serde_json::json!({"mode": "form", "message": "Who should we contact?",
                "requestedSchema": {"type": "object", "required": ["email"], "properties": {
                    "email": {"type": "string", "format": "email", "title": "Email"},
                    "plan": {"type": "string", "enum": ["team", "pro"], "default": "pro"}}}}),
        ),
        cx,
    );
    let submit = visual.debug_bounds("elicitation-submit:form-1").unwrap();
    visual.simulate_click(submit.center(), Modifiers::none());
    visual.run_until_parked();
    assert_eq!(responses(&commands), Vec::<serde_json::Value>::new());

    let email = visual
        .debug_bounds("elicitation-input:form-1:email")
        .unwrap();
    visual.simulate_click(email.center(), Modifiers::none());
    visual.simulate_input("ada@example.com");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let submit = visual.debug_bounds("elicitation-submit:form-1").unwrap();
    visual.simulate_click(submit.center(), Modifiers::none());
    visual.run_until_parked();
    assert_eq!(
        responses(&commands),
        [serde_json::json!({"action": "accept",
            "content": {"email": "ada@example.com", "plan": "pro"}})]
    );
}

/// A sign-in page opens from its button, which tells the server the user went there.
#[gpui_kit::test]
fn mcp_sign_in_pages_open_and_answer(cx: &mut TestAppContext) {
    let (visual, commands) = open(
        elicitation(
            "page-1",
            serde_json::json!({"mode": "url", "message": "Sign in to the CRM",
                "url": "https://auth.example.com/device?code=1", "elicitationId": "e"}),
        ),
        cx,
    );
    let open_page = visual.debug_bounds("elicitation-open:page-1").unwrap();
    visual.simulate_click(open_page.center(), Modifiers::none());
    visual.run_until_parked();
    assert_eq!(
        responses(&commands),
        [serde_json::json!({"action": "accept", "content": null})]
    );
}

fn command_approval() -> NativeSessionRecord {
    record(
        AgentKind::Codex,
        serde_json::json!({
            "provider": "codex", "session_id": "thread", "turn_id": "turn", "status": "waiting",
            "usage": null, "error": null, "revision": 1, "transcript": [],
            "requests": [{"id": "command-1", "method": "item/commandExecution/requestApproval",
                "parameters": {"threadId": "thread", "turnId": "turn", "itemId": "run",
                    "command": "cargo test", "cwd": "/tmp/project"}}]
        }),
    )
}

fn approvals(commands: &Commands) -> Vec<Vec<String>> {
    commands
        .lock()
        .unwrap()
        .iter()
        .filter(|invocation| invocation.command == "agents.native.approve")
        .map(|invocation| invocation.arguments[2..].to_vec())
        .collect()
}

/// A new approval takes focus from an empty composer, so Enter allows it once and Escape denies.
#[gpui_kit::test]
fn approvals_answer_enter_and_escape_once_focused(cx: &mut TestAppContext) {
    let (visual, commands) = open(command_approval(), cx);
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        approvals(&commands),
        [vec!["command-1".to_owned(), "allow".to_owned()]]
    );
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(
        approvals(&commands).last(),
        Some(&vec!["command-1".to_owned(), "deny".to_owned()])
    );
}

/// An approval that arrives while the person is writing leaves focus in the composer, so their
/// keys never answer it.
#[gpui_kit::test]
fn approvals_leave_a_draft_in_progress_focused(cx: &mut TestAppContext) {
    let mut record = command_approval();
    record.draft = Some(bootty_agents::NativeDraft {
        text: "Before you run that".to_owned(),
        attachments: Vec::new(),
    });
    let (visual, commands) = open(record, cx);
    visual.simulate_keystrokes("escape enter");
    visual.run_until_parked();
    assert_eq!(approvals(&commands), Vec::<Vec<String>>::new());
}

/// A request a subagent sent names that subagent; one from the conversation itself names none.
#[gpui_kit::test]
fn subagent_requests_name_who_asked(cx: &mut TestAppContext) {
    let mut record = command_approval();
    record.snapshot.requests[0].subagent = Some("Scout".to_owned());
    let (visual, _) = open(record, cx);
    assert!(
        visual
            .debug_bounds("request-subagent:command-1:Scout")
            .is_some()
    );
    let (visual, _) = open(command_approval(), cx);
    assert!(
        visual
            .debug_bounds("request-subagent:command-1:Scout")
            .is_none()
    );
}

fn codex_questions() -> NativeSessionRecord {
    let question = |id: &str, options: &[&str]| {
        serde_json::json!({"id": id, "header": id, "question": format!("Pick a {id}"),
            "options": options.iter().map(|label| serde_json::json!({"label": label}))
                .collect::<Vec<_>>()})
    };
    record(
        AgentKind::Codex,
        serde_json::json!({
            "provider": "codex", "session_id": "thread", "turn_id": "turn", "status": "waiting",
            "usage": null, "error": null, "revision": 1, "transcript": [],
            "requests": [{"id": "ask", "method": "item/tool/requestUserInput",
                "parameters": {"threadId": "thread", "turnId": "turn", "itemId": "tool",
                    "questions": [question("color", &["Red", "Blue"]),
                        question("size", &["Small", "Large"])]}}]
        }),
    )
}

/// A question takes focus like an approval: each digit picks an option and moves to the next
/// question, and Enter on the last one sends every answer.
#[gpui_kit::test]
fn question_digits_answer_in_order_and_enter_sends(cx: &mut TestAppContext) {
    let (visual, commands) = open(codex_questions(), cx);
    visual.simulate_keystrokes("2 1 enter");
    visual.run_until_parked();
    assert_eq!(
        responses(&commands),
        [serde_json::json!({"answers": {
            "color": {"answers": ["Blue"]},
            "size": {"answers": ["Small"]}
        }})]
    );
}

/// Enter moves past a question left unanswered but sends nothing until each has an answer, and
/// a digit with no option does nothing.
#[gpui_kit::test]
fn question_enter_waits_for_every_answer(cx: &mut TestAppContext) {
    let (visual, commands) = open(codex_questions(), cx);
    visual.simulate_keystrokes("9 enter 2 enter");
    visual.run_until_parked();
    assert_eq!(responses(&commands), Vec::<serde_json::Value>::new());
}

/// Escape cancels a Claude question the way its Cancel button does.
#[gpui_kit::test]
fn claude_question_escape_cancels(cx: &mut TestAppContext) {
    let (visual, commands) = open(
        record(
            AgentKind::Claude,
            serde_json::json!({
                "provider": "claude", "session_id": "session", "turn_id": "turn",
                "status": "waiting", "usage": null, "error": null, "revision": 1,
                "transcript": [],
                "requests": [{"id": "claude-ask", "method": "claude.permission", "parameters": {
                    "subtype": "can_use_tool", "tool_name": "AskUserQuestion",
                    "tool_use_id": "tool-1", "input": {"questions": [{"question": "Which?",
                        "header": "Which", "multiSelect": false,
                        "options": [{"label": "This", "description": "Pick this"},
                            {"label": "That", "description": "Pick that"}]}]}
                }}]
            }),
        ),
        cx,
    );
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(
        responses(&commands),
        [serde_json::json!({"decision": "decline"})]
    );
}
