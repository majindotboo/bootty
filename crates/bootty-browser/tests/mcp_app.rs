//! An MCP app's page: the policy it runs under, and the relay that speaks for its frame alone.

use bootty_browser::{McpAppMessage, McpAppPolicy, mcp_app_page};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::json;

fn policy(connect: &[&str], resources: &[&str]) -> McpAppPolicy {
    let owned = |domains: &[&str]| {
        domains
            .iter()
            .map(|domain| (*domain).to_owned())
            .collect::<Vec<_>>()
    };
    McpAppPolicy::new(&owned(connect), &owned(resources))
}

/// A declared origin becomes a source as written, and anything that isn't a plain origin is
/// dropped rather than trusted.
#[rstest]
#[case::bare_host("API.example.com", Some("https://api.example.com"))]
#[case::https("https://api.example.com", Some("https://api.example.com"))]
#[case::subdomains("https://*.cdn.example.com", Some("https://*.cdn.example.com"))]
#[case::socket("wss://live.example.com:8443", Some("wss://live.example.com:8443"))]
#[case::path("https://example.com/static/", Some("https://example.com/static/"))]
#[case::any_port("http://localhost:*", Some("http://localhost:*"))]
#[case::keyword("'self'", None)]
#[case::everything("*", None)]
#[case::every_host("https://*", None)]
#[case::directive("example.com; script-src *", None)]
#[case::scheme("data:", None)]
#[case::script("javascript:alert(1)", None)]
#[case::empty_label("https://example..com", None)]
#[case::empty_port("https://example.com:", None)]
fn declared_origins_become_plain_sources(#[case] domain: &str, #[case] source: Option<&str>) {
    let sandbox = policy(&[domain], &[]).sandbox();
    assert_eq!(
        sandbox,
        json!({"csp": {"connectDomains": source.into_iter().collect::<Vec<_>>(), "resourceDomains": []}})
    );
}

#[rstest]
fn the_policy_allows_only_declared_origins() {
    assert_eq!(
        policy(&[], &[]).content_security_policy(),
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
         img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; \
         base-uri 'none'; form-action 'none'"
    );
    assert_eq!(
        policy(&["https://api.example.com"], &["https://cdn.example.com"])
            .content_security_policy(),
        "default-src 'none'; script-src 'unsafe-inline' https://cdn.example.com; \
         style-src 'unsafe-inline' https://cdn.example.com; \
         img-src data: blob: https://cdn.example.com; font-src data: https://cdn.example.com; \
         media-src data: blob: https://cdn.example.com; connect-src https://api.example.com; \
         base-uri 'none'; form-action 'none'"
    );
}

proptest! {
    /// No declared origin can add a directive, a keyword or a scheme-wide source.
    #[test]
    fn declared_origins_cannot_widen_the_policy(
        connect in prop::collection::vec(any::<String>(), 0..4),
        resources in prop::collection::vec("[a-z*:/.;' -]{0,24}", 0..4),
    ) {
        let built = McpAppPolicy::new(&connect, &resources).content_security_policy();
        let fixed = policy(&[], &[]).content_security_policy();
        prop_assert_eq!(built.matches(';').count(), fixed.matches(';').count());
        for keyword in built.split([';', ' ']).filter(|part| part.contains('\'')) {
            prop_assert!(matches!(keyword, "'none'" | "'unsafe-inline'"), "{keyword}");
        }
        for source in built.split([';', ' ']).filter(|part| part.contains("://")) {
            let host = source.split("://").nth(1).unwrap_or_default();
            prop_assert!(!host.starts_with('*') || host.starts_with("*."), "{source}");
            prop_assert!(!host.is_empty() && !host.starts_with('/'), "{source}");
        }
    }
}

/// The app's document sits in an attribute, so its quotes can't end it early.
#[rstest]
fn the_page_holds_the_document_in_a_sandboxed_frame() {
    let page = mcp_app_page(
        r#"<p title="a & b">x</p>"#,
        &policy(&[], &["https://cdn.example.com"]),
        "secret-1",
    );
    assert!(page.contains(
        r#"<iframe id="app" sandbox="allow-scripts allow-forms" srcdoc="<p title=&quot;a &amp; b&quot;>x</p>"></iframe>"#
    ));
    assert!(page.contains(
        r#"<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline' https://cdn.example.com;"#
    ));
    assert!(page.contains(r#"secret="secret-1""#));
}

/// Every frame in the page can reach the native relay, so only the page's secret speaks for the
/// app.
#[rstest]
#[case::app(
    r#"{"secret": "s", "activated": true, "message": {"jsonrpc": "2.0", "method": "ping"}}"#,
    Some(McpAppMessage::App { text: r#"{"jsonrpc":"2.0","method":"ping"}"#.to_owned(), activated: true }),
)]
#[case::unprompted(
    r#"{"secret": "s", "message": {"jsonrpc": "2.0"}}"#,
    Some(McpAppMessage::App { text: r#"{"jsonrpc":"2.0"}"#.to_owned(), activated: false }),
)]
#[case::navigated(
    r#"{"secret": "s", "message": {"bootty": "navigated"}}"#,
    Some(McpAppMessage::Navigated)
)]
#[case::wrong_secret(r#"{"secret": "guess", "activated": true, "message": {}}"#, None)]
#[case::no_secret(r#"{"message": {"bootty": "navigated"}}"#, None)]
#[case::focus("browser-focus", None)]
fn only_the_page_speaks_for_the_app(#[case] body: &str, #[case] expected: Option<McpAppMessage>) {
    assert_eq!(McpAppMessage::parse(body, "s"), expected);
}
