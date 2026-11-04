//! The process boundary of a terminal worker. A local PTY and a mux attachment use
//! the same VT, input encoder and frame publisher.
use std::{
    io::Write,
    sync::{Arc, Mutex},
};

#[cfg(unix)]
use anyhow::Context as _;
use anyhow::Result;
use portable_pty::{MasterPty, PtySize};

use crate::terminal_session::SessionLaunchConfig;

pub use portable_pty::PtySize as TerminalSize;

pub trait TerminalControl: Send + Sync {
    fn close(&self);
    fn foreground_process_id(&self) -> Option<u32> {
        None
    }
    fn is_connected(&self) -> bool {
        true
    }
    /// # Errors
    /// Returns the process or transport resize failure.
    fn resize(&self, size: TerminalSize) -> Result<()>;
    /// # Errors
    /// Returns a lifecycle observation failure; a disconnect is not a child exit.
    fn exited(&self) -> Result<bool>;
}

pub struct TerminalTransport {
    pub reader: crate::terminal_output::TerminalReader,
    pub writer: Box<dyn Write + Send>,
    pub control: Arc<dyn TerminalControl>,
    pub tty_name: Option<String>,
}

struct PtyControl {
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<crate::terminal_launch::OwnedChild>,
}

impl TerminalControl for PtyControl {
    fn foreground_process_id(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            let foreground = self
                .master
                .lock()
                .ok()
                .and_then(|master| master.process_group_leader());
            if let Some(pid) = foreground.and_then(|pid| u32::try_from(pid).ok()) {
                return Some(pid);
            }
        }
        self.child.lock().ok()?.process_id()
    }
    fn close(&self) {
        if let Ok(mut child) = self.child.lock() {
            child.close();
        }
    }
    fn resize(&self, size: PtySize) -> Result<()> {
        self.master
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY lock poisoned"))?
            .resize(size)
    }

    fn exited(&self) -> Result<bool> {
        self.child
            .lock()
            .map_err(|_| anyhow::anyhow!("child lock poisoned"))?
            .exited()
    }
}

impl TerminalTransport {
    /// # Errors
    /// Returns a PTY launch or handle duplication error.
    pub fn launch(size: TerminalSize, config: &SessionLaunchConfig) -> Result<Self> {
        let mut config = config.clone();
        if config.pane_id.is_some() {
            config
                .env_remove
                .extend(["TMUX", "TMUX_PANE", "RMUX"].map(str::to_owned));
        }
        let (master, child, tty_name) = crate::terminal_launch::spawn(size, &config)?.into_parts();
        let reader = master.try_clone_reader()?;
        // portable-pty's Unix writer injects EOF on drop. An attachment must not.
        #[cfg(unix)]
        let writer: Box<dyn Write + Send> = Box::new(filedescriptor::FileDescriptor::dup(
            &master.as_raw_fd().context("terminal PTY descriptor")?,
        )?);
        #[cfg(not(unix))]
        let writer = master.take_writer()?;
        Ok(Self {
            reader: crate::terminal_output::TerminalReader::Bytes(reader),
            writer,
            tty_name,
            control: Arc::new(PtyControl {
                master: Mutex::new(master),
                child: Mutex::new(child),
            }),
        })
    }
}
