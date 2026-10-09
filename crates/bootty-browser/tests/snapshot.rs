use bootty_browser::{BrowserDocumentSnapshot, valid_document_token};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::json;

const DOCUMENT: &str = "1234567890abcdef1234567890abcdef";
const ADDRESS: &str = "https://example.com/article";

proptest! {
    #[test]
    fn snapshot_preserves_bounded_unicode_text(text in ".{0,1000}", title in ".{0,100}") {
        let expected = BrowserDocumentSnapshot {
            document: DOCUMENT.into(), address: ADDRESS.into(), title, text, truncated: false,
        };
        let encoded = serde_json::to_string(&expected).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let actual = BrowserDocumentSnapshot::parse(&encoded, DOCUMENT, ADDRESS)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(actual, expected);
    }
}

#[rstest]
#[case("document", json!("abcdef1234567890abcdef1234567890"))]
#[case("address", json!("https://example.com/other"))]
#[case("title", json!("x".repeat(1025)))]
#[case("text", json!("x".repeat(64 * 1024 + 1)))]
#[case("password", json!("page-controlled-extra-field"))]
fn changed_or_oversized_snapshots_are_rejected(
    #[case] field: &str,
    #[case] value: serde_json::Value,
) {
    let mut response = json!({"document":DOCUMENT,"address":ADDRESS,"title":"Article","text":"Visible text","truncated":false});
    response[field] = value;
    assert!(BrowserDocumentSnapshot::parse(&response.to_string(), DOCUMENT, ADDRESS).is_err());
}

#[rstest]
fn escaped_byte_limit_accepts_the_full_decoded_text_limit() {
    let response = json!({"document":DOCUMENT,"address":ADDRESS,"title":"","text":"\0".repeat(64 * 1024),"truncated":true});
    let snapshot =
        BrowserDocumentSnapshot::parse(&response.to_string(), DOCUMENT, ADDRESS).unwrap();
    assert_eq!(snapshot.text.len(), 64 * 1024);
    assert!(snapshot.truncated);
}

#[rstest]
#[case("")]
#[case("1234567890abcdef")]
#[case("1234567890abcdef1234567890abcdeg")]
#[case("1234567890abcdef1234567890abcdef\n")]
fn only_document_start_tokens_are_accepted(#[case] token: &str) {
    assert!(!valid_document_token(token));
}
