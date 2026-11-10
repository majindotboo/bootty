use super::*;
use pretty_assertions::assert_eq;

#[rstest]
#[case::cpr(b"\x1b[6n", b"\x1b[1;1R")]
#[case::da1(b"\x1b[c", b"\x1b[?62;22;52c")]
fn server_answers_queries_once(
    #[case] query: &[u8],
    #[case] reply: &[u8],
    #[values(0, 1, 2)] attachments: usize,
) {
    let outputs = Outputs::default();
    let (tx, rx) = mpsc::channel();
    let base = launcher(Arc::clone(&outputs), Arc::new(AtomicUsize::new(0)));
    let connector = native::memory::connector(Arc::new(move |size, config| {
        let mut transport = base(size, config)?;
        transport.writer = Box::new(Input(tx.clone(), Arc::new(AtomicUsize::new(0))));
        Ok(transport)
    }))
    .unwrap();
    let backend = NativeBackend::with_connector(Path::new("workspace"), connector);
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "session".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })
        .unwrap();
    let pane = backend.snapshot().unwrap().sessions[0]
        .anchor
        .pane_id
        .clone()
        .unwrap();
    let streams: Vec<_> = (0..attachments)
        .map(|_| attach_frames_backend(&backend, &pane).unwrap())
        .collect();
    outputs.lock().unwrap()[&pane].send(query.to_vec()).unwrap();
    assert_eq!(rx.recv().unwrap(), reply);
    backend
        .request(&Request::Input {
            pane: pane.clone(),
            input: PaneInput::Write(b"barrier".to_vec()),
        })
        .unwrap();
    assert_eq!(rx.recv().unwrap(), b"barrier");
    assert!(rx.try_recv().is_err());
    drop(streams);
}

#[rstest]
#[case::split(false, None)]
#[case::create(true, None)]
#[case::explicit_create(true, Some("/explicit"))]
fn split_launches_in_osc7_directory_when_process_is_unavailable(
    #[case] create: bool,
    #[case] cwd: Option<&str>,
) {
    let outputs = Outputs::default();
    let launches = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&launches);
    let base = launcher(Arc::clone(&outputs), Arc::new(AtomicUsize::new(0)));
    let connector = native::memory::connector(Arc::new(move |size, config| {
        recorded
            .lock()
            .unwrap()
            .push(config.working_directory.clone());
        base(size, config)
    }))
    .unwrap();
    let backend = NativeBackend::with_connector(Path::new("workspace"), connector);
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "session".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })
        .unwrap();
    let pane = backend.snapshot().unwrap().sessions[0]
        .anchor
        .pane_id
        .clone()
        .unwrap();
    let mut stream = attach_frames_backend(&backend, &pane).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut stream, &mut decoder).unwrap();
    outputs.lock().unwrap()[&pane]
        .send(b"\x1b]7;file://localhost/tmp\x1b\\".to_vec())
        .unwrap();
    next_frame(&mut stream, &mut decoder).unwrap();
    let before = backend.snapshot().unwrap();
    let command = if create {
        MuxCommand::CreatePane {
            session_id: "session".into(),
            pane_id: Some(pane),
            direction: MuxSplitDirection::Right,
            cwd: cwd.map(str::to_owned),
            argv: vec!["sh".into()],
        }
    } else {
        MuxCommand::SplitPane {
            session_id: "session".into(),
            pane_id: Some(pane),
            direction: MuxSplitDirection::Right,
        }
    };
    backend.execute(command).unwrap();
    let expected = cwd.unwrap_or("/tmp");
    assert_eq!(launches.lock().unwrap()[1], Some(expected.into()));
    assert_eq!(
        backend.snapshot().unwrap().sessions[0].windows[0].panes[0],
        before.sessions[0].windows[0].panes[0],
    );
}

