use std::process::Command;

use anyhow::Result;

use crate::command;

/// # Errors
/// Returns an error if formatting or Clippy fails or cannot start.
pub fn run() -> Result<()> {
    command::run(Command::new("cargo").args(["fmt", "--all", "--", "--check"]))?;
    command::run(Command::new("cargo").args([
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]))
}
