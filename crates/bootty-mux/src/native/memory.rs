//! In-process hosting of the same byte protocol, without a socket or executable.
use super::{
    backend::Connector, server::MuxServer, server_pane::PaneLauncher, stream::AsyncConnection,
};
use anyhow::Result;
use std::sync::Arc;

/// Host a native mux using an explicit process launcher and transport-neutral streams.
/// # Errors
/// Returns runtime initialization failures.
pub fn connector(launcher: PaneLauncher) -> Result<Connector> {
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    );
    let (server, mut exits) = MuxServer::new(launcher)?;
    let exit_server = Arc::clone(&server);
    runtime.spawn(async move {
        while let Some(event) = exits.recv().await {
            let _ = exit_server.handle_event(event).await;
        }
    });
    Ok(Arc::new(move || {
        let (client, stream) = tokio::io::duplex(64 * 1024);
        let server = Arc::clone(&server);
        runtime.spawn(async move {
            let _ = server.serve(stream).await;
        });
        Ok(Box::new(AsyncConnection::new(Arc::clone(&runtime), client)))
    }))
}
