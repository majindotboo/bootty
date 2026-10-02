use bootty_mobile::LiveWorkspace;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

fn listing(generation: u64, handle: &str) -> Value {
    json!([{"scope":"space", "name":"Project", "backend":"native", "host":"Local",
        "target":{"kind":"binding", "handle":"space target", "generation":"1"},
        "sessions":[{"name":"Shell", "target":{"kind":"session", "handle":handle, "generation":generation.to_string()},
            "terminal_target":{"kind":"terminal", "handle":"terminal target", "generation":"2"}}]}])
}

proptest! {
    #[test]
    fn desktop_targets_remain_opaque_and_generations_are_lossless(generation in any::<u64>(), handle in ".{1,40}") {
        let spaces = LiveWorkspace::decode_spaces(listing(generation, &handle)).unwrap();
        let session = &spaces.first().unwrap().sessions.first().unwrap();
        assert_eq!(&session.target.handle, &handle);
        assert_eq!(session.target.generation, generation.to_string());
        prop_assert!(!session.topology_supported);
        prop_assert!(session.windows.is_empty());
    }
}

#[rstest]
#[case("pane")]
#[case("binding")]
#[case("unknown")]
fn incompatible_session_targets_are_rejected(#[case] kind: &str) {
    let mut value = listing(1, "session");
    value[0]["sessions"][0]["target"]["kind"] = json!(kind);
    assert!(LiveWorkspace::decode_spaces(value).is_err());
}

#[rstest]
fn duplicate_sessions_are_rejected_instead_of_addressing_the_wrong_process() {
    let mut value = listing(1, "session");
    let session = value[0]["sessions"][0].clone();
    value[0]["sessions"].as_array_mut().unwrap().push(session);
    assert!(LiveWorkspace::decode_spaces(value).is_err());
}

#[rstest]
fn closed_or_replaced_terminal_targets_are_not_captured() {
    let spaces = LiveWorkspace::decode_spaces(listing(1, "session")).unwrap();
    let terminal = spaces
        .first()
        .unwrap()
        .sessions
        .first()
        .unwrap()
        .terminal_target
        .clone()
        .unwrap();
    assert!(LiveWorkspace::contains_terminal(&spaces, &terminal));
    assert!(!LiveWorkspace::contains_terminal(&[], &terminal));
    let mut replaced = terminal;
    replaced.generation = "3".into();
    assert!(!LiveWorkspace::contains_terminal(&spaces, &replaced));
}
