use bootty_host::{
    semantic_history::rerank,
    shell_history::{HistoryEntry, HistoryResult},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;
use serde_json::{Value, json};

#[derive(Debug, Arbitrary)]
struct Candidate {
    #[proptest(strategy = "\"[a-z][a-z0-9 ]{0,40}\"")]
    command: String,
    #[proptest(strategy = "0_u16..=1000")]
    score: u16,
    frequency: u32,
}

fn history(commands: &[String]) -> HistoryResult {
    HistoryResult {
        entries: commands
            .iter()
            .map(|command| HistoryEntry {
                command: command.clone(),
                cwd: Some("/private/project".into()),
                timestamp: Some(42),
                exit_code: Some(0),
                frequency: 1,
            })
            .collect(),
        truncated: false,
    }
}

fn response(answers: &Value) -> Vec<u8> {
    json!({
        "model": "jev-1.13.0",
        "answers": answers,
        "usage": {"input_tokens": 10, "output_tokens": 5}
    })
    .to_string()
    .into_bytes()
}

proptest! {
    #[test]
    fn scores_only_reorder_original_entries(candidates in prop::collection::vec(any::<Candidate>(), 1..=50)) {
        let mut source = history(&candidates.iter().map(|candidate| candidate.command.clone()).collect::<Vec<_>>());
        for (entry, candidate) in source.entries.iter_mut().zip(&candidates) {
            entry.frequency = candidate.frequency;
        }
        let mut expected: Vec<_> = source.entries.iter().cloned().zip(&candidates).collect();
        expected.sort_by_key(|(_, candidate)| std::cmp::Reverse(candidate.score));
        let result = rerank(source, "find the relevant command", |body| {
            let body: Value = serde_json::from_slice(body)?;
            // Metadata is useful locally, but it is not needed by this semantic judgment.
            assert_eq!(body["state"], json!({
                "query": "find the relevant command",
                "commands": candidates.iter().map(|candidate| &candidate.command).collect::<Vec<_>>()
            }));
            let answers = candidates.iter().enumerate().map(|(index, candidate)| {
                (index.to_string(), json!({"type":"noul", "noul": f64::from(candidate.score) / 1000.0}))
            }).collect::<serde_json::Map<_, _>>();
            Ok(response(&Value::Object(answers)))
        }).unwrap();
        assert_eq!(result.truncated, false);
        assert_eq!(result.entries.len(), expected.len());
        for (actual, (entry, candidate)) in result.entries.iter().zip(expected) {
            assert_eq!(&actual.entry, &entry);
            assert_eq!(actual.relevance.to_bits(), (f64::from(candidate.score) / 1000.0).to_bits());
        }
    }
}

#[rstest]
#[case(json!({}))]
#[case(json!({"wrong": {"type":"noul", "noul":0.9}}))]
#[case(json!({"0": {"type":"noul", "noul":1.1}}))]
#[case(json!({"0": {"type":"noul", "noul":-0.1}}))]
#[case(json!({"0": {"type":"noul", "noul":null}}))]
#[case(json!({"0": {"type":"choice", "choice":"invented command"}}))]
#[case(json!({"0": {"type":"noul", "noul":0.9}, "1": {"type":"noul", "noul":0.5}}))]
fn malformed_answers_never_become_history(#[case] answers: Value) {
    assert!(
        rerank(history(&["git status".into()]), "show changes", |_| Ok(
            response(&answers)
        ))
        .is_err()
    );
}

#[rstest]
#[case(60, 10, 50)]
#[case(10, 4096, 4)]
fn candidate_upload_is_bounded(
    #[case] count: usize,
    #[case] length: usize,
    #[case] accepted: usize,
) {
    let result = rerank(
        history(&vec!["x".repeat(length); count]),
        "find a command",
        |body| {
            let body: Value = serde_json::from_slice(body)?;
            assert_eq!(
                body["state"]["commands"].as_array().unwrap().len(),
                accepted
            );
            let answers = (0..accepted)
                .map(|index| (index.to_string(), json!({"type":"noul", "noul":0.0})))
                .collect::<serde_json::Map<_, _>>();
            Ok(response(&Value::Object(answers)))
        },
    )
    .unwrap();
    assert!(result.truncated);
    assert_eq!(result.entries.len(), accepted);
}

#[rstest]
#[case("")]
#[case(" ")]
#[case(&"x".repeat(4097))]
fn invalid_queries_do_not_send_history(#[case] query: &str) {
    assert!(
        rerank(history(&["pwd".into()]), query, |_| panic!(
            "must not upload"
        ))
        .is_err()
    );
}

#[rstest]
fn empty_or_excluded_history_does_not_call_the_model() {
    let result = rerank(
        history(&[" hidden".into(), "\u{1b}bad".into(), String::new()]),
        "show changes",
        |_| panic!("must not upload"),
    )
    .unwrap();
    assert!(result.entries.is_empty());
    assert!(result.truncated);
    assert_eq!(result.model, None);
}

#[rstest]
#[case(None)]
#[case(Some(b"not json".to_vec()))]
#[case(Some(vec![b' '; 64 * 1024 + 1]))]
fn service_failures_are_errors_instead_of_fabricated_rankings(#[case] body: Option<Vec<u8>>) {
    let result = rerank(history(&["pwd".into()]), "show changes", |_| {
        body.ok_or_else(|| anyhow::anyhow!("transport failed"))
    });
    assert!(result.is_err());
}
