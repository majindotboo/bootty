use bootty_agents::{NativeResponseCitation, NativeTranscriptItem};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn response(text: &str, role: &str) -> NativeTranscriptItem {
    NativeTranscriptItem {
        id: "response-1".to_owned(),
        role: role.to_owned(),
        text: text.to_owned(),
        complete: true,
        created_at: None,
        updated_at: None,
        tool: None,
        subagent: None,
        images: Vec::new(),
        attachments: Vec::new(),
        citations: Vec::new(),
        applications: Vec::new(),
        questions: Vec::new(),
    }
}

proptest! {
    #[test]
    fn inline_quote_renders_at_its_editor_range_without_replacing_literal_text(prefix in "[a-zé界😀 ]{0,32}") {
        let start = prefix.len().saturating_add("[quote] is literal; ".len());
        let text = format!("{prefix}[quote] is literal; [quote] please revise");
        let citation = NativeResponseCitation {
            message_id: "response-1".to_owned(),
            source_range: 0..5,
            prompt_range: Some(start..start.saturating_add(7)),
            quote: "claim".to_owned(),
            comment: "Clarify".to_owned(),
        };
        let mut item = response(&text, "user");
        item.citations = vec![citation.clone()];
        prop_assert_eq!(item.display_text(), format!("{prefix}[quote] is literal; [❝ Clarify](bootty-citation:0) please revise"));
        prop_assert!(bootty_agents::NativePrompt::new_with_context(text, vec![], bootty_agents::NativePromptAttachments::default(), vec![citation]).is_ok());
    }

    #[test]
    fn accepts_exact_source_bytes_after_unicode_prefix(prefix in "[a-zé界😀 ]{0,32}") {
        let quote = "**quoted response**";
        let source = format!("{prefix}{quote} and {quote}");
        let start = prefix.len().saturating_add(quote.len()).saturating_add(" and ".len());
        let citation = NativeResponseCitation {
            message_id: "response-1".to_owned(),
            source_range: start..start.saturating_add(quote.len()),
            prompt_range: None,
            quote: quote.to_owned(),
            comment: "Revise this part".to_owned(),
        };
        prop_assert_eq!(citation.validate(&[response(&source, "assistant")]), Ok(()));
        let encoded = serde_json::to_string(&citation).expect("encode citation");
        let decoded: NativeResponseCitation = serde_json::from_str(&encoded).expect("decode citation");
        prop_assert_eq!(decoded, citation);
    }
}

#[rstest]
#[case(0..7)]
#[case(20..27)]
#[case(1..2)]
fn rejects_quote_tokens_that_do_not_match_the_authored_prompt(
    #[case] range: std::ops::Range<usize>,
) {
    let citation = NativeResponseCitation {
        message_id: "response-1".to_owned(),
        source_range: 0..5,
        prompt_range: Some(range),
        quote: "claim".to_owned(),
        comment: String::new(),
    };
    assert!(
        bootty_agents::NativePrompt::new_with_context(
            "literal [quote]".to_owned(),
            vec![],
            bootty_agents::NativePromptAttachments::default(),
            vec![citation]
        )
        .is_err()
    );
}

#[rstest]
#[case("assistant", "response-1", 1..4, "界")]
#[case("assistant", "response-1", 0..0, "界")]
#[case("assistant", "response-1", 0..100, "界")]
#[case("assistant", "response-1", 0..3, "altered")]
#[case("assistant", "another-conversation", 0..3, "界")]
#[case("user", "response-1", 0..3, "界")]
fn rejects_unavailable_or_altered_response_quotes(
    #[case] role: &str,
    #[case] id: &str,
    #[case] range: std::ops::Range<usize>,
    #[case] quote: &str,
) {
    let citation = NativeResponseCitation {
        message_id: id.to_owned(),
        source_range: range,
        prompt_range: None,
        quote: quote.to_owned(),
        comment: String::new(),
    };
    assert_eq!(
        citation.validate(&[response("界 response", role)]).is_err(),
        true
    );
}

#[rstest]
#[case(8_000, 8_000, true)]
#[case(8_001, 0, false)]
#[case(1, 8_001, false)]
fn bounds_quote_and_comment_before_provider_submission(
    #[case] quote_bytes: usize,
    #[case] comment_bytes: usize,
    #[case] admitted: bool,
) {
    let quote = "q".repeat(quote_bytes);
    let citation = NativeResponseCitation {
        message_id: "response-1".to_owned(),
        source_range: 0..quote_bytes,
        prompt_range: None,
        quote: quote.clone(),
        comment: "c".repeat(comment_bytes),
    };
    assert_eq!(
        citation.validate(&[response(&quote, "assistant")]).is_ok(),
        admitted
    );
}

