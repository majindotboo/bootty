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
