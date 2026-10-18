//! An MCP app's page: the app's document in an opaque-origin frame, under a content policy built
//! from the origins the app declared, with a relay that speaks for that frame alone.

use serde_json::{Value, json};

/// What an app document may reach. A declared origin that isn't a plain http(s) or ws(s) source
/// is dropped, so a server can't widen the policy with keywords, wildcards or extra directives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpAppPolicy {
    connect: Vec<String>,
    resources: Vec<String>,
}

impl McpAppPolicy {
    #[must_use]
    pub fn new(connect: &[String], resources: &[String]) -> Self {
        let sources =
            |domains: &[String]| domains.iter().filter_map(|domain| source(domain)).collect();
        Self {
            connect: sources(connect),
            resources: sources(resources),
        }
    }

    /// The page's Content-Security-Policy, which its app frame inherits.
    #[must_use]
    pub fn content_security_policy(&self) -> String {
        let list = |fixed: &str, sources: &[String]| {
            std::iter::once(fixed)
                .chain(sources.iter().map(String::as_str))
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let resources = &self.resources;
        format!(
            "default-src 'none'; script-src {}; style-src {}; img-src {}; font-src {}; \
             media-src {}; connect-src {}; base-uri 'none'; form-action 'none'",
            list("'unsafe-inline'", resources),
            list("'unsafe-inline'", resources),
            list("data: blob:", resources),
            list("data:", resources),
            list("data: blob:", resources),
            if self.connect.is_empty() {
                "'none'".to_owned()
            } else {
                list("", &self.connect)
            },
        )
    }

    /// What the app is told the host enforces (`hostCapabilities.sandbox`).
    #[must_use]
    pub fn sandbox(&self) -> Value {
        json!({"csp": {"connectDomains": self.connect, "resourceDomains": self.resources}})
    }
}

/// A plain source for `domain`: `https://host`, a `*.` subdomain wildcard, an optional port and
/// path. A bare host means https.
fn source(domain: &str) -> Option<String> {
    let domain = domain.trim();
    let (scheme, rest) = domain.split_once("://").unwrap_or(("https", domain));
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "https" | "http" | "wss" | "ws") {
        return None;
    }
    let (authority, path) = rest
        .find('/')
        .map_or((rest, ""), |slash| rest.split_at(slash));
    let (host, port) = authority.split_once(':').unwrap_or((authority, ""));
    let named = host.strip_prefix("*.").unwrap_or(host);
    let valid = !named.is_empty()
        && named.split('.').all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
        && (port.is_empty() && !authority.ends_with(':')
            || port == "*"
            || !port.is_empty() && port.chars().all(|character| character.is_ascii_digit()))
        && path
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._~/%".contains(character));
    valid.then(|| format!("{scheme}://{}{path}", authority.to_ascii_lowercase()))
}

/// The page that hosts `html`.
///
/// The app runs in a sandboxed `srcdoc` frame, so its origin is
/// opaque and its `window.parent` is a real window, as the MCP Apps SDK requires. The page relays
/// the frame's messages, wrapped with `secret`, and delivers Bootty's through
/// `window.__boottyApp(message)`. Once the frame loads a second document, the page stops relaying
/// and says so, rather than letting a page Bootty never served pose as the app.
#[must_use]
pub fn mcp_app_page(html: &str, policy: &McpAppPolicy, secret: &str) -> String {
    let attribute = |value: &str| value.replace('&', "&amp;").replace('"', "&quot;");
    let secret = Value::from(secret).to_string();
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="{policy}">
<style>html,body{{margin:0;height:100%;overflow:hidden;background:transparent}}iframe{{border:0;display:block;width:100%;height:100%}}</style></head>
<body><iframe id="app" sandbox="allow-scripts allow-forms" srcdoc="{document}"></iframe>
<script>(function(){{var frame=document.getElementById("app"),secret={secret},loads=0,live=true;
var send=function(message,activated){{try{{window.ipc.postMessage(JSON.stringify({{secret:secret,activated:activated,message:message}}));}}catch(error){{}}}};
frame.addEventListener("load",function(){{loads+=1;if(loads>1&&live){{live=false;send({{bootty:"navigated"}},false);}}}});
window.addEventListener("message",function(event){{if(live&&event.source===frame.contentWindow){{send(event.data,!!(navigator.userActivation&&navigator.userActivation.isActive));}}}});
window.__boottyApp=function(message){{if(live&&frame.contentWindow)frame.contentWindow.postMessage(message,"*");}};}})();</script></body></html>"#,
        policy = attribute(&policy.content_security_policy()),
        document = attribute(html),
    )
}

/// Something an app page relayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpAppMessage {
    /// A message the app posted, and whether the user had just clicked or typed in it.
    App { text: String, activated: bool },
    /// The app's frame left its document, so the bridge stops.
    Navigated,
}

impl McpAppMessage {
    /// Reads a relayed message. Anything without the page's `secret` is dropped: every frame in
    /// the page can reach the native relay, including frames the app nests.
    #[must_use]
    pub fn parse(body: &str, secret: &str) -> Option<Self> {
        let envelope: Value = serde_json::from_str(body).ok()?;
        if envelope.get("secret").and_then(Value::as_str) != Some(secret) {
            return None;
        }
        let message = envelope.get("message")?;
        if message.get("bootty").and_then(Value::as_str) == Some("navigated") {
            return Some(Self::Navigated);
        }
        Some(Self::App {
            text: message.to_string(),
            activated: envelope.get("activated").and_then(Value::as_bool) == Some(true),
        })
    }
}
