use anyhow::Result;

use super::{command::MuxCommand, snapshot::MuxSnapshot};

pub trait MuxBackend {
    /// # Errors
    /// Returns a transport or backend error when current topology cannot be read.
    fn snapshot(&self) -> Result<MuxSnapshot>;
    /// # Errors
    /// Returns invalid target, transport, or backend command errors.
    fn execute(&mut self, command: MuxCommand) -> Result<()>;
}
