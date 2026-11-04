use super::*;
use pretty_assertions::assert_eq;

use bootty_mux::session_snapshot::{SavedTerminalPane, SavedTerminalSession, SavedTerminalWindow};

struct ObservedControl {
    inner: Arc<dyn TerminalControl>,
    closed: mpsc::Sender<()>,
}
impl TerminalControl for ObservedControl {
    fn close(&self) {
        self.inner.close();
        let _ = self.closed.send(());
    }
    fn resize(&self, size: TerminalSize) -> Result<()> {
        self.inner.resize(size)
    }
    fn exited(&self) -> Result<bool> {
        self.inner.exited()
    }
}

#[rstest]
#[case::success(false, false)]
#[case::failed_second_launch(true, false)]
#[case::panicked_second_launch(true, true)]
fn staged_launch_releases_global_mutex_and_cleans_failed_set(
    #[case] fail: bool,
    #[case] panic: bool,
) -> Result<()> {
    let base = launcher(Outputs::default(), Arc::new(AtomicUsize::new(0)));
    let (entered, entering) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let (closed, closing) = mpsc::channel();
    let count = AtomicUsize::new(0);
    let (server, _events) = native::server::MuxServer::new(Arc::new(move |size, launch| {
        let index = count.fetch_add(1, Ordering::AcqRel);
        if index == 2 {
            entered.send(())?;
            released.lock().unwrap().recv()?;
            panic_second_launch(panic);
            anyhow::ensure!(!fail, "injected second launch failure");
        }
        let mut transport = base(size, launch)?;
        if index == 1 {
            transport.control = Arc::new(ObservedControl {
                inner: transport.control,
                closed: closed.clone(),
            });
        }
        Ok(transport)
    }))?;
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let connecting_runtime = Arc::clone(&runtime);
    let connecting_server = Arc::clone(&server);
    let connector: native::Connector = Arc::new(move || {
        let (client, peer) = tokio::io::duplex(64 * 1024);
        let server = Arc::clone(&connecting_server);
        connecting_runtime.spawn(async move {
            let _ = server.serve(peer).await;
        });
        Ok(Box::new(native::AsyncConnection::new(
            Arc::clone(&connecting_runtime),
            client,
        )))
    });
    let other = NativeBackend::with_connector(Path::new("other-staged"), connector.clone());
    other.execute(MuxCommand::CreateProjectSession {
        session_id: "existing".into(),
        cwd: "/".into(),
        tag: MuxSessionTag::default(),
        argv: None,
    })?;
    let pane = other.snapshot()?.sessions[0]
        .anchor
        .pane_id
        .clone()
        .unwrap();
    let backend = NativeBackend::with_connector(Path::new("staged"), connector.clone());
    let observer = NativeBackend::with_connector(Path::new("staged"), connector);
    let command = MuxCommand::RestoreSession {
        session_id: "restored".into(),
        tag: MuxSessionTag {
            identity: Some("saved".into()),
            space: Some("space".into()),
        },
        snapshot: SavedTerminalSession {
            captured_at: 0,
            session_id: "saved".into(),
            backend_id: "saved".into(),
            active_window_id: None,
            windows: (0..2)
                .map(|index| SavedTerminalWindow {
                    id: format!("window-{index}"),
                    backend_id: format!("window-{index}"),
                    title: "saved".into(),
                    focused_pane_id: format!("pane-{index}"),
                    layout: None,
                    panes: vec![SavedTerminalPane {
                        id: format!("pane-{index}"),
                        backend_id: format!("pane-{index}"),
                        cwd: "/".into(),
                        cols: 80,
                        rows: 24,
                        text: "history".into(),
                        omitted_lines: 0,
                    }],
                })
                .collect(),
        },
    };
    // Keep the owner connection alive even after the blocking task panics.
    let mut owner = backend.connect()?;
    let launch = std::thread::spawn(move || -> Result<_> {
        use bootty_mux::native::protocol::{self, Frame};
        protocol::write_blocking(&mut owner, &Frame::json(&Request::Prepare(command))?)?;
        let response = read_topology_response(&mut owner);
        if matches!(response, Ok(Response::Ok)) {
            protocol::write_blocking(&mut owner, &Frame::json(&Request::Commit)?)?;
            read_topology_response(&mut owner)?;
        }
        Ok((owner, response))
    });
    entering.recv()?;
    // These complete before releasing the launcher, proving lock independence.
    assert_eq!(other.snapshot()?.sessions.len(), 1);
    assert_eq!(observer.snapshot()?.sessions, Vec::new());
    let mut frames = attach_frames_backend(&other, &pane)?;
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut frames, &mut decoder)?;
    anyhow::ensure!(
        matches!(
            other.request(&Request::Resize {
                pane,
                size: native::server::DEFAULT_SIZE,
            })?,
            Response::Ok
        ),
        "resize failed"
    );
    // A pane title event can publish a revision while the next pane is launching.
    // The reservation still owns the prepared topology across that publication.
    runtime.block_on(
        server.handle_event(native::server::ServerEvent::TopologyChanged {
            workspace: "staged".into(),
            pending: Arc::new(AtomicBool::new(true)),
        }),
    )?;
    release.send(())?;
    let (_owner, response) = launch.join().unwrap()?;
    assert_eq!(!matches!(response, Ok(Response::Ok)), fail);
    if fail {
        closing.recv()?;
        assert_eq!(observer.snapshot()?.sessions, Vec::new());
        observer.execute(MuxCommand::CreateProjectSession {
            session_id: "retry".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })?;
    } else {
        assert_eq!(observer.snapshot()?.sessions[0].windows.len(), 2);
    }
    Ok(())
}

