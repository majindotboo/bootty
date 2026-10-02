use bootty_browser::BrowserElement;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[fixture]
fn selection() -> serde_json::Value {
    serde_json::json!({"url":"https://example.com/", "selector":"#title", "text":"Page title", "tag":"h1", "bounds":[10.0, 20.0, 100.0, 30.0]})
}

#[rstest]
fn reviewed_feedback_preserves_user_comment_and_quotes_page_data(selection: serde_json::Value) {
    let element = BrowserElement::parse(&selection.to_string()).expect("bounded element");
    assert_eq!(element.selector, "#title");
    let feedback = element.feedback(" Increase the heading size ");
    assert!(feedback.contains("Selector (page data): \"#title\""));
    assert!(feedback.ends_with("Requested change:\nIncrease the heading size"));
}

#[rstest]
#[case("javascript:alert(1)")]
#[case("file:///tmp/page.html")]
#[case("about:blank")]
#[case("https://user:password@example.com")]
fn selections_reject_unsafe_addresses(mut selection: serde_json::Value, #[case] address: &str) {
    selection["url"] = address.into();
    assert!(BrowserElement::parse(&selection.to_string()).is_none());
}

#[rstest]
#[case("selector", 513)]
#[case("text", 2049)]
#[case("tag", 49)]
#[case("url", 4097)]
fn oversized_page_fields_are_rejected(
    mut selection: serde_json::Value,
    #[case] field: &str,
    #[case] size: usize,
) {
    selection[field] = "x".repeat(size).into();
    assert!(BrowserElement::parse(&selection.to_string()).is_none());
}

#[rstest]
#[case(serde_json::json!([0, 0, -1, 20]))]
#[case(serde_json::json!([0, 0, 20, -1]))]
#[case(serde_json::json!([0, 0, 20]))]
#[case(serde_json::json!([0, 0, 1e100, 20]))]
fn malformed_geometry_is_rejected(
    mut selection: serde_json::Value,
    #[case] bounds: serde_json::Value,
) {
    selection["bounds"] = bounds;
    assert!(BrowserElement::parse(&selection.to_string()).is_none());
}

proptest! {
    #[test]
    fn bounded_unicode_page_text_survives_native_selection(text in ".{0,256}") {
        let message = serde_json::json!({"url":"https://example.com/", "selector":"#title", "text":text, "tag":"h1", "bounds":[0, 0, 100, 30]}).to_string();
        let element = BrowserElement::parse(&message).ok_or_else(|| TestCaseError::fail("valid bounded selection rejected"))?;
        prop_assert_eq!(element.text, text);
    }
}
