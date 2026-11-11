use anyhow::{Context as _, Result};
use bootty_mux::{
    command::{MuxCommand, MuxSplitDirection, PaneInput},
    native::{
        self, Duplex, NativeBackend, PaneLauncher,
        protocol::{self, Frame, Hello, Request, Response},
    },
    snapshot::MuxSessionTag,
};
use bootty_terminal::{
    terminal_capture::CaptureOptions,
    terminal_engine::TerminalEngine,
    terminal_transport::{TerminalControl, TerminalSize, TerminalTransport},
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
};

const HISTORY_BYTES: usize = 32 * 1024 * 1024;

struct Output {
    rx: mpsc::Receiver<Vec<u8>>,
    pending: io::Cursor<Vec<u8>>,
}
impl Read for Output {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let n = self.pending.read(bytes)?;
        if n > 0 {
            return Ok(n);
        }
        self.pending = io::Cursor::new(self.rx.recv().unwrap_or_default());
        self.pending.read(bytes)
    }
}
struct Input(mpsc::Sender<Vec<u8>>, Arc<AtomicUsize>);
impl Write for Input {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.send(bytes.to_vec()).map_err(io::Error::other)?;
        self.1.fetch_add(1, Ordering::AcqRel);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Control {
    tx: mpsc::Sender<Vec<u8>>,
    closed: AtomicBool,
}
impl TerminalControl for Control {
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.tx.send(Vec::new());
    }
    fn resize(&self, _: TerminalSize) -> Result<()> {
        Ok(())
    }
    fn exited(&self) -> Result<bool> {
        Ok(self.closed.load(Ordering::Acquire))
    }
}
type Outputs = Arc<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>>;
struct Fixture {
    backend: NativeBackend,
    pane: String,
    outputs: Outputs,
}
fn launcher(outputs: Outputs, inputs: Arc<AtomicUsize>) -> PaneLauncher {
    Arc::new(move |_, config| {
        let (tx, rx) = mpsc::channel();
        outputs
            .lock()
            .map_err(|_| anyhow::anyhow!("outputs poisoned"))?
            .insert(config.pane_id.clone().context("pane id")?, tx.clone());
        Ok(TerminalTransport {
            reader: bootty_terminal::terminal_output::TerminalReader::Bytes(Box::new(Output {
                rx,
                pending: io::Cursor::new(Vec::new()),
            })),
            writer: Box::new(Input(tx.clone(), Arc::clone(&inputs))),
            control: Arc::new(Control {
                tx,
                closed: AtomicBool::new(false),
            }),
            tty_name: None,
        })
    })
}
// Each observation advances logical time beyond output settling. No wall-clock wait.
fn advancing_clock() -> Arc<dyn Fn() -> std::time::Instant + Send + Sync> {
    let origin = std::time::Instant::now();
    let ticks = AtomicUsize::new(0);
    Arc::new(move || {
        origin
            .checked_add(std::time::Duration::from_secs(
                u64::try_from(ticks.fetch_add(1, Ordering::Relaxed)).unwrap(),
            ))
            .unwrap()
    })
}
#[fixture]
fn fixture() -> Result<Fixture> {
    let outputs = Outputs::default();
    let inputs = Arc::new(AtomicUsize::new(0));
    let connector = pane_worker_behavior::backpressure_connector(launcher(
        Arc::clone(&outputs),
        Arc::clone(&inputs),
    ));
    let backend = NativeBackend::with_connector(Path::new("workspace"), connector);
    backend.configure_vt(protocol::VtPolicy {
        max_scrollback: HISTORY_BYTES,
        ..protocol::VtPolicy::default()
    });
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: "session".into(),
        cwd: "/".into(),
        tag: MuxSessionTag::default(),
        argv: None,
    })?;
    let pane = backend
        .snapshot()?
        .sessions
        .first()
        .context("session")?
        .anchor
        .pane_id
        .clone()
        .context("pane")?;
    Ok(Fixture {
        backend,
        pane,
        outputs,
    })
}
fn emit(fixture: &Fixture, bytes: Vec<u8>) -> Result<()> {
    fixture
        .outputs
        .lock()
        .map_err(|_| anyhow::anyhow!("outputs poisoned"))?
        .get(&fixture.pane)
        .context("pane output")?
        .send(bytes)?;
    Ok(())
}

#[rstest]
fn topology_is_pushed_and_layout_is_server_owned(fixture: Result<Fixture>) {
    let fixture = fixture.unwrap();
    let mut watch = fixture.backend.connect().unwrap();
    fixture
        .backend
        .execute(MuxCommand::SplitPane {
            session_id: "session".into(),
            pane_id: Some(fixture.pane),
            direction: MuxSplitDirection::Down,
        })
        .unwrap();
    let response: Response = protocol::read_blocking(&mut watch)
        .unwrap()
        .decode()
        .unwrap();
    let Response::Changed(snapshot) = response else {
        panic!("no topology event");
    };
    let window = snapshot
        .snapshot
        .sessions
        .first()
        .context("session")
        .unwrap()
        .windows
        .first()
        .context("window")
        .unwrap();
    assert_eq!(window.panes.len(), 2);
    assert!(matches!(
        window.layout,
        Some(bootty_mux::snapshot::MuxPaneLayout::Split {
            direction: bootty_mux::snapshot::MuxPaneSplitDirection::Down,
            ..
        })
    ));
}
#[rstest]
fn version_mismatch_is_explicit() {
    let connector =
        native::memory::connector(launcher(Outputs::default(), Arc::new(AtomicUsize::new(0))))
            .unwrap();
    let mut stream = connector().unwrap();
    protocol::write_blocking(
        &mut stream,
        &Frame::json(&Hello {
            version: 999,
            workspace: "workspace".into(),
            launch: None,
            vt: None,
        })
        .unwrap(),
    )
    .unwrap();
    let response: Response = protocol::read_blocking(&mut stream)
        .unwrap()
        .decode()
        .unwrap();
    assert!(matches!(
        response,
        Response::IncompatibleVersion(protocol::IncompatibleVersion {
            client: 999,
            server: protocol::WIRE_VERSION,
        })
    ));
}

#[rstest]
fn input_and_capture_are_pane_addressed(fixture: Result<Fixture>) {
    let fixture = fixture.unwrap();
    let mut stream = attach_frames(&fixture).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut stream, &mut decoder).unwrap();
    fixture
        .backend
        .request(&Request::Input {
            pane: fixture.pane.clone(),
            input: PaneInput::Write(b"typed".to_vec()),
        })
        .unwrap();
    while !next_frame(&mut stream, &mut decoder)
        .unwrap()
        .1
        .text_rows()
        .iter()
        .any(|row| row.contains("typed"))
    {}
    let response = fixture
        .backend
        .request(&Request::Capture {
            pane: fixture.pane,
            options: CaptureOptions::default(),
        })
        .unwrap();
    assert!(matches!(response, Response::Captured(capture) if capture.text == "typed"));
}

#[rstest]
fn concurrent_local_binds_choose_one_server() {
    let directory = assert_fs::TempDir::new().unwrap();
    let discriminator = directory
        .path()
        .file_name()
        .context("fixture name")
        .unwrap()
        .to_string_lossy();
    let endpoint = bootty_os::ipc::endpoint_for_label(&format!(
        "mux-test-{}-{discriminator}",
        std::process::id()
    ))
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let start = || {
            let _entered = runtime.enter();
            barrier.wait();
            native::local::bind(&endpoint)
        };
        let a = scope.spawn(start);
        let b = scope.spawn(start);
        (a.join(), b.join())
    });
    let a = results
        .0
        .map_err(|_| anyhow::anyhow!("first starter panicked"))
        .unwrap()
        .unwrap();
    let b = results
        .1
        .map_err(|_| anyhow::anyhow!("second starter panicked"))
        .unwrap()
        .unwrap();
    assert_eq!(
        usize::from(a.is_some()).saturating_add(usize::from(b.is_some())),
        1
    );
    drop((a, b));
    #[cfg(unix)]
    {
        std::fs::remove_file(endpoint.as_path()).unwrap();
        std::fs::remove_file(endpoint.as_path().with_extension("lock")).unwrap();
    }
}

#[cfg(unix)]
#[rstest]
fn a_live_server_keeps_its_lease_newer_than_temp_cleaners_delete() {
    let directory = assert_fs::TempDir::new().unwrap();
    let endpoint = bootty_os::ipc::LocalEndpoint::from_path(directory.path().join("mux"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let binding = {
        let _entered = runtime.enter();
        native::local::bind(&endpoint).unwrap().unwrap()
    };
    let lease = endpoint.as_path().with_extension("lock");
    let days_ago = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_hours(96))
        .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&lease)
        .unwrap()
        .set_modified(days_ago)
        .unwrap();

    binding.refresh_lease().unwrap();

    let age = std::fs::metadata(&lease)
        .unwrap()
        .modified()
        .unwrap()
        .elapsed()
        .unwrap_or_default();
    assert!(
        age < native::local::LEASE_REFRESH,
        "a held lease still looks {age:?} old"
    );
}

