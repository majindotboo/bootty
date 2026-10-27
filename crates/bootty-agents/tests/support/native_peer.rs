#![cfg(test)]

//! JSONL peers that exercise the real native protocol reader without a child process.

use std::{
    io::{self, Cursor, Read, Write},
    sync::{Arc, Mutex, mpsc},
};

use bootty_agents::{NativeChild, NativeChildControl};
use serde_json::Value;

type Output = Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>;

struct Input<F> {
    output: Output,
    handler: F,
    pending: Vec<u8>,
}

pub enum Frame {
    Json(Value),
    Raw(Vec<u8>),
    End,
}

impl<F: FnMut(Value) -> Vec<Frame>> Write for Input<F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let frame = self.pending.drain(..=end).collect::<Vec<_>>();
            let value = serde_json::from_slice(&frame)?;
            let output = self
                .output
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
            for reply in (self.handler)(value) {
                let bytes = match reply {
                    Frame::Json(value) => {
                        let mut bytes = ascii_json(&value).into_bytes();
                        bytes.push(b'\n');
                        bytes
                    }
                    Frame::Raw(bytes) => bytes,
                    Frame::End => {
                        self.output.lock().unwrap().take();
                        break;
                    }
                };
                // Accepting a stdin frame precedes any resulting protocol failure or teardown.
                let _ = output.send(bytes);
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct OutputReader {
    frames: mpsc::Receiver<Vec<u8>>,
    current: Cursor<Vec<u8>>,
}

impl Read for OutputReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            let count = self.current.read(bytes)?;
            if count > 0 {
                return Ok(count);
            }
            match self.frames.recv() {
                Ok(frame) => self.current = Cursor::new(frame),
                Err(_) => return Ok(0),
            }
        }
    }
}

struct Control(Output);

impl NativeChildControl for Control {
    fn terminate(&mut self) -> Result<(), String> {
        self.0.lock().unwrap().take();
        Ok(())
    }

    fn wait(&mut self) -> Result<String, String> {
        Ok("exit status: 0".to_owned())
    }
}

pub fn peer(mut handler: impl FnMut(Value) -> Vec<Value> + Send + 'static) -> NativeChild {
    raw_peer(move |value| handler(value).into_iter().map(Frame::Json).collect())
}

pub(crate) fn raw_peer(handler: impl FnMut(Value) -> Vec<Frame> + Send + 'static) -> NativeChild {
    let (send, frames) = mpsc::channel();
    let output = Arc::new(Mutex::new(Some(send)));
    NativeChild {
        stdin: Box::new(Input {
            output: Arc::clone(&output),
            handler,
            pending: Vec::new(),
        }),
        stdout: Box::new(OutputReader {
            frames,
            current: Cursor::new(Vec::new()),
        }),
        stderr: Box::new(io::empty()),
        control: Box::new(Control(output)),
    }
}

/// Match the escaped wire representation providers use, including oversized Unicode echoes.
pub(crate) fn ascii_json(value: &Value) -> String {
    use serde::Serialize as _;
    let mut serializer = serde_json::Serializer::with_formatter(Vec::new(), AsciiFormatter);
    value.serialize(&mut serializer).unwrap();
    String::from_utf8(serializer.into_inner()).unwrap()
}

struct AsciiFormatter;

impl serde_json::ser::Formatter for AsciiFormatter {
    fn write_string_fragment<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        if fragment.is_ascii() {
            return writer.write_all(fragment.as_bytes());
        }
        let mut start = 0;
        for (offset, character) in fragment.char_indices() {
            if !character.is_ascii() {
                if start != offset {
                    writer.write_all(fragment.get(start..offset).unwrap().as_bytes())?;
                }
                for unit in character.encode_utf16(&mut [0; 2]) {
                    let unit = *unit;
                    let hex = b"0123456789abcdef";
                    writer.write_all(&[
                        b'\\',
                        b'u',
                        hex[usize::from(unit >> 12)],
                        hex[usize::from((unit >> 8) & 0xf)],
                        hex[usize::from((unit >> 4) & 0xf)],
                        hex[usize::from(unit & 0xf)],
                    ])?;
                }
                start = offset.checked_add(character.len_utf8()).unwrap();
            }
        }
        writer.write_all(fragment.get(start..).unwrap().as_bytes())
    }
}

/// Advance logical time on each observation in tests that exercise protocol state, not timing.
/// # Panics
/// Panics if the logical counter exceeds the monotonic clock range.
#[must_use]
pub fn clock() -> Arc<dyn Fn() -> std::time::Instant + Send + Sync> {
    let origin = std::time::Instant::now();
    let ticks = std::sync::atomic::AtomicU64::new(0);
    Arc::new(move || {
        origin
            .checked_add(std::time::Duration::from_secs(
                ticks.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ))
            .unwrap()
    })
}
