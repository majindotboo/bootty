use bootty_agents::{
    NativeChild, NativeChildControl, TerminalProviderTransport, TerminalQueryOutput,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead as _, BufReader, Write as _},
    os::unix::{fs::PermissionsExt as _, net::UnixStream},
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

#[derive(Clone)]
pub struct Fixture {
    pub arguments: Option<String>,
    pub environment: Option<(String, String)>,
    pub response: Vec<u8>,
    pub exit: i32,
    pub pages: Option<Vec<Value>>,
    pub codex: bool,
}

impl Fixture {
    pub fn output(response: impl Into<Vec<u8>>) -> Self {
        Self {
            arguments: None,
            environment: None,
            response: response.into(),
            exit: 0,
            pages: None,
            codex: false,
        }
    }
}

#[derive(Default)]
pub struct Providers(Mutex<HashMap<String, Fixture>>);

impl Providers {
    pub fn register(&self, path: &Path, fixture: Fixture) -> String {
        fs::write(path, "in-process provider").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        let program = path.to_str().unwrap().to_owned();
        self.0.lock().unwrap().insert(program.clone(), fixture);
        program
    }

    fn fixture(&self, command: &Command) -> Fixture {
        let fixture = self
            .0
            .lock()
            .unwrap()
            .get(command.get_program().to_str().unwrap())
            .unwrap()
            .clone();
        if let Some(arguments) = &fixture.arguments {
            let actual = command
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect::<Vec<_>>()
                .join(" ");
            if actual != "--version" {
                assert_eq!(&actual, arguments);
            }
        }
        if let Some((name, value)) = &fixture.environment
            && !command.get_args().any(|arg| arg == "--version")
        {
            assert_eq!(
                command
                    .get_envs()
                    .find(|(key, _)| *key == name.as_str())
                    .and_then(|(_, value)| value)
                    .and_then(|value| value.to_str()),
                Some(value.as_str())
            );
        }
        fixture
    }
}

fn output(stdout: Vec<u8>, exit: i32) -> TerminalQueryOutput {
    TerminalQueryOutput {
        stdout,
        stderr: vec![],
        exit_code: Some(exit),
        successful: exit == 0,
    }
}

struct Peer(Option<thread::JoinHandle<()>>);
impl NativeChildControl for Peer {
    fn terminate(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn wait(&mut self) -> Result<String, String> {
        Ok("in-process peer closed".to_owned())
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.0.take().unwrap().join().unwrap();
    }
}

fn request(reader: &mut BufReader<UnixStream>, method: &str) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["method"], method);
    value
}

impl Providers {
    pub fn transport(self: &Arc<Self>) -> TerminalProviderTransport {
        let output = self.clone();
        let launch = self.clone();
        TerminalProviderTransport {
            output: Arc::new(move |command, _cancelled| Ok(output.output(&command))),
            launch: Arc::new(move |command| launch.connect(&command)),
            observer_launch: TerminalProviderTransport::default().observer_launch,
        }
    }

    fn output(&self, command: &Command) -> TerminalQueryOutput {
        let fixture = self.fixture(command);
        if command.get_args().any(|arg| arg == "--version") {
            return output(b"test-provider 1\n".to_vec(), 0);
        }
        output(fixture.response, fixture.exit)
    }

    fn connect(&self, command: &Command) -> Result<NativeChild, String> {
        let fixture = self.fixture(command);
        let (client, mut server) = UnixStream::pair().map_err(|error| error.to_string())?;
        server
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .map_err(|error| error.to_string())?;
        server
            .set_write_timeout(Some(std::time::Duration::from_secs(30)))
            .map_err(|error| error.to_string())?;
        let writer = client.try_clone().map_err(|error| error.to_string())?;
        let worker = thread::spawn(move || {
            if fixture.codex {
                let mut reader = BufReader::new(server.try_clone().unwrap());
                request(&mut reader, "initialize");
                writeln!(server, "{}", json!({"id":1,"result":{}})).unwrap();
                request(&mut reader, "initialized");
                if let Some(pages) = fixture.pages {
                    for (id, page) in (2_u64..).zip(pages) {
                        let value = request(&mut reader, "thread/list");
                        assert_eq!(value["params"]["useStateDbOnly"], true);
                        writeln!(server, "{}", json!({"id":id,"result":page})).unwrap();
                    }
                    return;
                }
                let value = request(&mut reader, "account/read");
                assert_eq!(value["params"]["refreshToken"], false);
                writeln!(server, "{}", json!({"method":"account/updated","params":{"accessToken":"discarded-notification"}})).unwrap();
            }
            // Invalid-response cases may close their read end as soon as parsing fails.
            let _ = server.write_all(&fixture.response);
        });
        Ok(NativeChild {
            stdout: Box::new(client),
            stdin: Box::new(writer),
            stderr: Box::new(std::io::empty()),
            control: Box::new(Peer(Some(worker))),
        })
    }
}

thread_local! { static PROVIDERS: Arc<Providers> = Arc::default(); }
pub fn providers() -> Arc<Providers> {
    PROVIDERS.with(Arc::clone)
}
