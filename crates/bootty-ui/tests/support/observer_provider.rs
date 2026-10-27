use bootty_agents::{NativeChild, NativeChildControl};
use serde_json::{Value, json};
use std::{
    io::{self, Read as _, Write as _},
    net::Shutdown,
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

type Result<T> = std::result::Result<T, String>;

struct Peer {
    socket: PathBuf,
    stopped: Arc<AtomicBool>,
    connection: Arc<Mutex<Option<UnixStream>>>,
    worker: Option<thread::JoinHandle<Result<()>>>,
}

impl NativeChildControl for Peer {
    fn try_wait(&mut self) -> Result<Option<String>> {
        Ok(self
            .worker
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
            .then(|| "fixture observer ended".into()))
    }
    fn terminate(&mut self) -> Result<()> {
        self.stopped.store(true, Ordering::Release);
        if let Some(stream) = self
            .connection
            .lock()
            .map_err(|error| error.to_string())?
            .as_ref()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
        // Wake an observer that never received a terminal client.
        let _ = UnixStream::connect(&self.socket);
        Ok(())
    }
    fn wait(&mut self) -> Result<String> {
        if let Some(worker) = self.worker.take() {
            let result = worker.join().map_err(|_| "fixture observer panicked")?;
            if !self.stopped.load(Ordering::Acquire) {
                result?;
            }
        }
        Ok("fixture observer ended".into())
    }
}

pub fn launch(command: &Command) -> Result<NativeChild> {
    if std::path::Path::new(command.get_program())
        .file_name()
        .is_none_or(|name| name != "interrupt-provider.py")
    {
        return Err(format!(
            "No active observer fixture for {:?}",
            command.get_program()
        ));
    }
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>();
    let socket = args
        .windows(2)
        .find_map(|pair| {
            (pair[0] == "--listen")
                .then(|| pair[1].strip_prefix("unix://"))
                .flatten()
        })
        .ok_or("missing observer socket")?;
    let socket = PathBuf::from(socket);
    let listener = UnixListener::bind(&socket).map_err(|error| error.to_string())?;
    let stopped = Arc::new(AtomicBool::new(false));
    let connection = Arc::new(Mutex::new(None));
    let stop = Arc::clone(&stopped);
    let connected = Arc::clone(&connection);
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
        *connected.lock().map_err(|error| error.to_string())? =
            Some(stream.try_clone().map_err(|error| error.to_string())?);
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        serve(&mut stream).map_err(|error| error.to_string())
    });
    Ok(NativeChild {
        stdin: Box::new(io::sink()),
        stdout: Box::new(io::empty()),
        stderr: Box::new(io::empty()),
        control: Box::new(Peer {
            socket,
            stopped,
            connection,
            worker: Some(worker),
        }),
    })
}

fn serve(stream: &mut UnixStream) -> io::Result<()> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 16 * 1024 {
            return Err(io::Error::other("oversized fixture handshake"));
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        header.extend(byte);
    }
    stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")?;
    while let Some(request) = receive(stream)? {
        let result = match request["method"].as_str() {
            Some("initialize" | "turn/interrupt") => json!({}),
            Some("thread/start") => {
                json!({"thread":{"id":"exact-control","status":{"type":"active","activeFlags":[]}}})
            }
            _ => continue,
        };
        send(stream, &json!({"id":request["id"],"result":result}))?;
        if request["method"] == "turn/interrupt" {
            send(
                stream,
                &json!({"method":"turn/completed","params":{"threadId":"exact-control","turn":{"status":"interrupted"}}}),
            )?;
        }
    }
    Ok(())
}

fn receive(stream: &mut UnixStream) -> io::Result<Option<Value>> {
    let mut header = [0; 2];
    if let Err(error) = stream.read_exact(&mut header) {
        return if error.kind() == io::ErrorKind::UnexpectedEof {
            Ok(None)
        } else {
            Err(error)
        };
    }
    if header[0] & 15 == 8 {
        return Ok(None);
    }
    let length = match header[1] & 127 {
        126 => {
            let mut size = [0; 2];
            stream.read_exact(&mut size)?;
            usize::from(u16::from_be_bytes(size))
        }
        127 => {
            let mut size = [0; 8];
            stream.read_exact(&mut size)?;
            usize::try_from(u64::from_be_bytes(size)).map_err(io::Error::other)?
        }
        size => usize::from(size),
    };
    if length > 1024 * 1024 {
        return Err(io::Error::other("oversized fixture frame"));
    }
    let mut mask = [0; 4];
    if header[1] & 128 != 0 {
        stream.read_exact(&mut mask)?;
    }
    let mut data = vec![0; length];
    stream.read_exact(&mut data)?;
    if header[1] & 128 != 0 {
        for (byte, mask) in data.iter_mut().zip(mask.iter().cycle()) {
            *byte ^= mask;
        }
    }
    serde_json::from_slice(&data)
        .map(Some)
        .map_err(io::Error::other)
}

fn send(stream: &mut UnixStream, value: &Value) -> io::Result<()> {
    let data = serde_json::to_vec(value)?;
    stream.write_all(&[129])?;
    if let Some(size) = u8::try_from(data.len()).ok().filter(|size| *size < 126) {
        stream.write_all(&[size])?;
    } else {
        stream.write_all(&[126])?;
        stream.write_all(
            &u16::try_from(data.len())
                .map_err(io::Error::other)?
                .to_be_bytes(),
        )?;
    }
    stream.write_all(&data)
}