#[cfg(unix)]
#[rstest]
fn a_client_gone_before_accept_does_not_stop_the_listener() {
    let directory = assert_fs::TempDir::new().unwrap();
    let endpoint = bootty_os::ipc::LocalEndpoint::from_path(directory.path().join("mux"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let binding = {
        let _entered = runtime.enter();
        native::local::bind(&endpoint).unwrap().unwrap()
    };
    // Its credentials are unreadable by the time the server accepts it.
    drop(std::os::unix::net::UnixStream::connect(endpoint.as_path()).unwrap());
    let _client = std::os::unix::net::UnixStream::connect(endpoint.as_path()).unwrap();

    let (_stream, peer) = runtime.block_on(binding.accept()).unwrap();

    assert!(peer.is_current_user());
}

#[rstest]
fn checkpoint_capture_needs_no_rendering_attachment() {
    let outputs = Outputs::default();
    let base = launcher(Arc::clone(&outputs), Arc::new(AtomicUsize::new(0)));
    let connector = native::memory::connector(Arc::new(move |size, launch| {
        let transport = base(size, launch)?;
        outputs.lock().unwrap()[&launch.pane_id.clone().unwrap()]
            .send(b"\x1b[32mhidden history\x1b[0m".to_vec())
            .unwrap();
        Ok(transport)
    }))
    .unwrap();
    let backend = NativeBackend::with_connector(Path::new("hidden"), connector);
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "hidden".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })
        .unwrap();
    let pane = backend
        .snapshot()
        .unwrap()
        .sessions
        .first()
        .unwrap()
        .anchor
        .pane_id
        .clone()
        .unwrap();
    let options = CaptureOptions {
        scope: bootty_terminal::terminal_capture::CaptureScope::History,
        format: bootty_terminal::terminal_capture::CaptureFormat::Ansi,
        max_bytes: 256 * 1024,
        ..CaptureOptions::default()
    };
    let response = backend
        .request(&Request::CaptureCheckpoint { pane, options })
        .unwrap();
    let Response::CheckpointCaptured(capture) = response else {
        panic!("missing checkpoint capture");
    };
    assert!(capture.text.contains("hidden history"));
    assert!(capture.text.contains("\x1b["));
    assert_eq!((capture.cols, capture.rows), (100, 30));
}

#[cfg(unix)]
#[rstest]
fn client_environment_reaches_server_panes() {
    let home = std::env::var("HOME").unwrap();
    let outputs = Outputs::default();
    let base = launcher(Arc::clone(&outputs), Arc::new(AtomicUsize::new(0)));
    let connector = native::memory::connector(Arc::new(move |size, launch| {
        anyhow::ensure!(
            launch
                .base_environment
                .as_ref()
                .is_some_and(|environment| environment
                    .iter()
                    .any(|(key, value)| key == "HOME" && value == std::ffi::OsStr::new(&home))),
            "client HOME was not sent to the server"
        );
        anyhow::ensure!(
            launch
                .env
                .iter()
                .any(|(key, value)| key == "LANG" && value == "en_US.UTF-8"),
            "client locale was not sent to the server"
        );
        let mut probe = launch.clone();
        // The child must use the supplied environment, without leaking server variables.
        probe.base_environment.as_mut().unwrap().retain(|(key, _)| key == "HOME");
        probe.command = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '%s|%s|%s|%s|%s\\n' \"$HOME\" \"$LANG\" \"$TERM\" \"$TERM_PROGRAM\" \"${USER-unset}\"".into(),
        ];
        probe.command_is_argv = true;
        let mut transport = native::local_launcher()(size, &probe)?;
        let mut output = Vec::new();
        while !output.contains(&b'\n') {
            let Some(bootty_terminal::terminal_output::TerminalOutput::Bytes { bytes, .. }) =
                transport.reader.next_output()?
            else {
                anyhow::bail!("probe ended without output");
            };
            output.extend(bytes);
        }
        assert_eq!(
            String::from_utf8(output)?.trim(),
            format!("{home}|en_US.UTF-8|xterm-256color|ghostty|unset")
        );
        transport.control.close();
        base(size, launch)
    }))
    .unwrap();
    let backend = NativeBackend::with_connector(Path::new("client-environment"), connector);
    backend.configure_launch(bootty_terminal::SessionLaunchConfig {
        env: vec![("LANG".into(), "en_US.UTF-8".into())],
        term: "xterm-256color".into(),
        ..bootty_terminal::SessionLaunchConfig::default()
    });
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "environment".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })
        .unwrap();
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum RemoteTransport {
    Quic,
    Tls,
}
#[cfg(unix)]
impl RemoteTransport {
    fn address(
        self,
        server: &bootty_host::remote_link::RemoteLinkServer,
    ) -> Result<std::net::SocketAddr> {
        match self {
            Self::Quic => server.address(),
            Self::Tls => server.tcp_address(),
        }
    }
}
#[cfg(unix)]
struct RemoteFixture {
    fixture: Fixture,
    link: Arc<Mutex<Option<Arc<bootty_host::remote_link::RemoteLink>>>>,
    server: tokio::task::JoinHandle<Result<()>>,
    exits: tokio::task::JoinHandle<()>,
    direct: NativeBackend,
    _local: LocalMuxFixture,
}
#[cfg(unix)]
fn remote_fixture(transport: RemoteTransport) -> Result<RemoteFixture> {
    use bootty_host::remote_link::{RemoteLink, RemoteLinkIdentity, RemoteLinkServer};
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    );
    let outputs = Outputs::default();
    let inputs = Arc::new(AtomicUsize::new(0));
    let (mux, mut events) = native::server::MuxServer::with_server_id(
        launcher(Arc::clone(&outputs), Arc::clone(&inputs)),
        "persisted-remote-server".into(),
    )?;
    let direct = NativeBackend::with_reconnect_schedule(
        Path::new("remote-space"),
        inprocess_connector(&runtime, &mux),
        None,
        immediate_retry(),
    );
    let mux_events = Arc::clone(&mux);
    let exits = runtime.spawn(async move {
        while let Some(event) = events.recv().await {
            let _ = mux_events.handle_event(event).await;
        }
    });
    let local = local_mux_fixture(&runtime, &mux)?;
    let namespace = local.namespace.clone();
    let identity = Arc::new(RemoteLinkIdentity::generate()?);
    let (server, address, certificate) = {
        let _context = runtime.enter();
        let server = RemoteLinkServer::bind("127.0.0.1:0".parse()?, identity.cert.clone())?
            .with_mux_connector(native::local::remote_connector());
        let address = transport.address(&server)?;
        let certificate = server.certificate();
        (server, address, certificate)
    };
    let server = runtime.spawn(server.serve_reconnecting());
    let link = Arc::new(Mutex::new(None::<Arc<RemoteLink>>));
    let connecting = Arc::clone(&link);
    let connector: native::Connector = Arc::new(move || {
        let mut link = connecting
            .lock()
            .map_err(|_| anyhow::anyhow!("remote link poisoned"))?;
        if link.is_none() {
            *link = Some(Arc::new(runtime.block_on(async {
                match transport {
                    RemoteTransport::Quic => {
                        RemoteLink::connect(&identity, certificate.clone(), address).await
                    }
                    RemoteTransport::Tls => {
                        RemoteLink::connect_tcp(&identity, certificate.clone(), address).await
                    }
                }
            })?));
        }
        let stream = runtime.block_on(
            link.as_ref()
                .context("remote link missing")?
                .open_mux(&namespace),
        )?;
        drop(link);
        Ok(Box::new(native::AsyncConnection::new(
            Arc::clone(&runtime),
            stream,
        )))
    });
    let backend = NativeBackend::with_reconnect_schedule(
        Path::new("remote-space"),
        connector,
        Some(Arc::new(|id| {
            anyhow::ensure!(
                id == "persisted-remote-server",
                "remote server identity mismatch"
            );
            Ok(())
        })),
        immediate_retry(),
    );
    let pane = remote_pane(&backend)?;
    Ok(RemoteFixture {
        fixture: Fixture {
            backend,
            pane,
            outputs,
        },
        link,
        server,
        exits,
        direct,
        _local: local,
    })
}
#[cfg(unix)]
struct LocalMuxFixture {
    namespace: String,
    endpoint: bootty_os::ipc::LocalEndpoint,
    listener: tokio::task::JoinHandle<()>,
}
#[cfg(unix)]
impl Drop for LocalMuxFixture {
    fn drop(&mut self) {
        self.listener.abort();
        let _ = std::fs::remove_file(self.endpoint.as_path());
        let _ = std::fs::remove_file(self.endpoint.as_path().with_extension("lock"));
    }
}
#[cfg(unix)]
fn local_mux_fixture(
    runtime: &Arc<tokio::runtime::Runtime>,
    mux: &Arc<native::server::MuxServer>,
) -> Result<LocalMuxFixture> {
    let namespace = format!("bootty-dev-{:016x}", getrandom::u64()?);
    let endpoint = bootty_os::ipc::endpoint_for_label(&format!(
        "{namespace}-mux-v{}",
        protocol::WIRE_VERSION
    ))?;
    let binding = {
        let _context = runtime.enter();
        native::local::bind(&endpoint)?.context("test mux endpoint occupied")?
    };
    let mux = Arc::clone(mux);
    let listener = runtime.spawn(async move {
        while let Ok((stream, peer)) = binding.accept().await {
            if !peer.is_current_user() {
                break;
            }
            let server = Arc::clone(&mux);
            tokio::spawn(async move {
                let _ = server.serve(stream).await;
            });
        }
    });
    Ok(LocalMuxFixture {
        namespace,
        endpoint,
        listener,
    })
}
#[cfg(unix)]
fn inprocess_connector(
    runtime: &Arc<tokio::runtime::Runtime>,
    mux: &Arc<native::server::MuxServer>,
) -> native::Connector {
    let runtime = Arc::clone(runtime);
    let mux = Arc::clone(mux);
    Arc::new(move || {
        let (client, peer) = tokio::io::duplex(64 * 1024);
        let server = Arc::clone(&mux);
        runtime.spawn(async move {
            let _ = server.serve(peer).await;
        });
        Ok(Box::new(native::AsyncConnection::new(
            Arc::clone(&runtime),
            client,
        )))
    })
}
#[cfg(unix)]
fn immediate_retry() -> native::ReconnectSchedule {
    use std::time::{Duration, Instant};
    let clock = Arc::new(Mutex::new(Instant::now()));
    let now = Arc::clone(&clock);
    native::ReconnectSchedule {
        now: Arc::new(move || {
            *now.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }),
        wait: Arc::new(move |duration| {
            let mut clock = clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *clock = clock.checked_add(duration).unwrap_or(*clock);
            drop(clock);
            std::thread::yield_now();
        }),
        deadline: Duration::from_secs(1),
    }
}
#[cfg(unix)]
fn remote_pane(backend: &NativeBackend) -> Result<String> {
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: "session".into(),
        cwd: "/".into(),
        tag: MuxSessionTag::default(),
        argv: None,
    })?;
    let pane = backend
        .snapshot()?
        .sessions
        .first()
        .context("session")?
        .anchor
        .pane_id
        .clone()
        .context("pane")?;
    Ok(pane)
}
#[cfg(unix)]
impl Drop for RemoteFixture {
    fn drop(&mut self) {
        self.link
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.server.abort();
        self.exits.abort();
    }
}

