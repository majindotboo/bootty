//! An MCP app's bridge: what Bootty tells the app, and what the app's messages ask of Bootty.

use base64::Engine as _;
use bootty_agents::{
    MCP_APP_PROTOCOL_VERSION, McpAppBridge, McpAppCall, McpAppDocument, McpAppEffect, McpAppTool,
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};
use serde_json::{Map, Value, json};

fn context(width: u64) -> Map<String, Value> {
    serde_json::from_value(
        json!({"theme": "dark", "containerDimensions": {"width": width, "height": 300}}),
    )
    .unwrap_or_default()
}

fn request(id: i64, method: &str, params: &Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn notification(method: &str) -> String {
    json!({"jsonrpc": "2.0", "method": method}).to_string()
}

/// What the effects post, as JSON, so failures show structure.
fn posts(effects: impl IntoIterator<Item = McpAppEffect>) -> Vec<Value> {
    effects
        .into_iter()
        .map(|effect| match effect {
            McpAppEffect::Post(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
            other => json!({"effect": format!("{other:?}")}),
        })
        .collect()
}

fn reply(id: i64, result: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: i64, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[fixture]
fn bridge() -> McpAppBridge {
    McpAppBridge::new(
        "1.2.3",
        json!({"csp": {"connectDomains": [], "resourceDomains": []}}),
        context(400),
        Some(McpAppCall {
            arguments: json!({"city": "Lisbon"}),
            result: Some(json!({
                "content": [{"type": "text", "text": "sunny"}],
                "structuredContent": {"temp": 21},
                "_meta": null,
                "isError": null,
            })),
        }),
    )
}

/// An app that has finished its handshake.
#[fixture]
fn initialized(mut bridge: McpAppBridge) -> McpAppBridge {
    bridge.receive(&request(0, "ui/initialize", &json!({})), false);
    bridge.receive(&notification("ui/notifications/initialized"), false);
    bridge
}

/// The app reads the host at initialize, then gets the call it shows, once, without Codex's
/// nulls, which the MCP Apps SDK rejects.
#[rstest]
fn an_initialized_app_receives_its_call_once(mut bridge: McpAppBridge) {
    let initialize = request(
        1,
        "ui/initialize",
        &json!({"protocolVersion": MCP_APP_PROTOCOL_VERSION}),
    );
    assert_eq!(
        posts(bridge.receive(&initialize, false)),
        [reply(
            1,
            &json!({
                "protocolVersion": MCP_APP_PROTOCOL_VERSION,
                "hostInfo": {"name": "bootty", "version": "1.2.3"},
                "hostCapabilities": {
                    "openLinks": {}, "serverTools": {}, "serverResources": {}, "logging": {},
                    "message": {"text": {}},
                    "sandbox": {"csp": {"connectDomains": [], "resourceDomains": []}},
                },
                "hostContext": context(400),
            })
        )]
    );
    let initialized = notification("ui/notifications/initialized");
    assert_eq!(
        posts(bridge.receive(&initialized, false)),
        [
            json!({"jsonrpc": "2.0", "method": "ui/notifications/tool-input",
                "params": {"arguments": {"city": "Lisbon"}}}),
            json!({"jsonrpc": "2.0", "method": "ui/notifications/tool-result",
                "params": {"content": [{"type": "text", "text": "sunny"}],
                    "structuredContent": {"temp": 21}}}),
        ]
    );
    assert_eq!(bridge.receive(&initialized, false), []);
}

/// Before it initializes the app reads the context it asks for; after, only what changed.
#[rstest]
fn an_initialized_app_hears_what_changed(mut bridge: McpAppBridge) {
    assert_eq!(bridge.set_context(context(500)), None);
    let response = posts(bridge.receive(&request(1, "ui/initialize", &json!({})), false));
    assert_eq!(response[0]["result"]["hostContext"], json!(context(500)));
    bridge.receive(&notification("ui/notifications/initialized"), false);
    assert_eq!(bridge.set_context(context(500)), None);
    assert_eq!(
        posts(bridge.set_context(context(600))),
        [
            json!({"jsonrpc": "2.0", "method": "ui/notifications/host-context-changed",
            "params": {"containerDimensions": {"width": 600, "height": 300}}})
        ]
    );
}

/// Each request Bootty carries out comes back as one effect and is answered once, in the shape
/// the spec gives its result.
#[rstest]
#[case::tool(
    request(7, "tools/call", &json!({"name": "look", "arguments": {"a": 1}})),
    true,
    "CallTool { id: McpAppRequestId(Number(7)), name: \"look\", arguments: Object {\"a\": Number(1)} }",
    json!({"content": [], "structuredContent": null, "_meta": {"k": 1}}),
    json!({"content": [], "_meta": {"k": 1}}),
)]
#[case::resource(
    request(7, "resources/read", &json!({"uri": "ui://app/data.json"})),
    false,
    "ReadResource { id: McpAppRequestId(Number(7)), uri: \"ui://app/data.json\" }",
    json!({"contents": [{"uri": "ui://app/data.json", "text": "{}"}]}),
    json!({"contents": [{"uri": "ui://app/data.json", "text": "{}"}]}),
)]
#[case::link(
    request(7, "ui/open-link", &json!({"url": "https://example.com/a"})),
    true,
    "OpenLink { id: McpAppRequestId(Number(7)), url: \"https://example.com/a\" }",
    Value::Null,
    json!({}),
)]
#[case::message(
    request(7, "ui/message", &json!({"role": "user", "content": [
        {"type": "text", "text": "first"}, {"type": "text", "text": "second "}]})),
    false,
    "Message { id: McpAppRequestId(Number(7)), text: \"first\\nsecond\" }",
    Value::Null,
    json!({}),
)]
fn requests_are_carried_out_and_answered_once(
    #[from(initialized)] mut bridge: McpAppBridge,
    #[case] message: String,
    #[case] activated: bool,
    #[case] effect: &str,
    #[case] result: Value,
    #[case] answered: Value,
) {
    let effects = bridge.receive(&message, activated);
    assert_eq!(
        effects
            .iter()
            .map(|effect| format!("{effect:?}"))
            .collect::<Vec<_>>(),
        [effect]
    );
    let id = match &effects[0] {
        McpAppEffect::CallTool { id, .. }
        | McpAppEffect::ReadResource { id, .. }
        | McpAppEffect::OpenLink { id, .. }
        | McpAppEffect::Message { id, .. } => id.clone(),
        other => panic!("expected a request, got {other:?}"),
    };
    assert_eq!(
        posts(bridge.answer(&id, Ok(result.clone()))),
        [reply(7, &answered)]
    );
    assert_eq!(bridge.answer(&id, Ok(result)), None);
}

