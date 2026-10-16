use bootty_config::config::ResponseStreaming;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

proptest! {
    #[test]
    fn completed_responses_preserve_every_byte(text in any::<String>()) {
        for mode in [ResponseStreaming::Live, ResponseStreaming::Paragraph, ResponseStreaming::Complete] {
            assert_eq!(mode.visible_text(&text, true), text.as_str());
        }
    }
}

#[rstest]
#[case("First paragraph.\n\nStill writing", "First paragraph.\n\n")]
#[case("```rust\nlet x = 1;\n\n", "")]
#[case("First.\n\n```rust\nlet x = 1;\n\n", "First.\n\n")]
#[case("```rust\nlet x = 1;\n```\n\nAfter", "```rust\nlet x = 1;\n```\n\n")]
#[case("日本語\n\n未完了", "日本語\n\n")]
#[case("````rust\n```\n\n", "")]
#[case("```rust\n``` still code\n\n", "")]
#[case("~~~rust\n~~~~\n\nAfter", "~~~rust\n~~~~\n\n")]
fn streaming_publishes_complete_markdown_blocks(#[case] text: &str, #[case] shown: &str) {
    assert_eq!(
        ResponseStreaming::Paragraph.visible_text(text, false),
        shown
    );
    assert_eq!(ResponseStreaming::Complete.visible_text(text, false), "");
}

/// Live streaming holds back an inline span until its closing marker arrives, so markers never
/// show raw and then change; code blocks and finished spans show as they come. A lone marker at
/// the end waits for the next character, which may pair it, and a bracket waits until it can't
/// become a link.
#[rstest]
#[case("Use **bold", "Use ")]
#[case("Use **bold** now", "Use **bold** now")]
#[case("Run `cargo t", "Run ")]
#[case("Run `cargo test` then ~~old", "Run `cargo test` then ")]
#[case("See [the docs](https://exa", "See ")]
#[case(
    "See [the docs](https://example.com) and",
    "See [the docs](https://example.com) and"
)]
#[case("A list:\n* one **two", "A list:\n* one ")]
#[case("```rust\nlet a = **b", "```rust\nlet a = **b")]
#[case("snake_case and 2 * 3", "snake_case and 2 * 3")]
#[case("Not that ~", "Not that ")]
#[case("See [the docs]", "See ")]
#[case("See [1] and [2", "See [1] and ")]
fn live_streaming_holds_unclosed_spans(#[case] text: &str, #[case] shown: &str) {
    assert_eq!(ResponseStreaming::Live.visible_text(text, false), shown);
}

proptest! {
    /// As a reply grows, live streaming only ever adds to what it showed.
    #[test]
    fn live_streaming_never_takes_back_what_it_showed(
        text in "[a *_~`\\[\\]()\\\\\n]{0,40}",
        split in 0usize..40,
    ) {
        let split = split.min(text.len());
        // The alphabet is ASCII, so every split is a character boundary.
        let earlier = ResponseStreaming::Live.visible_text(text.get(..split).unwrap_or_default(), false);
        let later = ResponseStreaming::Live.visible_text(&text, false);
        prop_assert!(later.starts_with(earlier), "{earlier:?} then {later:?}");
    }
}
