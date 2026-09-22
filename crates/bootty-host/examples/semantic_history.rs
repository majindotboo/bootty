//! Live, synthetic-only comparison: `cargo run -p bootty-host --example semantic_history`
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use bootty_host::{
    semantic_history::{evaluate, rerank},
    shell_history::{HistoryRequest, parse_history, rank_history},
};

fn main() -> Result<()> {
    let api_key = std::env::var("TYPESAFE_API_KEY").context("Set TYPESAFE_API_KEY")?;
    let source = parse_history(
        "bash",
        "git status\nlsof -i :3000\ndu -sh ./*\ngit log --oneline -5\ntar -czf backup.tar.gz src\nfind . -type f -size +100M\nps aux\ncargo nextest run\ncurl -I https://example.com\necho 'find what is listening on a port'\n",
    );
    let cases = [
        ("find what is listening on a port", Some("lsof -i :3000")),
        ("show disk usage for each directory", Some("du -sh ./*")),
        ("show the last few commits", Some("git log --oneline -5")),
        (
            "make a compressed backup of the source folder",
            Some("tar -czf backup.tar.gz src"),
        ),
        (
            "find files larger than a hundred megabytes",
            Some("find . -type f -size +100M"),
        ),
        ("run the Rust tests", Some("cargo nextest run")),
        (
            "show HTTP response headers",
            Some("curl -I https://example.com"),
        ),
        ("schedule a meeting in my calendar", None),
    ];
    let mut literal_hits = 0_usize;
    let mut semantic_hits = 0_usize;
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    for (query, expected) in cases {
        let literal = rank_history(source.clone(), query, "");
        literal_hits = literal_hits.saturating_add(usize::from(expected.is_some_and(|expected| {
            literal
                .first()
                .is_some_and(|entry| entry.command == expected)
        })));
        // An empty retrieval query avoids losing semantic candidates to lexical filtering.
        let history = HistoryRequest {
            shell: "bash".into(),
            path: String::new(),
            query: String::new(),
            cwd: String::new(),
            recent: source.clone(),
        }
        .execute()?;
        let candidate_recall = expected.is_some_and(|expected| {
            history
                .entries
                .iter()
                .any(|entry| entry.command == expected)
        });
        let started = Instant::now();
        let result = rerank(history, query, |body| {
            evaluate(body, &api_key, Duration::from_secs(10))
        })?;
        let best = result
            .entries
            .first()
            .context("synthetic history is nonempty")?;
        semantic_hits = semantic_hits.saturating_add(usize::from(
            expected.is_some_and(|expected| best.entry.command == expected),
        ));
        if let Some(usage) = result.usage {
            input_tokens = input_tokens.saturating_add(usage.input_tokens);
            output_tokens = output_tokens.saturating_add(usage.output_tokens);
        }
        println!(
            "{query}\n  top: {:?}; relevance: {:.3}; latency: {} ms; candidate present: {candidate_recall}; model: {}",
            best.entry.command,
            best.relevance,
            started.elapsed().as_millis(),
            result.model.as_deref().unwrap_or("none")
        );
        if expected.is_none() {
            println!(
                "  No matching command expected; inspect relevance rather than treating rank 1 as a match."
            );
        }
    }
    println!(
        "Top-1 on 7 answerable synthetic queries: local {literal_hits}/7; semantic {semantic_hits}/7. Tokens: {input_tokens} input, {output_tokens} output."
    );
    Ok(())
}
