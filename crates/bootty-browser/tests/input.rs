use bootty_browser::{BrowserInput, BrowserModifier};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

proptest! {
    #[test]
    fn bounded_unicode_typing_roundtrips(text in "[^\\x00]{1,700}") {
        let action = BrowserInput::Type { text };
        prop_assert!(action.validate().is_ok());
        let encoded = serde_json::to_string(&action).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let decoded: BrowserInput = serde_json::from_str(&encoded).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(decoded, action);
    }
}

#[rstest]
#[case(BrowserInput::Type { text: String::new() })]
#[case(BrowserInput::Type { text: "x".repeat(4097) })]
#[case(BrowserInput::Type { text: "secret\0suffix".into() })]
#[case(BrowserInput::Key { key: "unknown".into(), modifiers: vec![] })]
#[case(BrowserInput::Key { key: "\r".into(), modifiers: vec![] })]
#[case(BrowserInput::Key { key: "a".into(), modifiers: vec![BrowserModifier::Super, BrowserModifier::Super] })]
#[case(BrowserInput::Scroll { x: 0, y: 0, delta_x: i32::MIN, delta_y: 0 })]
fn native_input_rejects_unbounded_or_ambiguous_actions(#[case] action: BrowserInput) {
    assert_eq!(action.validate().is_err(), true);
}

#[rstest]
#[case(r#"{"type":"type","text":"ok","target":"another-window"}"#)]
#[case(r#"{"type":"click","x":-1,"y":0,"button":"left"}"#)]
#[case(r#"{"type":"key","key":"a","modifiers":["unknown"]}"#)]
fn native_input_wire_cannot_override_its_page(#[case] encoded: &str) {
    assert!(serde_json::from_str::<BrowserInput>(encoded).is_err());
}
