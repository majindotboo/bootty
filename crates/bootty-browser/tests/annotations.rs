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

#[rstest]
#[case("Escape", false, false, Some(bootty_browser::AnnotationAction::Cancel))]
#[case("Escape", true, true, Some(bootty_browser::AnnotationAction::Cancel))]
#[case("Enter", true, false, Some(bootty_browser::AnnotationAction::Paste("Change this heading".into())))]
#[case("Enter", false, false, None)]
#[case("Enter", true, true, None)]
#[case("k", true, false, None)]
fn editor_keyboard_preserves_typing_and_uses_explicit_submit(
    #[case] key: &str,
    #[case] command: bool,
    #[case] shift: bool,
    #[case] expected: Option<bootty_browser::AnnotationAction>,
) {
    let input = serde_json::json!({"action":"key", "value":{"key":key, "command":command, "shift":shift, "comment":"Change this heading"}});
    assert_eq!(
        bootty_browser::AnnotationAction::parse(&input.to_string()),
        expected
    );
}

#[rstest]
#[case("copy")]
#[case("paste")]
fn editor_actions_reject_blank_or_oversized_comments(#[case] action: &str) {
    for comment in [" \n\t".to_owned(), "x".repeat(4097), "🦀".repeat(1025)] {
        let input = serde_json::json!({"action":action, "value":{"comment":comment}});
        assert!(bootty_browser::AnnotationAction::parse(&input.to_string()).is_none());
    }
}

#[rstest]
#[case(serde_json::json!({"action":"cancel"}), Some(bootty_browser::AnnotationAction::Cancel))]
#[case(serde_json::json!({"action":"cancel","execute":true}), None)]
#[case(serde_json::json!({"action":"paste","value":{"comment":"Change this","selector":"#other"}}), None)]
#[case(serde_json::json!({"action":"execute","comment":"Change this"}), None)]
fn editor_dismissal_and_feedback_keep_a_closed_action_contract(
    #[case] input: serde_json::Value,
    #[case] expected: Option<bootty_browser::AnnotationAction>,
) {
    assert_eq!(
        bootty_browser::AnnotationAction::parse(&input.to_string()),
        expected
    );
}

proptest! {
    #[test]
    fn editor_export_preserves_bounded_unicode_comment(comment in "[^\\s].{0,255}") {
        let input = serde_json::json!({"action":"copy", "value":{"comment":comment}});
        prop_assert_eq!(bootty_browser::AnnotationAction::parse(&input.to_string()), Some(bootty_browser::AnnotationAction::Copy(comment)));
    }

    #[test]
    fn scrolled_selection_coordinates_remain_valid(x in -10_000_000i32..=10_000_000i32, y in -10_000_000i32..=10_000_000i32, width in 0i32..=10_000_000i32, height in 0i32..=10_000_000i32) {
        let message = serde_json::json!({"url":"https://example.com/", "selector":"#title", "text":"Title", "tag":"h1", "bounds":[x, y, width, height]}).to_string();
        let selection = BrowserElement::parse(&message).ok_or_else(|| TestCaseError::fail("finite offscreen geometry rejected"))?;
        prop_assert_eq!(selection.bounds.map(f64::to_bits), [f64::from(x),f64::from(y),f64::from(width),f64::from(height)].map(f64::to_bits));
    }
}
