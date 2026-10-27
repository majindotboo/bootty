#![cfg(unix)]

use std::fs;

use assert_fs::TempDir;
use assert_fs::prelude::*;
use pretty_assertions::assert_eq;
use serde_json::Value;

#[test]
fn hostile_soak_records_all_three_cases() {
    let temp = TempDir::new().unwrap();
    let output = temp.child("output");
    xtasks::benchmark::hostile::run_with(
        xtasks::benchmark::hostile::Args {
            output_dir: Some(output.path().to_owned()),
        },
        10,
        10,
        |command, _deadline| {
            assert_eq!(command.get_program(), "cargo");
            Ok(xtasks::benchmark::hostile::Outcome::Exited(
                std::process::ExitStatus::default(),
            ))
        },
    )
    .unwrap();
    let records = fs::read_to_string(output.child("summary.jsonl").path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["name"], "hostile_mixed_soak_256_rounds");
    assert_eq!(records[1]["name"], "hostile_extended_recovery_ladder");
    assert_eq!(records[2]["name"], "hostile_long_line_16mb_write");
    assert!(records.iter().all(|record| record["status"] == "pass"));
}
