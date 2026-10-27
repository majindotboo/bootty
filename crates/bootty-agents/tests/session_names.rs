#![cfg(unix)]
#![cfg(test)]

#[path = "support/quick_query.rs"]
pub mod quick_query;
use std::fs;

use assert_fs::TempDir;
use bootty_agents::{GeneratedSessionNames, QuickQuery, generate_session_names_using};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

proptest! {
    #[test]
    fn portable_names_preserve_unicode_titles(title in "[A-Za-z😀漢字]{1,60}", slug in "[a-z][a-z0-9]{0,40}") {
        let names = GeneratedSessionNames { title, slug };
        prop_assert!(names.validate().is_ok());
        let encoded = serde_json::to_vec(&names).unwrap();
        prop_assert_eq!(serde_json::from_slice::<GeneratedSessionNames>(&encoded).unwrap(), names);
    }
}

#[rstest]
#[case("fix-composer", false, true)]
#[case("../escape", false, false)]
#[case("fix-composer", true, false)]
fn naming_uses_a_private_ephemeral_query_and_rejects_invalid_output(
    #[case] slug: &str,
    #[case] cancelled: bool,
    #[case] accepted: bool,
) {
    let root = TempDir::new().expect("temporary account");
    let mut config = quick_query::config(
        root.path(),
        &serde_json::json!({"title":"Fix composer","slug":slug}),
    );
    config.arguments = vec!["--model".into(), "conversation-model".into()];
    let result = generate_session_names_using(
        &config,
        "quick-model",
        "Fix the composer",
        &QuickQuery {
            cancelled: &|| cancelled,
            output: &|command, _| Ok(quick_query::output(&command, "Fix the composer")),
        },
    );
    assert_eq!(result.is_ok(), accepted);
    if cancelled {
        assert!(
            !root.path().join("query-directory").exists(),
            "cancelled queries cannot launch providers"
        );
    } else {
        let path = fs::read_to_string(root.path().join("query-directory"))
            .expect("observed private query");
        assert!(
            !std::path::Path::new(&path).exists(),
            "private query files are removed on success and failure"
        );
    }
    if let Ok(names) = result {
        assert_eq!(names.title, "Fix composer");
    }
}