#[rstest]
#[case(999, true, "wire version mismatch")]
#[case(protocol::WIRE_VERSION, false, "required capability")]
#[case(protocol::WIRE_VERSION, true, "identity mismatch")]
fn incompatible_peer_receives_no_launch_environment(
    #[case] version: u32,
    #[case] capable: bool,
    #[case] error: &str,
) {
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let connector_runtime = Arc::clone(&runtime);
    let (observed, hello_rx) = mpsc::channel();
    let connector = Arc::new(move || {
        let (client, mut peer) = tokio::io::duplex(4096);
        let observed = observed.clone();
        connector_runtime.spawn(async move {
            use tokio::io::AsyncReadExt as _;
            let hello: Hello = protocol::read(&mut peer).await.unwrap().decode().unwrap();
            assert!(hello.launch.is_none());
            assert!(hello.vt.is_none());
            let welcome = protocol::Welcome {
                server_id: "wrong-server".into(),
                generation: 1,
                version,
                capabilities: if capable {
                    protocol::CAPABILITIES
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect()
                } else {
                    Vec::new()
                },
            };
            protocol::write(&mut peer, &Frame::json(&welcome).unwrap())
                .await
                .unwrap();
            let mut remaining = Vec::new();
            peer.read_to_end(&mut remaining).await.unwrap();
            observed.send(remaining).unwrap();
        });
        let stream: Box<dyn Duplex> = Box::new(native::AsyncConnection::new(
            Arc::clone(&connector_runtime),
            client,
        ));
        Ok(stream)
    });
    let backend = NativeBackend::with_identity_verifier(
        Path::new("incompatible"),
        connector,
        Some(Arc::new(|id| {
            anyhow::ensure!(
                id == "expected-server",
                "remote mux server identity mismatch"
            );
            Ok(())
        })),
    );
    let failure = backend.connect().err().context("must reject peer").unwrap();
    assert!(failure.to_string().contains(error));
    if version != protocol::WIRE_VERSION {
        pretty_assertions::assert_eq!(
            failure.downcast_ref::<protocol::IncompatibleVersion>(),
            Some(&protocol::IncompatibleVersion {
                client: protocol::WIRE_VERSION,
                server: version,
            })
        );
    }
    assert!(
        hello_rx.recv().unwrap().is_empty(),
        "configuration must not cross an incompatible route"
    );
}

#[rstest]
fn delayed_native_command_completion_cannot_invalidate_a_newer_session_target() {
    use bootty_mux::controller::{CommandSelection, MuxController};
    use std::time::{Duration, Instant};
    let root = assert_fs::TempDir::new().unwrap();
    let workspace = root.path().join("config.toml");
    let connector =
        native::memory::connector(launcher(Outputs::default(), Arc::new(AtomicUsize::new(0))))
            .unwrap();
    NativeBackend::configure_connector(&workspace, connector).unwrap();
    let mut controller = MuxController::new(Some(&workspace));
    controller.native_backend().snapshot().unwrap();
    let config = bootty_mux::MuxBindingConfig::default();
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    let command = |name: &str| MuxCommand::CreateProjectSession {
        session_id: name.into(),
        cwd: "/".into(),
        tag: MuxSessionTag::default(),
        argv: None,
    };
    let cancellation = bootty_control::CommandCancellation::default();
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    let old = controller
        .execute_command_authoritatively(
            &repaint,
            &config,
            command("first"),
            CommandSelection::Follow,
            (deadline, &cancellation),
            || Ok(()),
        )
        .recv()
        .unwrap();
    let newer = controller
        .execute_command_authoritatively(
            &repaint,
            &config,
            command("second"),
            CommandSelection::Follow,
            (deadline, &bootty_control::CommandCancellation::default()),
            || Ok(()),
        )
        .recv()
        .unwrap();
    controller
        .complete_authoritative_command(newer, &config)
        .unwrap();
    let generation = controller.session_generation("second").unwrap();
    controller
        .complete_authoritative_command(old, &config)
        .unwrap();
    controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    assert_eq!(controller.sessions().len(), 2);
    assert_eq!(controller.session_generation("second"), Some(generation));
}

#[path = "integration/shell_launch.rs"]
mod shell_launch;

#[path = "integration/pane_worker_behavior.rs"]
mod pane_worker_behavior;

#[path = "integration/query_backpressure.rs"]
mod query_backpressure;

fn attach_frames(fixture: &Fixture) -> Result<Box<dyn Duplex>> {
    attach_frames_backend(&fixture.backend, &fixture.pane)
}
fn attach_frames_backend(backend: &NativeBackend, pane: &str) -> Result<Box<dyn Duplex>> {
    let mut stream = backend.connect()?;
    protocol::write_blocking(
        &mut stream,
        &Frame::json(&Request::AttachFrames {
            pane: pane.to_owned(),
        })?,
    )?;
    Ok(stream)
}
fn next_frame(
    stream: &mut Box<dyn Duplex>,
    decoder: &mut bootty_terminal::frame_wire::FrameDecoder,
) -> Result<(bool, Arc<bootty_terminal::terminal_frame::RenderFrame>)> {
    loop {
        let frame = protocol::read_blocking(stream)?;
        match frame.kind {
            protocol::IMAGE_CONTENT => decoder.insert_image(
                bootty_terminal::frame_wire::decode_image_content(&frame.bytes)?,
            )?,
            protocol::FRAME_UPDATE => {
                let update = bootty_terminal::frame_wire::decode_update(&frame.bytes)?;
                let full = matches!(update.rows, bootty_terminal::frame_wire::FrameRows::Full(_));
                return Ok((full, decoder.apply(update)?));
            }
            _ => {}
        }
    }
}
// A shell over SSH reports the remote host's directory; terminal output never fails the pane.
#[rstest]
#[case::foreign_host(b"\x1b]7;file://another-host/home/developer\x07".as_slice())]
#[case::not_a_file_uri(b"\x1b]7;https://example.com/work\x07".as_slice())]
#[case::not_utf8(b"\x1b]7;file:///work/%FF\x07".as_slice())]
fn unusable_reported_directories_keep_the_pane_and_its_local_directory(
    fixture: Result<Fixture>,
    #[case] report: &[u8],
) {
    let fixture = fixture.unwrap();
    let mut stream = attach_frames(&fixture).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut stream, &mut decoder).unwrap();
    emit(&fixture, [report, b"still here"].concat()).unwrap();
    let frame = loop {
        let (_, frame) = next_frame(&mut stream, &mut decoder).unwrap();
        if frame
            .text_rows()
            .iter()
            .any(|row| row.contains("still here"))
        {
            break frame;
        }
    };
    assert_eq!(frame.current_working_directory.as_deref(), Some("/"));
    let response = fixture
        .backend
        .request(&Request::Capture {
            pane: fixture.pane,
            options: CaptureOptions::default(),
        })
        .unwrap();
    assert!(matches!(response, Response::Captured(capture) if capture.text == "still here"));
}
#[rstest]
fn frames_decode_wide_text_and_reconnect_full(fixture: Result<Fixture>) {
    let fixture = fixture.unwrap();
    let mut stream = attach_frames(&fixture).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    let (full, initial) = next_frame(&mut stream, &mut decoder).unwrap();
    assert!(full);
    let bytes = format!(
        "hello 界\r\n{}wrapped end\r\n",
        " ".repeat(usize::from(initial.cols))
    )
    .into_bytes();
    let mut expected = TerminalEngine::new(bootty_terminal::geometry::TerminalGeometry {
        cols: initial.cols,
        rows: initial.rows,
        cell_width: 1,
        cell_height: 1,
    })
    .unwrap();
    expected.import_history(&bytes);
    let expected_rows = expected.extract_frame().unwrap().text_rows();
    fixture
        .outputs
        .lock()
        .unwrap()
        .get(&fixture.pane)
        .unwrap()
        .send(bytes)
        .unwrap();
    loop {
        let (_, frame) = next_frame(&mut stream, &mut decoder).unwrap();
        if frame
            .text_rows()
            .iter()
            .any(|row| row.contains("wrapped end"))
        {
            assert_eq!(frame.text_rows(), expected_rows);
            break;
        }
    }
    let mut reconnect = attach_frames(&fixture).unwrap();
    let (full, frame) = next_frame(
        &mut reconnect,
        &mut bootty_terminal::frame_wire::FrameDecoder::default(),
    )
    .unwrap();
    assert!(full);
    assert!(frame.text_rows().iter().any(|row| row.contains("hello 界")));
}
#[rstest]
fn pane_commands_encode_paste_and_enter() {
    use bootty_terminal::terminal_input_model::{KeyInput, KeyMods, TerminalKey};
    let outputs = Outputs::default();
    let (input, received) = mpsc::channel();
    let base = launcher(Arc::clone(&outputs), Arc::new(AtomicUsize::new(0)));
    let connector = native::memory::connector(Arc::new(move |size, launch| {
        let mut transport = base(size, launch)?;
        transport.writer = Box::new(Input(input.clone(), Arc::new(AtomicUsize::new(0))));
        Ok(transport)
    }))
    .unwrap();
    let backend = NativeBackend::with_connector(Path::new("commands"), connector);
    let pane = remote_pane(&backend).unwrap();
    let fixture = Fixture {
        backend,
        pane,
        outputs,
    };
    let mut stream = attach_frames(&fixture).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut stream, &mut decoder).unwrap();
    fixture
        .outputs
        .lock()
        .unwrap()
        .get(&fixture.pane)
        .unwrap()
        .send(b"\x1b[?2004hREADY".to_vec())
        .unwrap();
    loop {
        if next_frame(&mut stream, &mut decoder)
            .unwrap()
            .1
            .text_rows()
            .iter()
            .any(|row| row.contains("READY"))
        {
            break;
        }
    }
    for (request_id, command, expected) in [
        (
            1,
            protocol::PaneCommand::Paste("paste".into()),
            b"\x1b[200~paste\x1b[201~".to_vec(),
        ),
        (
            2,
            protocol::PaneCommand::Key(KeyInput {
                key: TerminalKey::Enter,
                mods: KeyMods::default(),
                repeat: false,
                utf8: None,
                unshifted: None,
            }),
            vec![b'\r'],
        ),
    ] {
        protocol::write_blocking(
            &mut stream,
            &Frame::json(&Request::PaneCommand {
                pane: fixture.pane.clone(),
                request_id,
                command,
            })
            .unwrap(),
        )
        .unwrap();
        loop {
            let frame = protocol::read_blocking(&mut stream).unwrap();
            if frame.kind == protocol::CONTROL
                && let Response::PaneReply {
                    request_id: id,
                    result,
                } = frame.decode().unwrap()
            {
                assert_eq!(id, request_id);
                assert!(result.is_ok());
                break;
            }
        }
        assert_eq!(received.recv().unwrap(), expected);
    }
}
#[rstest]
fn slow_frame_attachment_does_not_stop_fast_attachment(fixture: Result<Fixture>) {
    let fixture = fixture.unwrap();
    let mut slow = attach_frames(&fixture).unwrap();
    let mut fast = attach_frames(&fixture).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut fast, &mut decoder).unwrap();
    for index in 0..256 {
        let marker = format!("progress-{index:03}");
        fixture
            .outputs
            .lock()
            .unwrap()
            .get(&fixture.pane)
            .unwrap()
            .send(format!("\x1b[H{}\r\x1b[2K{marker}", "x".repeat(4000)).into_bytes())
            .unwrap();
        loop {
            if next_frame(&mut fast, &mut decoder)
                .unwrap()
                .1
                .text_rows()
                .iter()
                .any(|row| row.contains(&marker))
            {
                break;
            }
        }
    }
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    loop {
        if next_frame(&mut slow, &mut decoder)
            .unwrap()
            .1
            .text_rows()
            .iter()
            .any(|row| row.contains("progress-255"))
        {
            break;
        }
    }
}

