use std::process::Command;

use anyhow::Result;
use clap::Args;

use crate::command::run as run_command;

#[derive(Args, Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildArgs {
    /// Build the production app with the release profile.
    #[arg(long)]
    pub release: bool,
}

/// # Errors
/// Returns an error if Cargo cannot start or the application build fails.
pub fn run(args: &BuildArgs) -> Result<()> {
    let mut command = Command::new("cargo");
    command.arg("build");
    if args.release {
        command.arg("--release");
    }
    command.args(["-p", "bootty", "--bins"]);
    run_command(&mut command)
}
