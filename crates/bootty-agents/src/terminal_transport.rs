//! Worker-owned transports for provider queries and active terminal observation.

use std::{process::Command, sync::Arc, time::Duration};

use crate::{NativeChild, TerminalQueryOutput};

/// Runs one bounded command and reports its output and observed exit status.
pub type TerminalQuery =
    Arc<dyn Fn(Command, &dyn Fn() -> bool) -> Result<TerminalQueryOutput, String> + Send + Sync>;
/// Opens one provider stdio peer from an already captured query command.
pub type TerminalLauncher = Arc<dyn Fn(Command) -> Result<NativeChild, String> + Send + Sync>;

/// Captured process and stream construction for provider queries and observation.
#[derive(Clone)]
pub struct TerminalProviderTransport {
    pub output: TerminalQuery,
    pub launch: TerminalLauncher,
    /// Launch an active Codex app-server; production retains its parent-bound watchdog.
    pub observer_launch: TerminalLauncher,
}

impl Default for TerminalProviderTransport {
    fn default() -> Self {
        Self {
            output: Arc::new(|command, cancelled| {
                crate::terminal_process::query_output_command(
                    command,
                    Duration::from_secs(5),
                    cancelled,
                )
            }),
            launch: Arc::new(spawn),
            observer_launch: Arc::new(|command| spawn_observer(&command)),
        }
    }
}

fn spawn(command: Command) -> Result<NativeChild, String> {
    let mut child = crate::terminal_process::spawn_query(command)?;
    let stdin = child
        .child_mut()
        .stdin
        .take()
        .ok_or("Provider stdin is unavailable")?;
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .ok_or("Provider stdout is unavailable")?;
    Ok(NativeChild {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr: Box::new(std::io::empty()),
        control: Box::new(child),
    })
}

#[cfg(unix)]
fn spawn_observer(command: &Command) -> Result<NativeChild, String> {
    use std::process::Stdio;
    let program = command
        .get_program()
        .to_str()
        .ok_or("Provider program is not UTF-8")?;
    let mut bound = crate::terminal_process::parent_bound_command(program);
    bound
        .args(command.get_args())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            bound.env(name, value);
        } else {
            bound.env_remove(name);
        }
    }
    if let Some(directory) = command.get_current_dir() {
        bound.current_dir(directory);
    }
    let mut child = crate::terminal_process::spawn_query(bound)?;
    let stdin = child
        .child_mut()
        .stdin
        .take()
        .ok_or("Provider lifetime pipe is unavailable")?;
    Ok(NativeChild {
        stdin: Box::new(stdin),
        stdout: Box::new(std::io::empty()),
        stderr: Box::new(std::io::empty()),
        control: Box::new(child),
    })
}

#[cfg(not(unix))]
fn spawn_observer(_command: &Command) -> Result<NativeChild, String> {
    Err("Codex terminal observation requires a private Unix transport".to_owned())
}
