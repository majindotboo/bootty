use bootty_ui::completion::{CompletionKind, CompletionTrigger};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case(" /rev", 5, Some(CompletionKind::Command), "rev")]
#[case("work $skill", 11, Some(CompletionKind::Skill), "skill")]
#[case("open @src/main.rs", 9, Some(CompletionKind::Mention), "src")]
#[case("name@example.com", 16, None, "")]
#[case("work /review", 12, None, "")]
#[case("\n/review", 8, None, "")]
#[case("$$HOME", 6, None, "")]
#[case("è @résumé", 10, Some(CompletionKind::Mention), "résum")]
fn authored_triggers_keep_utf8_source_ranges(
    #[case] text: &str,
    #[case] cursor: usize,
    #[case] kind: Option<CompletionKind>,
    #[case] query: &str,
) {
    let trigger = CompletionTrigger::at(text, cursor..cursor);
    assert_eq!(trigger.as_ref().map(|trigger| trigger.kind), kind);
    if let Some(trigger) = trigger {
        assert_eq!(trigger.query, query);
        assert!(text.get(trigger.range).is_some());
    }
}
proptest! {
    #[test]
    fn selections_never_open_completion(text in ".{0,80}",selected in ".{1,40}") {
        let editor=format!("{text}@{selected}");
        prop_assert!(CompletionTrigger::at(&editor,text.len()..editor.len()).is_none());
    }
    #[test]
    fn full_skill_token_is_replaced_when_cursor_is_in_the_middle(prefix in "[a-z]{1,12}",suffix in "[a-z]{1,12}") {
        let text=format!("${prefix}{suffix}");
        let trigger=CompletionTrigger::at(&text,prefix.len().saturating_add(1)..prefix.len().saturating_add(1)).unwrap();
        prop_assert_eq!(trigger.query,prefix);
        prop_assert_eq!(trigger.range,0..text.len());
    }
}

#[rstest]
fn command_ranking_prefers_names_and_does_not_fuzzy_match_descriptions() {
    let trigger = CompletionTrigger::at("/compact", 8..8).unwrap();
    let exact = trigger
        .score("/compact", "Summarize the conversation")
        .unwrap();
    let description = trigger.score("/context", "Show compact context").unwrap();
    assert!(exact > description);
    assert_eq!(
        trigger.score("/help", "Choose options, manage profiles and tasks"),
        None
    );
    assert!(
        CompletionTrigger::at("/cmpct", 6..6)
            .unwrap()
            .score("/compact", "")
            .is_some()
    );
}

proptest! {
    #[test]
    fn empty_completion_queries_preserve_catalog_order(name in ".{0,60}", description in ".{0,80}") {
        let trigger = CompletionTrigger::at("/", 1..1).unwrap();
        prop_assert_eq!(trigger.score(&name, &description), Some(0));
    }
}