#[rstest]
#[case::commit(false)]
#[case::disconnect(true)]
fn competing_splits_wait_for_staged_launch(#[case] disconnect: bool) -> Result<()> {
    use bootty_mux::native::protocol::{self, Frame};
    let base = launcher(Outputs::default(), Arc::new(AtomicUsize::new(0)));
    let (entered, entering) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let count = AtomicUsize::new(0);
    let connector = native::memory::connector(Arc::new(move |size, launch| {
        if count.fetch_add(1, Ordering::AcqRel) == 1 {
            entered.send(())?;
            released.lock().unwrap().recv()?;
        }
        base(size, launch)
    }))?;
    let backend = NativeBackend::with_connector(Path::new("competing-splits"), connector);
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: "session".into(),
        cwd: "/".into(),
        tag: MuxSessionTag::default(),
        argv: None,
    })?;
    let split = MuxCommand::SplitPane {
        session_id: "session".into(),
        pane_id: None,
        direction: MuxSplitDirection::Down,
    };
    let mut owner = backend.connect()?;
    protocol::write_blocking(&mut owner, &Frame::json(&Request::Prepare(split.clone()))?)?;
    entering.recv()?;
    let mut waiter = backend.connect()?;
    protocol::write_blocking(&mut waiter, &Frame::json(&Request::Execute(split))?)?;
    release.send(())?;
    read_topology_response(&mut owner)?;
    if disconnect {
        drop(owner);
    } else {
        protocol::write_blocking(&mut owner, &Frame::json(&Request::Commit)?)?;
        read_topology_response(&mut owner)?;
    }
    let response = read_topology_response(&mut waiter)?;
    let Response::Snapshot(snapshot) = response else {
        anyhow::bail!("expected snapshot: {response:?}");
    };
    assert_eq!(
        snapshot.snapshot.sessions[0].windows[0].panes.len(),
        if disconnect { 2 } else { 3 }
    );
    Ok(())
}

fn read_topology_response(stream: &mut Box<dyn native::Duplex>) -> Result<Response> {
    use bootty_mux::native::protocol;
    loop {
        let frame = protocol::read_blocking(stream)?;
        if let Ok(response) = frame.decode::<Response>() {
            if matches!(response, Response::Changed(_)) {
                continue;
            }
            anyhow::ensure!(!matches!(response, Response::Error(_)), "{response:?}");
            return Ok(response);
        }
    }
}

fn panic_second_launch(panic: bool) {
    assert!(!panic, "injected second launch panic");
}