/// Requests Bootty answers itself, or refuses before doing anything.
#[rstest]
#[case::ping("ping", json!({}), false, reply(3, &json!({})))]
#[case::display_mode("ui/request-display-mode", json!({"mode": "fullscreen"}), false,
    reply(3, &json!({"mode": "inline"})))]
#[case::nameless_tool("tools/call", json!({"arguments": {}}), false,
    error(3, -32602, "tools/call needs a tool name and object arguments."))]
#[case::list_arguments("tools/call", json!({"name": "look", "arguments": []}), false,
    error(3, -32602, "tools/call needs a tool name and object arguments."))]
#[case::no_uri("resources/read", json!({}), false, error(3, -32602, "resources/read needs a uri."))]
#[case::file_link("ui/open-link", json!({"url": "file:///etc/passwd"}), true,
    error(3, -32602, "Only http(s) links can be opened."))]
#[case::unprompted_link("ui/open-link", json!({"url": "https://example.com"}), false,
    error(3, -32000, "Links open only from a click in the app."))]
#[case::assistant_message("ui/message",
    json!({"role": "assistant", "content": [{"type": "text", "text": "hi"}]}), false,
    error(3, -32602, "Only non-empty text messages from the user role are supported."))]
#[case::image_message("ui/message", json!({"role": "user", "content": [{"type": "image"}]}), false,
    error(3, -32602, "Only non-empty text messages from the user role are supported."))]
#[case::model_context("ui/update-model-context", json!({}), false,
    error(3, -32601, "Method not found: ui/update-model-context"))]
fn some_requests_are_answered_at_once(
    #[from(initialized)] mut bridge: McpAppBridge,
    #[case] method: &str,
    #[case] params: Value,
    #[case] activated: bool,
    #[case] expected: Value,
) {
    assert_eq!(
        posts(bridge.receive(&request(3, method, &params), activated)),
        [expected]
    );
}

#[rstest]
fn a_failed_request_reports_why(#[from(initialized)] mut bridge: McpAppBridge) {
    let effects = bridge.receive(&request(4, "tools/call", &json!({"name": "change"})), false);
    let McpAppEffect::CallTool { id, .. } = &effects[0] else {
        panic!("expected a tool call")
    };
    assert_eq!(
        posts(bridge.answer(id, Err("Declined by the user.".to_owned()))),
        [error(4, -32000, "Declined by the user.")]
    );
}

/// An app can't queue unbounded work: past sixteen requests in flight, more are refused.
#[rstest]
fn requests_in_flight_are_bounded(#[from(initialized)] mut bridge: McpAppBridge) {
    for id in 0..16 {
        let effects = bridge.receive(
            &request(id, "resources/read", &json!({"uri": "ui://a"})),
            false,
        );
        assert!(matches!(effects[..], [McpAppEffect::ReadResource { .. }]));
    }
    assert_eq!(
        posts(bridge.receive(
            &request(16, "resources/read", &json!({"uri": "ui://a"})),
            false
        )),
        [error(16, -32000, "Too many requests in flight.")]
    );
}