#[rstest]
#[case(vec![7], bootty_terminal::terminal_side_effect::TerminalSideEffect::Bell)]
#[case(b"\x1b]2;frame title\x07".to_vec(), bootty_terminal::terminal_side_effect::TerminalSideEffect::WindowTitle("frame title".into()))]
fn latest_frame_client_receives_effects(
    fixture: Result<Fixture>,
    #[case] bytes: Vec<u8>,
    #[case] expected: bootty_terminal::terminal_side_effect::TerminalSideEffect,
) {
    let fixture = fixture.unwrap();
    let mut stream = attach_frames(&fixture).unwrap();
    protocol::write_blocking(
        &mut stream,
        &Frame::json(&Request::PaneCommand {
            pane: fixture.pane.clone(),
            request_id: 1,
            command: protocol::PaneCommand::Focus(true),
        })
        .unwrap(),
    )
    .unwrap();
    loop {
        let frame = protocol::read_blocking(&mut stream).unwrap();
        if frame.kind == protocol::CONTROL
            && matches!(
                frame.decode::<Response>().unwrap(),
                Response::PaneReply { .. }
            )
        {
            break;
        }
    }
    emit(&fixture, bytes).unwrap();
    loop {
        let frame = protocol::read_blocking(&mut stream).unwrap();
        if frame.kind == protocol::CONTROL
            && let Response::PaneEffect(effect) = frame.decode().unwrap()
        {
            assert_eq!(effect, expected);
            break;
        }
    }
}

#[rstest]
fn native_terminal_viewer_input_frames_and_effects(fixture: Result<Fixture>) -> Result<()> {
    use bootty_mux::terminal::{NativeTerminal, TerminalRuntime as _};
    use bootty_terminal::{
        frame_source::TerminalFrameSource as _,
        terminal_input_model::{KeyInput, KeyMods, TerminalKey},
        terminal_side_effect::TerminalSideEffect,
    };
    let fixture = fixture?;
    let (wake_tx, wake_rx) = mpsc::channel();
    let (effect_tx, effect_rx) = mpsc::channel();
    let config = bootty_terminal::TerminalSessionConfig {
        side_effect_tx: Some(effect_tx),
        ..Default::default()
    };
    let mut viewer = NativeTerminal::connect(
        fixture.backend.clone(),
        fixture.pane.clone(),
        bootty_terminal::geometry::TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        config,
        Arc::new(move || {
            let _ = wake_tx.send(());
        }),
    )?;
    viewer.encode_key(KeyInput {
        key: TerminalKey::Enter,
        mods: KeyMods::default(),
        repeat: false,
        utf8: None,
        unshifted: None,
    })?;
    viewer
        .capture(CaptureOptions::default())?
        .receive("attach barrier")?
        .map_err(anyhow::Error::msg)?;
    fixture
        .outputs
        .lock()
        .map_err(|_| anyhow::anyhow!("outputs"))?
        .get(&fixture.pane)
        .context("pane output")?
        .send(b"\x1b]2;viewer-title\x07\x07viewer-output".to_vec())?;
    // Channel timeouts are hang guards; frame contents determine success.
    while !viewer
        .extract_frame()?
        .text_rows()
        .join("\n")
        .contains("viewer-output")
    {
        wake_rx.recv_timeout(std::time::Duration::from_secs(5))?;
    }
    let mut title = false;
    let mut bell = false;
    while !title || !bell {
        match effect_rx
            .recv_timeout(std::time::Duration::from_secs(5))?
            .effect
        {
            TerminalSideEffect::WindowTitle(value) => title |= value == "viewer-title",
            TerminalSideEffect::Bell => bell = true,
            _ => {}
        }
    }
    viewer.search_viewport(
        "viewer-output",
        bootty_terminal::terminal_engine::TerminalSearchDirection::Current,
    )?;
    while viewer.extract_frame()?.search_query != "viewer-output" {
        wake_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .context("search or cwd frame")?;
    }
    assert_eq!(viewer.extract_frame()?.search_match_count, 1);
    viewer.search_viewport(
        "absent-query",
        bootty_terminal::terminal_engine::TerminalSearchDirection::Current,
    )?;
    while viewer.extract_frame()?.search_query != "absent-query" {
        wake_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .context("zero match frame")?;
    }
    assert_eq!(viewer.extract_frame()?.search_match_count, 0);
    fixture
        .outputs
        .lock()
        .unwrap()
        .get(&fixture.pane)
        .unwrap()
        .send(b"\x1b]7;file://localhost/tmp\x07".to_vec())?;
    while viewer.current_working_directory()?.as_deref() != Some("/tmp") {
        wake_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .with_context(|| {
                format!(
                    "cwd frame: {:?}",
                    viewer
                        .extract_frame()
                        .map(|f| f.current_working_directory.clone())
                )
            })?;
    }
    viewer.enter_copy_mode()?;
    viewer.handle_copy_mode_action(
        bootty_terminal::terminal_engine::TerminalCopyModeAction::SelectLine,
    )?;
    for format in [
        bootty_terminal::terminal_engine::TerminalSelectionFormat::PlainText,
        bootty_terminal::terminal_engine::TerminalSelectionFormat::Html,
    ] {
        let pending = viewer.format_selection(format)?;
        let selected = pending
            .receive("selection reply")?
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(selected.is_some(), "selection reply");
    }
    viewer.handle_copy_mode_action(
        bootty_terminal::terminal_engine::TerminalCopyModeAction::CopySelectionAndCancel,
    )?;
    loop {
        if let TerminalSideEffect::ClipboardWrite(text) = effect_rx
            .recv_timeout(std::time::Duration::from_secs(5))?
            .effect
        {
            anyhow::ensure!(text.contains("viewer-output"), "clipboard text");
            break;
        }
    }
    assert_eq!(viewer.child_exited()?, false);
    Ok(())
}

#[rstest]
fn native_terminal_viewer_bracketed_paste() -> Result<()> {
    use bootty_mux::terminal::{NativeTerminal, TerminalRuntime as _};
    use bootty_terminal::frame_source::TerminalFrameSource as _;
    let backend = NativeBackend::with_connector(
        Path::new("viewer-paste"),
        native::memory::connector(native::local_launcher())?,
    );
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: "session".into(),
        cwd: "/".into(),
        tag: MuxSessionTag::default(),
        argv: Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty -echo -icanon; printf '\\033[?2004hready'; od -An -tx1 -N 23".into(),
        ]),
    })?;
    let pane = backend
        .snapshot()?
        .sessions
        .first()
        .context("session")?
        .anchor
        .pane_id
        .clone()
        .context("pane")?;
    let (tx, rx) = mpsc::channel();
    let mut viewer = NativeTerminal::connect(
        backend,
        pane,
        bootty_terminal::geometry::TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )?;
    while !viewer
        .extract_frame()?
        .text_rows()
        .join("")
        .contains("ready")
    {
        rx.recv_timeout(std::time::Duration::from_secs(5))?;
    }
    viewer.write_paste("paste-value")?;
    while !viewer.child_exited()? {
        rx.recv_timeout(std::time::Duration::from_secs(5))?;
    }
    let screen = viewer
        .extract_frame()?
        .text_rows()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    anyhow::ensure!(screen.contains("1b 5b 32 30 30 7e"), "{screen}");
    anyhow::ensure!(screen.contains("1b 5b 32 30 31 7e"), "{screen}");
    Ok(())
}

