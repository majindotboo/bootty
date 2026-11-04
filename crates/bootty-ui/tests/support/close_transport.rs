use bootty_terminal::terminal_transport::{TerminalControl, TerminalSize, TerminalTransport};
use std::{
    io::{self, Read},
    sync::{Arc, mpsc},
};

struct Reader(mpsc::Receiver<()>);
impl Read for Reader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        let _ = self.0.recv();
        Ok(0)
    }
}
struct Control {
    pane: String,
    output: mpsc::Sender<()>,
    closed: mpsc::Sender<String>,
}
impl TerminalControl for Control {
    fn close(&self) {
        let _ = self.output.send(());
        let _ = self.closed.send(self.pane.clone());
    }
    fn resize(&self, _: TerminalSize) -> anyhow::Result<()> {
        Ok(())
    }
    fn exited(&self) -> anyhow::Result<bool> {
        Ok(false)
    }
}

pub fn launcher() -> (bootty_mux::native::PaneLauncher, mpsc::Receiver<String>) {
    let (closed, events) = mpsc::channel();
    let launcher = Arc::new(
        move |_, launch: &bootty_terminal::terminal_session::SessionLaunchConfig| {
            let (output, reader) = mpsc::channel();
            Ok(TerminalTransport {
                reader: bootty_terminal::terminal_output::TerminalReader::Bytes(Box::new(Reader(
                    reader,
                ))),
                writer: Box::new(io::sink()),
                control: Arc::new(Control {
                    pane: launch.pane_id.clone().expect("native pane identity"),
                    output,
                    closed: closed.clone(),
                }),
                tty_name: None,
            })
        },
    );
    (launcher, events)
}