/// The shell prints, then changes directory only after the test has seen that output's frame, so
/// the directory change lands right after a published frame. The next frame must still name it.
#[cfg(unix)]
#[rstest]
fn split_launches_in_shell_process_directory() {
    let directory = assert_fs::TempDir::new().unwrap();
    let expected = directory.path().canonicalize().unwrap();
    let release = expected.join("release");
    let launches = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&launches);
    let target = expected.clone();
    let barrier = release.clone();
    let base = launcher(Outputs::default(), Arc::new(AtomicUsize::new(0)));
    let connector = native::memory::connector(Arc::new(move |size, config| {
        let mut launches = recorded.lock().unwrap();
        launches.push(config.working_directory.clone());
        if launches.len() != 1 {
            return base(size, config);
        }
        drop(launches);
        let launch = bootty_terminal::SessionLaunchConfig {
            shell: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                "printf READY && read line && printf ECHOED && \
                 while [ ! -e \"$2\" ]; do :; done && cd \"$1\" && printf CHANGED && read line"
                    .into(),
                "sh".into(),
                target.to_string_lossy().into_owned(),
                barrier.to_string_lossy().into_owned(),
            ],
            shell_integration: false,
            ..config.clone()
        };
        let mut transport = bootty_terminal::terminal_transport::TerminalTransport::launch(
            bootty_terminal::terminal_transport::TerminalSize {
                cols: size.cols,
                rows: size.rows,
                pixel_width: 0,
                pixel_height: 0,
            },
            &launch,
        )?;
        let mut ready = Vec::new();
        while !ready.ends_with(b"READY") {
            let Some(bootty_terminal::terminal_output::TerminalOutput::Bytes { bytes, .. }) =
                transport.reader.next_output()?
            else {
                anyhow::bail!("shell exited before cd");
            };
            ready.extend(bytes);
        }
        Ok(transport)
    }))
    .unwrap();
    let backend = NativeBackend::with_connector(Path::new("process-cwd"), connector);
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "session".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })
        .unwrap();
    let pane = backend.snapshot().unwrap().sessions[0]
        .anchor
        .pane_id
        .clone()
        .unwrap();
    let mut stream = attach_frames_backend(&backend, &pane).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    let (_, frame) = next_frame(&mut stream, &mut decoder).unwrap();
    assert_eq!(frame.current_working_directory.as_deref(), Some("/"));
    backend
        .request(&Request::Input {
            pane: pane.clone(),
            input: PaneInput::Write(b"\n".to_vec()),
        })
        .unwrap();
    let (_, echoed) = next_frame(&mut stream, &mut decoder).unwrap();
    assert_eq!(echoed.current_working_directory.as_deref(), Some("/"));
    std::fs::write(&release, "").unwrap();
    loop {
        let (_, frame) = next_frame(&mut stream, &mut decoder).unwrap();
        if frame.current_working_directory.as_deref() == expected.to_str() {
            break;
        }
    }
    backend
        .execute(MuxCommand::SplitPane {
            session_id: "session".into(),
            pane_id: None,
            direction: MuxSplitDirection::Right,
        })
        .unwrap();
    assert_eq!(launches.lock().unwrap()[1], Some(expected));
}

#[rstest]
fn identical_resize_does_not_publish_an_attached_frame(fixture: Result<Fixture>) {
    let fixture = fixture.unwrap();
    let mut stream = attach_frames(&fixture).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    let initial = next_frame(&mut stream, &mut decoder).unwrap().1;
    let mut size = native::server::DEFAULT_SIZE;
    for _ in 0..100 {
        fixture
            .backend
            .request(&Request::Resize {
                pane: fixture.pane.clone(),
                size,
            })
            .unwrap();
    }
    size.cols = size.cols.saturating_add(1);
    fixture
        .backend
        .request(&Request::Resize {
            pane: fixture.pane.clone(),
            size,
        })
        .unwrap();
    let changed = next_frame(&mut stream, &mut decoder).unwrap().1;
    assert_eq!(changed.cols, initial.cols.saturating_add(1));
}