#[rstest]
fn native_terminal_viewer_shell_output_and_exit() -> Result<()> {
    use bootty_mux::terminal::{NativeTerminal, TerminalRuntime as _};
    use bootty_terminal::{
        frame_source::TerminalFrameSource as _,
        terminal_input_model::{KeyInput, KeyMods, TerminalKey},
    };
    let backend = NativeBackend::with_connector(
        Path::new("viewer-shell"),
        native::memory::connector(native::local_launcher())?,
    );
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: "session".into(),
        cwd: "/".into(),
        tag: MuxSessionTag::default(),
        argv: Some(vec!["/bin/sh".into()]),
    })?;
    let pane = backend
        .snapshot()?
        .sessions
        .first()
        .context("session")?
        .anchor
        .pane_id
        .clone()
        .context("pane")?;
    let (tx, rx) = mpsc::channel();
    let mut viewer = NativeTerminal::connect(
        backend,
        pane,
        bootty_terminal::geometry::TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(move || {
            let _ = tx.send(());
        }),
    )?;
    viewer.write_paste("printf 'viewer-%s\\n' shell-proof")?;
    viewer.encode_key(KeyInput {
        key: TerminalKey::Enter,
        mods: KeyMods::default(),
        repeat: false,
        utf8: None,
        unshifted: None,
    })?;
    while !viewer
        .extract_frame()?
        .text_rows()
        .join("\n")
        .contains("viewer-shell-proof")
    {
        rx.recv_timeout(std::time::Duration::from_secs(5))?;
    }
    viewer.write_paste("exit")?;
    viewer.encode_key(KeyInput {
        key: TerminalKey::Enter,
        mods: KeyMods::default(),
        repeat: false,
        utf8: None,
        unshifted: None,
    })?;
    while !viewer.child_exited()? {
        rx.recv_timeout(std::time::Duration::from_secs(5))?;
    }
    assert_eq!(viewer.child_exited()?, true);
    Ok(())
}

/// Runs once, on the first write through a split writer: a viewer attaching its pane.
type AttachHook = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;
struct ViewerFaultStream {
    stream: Box<dyn Duplex>,
    drop_read: Arc<AtomicBool>,
    attach_hook: AttachHook,
}
struct HookedWriter {
    writer: Box<dyn Write + Send>,
    attach_hook: AttachHook,
}
impl Write for HookedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let hook = self.attach_hook.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        self.writer.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}
impl Read for ViewerFaultStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.drop_read.swap(false, Ordering::AcqRel) {
            return Err(io::Error::other("injected connection drop"));
        }
        self.stream.read(bytes)
    }
}
impl Write for ViewerFaultStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
struct ViewerFaultReader {
    stream: Box<dyn Read + Send>,
    drop_read: Arc<AtomicBool>,
}
impl Read for ViewerFaultReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.stream.read(bytes)?;
        if self.drop_read.swap(false, Ordering::AcqRel) {
            return Err(io::Error::other("injected connection drop"));
        }
        Ok(count)
    }
}
impl Duplex for ViewerFaultStream {
    fn split(self: Box<Self>) -> Result<native::SplitDuplex> {
        let (reader, writer, close) = self.stream.split()?;
        Ok((
            Box::new(ViewerFaultReader {
                stream: reader,
                drop_read: self.drop_read,
            }),
            Box::new(HookedWriter {
                writer,
                attach_hook: self.attach_hook,
            }),
            close,
        ))
    }

    fn read_timeout(&mut self, timeout: std::time::Duration) -> Result<()> {
        self.stream.read_timeout(timeout)
    }
}
#[rstest]
fn native_terminal_viewer_reconnect_restores_screen() -> Result<()> {
    use bootty_mux::terminal::{NativeTerminal, TerminalRuntime as _};
    use bootty_terminal::frame_source::TerminalFrameSource as _;
    let outputs = Outputs::default();
    let connector = native::memory::connector(launcher(
        Arc::clone(&outputs),
        Arc::new(AtomicUsize::new(0)),
    ))?;
    let drop_read = Arc::new(AtomicBool::new(false));
    let fail_connect = Arc::new(AtomicBool::new(false));
    let read_fault = Arc::clone(&drop_read);
    let connect_fault = Arc::clone(&fail_connect);
    let backend = NativeBackend::with_connector(
        Path::new("viewer-reconnect"),
        Arc::new(move || {
            anyhow::ensure!(
                !connect_fault.swap(false, Ordering::AcqRel),
                "injected reconnect pause"
            );
            Ok(Box::new(ViewerFaultStream {
                stream: connector()?,
                drop_read: Arc::clone(&read_fault),
                attach_hook: AttachHook::default(),
            }))
        }),
    );
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: "session".into(),
        cwd: "/".into(),
        tag: MuxSessionTag::default(),
        argv: None,
    })?;
    let pane = backend
        .snapshot()?
        .sessions
        .first()
        .context("session")?
        .anchor
        .pane_id
        .clone()
        .context("pane")?;
    let (wait_tx, wait_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let resume_rx = Mutex::new(resume_rx);
    let schedule = native::ReconnectSchedule {
        wait: Arc::new(move |_| {
            let _ = wait_tx.send(());
            let _ = resume_rx.lock().unwrap().recv();
        }),
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel();
    let mut viewer = NativeTerminal::connect_with_schedule(
        backend,
        pane.clone(),
        bootty_terminal::geometry::TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(move || {
            let _ = tx.send(());
        }),
        schedule,
    )?;
    outputs
        .lock()
        .unwrap()
        .get(&pane)
        .context("pane")?
        .send(b"reconnect-screen".to_vec())?;
    while !viewer
        .extract_frame()?
        .text_rows()
        .join("")
        .contains("reconnect-screen")
    {
        rx.recv_timeout(std::time::Duration::from_secs(5))?;
    }
    fail_connect.store(true, Ordering::Release);
    drop_read.store(true, Ordering::Release);
    outputs
        .lock()
        .unwrap()
        .get(&pane)
        .context("pane")?
        .send(b" trigger".to_vec())?;
    wait_rx.recv_timeout(std::time::Duration::from_secs(5))?;
    let disconnected = viewer.write_input(b"not-replayed").unwrap_err();
    anyhow::ensure!(
        disconnected
            .downcast_ref::<bootty_mux::terminal::NativeTerminalError>()
            .is_some()
    );
    assert_eq!(viewer.child_exited()?, false);
    outputs
        .lock()
        .unwrap()
        .get(&pane)
        .context("pane")?
        .send(b" after-reconnect".to_vec())?;
    resume_tx.send(())?;
    while !viewer
        .extract_frame()?
        .text_rows()
        .join("")
        .contains("after-reconnect")
    {
        rx.recv_timeout(std::time::Duration::from_secs(5))?;
    }
    anyhow::ensure!(
        viewer
            .extract_frame()?
            .text_rows()
            .join("")
            .contains("reconnect-screen")
    );
    Ok(())
}

/// The topology watcher shares the backend with every viewer. Its disconnect landing between a
/// viewer's connect and that viewer reading its generation once made the viewer give up on its next
/// reconnect and leave the pane black.
#[rstest]
fn native_terminal_viewer_reconnects_after_the_watcher_drops_mid_attach() -> Result<()> {
    use bootty_mux::terminal::NativeTerminal;
    use bootty_terminal::frame_source::TerminalFrameSource as _;
    let outputs = Outputs::default();
    let connector = native::memory::connector(launcher(
        Arc::clone(&outputs),
        Arc::new(AtomicUsize::new(0)),
    ))?;
    let drop_read = Arc::new(AtomicBool::new(false));
    let attach_hook = AttachHook::default();
    let (read_fault, hook) = (Arc::clone(&drop_read), Arc::clone(&attach_hook));
    let (watcher_down_tx, watcher_down) = mpsc::channel();
    let (release_watcher, watcher_released) = mpsc::channel::<()>();
    let watcher_released = Mutex::new(watcher_released);
    let backend = NativeBackend::with_reconnect_schedule(
        Path::new("viewer-watcher-race"),
        Arc::new(move || {
            Ok(Box::new(ViewerFaultStream {
                stream: connector()?,
                drop_read: Arc::clone(&read_fault),
                attach_hook: Arc::clone(&hook),
            }))
        }),
        None,
        native::ReconnectSchedule {
            wait: Arc::new(move |_| {
                let _ = watcher_down_tx.send(());
                let _ = watcher_released.lock().unwrap().recv();
            }),
            ..Default::default()
        },
    );
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: "session".into(),
        cwd: "/".into(),
        tag: MuxSessionTag::default(),
        argv: None,
    })?;
    let pane = backend
        .snapshot()?
        .sessions
        .first()
        .context("session")?
        .anchor
        .pane_id
        .clone()
        .context("pane")?;
    let watcher_fault = Arc::clone(&drop_read);
    *attach_hook.lock().unwrap() = Some(Box::new(move || {
        // Only the watcher is reading now, so it takes the fault and holds in its retry wait.
        watcher_fault.store(true, Ordering::Release);
        watcher_down
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("watcher disconnects");
    }));
    let (tx, rx) = mpsc::channel();
    let mut viewer = NativeTerminal::connect_with_schedule(
        backend,
        pane.clone(),
        bootty_terminal::geometry::TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(move || {
            let _ = tx.send(());
        }),
        native::ReconnectSchedule {
            wait: Arc::new(|_| {}),
            ..Default::default()
        },
    )?;
    let send = |bytes: &[u8]| -> Result<()> {
        outputs
            .lock()
            .unwrap()
            .get(&pane)
            .context("pane")?
            .send(bytes.to_vec())?;
        Ok(())
    };
    let shows = |viewer: &mut NativeTerminal, text: &str| -> Result<()> {
        while !viewer.extract_frame()?.text_rows().join("").contains(text) {
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .with_context(|| format!("viewer never showed {text:?}"))?;
        }
        Ok(())
    };
    send(b"before-drop")?;
    shows(&mut viewer, "before-drop")?;
    drop_read.store(true, Ordering::Release);
    send(b" after-reconnect")?;
    shows(&mut viewer, "after-reconnect")?;
    drop(release_watcher);
    Ok(())
}

