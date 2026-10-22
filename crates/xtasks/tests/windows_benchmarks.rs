#![cfg(windows)]

use std::fs;
use std::process::Command;

use assert_fs::TempDir;
use serde_json::Value;

#[test]
fn smoke_gate_runs_python_validators_and_reports_cargo_failure() {
    let workspace = TempDir::new().unwrap();
    let scripts = workspace.join("scripts");
    fs::create_dir(&scripts).unwrap();
    for name in [
        "validate-benchmark-manifests.py",
        "build-benchmark-dashboard.py",
    ] {
        fs::write(scripts.join(name), "print('validator ran')\n").unwrap();
    }
    let evidence = workspace.join("evidence");
    let output = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args(["benchmark", "suite", "--ci-smoke", "--output"])
        .arg(&evidence)
        .current_dir(workspace.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
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
