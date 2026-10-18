//! The host side of an MCP app (MCP Apps, spec 2026-01-26): the HTML document an MCP server
//! offers to show one of its tool calls, and the JSON-RPC 2.0 bridge between that document and
//! Bootty. The bridge holds no transport. Its view posts what it says and carries out what the app
//! asks, answering each request through [`McpAppBridge::answer`].

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const MCP_APP_PROTOCOL_VERSION: &str = "2026-01-26";
/// Larger messages are refused rather than parsed further.
const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// Requests an app may have in flight at once; more are refused, not queued.
const MAX_PENDING_REQUESTS: usize = 16;
/// The largest app document Bootty shows. Windows webviews load at most 2 MiB of inline HTML;
/// serving documents through a custom protocol would lift this.
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// The tool call an app shows: what it was asked and, once it finished, what it returned.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpAppCall {
    pub arguments: Value,
    pub result: Option<Value>,
}

/// An app document, read from its server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpAppDocument {
    pub html: String,
    /// Origins the app may fetch from.
    pub connect_domains: Vec<String>,
    /// Origins the app may load scripts, styles, images, fonts and media from.
    pub resource_domains: Vec<String>,
}

impl McpAppDocument {
    /// Reads the app's HTML and declared origins from a `resources/read` result.
    /// # Errors
    /// Returns a result without an HTML document, or one larger than Bootty shows.
    pub fn parse(result: &Value) -> Result<Self, String> {
        let contents = result
            .get("contents")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let content = contents
            .iter()
            .find(|content| {
                content
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .is_some_and(|mime| mime.starts_with("text/html"))
            })
            .or_else(|| contents.first())
            .ok_or("The app's server returned no document")?;
        let html = if let Some(text) = content.get("text").and_then(Value::as_str) {
            text.to_owned()
        } else {
            let blob = content
                .get("blob")
                .and_then(Value::as_str)
                .ok_or("The app's document is empty")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(blob)
                .map_err(|_| "The app's document isn't valid base64")?;
            String::from_utf8(bytes).map_err(|_| "The app's document isn't UTF-8 text")?
        };
        if html.len() > MAX_DOCUMENT_BYTES {
            return Err("The app's document is larger than the 1 MiB Bootty shows".to_owned());
        }
        let csp = content
            .get("_meta")
            .and_then(|meta| meta.get("ui"))
            .and_then(|ui| ui.get("csp"));
        let domains = |key: &str| -> Vec<String> {
            csp.and_then(|csp| csp.get(key))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        };
        Ok(Self {
            html,
            connect_domains: domains("connectDomains"),
            resource_domains: domains("resourceDomains"),
        })
    }
}

/// What a tool's MCP definition says about calling it from an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpAppTool {
    pub title: String,
    /// The server says the tool changes nothing, so it runs without asking.
    pub read_only: bool,
}