#[cfg(unix)]
#[rstest]
fn remote_frame_attachment_reconnects_with_exact_state(
    #[values(RemoteTransport::Quic, RemoteTransport::Tls)] transport: RemoteTransport,
) -> Result<()> {
    let remote = remote_fixture(transport)?;
    let mut stream = attach_frames(&remote.fixture)?;
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut stream, &mut decoder)?;
    emit(&remote.fixture, "remote 🥟 ABC █ ┃".as_bytes().to_vec())?;
    let expected = next_frame(&mut stream, &mut decoder)?;
    drop(stream);
    let mut stream = attach_frames(&remote.fixture)?;
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    let restored = next_frame(&mut stream, &mut decoder)?;
    assert_eq!(expected.1.text, restored.1.text);
    assert_eq!(expected.1.cursor, restored.1.cursor);
    assert_eq!(remote.direct.snapshot()?.sessions.len(), 1);
    Ok(())
}

#[rstest]
fn respawn_preserves_geometry_and_frames(fixture: Result<Fixture>) -> Result<()> {
    let fixture = fixture?;
    let mut stream = attach_frames(&fixture)?;
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut stream, &mut decoder)?;
    fixture.backend.request(&Request::Resize {
        pane: fixture.pane.clone(),
        size: bootty_terminal::geometry::TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
    })?;
    loop {
        let resized = next_frame(&mut stream, &mut decoder)?.1;
        if (resized.cols, resized.rows) == (80, 24) {
            break;
        }
    }
    fixture.backend.request(&Request::Respawn {
        pane: fixture.pane.clone(),
        launch: bootty_terminal::SessionLaunchConfig::default(),
        history: "restored".into(),
    })?;
    loop {
        let frame = next_frame(&mut stream, &mut decoder)?.1;
        assert_eq!((frame.cols, frame.rows), (80, 24));
        if frame.text_rows().iter().any(|row| row.contains("restored")) {
            break;
        }
    }
    Ok(())
}

#[rstest]
fn custom_window_name_survives_title_and_frame_reattach(fixture: Result<Fixture>) -> Result<()> {
    let fixture = fixture?;
    let snapshot = fixture.backend.snapshot()?;
    let window = snapshot.sessions[0].windows[0].id.clone();
    fixture.backend.execute(MuxCommand::RenameWindow {
        session_id: "session".into(),
        window_id: window,
        name: "custom".into(),
    })?;
    let mut stream = attach_frames(&fixture)?;
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut stream, &mut decoder)?;
    emit(&fixture, b"\x1b]2;process title\x07marker".to_vec())?;
    while !next_frame(&mut stream, &mut decoder)?
        .1
        .text_rows()
        .iter()
        .any(|row| row.contains("marker"))
    {}
    drop(stream);
    let mut stream = attach_frames(&fixture)?;
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut stream, &mut decoder)?;
    assert_eq!(
        fixture.backend.snapshot()?.sessions[0].windows[0].name,
        "custom"
    );
    Ok(())
}

#[rstest]
fn native_terminal_viewer_idle_input_pending_capture_and_drop() -> Result<()> {
    use bootty_mux::native::protocol::{PaneCommand, PaneResult};
    use bootty_mux::terminal::{NativeTerminal, TerminalRuntime as _};
    use bootty_terminal::frame_source::TerminalFrameSource as _;
    use bootty_terminal::terminal_input_model::{KeyInput, KeyMods, TerminalKey};
    let resizes = Arc::new(AtomicUsize::new(0));
    let recorded_resizes = Arc::clone(&resizes);
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    );
    let (observed, events) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let held = Arc::new(Mutex::new(held));
    let retained_runtime = Arc::clone(&runtime);
    let backend = NativeBackend::with_connector(
        Path::new("held-viewer"),
        Arc::new(move || {
            let (client, mut peer) = tokio::io::duplex(4096);
            let observed = observed.clone();
            let resizes = Arc::clone(&recorded_resizes);
            let held = Arc::clone(&held);
            runtime.spawn(async move {
                let _: Hello = protocol::read(&mut peer).await.unwrap().decode().unwrap();
                protocol::write(
                    &mut peer,
                    &Frame::json(&protocol::Welcome {
                        server_id: "viewer-test".into(),
                        generation: 1,
                        version: protocol::WIRE_VERSION,
                        capabilities: protocol::CAPABILITIES
                            .iter()
                            .map(|value| (*value).to_owned())
                            .collect(),
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
                while let Ok(frame) = protocol::read(&mut peer).await {
                    match frame.decode::<Request>().unwrap() {
                        Request::Resize { .. } => {
                            resizes.fetch_add(1, Ordering::SeqCst);
                        }
                        Request::Configure { .. } => {
                            protocol::write(&mut peer, &Frame::json(&Response::Ok).unwrap())
                                .await
                                .unwrap();
                        }
                        Request::PaneCommand {
                            command: PaneCommand::Key(_),
                            ..
                        } => observed.send("key").unwrap(),
                        Request::PaneCommand {
                            request_id,
                            command: PaneCommand::Capture { .. },
                            ..
                        } => {
                            observed.send("capture").unwrap();
                            held.lock().unwrap().recv().unwrap();
                            let capture =
                                TerminalEngine::new(bootty_terminal::geometry::TerminalGeometry {
                                    cols: 80,
                                    rows: 24,
                                    cell_width: 10,
                                    cell_height: 20,
                                })
                                .unwrap()
                                .capture(CaptureOptions::default())
                                .unwrap();
                            protocol::write(
                                &mut peer,
                                &Frame::json(&Response::PaneReply {
                                    request_id,
                                    result: Ok(PaneResult::Captured(capture)),
                                })
                                .unwrap(),
                            )
                            .await
                            .unwrap();
                        }
                        _ => {}
                    }
                }
                observed.send("closed").unwrap();
            });
            Ok(Box::new(native::AsyncConnection::new(
                Arc::clone(&runtime),
                client,
            )))
        }),
    );
    let mut viewer = NativeTerminal::connect(
        backend,
        "pane".into(),
        bootty_terminal::geometry::TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(|| {}),
    )?;
    let size = bootty_terminal::geometry::TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 10,
        cell_height: 20,
    };
    for _ in 0..100 {
        viewer.set_display_scale(1.0)?;
        viewer.set_render_cell_metrics(size.cell_metrics())?;
        viewer.resize(size)?;
    }
    viewer.encode_key(KeyInput {
        key: TerminalKey::Enter,
        mods: KeyMods::default(),
        repeat: false,
        utf8: None,
        unshifted: None,
    })?;
    assert_eq!(
        events.recv_timeout(std::time::Duration::from_secs(5))?,
        "key"
    );
    assert_eq!(resizes.load(Ordering::SeqCst), 1);
    let pending = viewer.capture(CaptureOptions::default())?;
    assert_eq!(
        events.recv_timeout(std::time::Duration::from_secs(5))?,
        "capture"
    );
    release.send(())?;
    pending
        .receive("held capture")?
        .map_err(anyhow::Error::msg)?;
    drop(viewer);
    assert_eq!(
        events.recv_timeout(std::time::Duration::from_secs(5))?,
        "closed"
    );
    drop(retained_runtime);
    Ok(())
}

struct StartupConnection {
    welcome: io::Cursor<Vec<u8>>,
    writer: StartupWriter,
    reader: Output,
    close: mpsc::Sender<Vec<u8>>,
}
impl Read for StartupConnection {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.welcome.read(bytes)
    }
}
impl Write for StartupConnection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Duplex for StartupConnection {
    fn split(self: Box<Self>) -> Result<native::SplitDuplex> {
        let close = self.close;
        Ok((
            Box::new(self.reader),
            Box::new(self.writer),
            Arc::new(move || {
                let _ = close.send(Vec::new());
            }),
        ))
    }
    fn read_timeout(&mut self, _: std::time::Duration) -> Result<()> {
        Ok(())
    }
}
struct StartupWriter {
    bytes: Vec<u8>,
    requests: mpsc::Sender<Request>,
    blocked: mpsc::Sender<()>,
    release: Option<mpsc::Receiver<()>>,
    blocked_requests: usize,
    blocking_started: bool,
}
impl Write for StartupWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let frame = protocol::read_blocking(&mut io::Cursor::new(std::mem::take(&mut self.bytes)))
            .map_err(io::Error::other)?;
        let request: Request = frame.decode().map_err(io::Error::other)?;
        if self.blocked_requests > 0
            && (self.blocking_started || matches!(request, Request::Resize { .. }))
            && let Some(release) = &self.release
        {
            self.blocking_started = true;
            self.blocked.send(()).map_err(io::Error::other)?;
            release.recv().map_err(io::Error::other)?;
            self.blocked_requests = self.blocked_requests.saturating_sub(1);
        }
        self.requests.send(request).map_err(io::Error::other)
    }
}

