use super::{hostile, live_remote, power, replay, suite};

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Suite(suite::Args),
    RecordReplay(replay::Args),
    HostileSoak(hostile::Args),
    LiveRemote(live_remote::Args),
    PowerThermal(power::Args),
}

/// # Errors
/// Returns errors from the selected benchmark suite or its output files.
pub fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Suite(args) => suite::run(args),
        Command::RecordReplay(args) => replay::run(args),
        Command::HostileSoak(args) => hostile::run(args),
        Command::LiveRemote(args) => live_remote::run(args),
        Command::PowerThermal(args) => power::run(&args),
    }
}
