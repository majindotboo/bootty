//! The stdio peer and lifecycle of one native provider invocation.

use std::{
    io::{Read, Write},
    path::Path,
    process::Command,
    sync::Arc,
};

use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild};

use crate::NativeSessionConfig;

/// Captured inputs to the provider's stdio peer, after tool arguments are resolved.
pub struct NativeLaunch<'a> {
    pub config: &'a NativeSessionConfig,
    pub tool_arguments: &'a [String],
    pub attachment_directory: Option<&'a Path>,
}

impl NativeLaunch<'_> {
    /// Build the captured provider command without starting it.
    /// # Errors
    /// Returns invalid provider configuration or command construction errors.
    pub fn command(&self) -> Result<Command, String> {
        crate::native_session::native_command(
            self.config,
            self.tool_arguments,
            self.attachment_directory,
        )
    }
}

/// Owns termination and reaping independently from the stdio readers and writer.
pub trait NativeChildControl: Send {
    /// Terminate the peer and all descendants it owns.
    /// # Errors
    /// Returns the peer's termination error.
    fn terminate(&mut self) -> Result<(), String>;

    /// Observe exit without blocking, using the same description as `wait`.
    ///
    /// The default `Ok(None)` means this handle cannot observe exit. Fakes that
    /// can exit must override this method to report their completed lifetime.
    /// # Errors
    /// Returns the peer's status observation error.
    fn try_wait(&mut self) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// Reap the peer and return its observed exit description.
    /// # Errors
    /// Returns the peer's wait error.
    fn wait(&mut self) -> Result<String, String>;
}

/// One provider stdio peer, with its lifecycle owned for as long as the session.
pub struct NativeChild {
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
    pub stderr: Box<dyn Read + Send>,
    pub control: Box<dyn NativeChildControl>,
}

/// Constructs the stdio peer for one captured native launch.
pub type NativeLauncher =
    Arc<dyn Fn(&NativeLaunch<'_>) -> Result<NativeChild, String> + Send + Sync>;

impl NativeChild {
    /// Spawn the production peer in an owned process tree.
    /// # Errors
    /// Returns command construction, spawn, or missing pipe errors.
    pub fn spawn(launch: &NativeLaunch<'_>) -> Result<Self, String> {
        let mut command = launch.command()?;
        let mut child = ProcessTreeChild::spawn_with_console_window(
            &mut command,
            ConsoleWindowBehavior::Suppress,
        )
        .map_err(|error| format!("Cannot start {}: {error}", launch.config.provider))?;
        let stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or("Agent stdin was not opened")?;
        let stdout = child
            .child_mut()
            .stdout
            .take()
            .ok_or("Agent stdout was not opened")?;
        let stderr = child
            .child_mut()
            .stderr
            .take()
            .ok_or("Agent stderr was not opened")?;
        Ok(Self {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
            control: Box::new(child),
        })
    }
}

impl NativeChildControl for ProcessTreeChild {
    fn terminate(&mut self) -> Result<(), String> {
        Self::terminate(self).map_err(|error| error.to_string())
    }

    fn try_wait(&mut self) -> Result<Option<String>, String> {
        self.child_mut()
            .try_wait()
            .map(|status| status.map(|status| status.to_string()))
            .map_err(|error| error.to_string())
    }

    fn wait(&mut self) -> Result<String, String> {
        Self::wait(self)
            .map(|status| status.to_string())
            .map_err(|error| error.to_string())
    }
}