#[rstest]
#[case::raw_input(false)]
#[case::ordered_action(true)]
fn startup_replay_survives_overload_and_preserves_fifo(#[case] ordered_action: bool) -> Result<()> {
    use bootty_mux::terminal::{StartingNativeTerminal, TerminalRuntime as _};
    use bootty_terminal::{frame_source::TerminalFrameSource as _, geometry::TerminalGeometry};
    let (requests, received) = mpsc::channel();
    let (blocked, blocking) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let (close, reader) = mpsc::channel();
    let mut welcome = Vec::new();
    protocol::write_blocking(
        &mut welcome,
        &Frame::json(&protocol::Welcome {
            server_id: "startup-test".into(),
            generation: 1,
            version: protocol::WIRE_VERSION,
            capabilities: protocol::CAPABILITIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        })?,
    )?;
    protocol::write_blocking(&mut welcome, &Frame::json(&Response::Ok)?)?;
    let connection = Mutex::new(Some(StartupConnection {
        welcome: io::Cursor::new(welcome),
        writer: StartupWriter {
            bytes: Vec::new(),
            requests,
            blocked,
            release: Some(held),
            blocked_requests: if ordered_action { 2 } else { 1 },
            blocking_started: false,
        },
        reader: Output {
            rx: reader,
            pending: io::Cursor::new(Vec::new()),
        },
        close,
    }));
    let (connect, connecting) = mpsc::channel();
    let connecting = Mutex::new(connecting);
    let backend = NativeBackend::with_connector(
        Path::new("startup-overload"),
        Arc::new(move || {
            connecting.lock().unwrap().recv()?;
            Ok(Box::new(connection.lock().unwrap().take().unwrap()))
        }),
    );
    let (wake, wakes) = mpsc::channel();
    let size = TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 10,
        cell_height: 20,
    };
    let mut terminal = StartingNativeTerminal::spawn(
        backend,
        "pane".into(),
        size,
        1.0,
        size.cell_metrics(),
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
    );
    terminal.apply_live_config(bootty_terminal::terminal_engine::TerminalLiveConfig {
        colors: bootty_terminal::terminal_engine::TerminalColorConfig::default(),
        cursor: bootty_terminal::terminal_engine::TerminalCursorConfig::default(),
        features: bootty_terminal::terminal_engine::TerminalFeatureConfig::default(),
    })?;
    let selection = bootty_terminal::terminal_engine::TerminalSelectionEvent {
        surface: bootty_terminal::geometry::TerminalSurface::for_logical_size(
            1200.0,
            480.0,
            size.cell_metrics(),
            bootty_terminal::geometry::TerminalPadding::default(),
        ),
        position: bootty_terminal::geometry::SurfacePoint { x: 1100.0, y: 10.0 },
        rectangle: false,
    };
    for index in 0_u16..256 {
        if index < 2 {
            let geometry = TerminalGeometry {
                cols: if index == 0 { 120 } else { 100 },
                ..size
            };
            for _ in 0..512 {
                terminal.resize(geometry)?;
                terminal.set_display_scale(1.0)?;
                terminal.set_render_cell_metrics(size.cell_metrics())?;
                terminal.extract_frame()?;
            }
        }
        if index == 0 {
            terminal.begin_selection(selection)?;
        } else {
            terminal.write_input(&index.to_le_bytes())?;
        }
    }
    terminal.resize(TerminalGeometry { cols: 81, ..size })?;
    connect.send(())?;
    blocking.recv()?;
    wakes.recv()?;
    anyhow::ensure!(
        terminal.started()?,
        "connected terminal must survive replay"
    );
    // Resize occupies the writer; live config leaves fewer than 256 slots for replay.
    terminal.resize(TerminalGeometry { cols: 121, ..size })?;
    terminal.update_selection(selection)?;
    terminal.resize(TerminalGeometry { cols: 101, ..size })?;
    for _ in 0..512 {
        terminal.resize(TerminalGeometry { cols: 101, ..size })?;
        terminal.set_display_scale(1.0)?;
        terminal.set_render_cell_metrics(size.cell_metrics())?;
        terminal.extract_frame()?;
    }
    terminal.write_input(&256_u16.to_le_bytes())?;
    anyhow::ensure!(
        terminal.started()?,
        "connected terminal must survive replay"
    );
    if ordered_action {
        terminal.write_input(&257_u16.to_le_bytes())?;
        terminal.write_input(&258_u16.to_le_bytes())?;
    }
    let latest = TerminalGeometry {
        cols: 93,
        cell_width: 20,
        cell_height: 40,
        ..size
    };
    let render = (|| -> Result<()> {
        terminal.resize(TerminalGeometry { cols: 92, ..size })?;
        terminal.set_display_scale(2.0)?;
        terminal.set_render_cell_metrics(size.cell_metrics())?;
        terminal.resize(latest)?;
        terminal.extract_frame()?;
        Ok(())
    })();
    release.send(())?;
    if ordered_action {
        // The writer dequeued exactly one more request, freeing one queue slot.
        blocking.recv()?;
        let action = terminal.handle_copy_mode_action(
            bootty_terminal::terminal_engine::TerminalCopyModeAction::CopySelectionAndCancel,
        );
        let search = terminal.search_viewport(
            "ordered",
            bootty_terminal::terminal_engine::TerminalSearchDirection::Next,
        );
        let frame = terminal.extract_frame();
        release.send(())?;
        let error = action.unwrap_err();
        anyhow::ensure!(matches!(
            error.downcast_ref(),
            Some(bootty_mux::terminal::NativeTerminalError::Overloaded)
        ));
        search?;
        frame?;
    }
    render?;
    let count = if ordered_action { 259 } else { 257 };
    let mut inputs = Vec::new();
    let mut geometry_order = Vec::new();
    let mut search_seen = false;
    let mut latest_seen = false;
    while inputs.len() < usize::from(count) || !latest_seen {
        terminal.extract_frame()?;
        while let Ok(request) = received.try_recv() {
            match &request {
                Request::Resize { size: geometry, .. }
                    if [120, 100, 121, 101].contains(&geometry.cols) =>
                {
                    geometry_order.push(geometry.cols);
                }
                Request::PaneCommand {
                    command: protocol::PaneCommand::SelectionBegin(_),
                    ..
                } => {
                    geometry_order.push(1);
                    inputs.push(0_u16.to_le_bytes().to_vec());
                }
                Request::PaneCommand {
                    command: protocol::PaneCommand::SelectionUpdate(_),
                    ..
                } => {
                    geometry_order.push(3);
                }
                Request::Input {
                    input: PaneInput::Write(bytes),
                    ..
                } if bytes == &256_u16.to_le_bytes() => {
                    geometry_order.push(4);
                }
                Request::Input {
                    input: PaneInput::Write(bytes),
                    ..
                } if bytes == &1_u16.to_le_bytes() => {
                    geometry_order.push(2);
                }
                _ => {}
            }
            if let Request::Resize { size: geometry, .. } = &request
                && *geometry == latest
            {
                assert_eq!(inputs.len(), usize::from(count));
                latest_seen = true;
            }
            if matches!(
                &request,
                Request::PaneCommand {
                    command: protocol::PaneCommand::Search { .. },
                    ..
                }
            ) {
                assert_eq!(inputs.len(), usize::from(count));
                search_seen = true;
            }
            if let Request::Input {
                input: PaneInput::Write(bytes),
                ..
            } = request
            {
                inputs.push(bytes);
            }
        }
        std::thread::yield_now();
    }
    assert_eq!(geometry_order, vec![120, 1, 100, 2, 121, 3, 101, 4]);
    assert_eq!(
        inputs,
        (0_u16..count)
            .map(|index| index.to_le_bytes().to_vec())
            .collect::<Vec<_>>()
    );
    if ordered_action {
        terminal.handle_copy_mode_action(
            bootty_terminal::terminal_engine::TerminalCopyModeAction::CopySelectionAndCancel,
        )?;
        loop {
            let request = received.recv()?;
            if matches!(
                &request,
                Request::PaneCommand {
                    command: protocol::PaneCommand::Search { .. },
                    ..
                }
            ) {
                search_seen = true;
            }
            if let Request::PaneCommand {
                command: protocol::PaneCommand::CopyMode(_),
                ..
            } = request
            {
                break;
            }
        }
    }
    assert_eq!(search_seen, ordered_action);
    Ok(())
}