struct BackpressuredInput {
    entered: mpsc::Sender<Vec<u8>>,
    drain: mpsc::Receiver<()>,
}
impl Write for BackpressuredInput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.entered
            .send(bytes.to_vec())
            .map_err(io::Error::other)?;
        self.drain.recv().map_err(io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn backpressure_connector(launcher: PaneLauncher) -> native::Connector {
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let (server, mut events) = native::server::MuxServer::with_server_id_and_clock(
        launcher,
        "backpressure".into(),
        advancing_clock(),
    )
    .unwrap();
    let event_server = Arc::clone(&server);
    runtime.spawn(async move {
        while let Some(event) = events.recv().await {
            let _ = event_server.handle_event(event).await;
        }
    });
    Arc::new(move || {
        let (client, peer) = tokio::io::duplex(64 * 1024);
        let server = Arc::clone(&server);
        runtime.spawn(async move {
            let _ = server.serve(peer).await;
        });
        Ok(Box::new(native::AsyncConnection::new(
            Arc::clone(&runtime),
            client,
        )))
    })
}

#[rstest]
#[case::prompt(true)]
#[case::paste(false)]
fn paste_backpressure_preserves_frames_commands_and_split(#[case] prompt: bool) {
    let outputs = Outputs::default();
    let (entered, writing) = mpsc::channel();
    let (drain, drained) = mpsc::channel();
    let writer = Mutex::new(Some(BackpressuredInput {
        entered,
        drain: drained,
    }));
    let base = launcher(Arc::clone(&outputs), Arc::new(AtomicUsize::new(0)));
    let connector = backpressure_connector(Arc::new(move |size, config| {
        let mut transport = base(size, config)?;
        let pending_writer = writer.lock().unwrap().take();
        if let Some(writer) = pending_writer {
            transport.writer = Box::new(writer);
        }
        Ok(transport)
    }));
    let backend = NativeBackend::with_connector(Path::new("paste-backpressure"), connector);
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "session".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })
        .unwrap();
    let pane = backend.snapshot().unwrap().sessions[0]
        .anchor
        .pane_id
        .clone()
        .unwrap();
    let mut stream = attach_frames_backend(&backend, &pane).unwrap();
    let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
    next_frame(&mut stream, &mut decoder).unwrap();
    let command = |request_id, command| {
        backend
            .request(&Request::PaneCommand {
                pane: pane.clone(),
                request_id,
                command,
            })
            .unwrap()
    };
    outputs.lock().unwrap()[&pane]
        .send(b"\x1b]133;P;bash;;1\x07ready".to_vec())
        .unwrap();
    next_frame(&mut stream, &mut decoder).unwrap();
    assert!(matches!(
        command(1, protocol::PaneCommand::Paste("x".repeat(1024 * 1024))),
        Response::PaneReply {
            result: Ok(protocol::PaneResult::Ok),
            ..
        }
    ));
    assert_eq!(writing.recv().unwrap().len(), 1024 * 1024);
    // The child cannot consume input until its output has been drained.
    outputs.lock().unwrap()[&pane]
        .send(b"output-drained".to_vec())
        .unwrap();
    let frame = next_frame(&mut stream, &mut decoder).unwrap().1;
    assert!(
        frame
            .text
            .iter()
            .collect::<String>()
            .contains("output-drained")
    );
    assert!(matches!(
        command(2, protocol::PaneCommand::CurrentDirectory),
        Response::PaneReply {
            result: Ok(protocol::PaneResult::Directory(_)),
            ..
        }
    ));
    backend
        .execute(MuxCommand::SplitPane {
            session_id: "session".into(),
            pane_id: Some(pane.clone()),
            direction: MuxSplitDirection::Right,
        })
        .unwrap();
    for id in 3..6 {
        assert!(matches!(
            command(
                id,
                protocol::PaneCommand::Paste("q".repeat(1024 * 1024 - 64))
            ),
            Response::PaneReply { result: Ok(_), .. }
        ));
    }
    // Fill the remaining bytes exactly, including conservative encoding headroom.
    assert!(matches!(
        command(6, protocol::PaneCommand::Paste("q".repeat(128))),
        Response::PaneReply { result: Ok(_), .. }
    ));
    outputs.lock().unwrap()[&pane]
        .send(b"\x1b[?2004h\x1b]133;D;0\x07\x1b]133;P;bash;;1\x07\x1b[6nready-again".to_vec())
        .unwrap();
    next_frame(&mut stream, &mut decoder).unwrap();
    let Response::PaneReply {
        result: Ok(protocol::PaneResult::Prompt(before)),
        ..
    } = command(7, protocol::PaneCommand::Prompt(None))
    else {
        panic!("missing prompt")
    };
    assert!(before.editable);
    let rejected = if prompt {
        protocol::PaneCommand::Prompt(Some((before.revision, "retry".into(), false)))
    } else {
        protocol::PaneCommand::Paste("rejected".into())
    };
    assert!(matches!(
        command(8, rejected),
        Response::PaneReply {
            result: Err(protocol::PaneError::Overloaded),
            ..
        }
    ));
    let Response::PaneReply {
        result: Ok(protocol::PaneResult::Prompt(after)),
        ..
    } = command(9, protocol::PaneCommand::Prompt(None))
    else {
        panic!("missing prompt")
    };
    assert_eq!(before, after);
    drain.send(()).unwrap();
    for _ in 0..3 {
        assert_eq!(writing.recv().unwrap().len(), 1024 * 1024 - 64);
        drain.send(()).unwrap();
    }
    assert_eq!(writing.recv().unwrap().len(), 128);
    drain.send(()).unwrap();
    let response = writing.recv().unwrap();
    assert!(response.starts_with(b"\x1b[") && response.ends_with(b"R"));
    drain.send(()).unwrap();
    // Raw input acknowledges write completion, so this barriers all queued pastes.
    drain.send(()).unwrap();
    backend
        .request(&Request::Input {
            pane: pane.clone(),
            input: PaneInput::Write(Vec::new()),
        })
        .unwrap();
    assert!(matches!(
        command(
            10,
            protocol::PaneCommand::Prompt(Some((before.revision, "retry".into(), false)))
        ),
        Response::PaneReply {
            result: Ok(protocol::PaneResult::Prompt(_)),
            ..
        }
    ));
    assert_eq!(writing.recv().unwrap(), b"\x1b[200~retry\x1b[201~");
    drain.send(()).unwrap();
}

