#![cfg(unix)]

use anyhow::Result;
use bootty_mux::{
    command::MuxCommand,
    native::{self, NativeBackend},
    snapshot::MuxSessionTag,
};
use std::{
    path::Path,
    sync::{Arc, Barrier, mpsc},
    time::{Duration, Instant},
};

#[rstest::rstest]
#[case(8)]
#[case(32)]
fn concurrent_panes_report_exit(#[case] count: usize) -> Result<()> {
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    );
    // A fixed clock supplies the worker's bookkeeping; completion is event-driven.
    let now = Instant::now();
    let (server, mut events) = native::server::MuxServer::with_server_id_and_clock(
        native::local_launcher(),
        "concurrent-launch-test".into(),
        Arc::new(move || now),
    )?;
    let connector: native::Connector = {
        let runtime = runtime.clone();
        Arc::new(move || {
            let (client, peer) = tokio::io::duplex(64 * 1024);
            let server = server.clone();
            runtime.spawn(async move {
                let _ = server.serve(peer).await;
            });
            Ok(Box::new(native::AsyncConnection::new(
                runtime.clone(),
                client,
            )))
        })
    };
    let barrier = Arc::new(Barrier::new(count));
    let (tx, rx) = mpsc::channel();
    for index in 0..count {
        let barrier = barrier.clone();
        let tx = tx.clone();
        let connector = connector.clone();
        std::thread::spawn(move || {
            let workspace = format!("workspace-{index}");
            let backend = NativeBackend::with_connector(Path::new(&workspace), connector);
            barrier.wait();
            let result = backend.execute(MuxCommand::CreateProjectSession {
                session_id: "session".into(),
                cwd: "/".into(),
                tag: MuxSessionTag::default(),
                argv: Some(vec!["/bin/sh".into(), "-c".into(), "exit 0".into()]),
            });
            let _ = tx.send(result.map_err(|error| error.to_string()));
        });
    }
    drop(tx);
    for _ in 0..count {
        rx.recv_timeout(Duration::from_secs(30))?
            .map_err(anyhow::Error::msg)?;
    }
    runtime.block_on(async {
        let mut exited = std::collections::HashSet::new();
        while exited.len() < count {
            // A timeout is only a hang guard; success requires every pane's real exit event.
            let event = tokio::time::timeout(Duration::from_secs(30), events.recv())
                .await?
                .ok_or_else(|| anyhow::anyhow!("pane event stream closed"))?;
            if let native::server::ServerEvent::PaneExited { workspace, .. } = event {
                anyhow::ensure!(exited.insert(workspace), "duplicate pane exit");
            }
        }
        Ok(())
    })
}