struct ImageBudgetPeer(io::Cursor<Vec<u8>>);
impl Read for ImageBudgetPeer {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}
impl Write for ImageBudgetPeer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Duplex for ImageBudgetPeer {
    fn split(self: Box<Self>) -> Result<native::SplitDuplex> {
        Ok((Box::new(self.0), Box::new(io::sink()), Arc::new(|| {})))
    }
    fn read_timeout(&mut self, _: std::time::Duration) -> Result<()> {
        Ok(())
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn repeated_image_budget_protocol_errors_are_paced(#[case] eof: bool) -> Result<()> {
    use bootty_mux::terminal::NativeTerminal;
    use bootty_terminal::frame_wire::{
        ImageContent, ImageKey, MAX_RETAINED_IMAGE_BYTES, encode_image_content,
    };
    let connects = Arc::new(AtomicUsize::new(0));
    let count = connects.clone();
    let backend = NativeBackend::with_connector(
        Path::new("image-budget-peer"),
        Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            let mut bytes = Vec::new();
            protocol::write_blocking(
                &mut bytes,
                &Frame::json(&protocol::Welcome {
                    server_id: "budget-peer".into(),
                    generation: 1,
                    version: protocol::WIRE_VERSION,
                    capabilities: protocol::CAPABILITIES.iter().map(|s| (*s).into()).collect(),
                })?,
            )?;
            protocol::write_blocking(&mut bytes, &Frame::json(&Response::Ok)?)?;
            for generation in 0..if eof {
                0
            } else {
                MAX_RETAINED_IMAGE_BYTES / (8 * 1024 * 1024) + 1
            } {
                protocol::write_blocking(
                    &mut bytes,
                    &Frame {
                        kind: protocol::IMAGE_CONTENT,
                        bytes: encode_image_content(&ImageContent {
                            key: ImageKey {
                                image_id: 1,
                                generation: u64::try_from(generation)?,
                            },
                            bytes: vec![0; 8 * 1024 * 1024],
                        })?,
                    },
                )?;
            }
            Ok(Box::new(ImageBudgetPeer(io::Cursor::new(bytes))))
        }),
    );
    let (wait_tx, wait_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let resume_rx = Mutex::new(resume_rx);
    let viewer = NativeTerminal::connect_with_schedule(
        backend,
        "pane".into(),
        bootty_terminal::geometry::TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(|| {}),
        native::ReconnectSchedule {
            wait: Arc::new(move |delay| {
                wait_tx.send(delay).unwrap();
                let _ = resume_rx.lock().unwrap().recv();
            }),
            ..Default::default()
        },
    )?;
    for expected in 1..=2 {
        assert_eq!(wait_rx.recv()?, std::time::Duration::from_secs(5));
        assert_eq!(connects.load(Ordering::SeqCst), expected);
        if expected == 1 {
            resume_tx.send(())?;
        }
    }
    drop(resume_tx);
    drop(viewer);
    Ok(())
}

#[path = "integration/staged_launch.rs"]
mod staged_launch;

#[rstest]
fn raw_input_and_clipboard_use_compact_control_payloads() -> Result<()> {
    use bootty_mux::command::PaneInput;
    use bootty_terminal::terminal_side_effect::TerminalSideEffect;
    let bytes = vec![255; 64 * 1024];
    let request = Request::Input {
        pane: "pane".into(),
        input: PaneInput::Write(bytes.clone()),
    };
    let frame = Frame::json(&request)?;
    anyhow::ensure!(
        frame.bytes.len() < bytes.len().saturating_mul(2),
        "byte payload expanded"
    );
    let Request::Input {
        input: PaneInput::Write(decoded),
        ..
    } = frame.decode()?
    else {
        anyhow::bail!("expected raw input")
    };
    assert_eq!(decoded, bytes);
    for result in [None, Some(Vec::new()), Some(bytes.clone())] {
        let frame = Frame::json(&protocol::PaneResult::Selection(result.clone()))?;
        anyhow::ensure!(
            frame.bytes.len() < bytes.len().saturating_mul(2),
            "byte payload expanded"
        );
        let protocol::PaneResult::Selection(decoded) = frame.decode()? else {
            anyhow::bail!("expected selection")
        };
        assert_eq!(decoded, result);
    }
    let frame = Frame::json(&Response::PaneEffect(TerminalSideEffect::ClipboardPacket(
        bytes.clone(),
    )))?;
    anyhow::ensure!(
        frame.bytes.len() < bytes.len().saturating_mul(2),
        "byte payload expanded"
    );
    let Response::PaneEffect(TerminalSideEffect::ClipboardPacket(decoded)) = frame.decode()? else {
        anyhow::bail!("expected clipboard packet")
    };
    assert_eq!(decoded, bytes);
    Ok(())
}

#[rstest]
fn oversized_image_does_not_break_frame_stream() -> Result<()> {
    use bootty_terminal::frame_wire::{
        FrameDecoder, FrameEncoder, encode_image_content, encode_update,
    };
    let mut engine = TerminalEngine::new(bootty_terminal::geometry::TerminalGeometry {
        cols: 40,
        rows: 12,
        cell_width: 10,
        cell_height: 20,
    })?;
    engine.write_vt(b"text\x1b_Ga=T,f=32,s=1,v=1,i=1;/////w==\x1b\\");
    let mut source = engine.extract_frame()?.clone();
    let small = source.images.placements[0].clone();
    let mut large = small.clone();
    large.image_id = 2;
    large.data = Arc::new(vec![0; 17 * 1024 * 1024]);
    source.images.placements = vec![large, small.clone()];
    let mut encoder = FrameEncoder::default();
    let mut bytes = Vec::new();
    for text in ['a', 'b'] {
        source.text[0] = text;
        let update = encoder.encode(&source);
        for image in encoder.take_image_contents() {
            protocol::write_blocking(
                &mut bytes,
                &Frame {
                    kind: protocol::IMAGE_CONTENT,
                    bytes: encode_image_content(&image)?,
                },
            )?;
        }
        protocol::write_blocking(
            &mut bytes,
            &Frame {
                kind: protocol::FRAME_UPDATE,
                bytes: encode_update(&update)?,
            },
        )?;
    }
    let mut stream: Box<dyn Duplex> = Box::new(ImageBudgetPeer(io::Cursor::new(bytes)));
    let mut decoder = FrameDecoder::default();
    for text in ['a', 'b'] {
        let (_, frame) = next_frame(&mut stream, &mut decoder)?;
        assert_eq!(frame.text[0], text);
        assert_eq!(frame.images.placements, vec![small.clone()]);
    }
    Ok(())
}

#[rstest]
fn cleared_server_progress_clears_the_host_indicator() -> Result<()> {
    use bootty_mux::terminal::NativeTerminal;
    use bootty_terminal::{
        frame_wire::{FrameEncoder, encode_update},
        terminal_side_effect::TerminalSideEffect,
    };
    let geometry = bootty_terminal::geometry::TerminalGeometry {
        cols: 20,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    };
    let mut frame = TerminalEngine::new(geometry)?.extract_frame()?.clone();
    let mut encoder = FrameEncoder::default();
    let mut bytes = Vec::new();
    for message in [
        Frame::json(&protocol::Welcome {
            server_id: "progress-peer".into(),
            generation: 1,
            version: protocol::WIRE_VERSION,
            capabilities: protocol::CAPABILITIES.iter().map(|s| (*s).into()).collect(),
        })?,
        Frame::json(&Response::Ok)?,
    ] {
        protocol::write_blocking(&mut bytes, &message)?;
    }
    for progress in [Some(("normal".to_owned(), Some(42))), None] {
        frame.progress = progress;
        protocol::write_blocking(
            &mut bytes,
            &Frame {
                kind: protocol::FRAME_UPDATE,
                bytes: encode_update(&encoder.encode(&frame))?,
            },
        )?;
    }
    let backend = NativeBackend::with_connector(
        Path::new("progress-peer"),
        Arc::new(move || Ok(Box::new(ImageBudgetPeer(io::Cursor::new(bytes.clone()))))),
    );
    let (effect_tx, effect_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let resume_rx = Mutex::new(resume_rx);
    let viewer = NativeTerminal::connect_with_schedule(
        backend,
        "pane".into(),
        geometry,
        bootty_terminal::TerminalSessionConfig {
            side_effect_tx: Some(effect_tx),
            ..Default::default()
        },
        Arc::new(|| {}),
        native::ReconnectSchedule {
            // Park the reconnect after the scripted stream ends.
            wait: Arc::new(move |_| {
                let _ = resume_rx.lock().unwrap().recv();
            }),
            ..Default::default()
        },
    )?;
    let mut observed = Vec::new();
    while observed.len() < 2 {
        if let TerminalSideEffect::ConEmuProgress { state, value } = effect_rx
            .recv_timeout(std::time::Duration::from_secs(5))?
            .effect
        {
            observed.push((state, value));
        }
    }
    pretty_assertions::assert_eq!(
        observed,
        vec![("normal".to_owned(), Some(42)), ("remove".to_owned(), None)]
    );
    drop(resume_tx);
    drop(viewer);
    Ok(())
}

#[rstest]
fn native_wheel_moves_scrollback_viewport(fixture: Result<Fixture>) {
    use bootty_terminal::terminal_input_model::{
        KeyMods, MouseAction, MouseButton, MouseEncoderSize, MouseInput,
    };
    let fixture = fixture.unwrap();
    let mut output = b"history\r\n".repeat(100);
    output.extend_from_slice(b"row-99");
    emit(&fixture, output).unwrap();
    let mut stream = attach_frames(&fixture).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    let bottom = loop {
        let (_, frame) = next_frame(&mut stream, &mut decoder).unwrap();
        if frame.text_rows().iter().any(|row| row.contains("row-99")) {
            break frame;
        }
    };
    fixture
        .backend
        .request(&Request::PaneCommand {
            pane: fixture.pane.clone(),
            request_id: 1,
            command: protocol::PaneCommand::MouseWheel {
                input: MouseInput {
                    action: MouseAction::Press,
                    button: Some(MouseButton::Four),
                    mods: KeyMods::default(),
                    x: 0.,
                    y: 0.,
                    pixel_x: 0.,
                    pixel_y: 0.,
                    size: MouseEncoderSize {
                        screen_width: 800,
                        screen_height: 480,
                        cell_width: 10,
                        cell_height: 20,
                        padding_top: 0,
                        padding_bottom: 0,
                        padding_left: 0,
                        padding_right: 0,
                    },
                },
                delta: 3,
            },
        })
        .unwrap();
    // A following request observes the completed frame publication.
    fixture
        .backend
        .request(&Request::Capture {
            pane: fixture.pane.clone(),
            options: CaptureOptions::default(),
        })
        .unwrap();
    let mut reattached = attach_frames(&fixture).unwrap();
    let (_, scrolled) = next_frame(
        &mut reattached,
        &mut bootty_terminal::frame_wire::FrameDecoder::default(),
    )
    .unwrap();
    assert_eq!(
        scrolled.scrollbar.unwrap().offset,
        bottom.scrollbar.unwrap().offset.saturating_sub(3)
    );
    assert_ne!(scrolled.text_rows(), bottom.text_rows());
}
