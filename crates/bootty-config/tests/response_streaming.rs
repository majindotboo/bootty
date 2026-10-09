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
