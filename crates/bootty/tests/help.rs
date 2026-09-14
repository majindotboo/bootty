#![cfg(test)]

use bootty::cli::Cli;
use clap::{CommandFactory, Parser};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case("--theme")]
#[case("--fullscreen")]
fn app_override_is_documented_only_under_the_app_command(#[case] option: &str) {
    let top_level = Cli::command().render_help().to_string();
    let app = Cli::try_parse_from(["bootty", "app", "--help"])
        .expect_err("app help exits through clap")
        .to_string();

    assert_eq!(
        (top_level.contains(option), app.contains(option)),
        (false, true)
    );
}

#[rstest]
fn explicit_command_accepts_stdin_json_without_treating_it_as_an_argument() {
    let cli = Cli::try_parse_from(["bootty", "command", "agents.pi.ingest", "--stdin-json"])
        .expect("parse the installed Pi adapter command");
    assert!(matches!(
        cli.subcommand(),
        Some(bootty::cli::Command::Invoke { stdin_json: true, arguments, .. })
            if arguments.is_empty()
    ));
}

#[rstest]
fn raw_stdin_is_separate_from_explicit_arguments_and_json_stdin() {
    let cli = Cli::try_parse_from([
        "bootty",
        "command",
        "agents.codex.ingest",
        "--stdin",
        "%1",
        "{}",
    ])
    .unwrap();
    assert!(matches!(
        cli.subcommand(),
        Some(bootty::cli::Command::Invoke { stdin: true, arguments, .. })
            if arguments == &["%1", "{}"]
    ));
    assert!(
        Cli::try_parse_from([
            "bootty",
            "command",
            "agents.codex.ingest",
            "--stdin",
            "--stdin-json",
        ])
        .is_err()
    );
}