#[rstest]
#[case::not_json("{")]
#[case::not_json_rpc(r#"{"id": 1, "method": "ping"}"#)]
#[case::unknown_response(r#"{"jsonrpc": "2.0", "id": "x", "result": {}}"#)]
#[case::logging(r#"{"jsonrpc": "2.0", "method": "notifications/message", "params": {}}"#)]
#[case::resize(
    r#"{"jsonrpc": "2.0", "method": "ui/notifications/size-changed", "params": {"height": 9}}"#
)]
fn other_messages_ask_nothing(
    #[from(initialized)] mut bridge: McpAppBridge,
    #[case] message: &str,
) {
    assert_eq!(bridge.receive(message, true), []);
}

#[rstest]
fn an_oversized_request_is_refused(#[from(initialized)] mut bridge: McpAppBridge) {
    let uri = "x".repeat(300 * 1024);
    assert_eq!(
        posts(bridge.receive(&request(5, "resources/read", &json!({"uri": uri})), false)),
        [error(5, -32600, "Message too large.")]
    );
}

/// An initialized app gets to wrap up before its document goes; one that never initialized has
/// nothing to save.
#[rstest]
fn closing_gives_an_initialized_app_its_teardown(
    mut bridge: McpAppBridge,
    #[from(initialized)] mut ready: McpAppBridge,
) {
    assert_eq!(bridge.teardown(), None);
    assert_eq!(
        posts(ready.teardown()),
        [json!({"jsonrpc": "2.0", "id": "bootty-teardown",
            "method": "ui/resource-teardown", "params": {}})]
    );
    assert_eq!(ready.teardown(), None);
    assert_eq!(
        ready.receive(
            r#"{"jsonrpc": "2.0", "id": "bootty-teardown", "result": {}}"#,
            false
        ),
        [McpAppEffect::TornDown]
    );
    assert_eq!(
        ready.receive(&notification("ui/notifications/request-teardown"), false),
        [McpAppEffect::Close]
    );
}

#[rstest]
#[case::text(
    json!({"contents": [
        {"uri": "ui://app/notes.txt", "mimeType": "text/plain", "text": "notes"},
        {"uri": "ui://app/index.html", "mimeType": "text/html;profile=mcp-app", "text": "<p>hi</p>",
            "_meta": {"ui": {"csp": {"connectDomains": ["https://api.example.com"],
                "resourceDomains": ["https://cdn.example.com"]}}}},
    ]}),
    Ok(McpAppDocument {
        html: "<p>hi</p>".to_owned(),
        connect_domains: vec!["https://api.example.com".to_owned()],
        resource_domains: vec!["https://cdn.example.com".to_owned()],
    }),
)]
#[case::blob(
    json!({"contents": [{"uri": "ui://app", "blob":
        base64::engine::general_purpose::STANDARD.encode("<p>blob</p>")}]}),
    Ok(McpAppDocument {
        html: "<p>blob</p>".to_owned(),
        connect_domains: Vec::new(),
        resource_domains: Vec::new(),
    }),
)]
#[case::nothing(json!({"contents": []}), Err("The app's server returned no document".to_owned()))]
#[case::empty(json!({"contents": [{"uri": "ui://app"}]}), Err("The app's document is empty".to_owned()))]
#[case::huge(
    json!({"contents": [{"uri": "ui://app", "text": "x".repeat(1024 * 1024 + 1)}]}),
    Err("The app's document is larger than the 1 MiB Bootty shows".to_owned()),
)]
fn a_document_is_read_from_its_resource(
    #[case] result: Value,
    #[case] expected: Result<McpAppDocument, String>,
) {
    assert_eq!(McpAppDocument::parse(&result), expected);
}

/// Apps call only the tools their server lets them see, and run without asking only those it
/// marks read-only.
#[rstest]
#[case::read_only(json!({"annotations": {"readOnlyHint": true}}), Ok(("look", true)))]
#[case::titled(json!({"title": "Look closer"}), Ok(("Look closer", false)))]
#[case::annotated(json!({"annotations": {"title": "Look again"}}), Ok(("Look again", false)))]
#[case::for_apps(json!({"_meta": {"ui": {"visibility": ["app"]}}}), Ok(("look", false)))]
#[case::model_only(json!({"_meta": {"ui": {"visibility": ["model"]}}}), Err("This app can't call look"))]
fn a_tool_definition_says_how_an_app_may_call_it(
    #[case] definition: Value,
    #[case] expected: Result<(&str, bool), &str>,
) {
    assert_eq!(
        McpAppTool::from_definition("look", &definition),
        expected
            .map(|(title, read_only)| McpAppTool {
                title: title.to_owned(),
                read_only,
            })
            .map_err(str::to_owned)
    );
}
