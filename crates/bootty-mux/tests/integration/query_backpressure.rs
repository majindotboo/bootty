use super::*;

#[rstest]
fn configuration_ingress_progresses_while_topology_output_is_backpressured() {
    use tokio::io::AsyncReadExt as _;

    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let outputs = Outputs::default();
    let (server, _events) = native::server::MuxServer::with_server_id_and_clock(
        launcher(outputs, Arc::new(AtomicUsize::new(0))),
        "query-backpressure".into(),
        advancing_clock(),
    )
    .unwrap();
    let connecting_runtime = Arc::clone(&runtime);
    let connecting_server = Arc::clone(&server);
    let backend = NativeBackend::with_connector(
        Path::new("query-backpressure"),
        Arc::new(move || {
            let (client, peer) = tokio::io::duplex(64 * 1024);
            let server = Arc::clone(&connecting_server);
            connecting_runtime.spawn(async move {
                let _ = server.serve(peer).await;
            });
            Ok(Box::new(native::AsyncConnection::new(
                Arc::clone(&connecting_runtime),
                client,
            )))
        }),
    );
    let (mut client, peer) = tokio::io::duplex(256);
    let serving = Arc::clone(&server);
    runtime.spawn(async move {
        let _ = serving.serve(peer).await;
    });
    runtime.block_on(async {
        protocol::write(
            &mut client,
            &Frame::json(&Hello {
                version: protocol::WIRE_VERSION,
                workspace: "query-backpressure".into(),
                launch: None,
                vt: None,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        protocol::read(&mut client).await.unwrap();
    });
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "session".into(),
            cwd: "/".into(),
            tag: MuxSessionTag::default(),
            argv: None,
        })
        .unwrap();
    runtime.block_on(async {
        // Observe the notification header before writing: its body exceeds the
        // stream capacity, so the server is now writing while we send Configure.
        let mut first = [0];
        client.read_exact(&mut first).await.unwrap();
        let mut launch = bootty_terminal::SessionLaunchConfig::default();
        launch.env.push(("QUERY_TEST".into(), "x".repeat(4096)));
        let configure = Frame::json(&Request::Configure {
            launch: Some(launch),
            vt: None,
        })
        .unwrap();
        protocol::write(&mut client, &configure).await.unwrap();
        let mut header = [0; 4];
        header[0] = first[0];
        client.read_exact(&mut header[1..]).await.unwrap();
        let mut notification = vec![0; usize::try_from(u32::from_be_bytes(header)).unwrap()];
        client.read_exact(&mut notification).await.unwrap();
        let frame = Frame {
            kind: notification[0],
            bytes: notification[1..].to_vec(),
        };
        assert!(matches!(
            frame.decode::<Response>().unwrap(),
            Response::Changed(_)
        ));
        assert!(matches!(
            protocol::read(&mut client)
                .await
                .unwrap()
                .decode::<Response>()
                .unwrap(),
            Response::Ok
        ));
    });
}

#[rstest]
fn configuration_ingress_progresses_while_frame_output_is_backpressured() {
    use tokio::io::AsyncReadExt as _;

    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let outputs = Outputs::default();
    let (server, _events) = native::server::MuxServer::with_server_id_and_clock(
        launcher(Arc::clone(&outputs), Arc::new(AtomicUsize::new(0))),
        "frame-backpressure".into(),
        advancing_clock(),
    )
    .unwrap();
    let connecting_runtime = Arc::clone(&runtime);
    let connecting_server = Arc::clone(&server);
    let backend = NativeBackend::with_connector(
        Path::new("frame-backpressure"),
        Arc::new(move || {
            let (client, peer) = tokio::io::duplex(64 * 1024);
            let server = Arc::clone(&connecting_server);
            connecting_runtime.spawn(async move {
                let _ = server.serve(peer).await;
            });
            Ok(Box::new(native::AsyncConnection::new(
                Arc::clone(&connecting_runtime),
                client,
            )))
        }),
    );
    let (mut client, peer) = tokio::io::duplex(256);
    let serving = Arc::clone(&server);
    runtime.spawn(async move {
        let _ = serving.serve(peer).await;
    });
    runtime.block_on(async {
        protocol::write(
            &mut client,
            &Frame::json(&Hello {
                version: protocol::WIRE_VERSION,
                workspace: "frame-backpressure".into(),
                launch: None,
                vt: None,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        protocol::read(&mut client).await.unwrap();
    });
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
    runtime.block_on(async {
        // Drain topology before attaching the frame stream.
        protocol::read(&mut client).await.unwrap();
        protocol::write(
            &mut client,
            &Frame::json(&Request::AttachFrames { pane: pane.clone() }).unwrap(),
        )
        .await
        .unwrap();
        let reply = protocol::read(&mut client)
            .await
            .unwrap()
            .decode::<Response>()
            .unwrap();
        assert!(matches!(reply, Response::Ok));
        let mut first = [0];
        client.read_exact(&mut first).await.unwrap();
        let mut launch = bootty_terminal::SessionLaunchConfig::default();
        launch.env.push(("QUERY_TEST".into(), "x".repeat(4096)));
        let configure = Frame::json(&Request::Configure {
            launch: Some(launch),
            vt: None,
        })
        .unwrap();
        protocol::write(&mut client, &configure).await.unwrap();
        outputs.lock().unwrap()[&pane]
            .send(b"latest-frame-marker".to_vec())
            .unwrap();
        let mut header = [0; 4];
        header[0] = first[0];
        client.read_exact(&mut header[1..]).await.unwrap();
        let mut notification = vec![0; usize::try_from(u32::from_be_bytes(header)).unwrap()];
        client.read_exact(&mut notification).await.unwrap();
        let frame = Frame {
            kind: notification[0],
            bytes: notification[1..].to_vec(),
        };
        pretty_assertions::assert_eq!(frame.kind, protocol::FRAME_UPDATE);
        let mut decoder = bootty_terminal::frame_wire::FrameDecoder::default();
        let update = bootty_terminal::frame_wire::decode_update(&frame.bytes).unwrap();
        decoder.apply(update).unwrap();
        {
            let mut acknowledged = false;
            let mut latest = false;
            while !acknowledged || !latest {
                let wire = protocol::read(&mut client).await.unwrap();
                if wire.kind == protocol::FRAME_UPDATE {
                    let frame = decoder
                        .apply(bootty_terminal::frame_wire::decode_update(&wire.bytes).unwrap())
                        .unwrap();
                    if frame
                        .text
                        .iter()
                        .collect::<String>()
                        .contains("latest-frame-marker")
                    {
                        latest = true;
                    }
                } else {
                    assert!(matches!(wire.decode::<Response>().unwrap(), Response::Ok));
                    acknowledged = true;
                }
            }
        }
    });
}