impl McpAppTool {
    /// Reads a tool's definition (MCP `Tool`).
    /// # Errors
    /// Returns a tool its server keeps from apps, which see only tools whose
    /// `_meta.ui.visibility` includes `app`.
    pub fn from_definition(name: &str, definition: &Value) -> Result<Self, String> {
        let visible = definition
            .pointer("/_meta/ui/visibility")
            .and_then(Value::as_array)
            .is_none_or(|visibility| visibility.iter().any(|value| value == "app"));
        if !visible {
            return Err(format!("This app can't call {name}"));
        }
        let annotations = definition.get("annotations");
        let title = definition
            .get("title")
            .or_else(|| annotations.and_then(|annotations| annotations.get("title")))
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(name);
        Ok(Self {
            title: title.to_owned(),
            read_only: annotations
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// The outcome of an app's tool call: done, or waiting for the user to allow a tool that may
/// change something.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAppToolCall {
    Approve { title: String },
    Result(Value),
}

/// A JSON-RPC request id the app chose: a string or a number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpAppRequestId(Value);

/// What the app said that Bootty must act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpAppEffect {
    /// A message for the app.
    Post(String),
    CallTool {
        id: McpAppRequestId,
        name: String,
        arguments: Value,
    },
    ReadResource {
        id: McpAppRequestId,
        uri: String,
    },
    /// An http(s) link the app opened from a click.
    OpenLink {
        id: McpAppRequestId,
        url: String,
    },
    /// Text the app offers as the user's next message.
    Message {
        id: McpAppRequestId,
        text: String,
    },
    /// The app asked to be closed.
    Close,
    /// The app finished its teardown, so its document can go.
    TornDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingKind {
    Tool,
    Resource,
    /// Answered with an empty result once done.
    Done,
}

/// The bridge to one app document.
pub struct McpAppBridge {
    version: String,
    /// What the host enforces on the document, as the app is told at initialize.
    sandbox: Value,
    context: Map<String, Value>,
    sent_context: Option<Map<String, Value>>,
    call: Option<McpAppCall>,
    call_sent: bool,
    initialized: bool,
    pending: Vec<(McpAppRequestId, PendingKind)>,
    teardown: Option<Value>,
}

impl McpAppBridge {
    /// `context` is the host context the app reads at initialize; `sandbox` says what the host
    /// enforces on its document.
    #[must_use]
    pub fn new(
        version: &str,
        sandbox: Value,
        context: Map<String, Value>,
        call: Option<McpAppCall>,
    ) -> Self {
        Self {
            version: version.to_owned(),
            sandbox,
            context,
            sent_context: None,
            call,
            call_sent: false,
            initialized: false,
            pending: Vec::new(),
            teardown: None,
        }
    }

    /// Handles one message the app posted. `activated` says the user had just clicked or typed
    /// in the app, which opening a link needs.
    pub fn receive(&mut self, text: &str, activated: bool) -> Vec<McpAppEffect> {
        let Ok(Value::Object(message)) = serde_json::from_str::<Value>(text) else {
            return Vec::new();
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Vec::new();
        }
        let id = message.get("id").filter(|id| is_id(id)).cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            if id.is_some_and(|id| self.teardown.as_ref() == Some(&id))
                && (message.contains_key("result") || message.contains_key("error"))
            {
                self.teardown = None;
                return vec![McpAppEffect::TornDown];
            }
            return Vec::new();
        };
        let empty = Map::new();
        let params = message
            .get("params")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        match id {
            Some(id) if text.len() > MAX_MESSAGE_BYTES => {
                vec![Self::fail(
                    &McpAppRequestId(id),
                    -32600,
                    "Message too large.",
                )]
            }
            Some(id) => self.request(McpAppRequestId(id), method, params, activated),
            None => self.notification(method),
        }
    }

    /// Answers a request the app made, once Bootty has carried it out. An unknown or already
    /// answered request is ignored.
    pub fn answer(
        &mut self,
        id: &McpAppRequestId,
        result: Result<Value, String>,
    ) -> Option<McpAppEffect> {
        let index = self.pending.iter().position(|(pending, _)| pending == id)?;
        let (id, kind) = self.pending.remove(index);
        Some(match result {
            Ok(value) => Self::respond(
                &id,
                &match kind {
                    PendingKind::Tool => tool_result(&value),
                    PendingKind::Resource => value,
                    PendingKind::Done => json!({}),
                },
            ),
            Err(message) => Self::fail(&id, -32000, &message),
        })
    }

    /// Updates the host context, telling an initialized app what changed.
    pub fn set_context(&mut self, context: Map<String, Value>) -> Option<McpAppEffect> {
        self.context = context;
        self.send_context()
    }

    /// Asks the app to wrap up before its document goes. Returns nothing when the app has not
    /// initialized, since it then has nothing to save and may not be listening.
    pub fn teardown(&mut self) -> Option<McpAppEffect> {
        if !self.initialized || self.teardown.is_some() {
            return None;
        }
        let id = Value::from("bootty-teardown");
        self.teardown = Some(id.clone());
        Some(post(
            &json!({"jsonrpc":"2.0","id":id,"method":"ui/resource-teardown","params":{}}),
        ))
    }

    fn request(
        &mut self,
        id: McpAppRequestId,
        method: &str,
        params: &Map<String, Value>,
        activated: bool,
    ) -> Vec<McpAppEffect> {
        let effect = match method {
            "ui/initialize" => return vec![self.initialize(&id)],
            "ping" => return vec![Self::respond(&id, &json!({}))],
            // Bootty shows an app in a fixed pane; that is its only mode.
            "ui/request-display-mode" => {
                return vec![Self::respond(&id, &json!({"mode": "inline"}))];
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).map(str::trim);
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match name {
                    Some(name) if !name.is_empty() && arguments.is_object() => (
                        PendingKind::Tool,
                        McpAppEffect::CallTool {
                            id: id.clone(),
                            name: name.to_owned(),
                            arguments,
                        },
                    ),
                    _ => {
                        return vec![Self::fail(
                            &id,
                            -32602,
                            "tools/call needs a tool name and object arguments.",
                        )];
                    }
                }
            }
            "resources/read" => match params.get("uri").and_then(Value::as_str) {
                Some(uri) if !uri.trim().is_empty() => (
                    PendingKind::Resource,
                    McpAppEffect::ReadResource {
                        id: id.clone(),
                        uri: uri.to_owned(),
                    },
                ),
                _ => return vec![Self::fail(&id, -32602, "resources/read needs a uri.")],
            },
            "ui/open-link" => {
                let url = params
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let lower = url.to_ascii_lowercase();
                if !(lower.starts_with("https://") || lower.starts_with("http://")) {
                    return vec![Self::fail(&id, -32602, "Only http(s) links can be opened.")];
                }
                if !activated {
                    return vec![Self::fail(
                        &id,
                        -32000,
                        "Links open only from a click in the app.",
                    )];
                }
                (
                    PendingKind::Done,
                    McpAppEffect::OpenLink {
                        id: id.clone(),
                        url: url.to_owned(),
                    },
                )
            }
            "ui/message" => match message_text(params) {
                Some(text) => (
                    PendingKind::Done,
                    McpAppEffect::Message {
                        id: id.clone(),
                        text,
                    },
                ),
                None => {
                    return vec![Self::fail(
                        &id,
                        -32602,
                        "Only non-empty text messages from the user role are supported.",
                    )];
                }
            },
            _ => {
                return vec![Self::fail(
                    &id,
                    -32601,
                    &format!("Method not found: {method}"),
                )];
            }
        };
        if self.pending.len() >= MAX_PENDING_REQUESTS {
            return vec![Self::fail(&id, -32000, "Too many requests in flight.")];
        }
        self.pending.push((id, effect.0));
        vec![effect.1]
    }

    /// The answer to `ui/initialize`: what Bootty offers, and the context the app starts with.
    /// Bootty speaks one protocol version, which an app asking for another may decline.
    fn initialize(&mut self, id: &McpAppRequestId) -> McpAppEffect {
        self.sent_context = Some(self.context.clone());
        Self::respond(
            id,
            &json!({
                "protocolVersion": MCP_APP_PROTOCOL_VERSION,
                "hostInfo": {"name": "bootty", "version": self.version},
                "hostCapabilities": {
                    "openLinks": {},
                    "serverTools": {},
                    "serverResources": {},
                    "logging": {},
                    "message": {"text": {}},
                    "sandbox": self.sandbox,
                },
                "hostContext": self.context,
            }),
        )
    }

    fn notification(&mut self, method: &str) -> Vec<McpAppEffect> {
        match method {
            "ui/notifications/initialized" if !self.initialized => {
                self.initialized = true;
                let mut effects = Vec::new();
                if let Some(call) = self.call.as_ref().filter(|_| !self.call_sent) {
                    let arguments = if call.arguments.is_object() {
                        call.arguments.clone()
                    } else {
                        json!({})
                    };
                    effects.push(notify(
                        "ui/notifications/tool-input",
                        &json!({"arguments": arguments}),
                    ));
                    if let Some(result) = &call.result {
                        effects.push(notify("ui/notifications/tool-result", &tool_result(result)));
                    }
                    self.call_sent = true;
                }
                // The context may have changed since the initialize response.
                effects.extend(self.send_context());
                effects
            }
            "ui/notifications/request-teardown" => vec![McpAppEffect::Close],
            // Logging, size changes for a fixed pane, and unknown notifications.
            _ => Vec::new(),
        }
    }

    fn send_context(&mut self) -> Option<McpAppEffect> {
        if !self.initialized {
            return None;
        }
        let sent = self
            .sent_context
            .replace(self.context.clone())
            .unwrap_or_default();
        let changes: Map<String, Value> = self
            .context
            .iter()
            .filter(|(key, value)| sent.get(*key) != Some(value))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        (!changes.is_empty()).then(|| {
            notify(
                "ui/notifications/host-context-changed",
                &Value::Object(changes),
            )
        })
    }

    fn respond(id: &McpAppRequestId, result: &Value) -> McpAppEffect {
        post(&json!({"jsonrpc":"2.0","id":id.0,"result":result}))
    }

    fn fail(id: &McpAppRequestId, code: i64, message: &str) -> McpAppEffect {
        post(&json!({"jsonrpc":"2.0","id":id.0,"error":{"code":code,"message":message}}))
    }
}

fn is_id(value: &Value) -> bool {
    value.is_string() || value.as_f64().is_some_and(f64::is_finite)
}

fn post(message: &Value) -> McpAppEffect {
    McpAppEffect::Post(message.to_string())
}

fn notify(method: &str, params: &Value) -> McpAppEffect {
    post(&json!({"jsonrpc":"2.0","method":method,"params":params}))
}

/// The text of a `ui/message` from the user role. The SDK sends an array of content blocks; the
/// spec shows one block.
fn message_text(params: &Map<String, Value>) -> Option<String> {
    if params.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = params.get("content")?;
    let blocks = content
        .as_array()
        .map_or_else(|| std::slice::from_ref(content), Vec::as_slice);
    let texts = blocks
        .iter()
        .map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Option<Vec<_>>>()?;
    let text = texts.join("\n").trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// A `CallToolResult` as the spec's schema accepts it. Codex reports absent fields as null,
/// which the MCP Apps SDK rejects along with the whole message.
fn tool_result(result: &Value) -> Value {
    let mut normalized = Map::new();
    normalized.insert(
        "content".to_owned(),
        result
            .get("content")
            .filter(|content| content.is_array())
            .cloned()
            .unwrap_or_else(|| json!([])),
    );
    for key in ["structuredContent", "_meta"] {
        if let Some(value) = result.get(key).filter(|value| value.is_object()) {
            normalized.insert(key.to_owned(), value.clone());
        }
    }
    if result.get("isError") == Some(&Value::Bool(true)) {
        normalized.insert("isError".to_owned(), Value::Bool(true));
    }
    Value::Object(normalized)
}