struct PolicyControl {
    inner: Arc<dyn TerminalControl>,
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl TerminalControl for PolicyControl {
    fn close(&self) {
        self.inner.close();
    }
    fn exited(&self) -> Result<bool> {
        self.inner.exited()
    }
    fn resize(&self, size: TerminalSize) -> Result<()> {
        if self.entered.send(()).is_ok() {
            self.release.lock().unwrap().recv()?;
        }
        self.inner.resize(size)
    }
}
struct PolicyOutput {
    output: Output,
    read: mpsc::Sender<()>,
}
impl Read for PolicyOutput {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.output.read(bytes)?;
        if count > 0 {
            let _ = self.read.send(());
        }
        Ok(count)
    }
}

#[rstest]
fn policy_restore_converges_with_a_saturated_pane_queue() {
    let outputs = Outputs::default();
    let base = launcher(Arc::clone(&outputs), Arc::new(AtomicUsize::new(0)));
    let (entered, blocked) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (read, reads) = mpsc::channel();
    let blocked_transport = Mutex::new(Some((released, read)));
    let sending = Arc::clone(&outputs);
    let connector = backpressure_connector(Arc::new(move |size, config| {
        let mut transport = base(size, config)?;
        let blocked = blocked_transport.lock().unwrap().take();
        if let Some((released, read)) = blocked {
            let (tx, rx) = mpsc::channel();
            outputs
                .lock()
                .unwrap()
                .insert(config.pane_id.clone().unwrap(), tx);
            transport.reader =
                bootty_terminal::terminal_output::TerminalReader::Bytes(Box::new(PolicyOutput {
                    output: Output {
                        rx,
                        pending: io::Cursor::new(Vec::new()),
                    },
                    read,
                }));
            transport.control = Arc::new(PolicyControl {
                inner: transport.control,
                entered: entered.clone(),
                release: Mutex::new(released),
            });
        }
        Ok(transport)
    }));
    let backend = Arc::new(NativeBackend::with_connector(
        Path::new("policy-pressure"),
        Arc::clone(&connector),
    ));
    let mut first_pane = None;
    for id in ["a", "b"] {
        backend
            .execute(MuxCommand::CreateProjectSession {
                session_id: id.into(),
                cwd: "/".into(),
                tag: MuxSessionTag::default(),
                argv: None,
            })
            .unwrap();
        if first_pane.is_none() {
            first_pane = backend.snapshot().unwrap().sessions[0]
                .anchor
                .pane_id
                .clone();
        }
    }
    let mut panes: Vec<_> = backend
        .snapshot()
        .unwrap()
        .sessions
        .iter()
        .map(|s| s.anchor.pane_id.clone().unwrap())
        .collect();
    panes.sort_by_key(|pane| Some(pane) != first_pane.as_ref());
    let mut streams: Vec<_> = panes
        .iter()
        .map(|pane| {
            let mut stream = attach_frames_backend(&backend, pane).unwrap();
            let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
            let frame = next_frame(&mut stream, &mut decoder).unwrap().1;
            (stream, decoder, frame.colors)
        })
        .collect();
    // Block the first worker inside resize, then observe 129 output reads:
    // 128 commands are queued and the reader is blocked sending the last one.
    let resizing = NativeBackend::with_connector(Path::new("policy-pressure"), connector);
    let pane = panes[0].clone();
    let resize = std::thread::spawn(move || {
        resizing
            .request(&Request::Resize {
                pane,
                size: bootty_terminal::geometry::TerminalGeometry {
                    cols: 101,
                    ..native::server::DEFAULT_SIZE
                },
            })
            .unwrap()
    });
    blocked.recv().unwrap();
    // The launcher records the same sender map shared by the fixture.
    for _ in 0..129 {
        sending.lock().unwrap()[&panes[0]]
            .send(b"x".to_vec())
            .unwrap();
        reads.recv().unwrap();
    }
    let p0 = protocol::VtPolicy::default();
    let mut p1 = p0.clone();
    p1.colors.background = Some(bootty_config::color::Color::from_hex("#123456").unwrap());
    assert!(matches!(
        backend
            .request(&Request::Configure {
                launch: None,
                vt: Some(p1)
            })
            .unwrap(),
        Response::Ok
    ));
    // Establish that the unblocked pane actually applied P1 before restoring P0.
    let (stream, decoder, initial) = &mut streams[1];
    let changed = next_frame(stream, decoder).unwrap().1;
    assert_ne!(changed.colors.background, initial.background);
    assert!(matches!(
        backend
            .request(&Request::Configure {
                launch: None,
                vt: Some(p0)
            })
            .unwrap(),
        Response::Ok
    ));
    release.send(()).unwrap();
    resize.join().unwrap();
    drop(blocked);
    for (pane, (mut stream, mut decoder, colors)) in panes.iter().zip(streams) {
        backend
            .request(&Request::Resize {
                pane: pane.clone(),
                size: bootty_terminal::geometry::TerminalGeometry {
                    cols: 102,
                    ..native::server::DEFAULT_SIZE
                },
            })
            .unwrap();
        loop {
            let frame = next_frame(&mut stream, &mut decoder).unwrap().1;
            if frame.cols == 102 {
                assert_eq!(frame.colors, colors);
                break;
            }
        }
    }
}
