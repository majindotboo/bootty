use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::{benchmark, build, daemon, install, launch, package, pre_commit, release, signing};

#[derive(Parser)]
#[command(name = "xtasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Build(build::BuildArgs),
    Launch(launch::Args),
    Package(package::Args),
    Install(install::Args),
    Daemon(daemon::DaemonArgs),
    Release(release::Args),
    Benchmark(benchmark::Args),
    PreCommit,
    /// Create and trust the local macOS code-signing identity.
    SignSetup,
}

/// # Errors
/// Returns the selected task's build, packaging, release, or benchmark error.
pub fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Build(args) => build::run(&args),
        Command::Launch(args) => launch::run(args),
        Command::Package(args) => package::run(args),
        Command::Install(args) => install::run(&args),
        Command::Daemon(args) => daemon::run(&args),
        Command::Release(args) => release::run(args),
        Command::Benchmark(args) => benchmark::run(args),
        Command::PreCommit => pre_commit::run(),
        Command::SignSetup => signing::run(),
    }
}
