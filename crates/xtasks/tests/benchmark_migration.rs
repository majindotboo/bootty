#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use assert_fs::TempDir;
use assert_fs::prelude::*;
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::Value;

#[rstest]
fn suite_runs_every_ci_smoke_step_and_records_each_failure() {
    let temp = TempDir::new().expect("temp directory");
    let output_dir = temp.child("evidence");
    let metadata = xtasks::benchmark::suite::Metadata::new(
        "1970-01-01T00:00:00Z".into(),
        "revision".into(),
        "host".into(),
        "rustc".into(),
        "cargo".into(),
    );
    let outcome = xtasks::benchmark::suite::run_with(
        xtasks::benchmark::suite::Args {
            ci_smoke: true,
            quick: false,
            output: Some(output_dir.path().to_owned()),
        },
        metadata,
        |command, log| {
            use std::os::unix::process::ExitStatusExt as _;
            let (code, text) = match command.get_program().to_str().unwrap() {
                "scripts/validate-benchmark-manifests.py" => (9, "manifest-failure\n"),
                "scripts/build-benchmark-dashboard.py" => (0, "dashboard-pass\n"),
                "cargo" => (17, "cargo-failure\n"),
                program => panic!("unplanned executable: {program}"),
            };
            fs::write(log, text)?;
            Ok(std::process::ExitStatus::from_raw(code << 8))
        },
    );
    assert!(outcome.is_err());
    let records = json_lines(&output_dir.child("summary.jsonl")).expect("JSON records");
    assert_eq!(records.len(), 4);
    assert_eq!(records[1]["name"], "validate_benchmark_manifests");
    assert_eq!(records[1]["status"], "fail");
    assert_eq!(records[1]["exit_code"], 9);
    assert_eq!(records[2]["name"], "validate_benchmark_dashboard");
    assert_eq!(records[2]["status"], "pass");
    assert_eq!(records[3]["name"], "compile_pipeline_resources");
    assert_eq!(records[3]["status"], "fail");
    assert_eq!(records[3]["exit_code"], 17);
    assert_eq!(
        fs::read_to_string(output_dir.child("compile_pipeline_resources.log").path())
            .expect("cargo log"),
        "cargo-failure\n"
    );
}

#[rstest]
fn full_suite_compile_checks_the_gpui_terminal_scene() {
    let planned = xtasks::benchmark::suite::plan(false, false);
    assert!(planned.iter().any(|step| {
        step.name == "compile_gpui_terminal_scene"
            && step.program == "cargo"
            && step.args
                == [
                    "test",
                    "-p",
                    "bootty-ui",
                    "--bench",
                    "gpui_terminal_scene",
                    "--no-run",
                ]
    }));
}

#[rstest]
fn recorder_passes_command_arguments_directly_and_writes_a_complete_bundle() {
    let temp = TempDir::new().expect("temp directory");
    let bin = temp.child("bin");
    bin.create_dir_all().expect("bin directory");
    executable(
        &bin.child("script"),
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  echo 'BSD script help'
  exit 0
fi
stream=$2
shift 2
printf '%s\n' "$@" >"$stream"
"#,
    )
    .expect("executable fixture");
    let fixtures = temp.child("fixtures");

    let output = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .args([
            "benchmark",
            "record-replay",
            "shell-characters",
            fixtures.path().to_str().expect("UTF-8 fixture path"),
            "--",
            "printf",
            "two words",
            "$(not-executed)",
        ])
        .env("PATH", path_with(bin.path()).expect("joined PATH"))
        .env("COLUMNS", "132")
        .env("LINES", "43")
        .output()
        .expect("record replay fixture");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixture = fixtures.child("shell-characters");
    assert_eq!(
        fs::read_to_string(fixture.child("stream.pty").path()).expect("stream"),
        "printf\ntwo words\n$(not-executed)\n"
    );
    let timing = fs::read_to_string(fixture.child("timing.tsv").path()).expect("timing");
    assert!(timing.starts_with("start_ns\t"));
    assert!(timing.contains("\nend_ns\t"));
    assert!(timing.contains("\nduration_ns\t"));
    let metadata = fs::read_to_string(fixture.child("metadata.env").path()).expect("metadata");
    assert!(metadata.contains("cols=132\n"));
    assert!(metadata.contains("rows=43\n"));
    assert!(metadata.contains("'two words'"));
    assert!(metadata.contains("'$(not-executed)'"));
    let sums = fs::read_to_string(fixture.child("SHA256SUMS").path()).expect("checksums");
    assert_eq!(sums.lines().count(), 3);
    assert!(sums.contains("  stream.pty\n"));
    assert!(sums.contains("  timing.tsv\n"));
    assert!(sums.contains("  metadata.env\n"));
}

#[rstest]
#[case("../outside")]
#[case("nested/fixture")]
#[case("/tmp/outside")]
fn recorder_rejects_fixture_names_that_escape_the_output_root(#[case] name: &str) {
    let error = xtasks::benchmark::replay::validate_fixture_name(name).unwrap_err();
    assert!(
        error.to_string().contains("one path component"),
        "{error:#}"
    );
}

fn json_lines(file: &assert_fs::fixture::ChildPath) -> anyhow::Result<Vec<Value>> {
    Ok(fs::read_to_string(file.path())?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
}

fn path_with(first: &Path) -> Result<std::ffi::OsString, std::env::JoinPathsError> {
    std::env::join_paths([first])
}

fn executable(path: &assert_fs::fixture::ChildPath, contents: &str) -> anyhow::Result<()> {
    bootty_test_support::write_executable(path.path(), contents)?;
    Ok(())
}
