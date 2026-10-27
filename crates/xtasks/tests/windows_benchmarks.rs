#![cfg(windows)]

use std::fs;

use assert_fs::TempDir;
use serde_json::Value;

#[test]
fn smoke_gate_selects_python_validators_and_reports_cargo_failure() {
    let workspace = TempDir::new().unwrap();
    let evidence = workspace.join("evidence");
    let outcome = xtasks::benchmark::suite::run_with(
        xtasks::benchmark::suite::Args {
            ci_smoke: true,
            quick: false,
            output: Some(evidence.clone()),
        },
        xtasks::benchmark::suite::Metadata::new(
            "time".into(),
            "revision".into(),
            "host".into(),
            "rustc".into(),
            "cargo".into(),
        ),
        |command, log| {
            use std::os::windows::process::ExitStatusExt as _;
            let code = if command.get_program() == "python" {
                assert!(
                    command
                        .get_args()
                        .next()
                        .unwrap()
                        .to_string_lossy()
                        .ends_with(".py")
                );
                fs::write(log, "validator ran\n")?;
                0
            } else {
                assert_eq!(command.get_program(), "cargo");
                101
            };
            Ok(std::process::ExitStatus::from_raw(code))
        },
    );
    assert!(outcome.is_err());
    let records: Vec<Value> = fs::read_to_string(evidence.join("summary.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 4);
    assert_eq!(records[1]["status"], "pass", "{}", records[1]);
    assert_eq!(records[2]["status"], "pass", "{}", records[2]);
    assert_eq!(records[3]["name"], "compile_pipeline_resources");
    assert_eq!(records[3]["status"], "fail");
}