proptest! {
    #[test]
    fn inline_files_preserve_literal_text_and_quote_coordinates(prefix in "[a-zé界😀 ]{0,32}") {
        let literal = "[notes.md] stays literal; ";
        let start = prefix.len().saturating_add(literal.len());
        let quote_start = start.saturating_add("[notes.md] then ".len());
        let mut item = response(&format!("{prefix}{literal}[notes.md] then [quote]"), "user");
        item.attachments = vec![bootty_agents::NativeAttachmentReference {
            id: "file-id".into(), kind: bootty_agents::NativeAttachmentKind::File,
            name: "notes.md".into(), mime_type: "text/markdown".into(), size_bytes: 10,
            pixel_width: None, pixel_height: None, prompt_ranges: std::iter::once(start..start.saturating_add(10)).collect(),
        }];
        item.citations = vec![NativeResponseCitation {
            message_id: "response-1".into(), source_range: 0..5,
            prompt_range: Some(quote_start..quote_start.saturating_add(7)),
            quote: "claim".into(), comment: "Clarify".into(),
        }];
        prop_assert_eq!(item.display_text(), format!("{prefix}{literal}` notes.md ` then [❝ Clarify](bootty-citation:0)"));
        let saved: NativeTranscriptItem = serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();
        prop_assert_eq!(saved.display_text(), item.display_text());
    }
}

/// An image shows as its own preview, so its token leaves the message text; a file keeps its
/// name in place.
#[rstest]
#[case::image(bootty_agents::NativeAttachmentKind::Image, "Look at  and fix it")]
#[case::file(
    bootty_agents::NativeAttachmentKind::File,
    "Look at ` shot.png ` and fix it"
)]
fn image_tokens_leave_the_text_and_file_tokens_stay(
    #[case] kind: bootty_agents::NativeAttachmentKind,
    #[case] expected: &str,
) {
    let mut item = response("Look at [shot.png] and fix it", "user");
    item.attachments = vec![bootty_agents::NativeAttachmentReference {
        id: "shot".into(),
        kind,
        name: "shot.png".into(),
        mime_type: "image/png".into(),
        size_bytes: 10,
        pixel_width: None,
        pixel_height: None,
        prompt_ranges: std::iter::once(8..18).collect(),
    }];
    assert_eq!(item.display_text(), expected);
}

#[rstest]
#[case(
    "Handled. :codex-annotation{index=\"1\"}",
    "Handled. [Annotation 1](bootty-annotation:1)"
)]
#[case(
    "` :codex-annotation{index=\"1\"} `",
    "` :codex-annotation{index=\"1\"} `"
)]
#[case(
    "```text\n:codex-annotation{index=\"1\"}\n```",
    "```text\n:codex-annotation{index=\"1\"}\n```"
)]
#[case("\\:codex-annotation{index=\"1\"}", "\\:codex-annotation{index=\"1\"}")]
#[case(
    "[ :codex-annotation{index=\"1\"} ](https://example.com)",
    "[ :codex-annotation{index=\"1\"} ](https://example.com)"
)]
#[case(":codex-annotation{index=\"0\"}", ":codex-annotation{index=\"0\"}")]
#[case(
    ":codex-annotation{index=\"1\"} 界 :codex-annotation{index=\"2\"}",
    "[Annotation 1](bootty-annotation:1) 界 [Annotation 2](bootty-annotation:2)"
)]
fn assistant_annotation_references_render_without_rewriting_code(
    #[case] text: &str,
    #[case] expected: &str,
) {
    assert_eq!(response(text, "assistant").display_text(), expected);
    assert_eq!(response(text, "user").display_text(), text);
}

#[rstest]
fn annotation_envelopes_render_selected_text_and_user_comment() {
    let text = "Before <response-annotations>[{\"text\":\"Selected text\",\"annotation\":\"Please fix it\",\"source\":{\"messageId\":\"earlier\"}}]</response-annotations> After";
    let annotations = bootty_agents::response_annotations(text);
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0].text, "Selected text");
    assert_eq!(annotations[0].annotation, "Please fix it");
    assert_eq!(
        response(text, "user").display_text(),
        "Before \n\n**Annotation 1**\n\n> Selected text\n\nPlease fix it After"
    );
    let malformed = "<response-annotations>not json</response-annotations>";
    assert_eq!(
        bootty_agents::response_annotations(malformed),
        Vec::<bootty_agents::NativeResponseAnnotation>::new()
    );
    assert_eq!(response(malformed, "user").display_text(), malformed);
}
