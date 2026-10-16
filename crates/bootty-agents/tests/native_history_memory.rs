use bootty_agents::{NativeHistoryPage, NativeTranscriptItem};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn page(ids: &[&str], latest: bool) -> Result<NativeHistoryPage, serde_json::Error> {
    Ok(NativeHistoryPage {
        transcript: ids
            .iter()
            .map(|id| {
                serde_json::from_value::<NativeTranscriptItem>(
                    serde_json::json!({"id":id,"role":"assistant","text":id,"complete":true}),
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
        has_older: ids.first() != Some(&"first"),
        has_newer: !latest,
        at_latest: latest,
    })
}

#[rstest]
#[case(false, vec!["first", "middle"])]
#[case(true, vec!["middle", "last"])]
fn adjacent_pages_retain_loaded_turns_and_deduplicate_overlap(
    #[case] latest: bool,
    #[case] incoming: Vec<&str>,
) {
    let loaded = if latest {
        page(&["first", "middle"], false).unwrap()
    } else {
        page(&["middle", "last"], true).unwrap()
    };
    let mut next = page(&incoming, latest).unwrap();
    next.retain_loaded(&loaded);
    assert_eq!(
        next.transcript
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "middle", "last"]
    );
    assert_eq!(next.has_older, false);
    assert_eq!(next.at_latest, true);
    assert_eq!(next.has_newer, false);
}

#[rstest]
#[case(
    "assistant",
    true,
    "beginning and the complete reply",
    "beginning and the complete reply"
)]
#[case(
    "thinking",
    true,
    "beginning and the complete reply",
    "beginning and the complete reply"
)]
#[case("assistant", false, "beginning and the complete reply", "cached")]
#[case("user", true, "beginning and the complete reply", "cached")]
#[case("tool", true, "beginning and the complete reply", "cached")]
#[case("assistant", true, "a changed reply", "cached")]
fn provider_history_recovers_only_matching_completed_truncated_replies(
    #[case] role: &str,
    #[case] complete: bool,
    #[case] provider_text: &str,
    #[case] expected: &str,
) {
    let mut history = page(&["first", "reply"], true).unwrap();
    history.transcript[1].role = role.into();
    history.transcript[1].text = provider_text.into();
    let mut cached = history.transcript[1].clone();
    cached.text = "beginning\n[This recent transcript view truncates output after 16 KiB]".into();
    cached.complete = complete;
    let merged = history.with_live_transcript(&[cached.clone()]);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0], history.transcript[0]);
    let mut restored = cached;
    if expected != "cached" {
        restored.text = expected.into();
    }
    assert_eq!(merged[1], restored);
}

/// History and the live view merge in the provider's order: a reply the live view let go stays
/// between the prompts it kept, and live items past the page follow it.
#[rstest]
#[case::gap_in_the_live_view(
    &["u1", "r1", "u2", "r2", "u3", "r3"],
    &["u1", "u2", "r2", "u3", "r3", "r4"],
    &["u1", "r1", "u2", "r2", "u3", "r3", "r4"]
)]
#[case::older_page(&["a", "b"], &["c", "d"], &["a", "b", "c", "d"])]
#[case::live_only_between(&["u1", "r1"], &["u1", "note", "r1"], &["u1", "note", "r1"])]
fn history_and_live_items_merge_in_provider_order(
    #[case] history: &[&str],
    #[case] live: &[&str],
    #[case] expected: &[&str],
) {
    let history = page(history, true).unwrap();
    let live = page(live, true).unwrap().transcript;
    let merged = history.with_live_transcript(&live);
    assert_eq!(
        merged
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        expected
    );
}
