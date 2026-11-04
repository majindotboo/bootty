//! Explicit in-process daemon fixtures. No endpoint, login shell, or installed daemon is used.
use anyhow::Result;
use bootty_mux::native::{NativeBackend, PaneLauncher, local_launcher, memory};
use bootty_terminal::terminal_transport::{TerminalControl, TerminalSize, TerminalTransport};
use std::{
    collections::HashMap,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, mpsc},
};

/// # Errors
/// Returns in-process server setup failures.
pub fn local(path: &Path) -> Result<()> {
    configure(path, local_launcher())
}
/// # Errors
/// Rejects replacing a live fixture or a server setup failure.
pub fn configure(path: &Path, launcher: PaneLauncher) -> Result<()> {
    static CONFIGURED: OnceLock<Mutex<HashMap<PathBuf, NativeBackend>>> = OnceLock::new();
    let mut configured = CONFIGURED
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("native mux fixture registry poisoned"))?;
    if configured.contains_key(path) {
        return Ok(());
    }
    NativeBackend::configure_connector(path, memory::connector(launcher)?)?;
    let backend = NativeBackend::for_workspace(path);
    backend.snapshot()?;
    configured.insert(path.to_path_buf(), backend);
    drop(configured);
    Ok(())
}
struct Reader(mpsc::Receiver<()>);
impl Read for Reader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        let _ = self.0.recv();
        Ok(0)
    }
}
struct Control(mpsc::Sender<()>);
impl TerminalControl for Control {
    fn close(&self) {
        let _ = self.0.send(());
    }
    fn resize(&self, _: TerminalSize) -> Result<()> {
        Ok(())
    }
    fn exited(&self) -> Result<bool> {
        Ok(false)
    }
}
/// A topology fixture owns no OS process and rejects command-launch tests.
/// # Errors
/// Returns fixture configuration failures.
pub fn topology(path: &Path) -> Result<()> {
    configure(
        path,
        Arc::new(|_, launch| {
            anyhow::ensure!(
                launch.command.is_empty(),
                "topology fixture does not launch commands"
            );
            let (tx, rx) = mpsc::channel();
            Ok(TerminalTransport {
                reader: bootty_terminal::terminal_output::TerminalReader::Bytes(Box::new(Reader(
                    rx,
                ))),
                writer: Box::new(io::sink()),
                control: Arc::new(Control(tx)),
                tty_name: None,
            })
        }),
    )
}
