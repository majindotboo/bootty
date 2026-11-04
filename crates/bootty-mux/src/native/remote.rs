use super::{
    backend::{Connector, Duplex, IdentityVerifier, validate_welcome},
    protocol::{self, Frame, Hello, Response, WIRE_VERSION, Welcome},
    stream::AsyncConnection,
};
use bootty_config::config::SshRemoteConfig;
use bootty_host::{remote_link::RemoteMux, ssh::SshRemote};
use std::sync::{Arc, Mutex};

pub(super) fn connector(
    config: SshRemoteConfig,
    workspace: String,
    verify: IdentityVerifier,
) -> Connector {
    // Lazy: neither runtime creation, installation nor SSH runs on the UI thread.
    let owner = Mutex::new(None);
    Arc::new(move || {
        let mut owner = owner
            .lock()
            .map_err(|_| anyhow::anyhow!("remote mux owner poisoned"))?;
        let (runtime, mux) = if let Some(owner) = owner.as_ref() {
            owner
        } else {
            let runtime = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()?,
            );
            let mux = {
                let _context = runtime.enter();
                RemoteMux::new(
                    SshRemote::new(config.clone()),
                    bootty_config::ApplicationIdentity::for_process()
                        .namespace()
                        .to_owned(),
                    {
                        let verify = Arc::clone(&verify);
                        let workspace = workspace.clone();
                        Arc::new(move |mut stream| {
                            let verify = Arc::clone(&verify);
                            let workspace = workspace.clone();
                            Box::pin(async move {
                                protocol::write(
                                    &mut stream,
                                    &Frame::json(&Hello {
                                        version: WIRE_VERSION,
                                        workspace,
                                        launch: None,
                                        vt: None,
                                    })?,
                                )
                                .await?;
                                let frame = protocol::read(&mut stream).await?;
                                if let Ok(Response::Error(error)) = frame.decode::<Response>() {
                                    anyhow::bail!(error);
                                }
                                if let Ok(Response::IncompatibleVersion(error)) =
                                    frame.decode::<Response>()
                                {
                                    return Err(error.into());
                                }
                                let welcome: Welcome = frame.decode()?;
                                validate_welcome(&welcome, Some(&verify))
                            })
                        })
                    },
                )?
            };
            owner.insert((runtime, mux))
        };
        let stream = runtime.block_on(mux.open())?;
        let stream: Box<dyn Duplex> = Box::new(AsyncConnection::new(Arc::clone(runtime), stream));
        Ok(stream)
    })
}
