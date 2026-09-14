use clap::{Parser, Subcommand};
use pretty_assertions::assert_eq;
use rstest::rstest;
use xtasks::release;

#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Release(release::Args),
}

#[rstest]
fn parses_task_commands() {
    let cli = Cli::try_parse_from([
        "xtasks", "release", "prepare", "notes.md", "--bump", "patch",
    ])
    .unwrap();
    let Command::Release(release::Args {
        command: release::Command::Prepare(args),
    }) = cli.command
    else {
        panic!("expected release prepare command");
    };
    assert_eq!(args.bump, release::Bump::Patch);

    assert!(Cli::try_parse_from(["xtasks", "release", "validate-notes", "-"]).is_ok());
    assert!(Cli::try_parse_from(["xtasks", "release", "verify-tag", "v1.2.3"]).is_ok());
    assert!(Cli::try_parse_from(["xtasks", "release", "tag-and-dispatch"]).is_ok());
    assert!(Cli::try_parse_from(["xtasks", "release", "publish", "v1.2.3", "release"]).is_ok());
}
