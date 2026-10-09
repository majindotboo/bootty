use super::{
    client::RemoteLink,
    protocol::{self, RemoteProcessRequest, RemoteTerminalSize},
    server::Bootstrap,
    tls::Identity,
};
use crate::{CancellableCommandRunner, CommandCancellation, CommandRunner as _, ssh::SshRemote};
use anyhow::{Context as _, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_config::{config::SshRemoteConfig, identity::ApplicationIdentity};
use bootty_write::{NewFileMode, WriteTarget};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::{BufRead as _, Read as _, Write as _},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[derive(Deserialize, Serialize, PartialEq, Eq)]
enum Descriptor {
    Ready {
        port: u16,
        token: Vec<u8>,
        certificate: Vec<u8>,
        #[serde(default)]
        transport: String,
    },
    Unavailable {
        retry_after: u64,
    },
}

#[derive(Deserialize, Serialize)]
struct LocalRequest {
    token: Vec<u8>,
    request: RemoteProcessRequest,
}

/// Returns a local relay argv without doing network or filesystem mutations on the UI thread.
/// # Errors
/// Returns invalid request or serialization errors.
pub fn proxy_command(
    remote: &SshRemote,
    program: &str,
    args: &[String],
    terminal: bool,
    cwd: Option<&str>,
) -> Result<Option<(String, Vec<String>)>> {
    let Some(daemon) = local_daemon() else {
        return Ok(None);
    };
    let request = RemoteProcessRequest {
        program: program.to_owned(),
        args: args.to_vec(),
        cwd: cwd.map(str::to_owned),
        terminal: terminal.then_some(RemoteTerminalSize { cols: 80, rows: 24 }),
    };
    request.validate()?;
    Ok(Some((
        daemon.to_string_lossy().into_owned(),
        vec![
            "--application-identity".into(),
            identity_argument().into(),
            "remote-link-exec".into(),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(remote.target())?),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&request)?),
        ],
    )))
}

fn local_daemon() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let daemon = executable.with_file_name(if cfg!(windows) {
        "bootty-daemon.exe"
    } else {
        "bootty-daemon"
    });
    daemon.is_file().then_some(daemon)
}

fn descriptor_path(config: &SshRemoteConfig) -> Result<PathBuf> {
    let config_path = ApplicationIdentity::for_process().default_config_path();
    let root = config_path
        .parent()
        .context("remote transport config directory")?
        .join("remote-links");
    fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    let hash = Sha256::digest(serde_json::to_vec(config)?);
    Ok(root.join(format!(
        "{}-{}.json",
        crate::REMOTE_DAEMON_PROTOCOL_VERSION,
        URL_SAFE_NO_PAD.encode(hash)
    )))
}

fn read_descriptor(path: &Path) -> Result<Descriptor> {
    let file = fs::File::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            bail!("remote transport descriptor is not private");
        }
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        bail!("remote transport descriptor is too large");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

type LocalStream = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;

async fn connect_descriptor(descriptor: &Descriptor) -> Result<(LocalStream, Vec<u8>)> {
    let Descriptor::Ready {
        port,
        token,
        certificate,
        ..
    } = descriptor
    else {
        bail!("shared remote transport is temporarily unavailable");
    };
    if token.len() != 32 || *port == 0 {
        bail!("invalid remote transport descriptor");
    }
    let config = super::tls::local_client(certificate.clone().into())?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let stream = tokio::time::timeout(Duration::from_secs(1), async {
        let stream =
            tokio::net::TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, *port))).await?;
        stream.set_nodelay(true)?;
        // Authenticate the relay before sending its token or a captured command.
        connector
            .connect(
                rustls::pki_types::ServerName::try_from(protocol::SERVER_NAME)?,
                stream,
            )
            .await
            .map_err(anyhow::Error::from)
    })
    .await??;
    Ok((stream, token.clone()))
}

async fn connect_broker(config: &SshRemoteConfig) -> Result<(LocalStream, Vec<u8>)> {
    let path = descriptor_path(config)?;
    let previous = read_descriptor(&path).ok();
    if let Some(descriptor) = &previous {
        if let Ok(connection) = connect_descriptor(descriptor).await {
            return Ok(connection);
        }
        if matches!(descriptor, Descriptor::Unavailable { retry_after } if *retry_after > now_seconds())
        {
            bail!("shared remote transport is temporarily unavailable");
        }
    }
    let descriptor = prepare_descriptor(config, &path, previous.as_ref())?;
    connect_descriptor(&descriptor).await
}

fn prepare_descriptor(
    config: &SshRemoteConfig,
    path: &Path,
    previous: Option<&Descriptor>,
) -> Result<Descriptor> {
    let target = WriteTarget::resolve(path).map_err(bootty_write::ResolveTargetError::into_io)?;
    let lock = target.lock()?;
    if let Ok(descriptor) = read_descriptor(path)
        && (Some(&descriptor) != previous
            || matches!(&descriptor, Descriptor::Unavailable { retry_after } if *retry_after > now_seconds()))
    {
        return Ok(descriptor);
    }
    let mut command = Command::new(local_daemon().context("local Bootty daemon is missing")?);
    command
        .args([
            "--application-identity",
            identity_argument(),
            "remote-link-broker",
        ])
        .arg(URL_SAFE_NO_PAD.encode(serde_json::to_vec(config)?))
        .stdout(Stdio::piped())
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    rmux_os::daemon::configure_hidden_daemon_command(&mut command, true);
    let mut child = rmux_os::daemon::spawn_hidden_daemon_command(&mut command)?;
    let mut response = String::new();
    std::io::BufReader::new(child.stdout.take().context("remote relay startup")?)
        .take(4096)
        .read_line(&mut response)?;
    let descriptor: Descriptor =
        serde_json::from_str(&response).context("direct remote transport startup failed")?;
    lock.replace(&serde_json::to_vec(&descriptor)?, NewFileMode::Private)
        .map_err(bootty_write::CommitError::into_io)?;
    drop(lock);
    Ok(descriptor)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_secs())
}

/// # Errors
/// Returns invalid target, listener, or authentication setup errors.
pub fn run_broker(payload: &str) -> Result<()> {
    let config: SshRemoteConfig = decode(payload)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let identity = Identity::generate()?;
        let remote = SshRemote::new(config);
        let link = match bootstrap(&remote, &identity).await {
            Ok(link) => link,
            Err(error) => {
                eprintln!("Direct remote transport unavailable: {error:#}");
                println!(
                    "{}",
                    serde_json::to_string(&Descriptor::Unavailable {
                        retry_after: now_seconds().saturating_add(60)
                    })?
                );
                return Ok(());
            }
        };
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(
            identity.server_tls(None, protocol::LOCAL_ALPN)?,
        ));
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let mut token = vec![0; 32];
        getrandom::fill(&mut token)?;
        println!(
            "{}",
            serde_json::to_string(&Descriptor::Ready {
                port: listener.local_addr()?.port(),
                token: token.clone(),
                certificate: identity.cert.to_vec(),
                transport: link.transport_name().to_owned(),
            })?
        );
        std::io::stdout().flush()?;
        let link = Arc::new(link);
        let mut clients = tokio::task::JoinSet::new();
        loop {
            if clients.len() >= 64 {
                _ = clients.join_next().await;
                continue;
            }
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break; };
                    if link.is_closed() { break; }
                    clients.spawn(relay(stream, token.clone(), link.clone(), acceptor.clone()));
                }
                _ = clients.join_next(), if !clients.is_empty() => {},
                () = link.closed() => break,
                () = tokio::time::sleep(Duration::from_secs(300)), if clients.is_empty() => break,
            }
        }
        clients.abort_all();
        Ok(())
    })
}

async fn bootstrap(remote: &SshRemote, identity: &Identity) -> Result<RemoteLink> {
    let (program, args) = remote.raw_command(&crate::exec::remote_program_line(&[
        "--application-identity".into(),
        identity_argument().into(),
        "remote-link-bootstrap".into(),
    ]));
    let certificate = identity.cert.to_vec();
    let response = tokio::task::spawn_blocking(move || {
        let runner = CancellableCommandRunner::with_deadline(
            CommandCancellation::default(),
            Instant::now()
                .checked_add(Duration::from_secs(8))
                .context("bootstrap deadline overflow")?,
        );
        let output = runner.run_with_input(&program, &args, certificate)?;
        if !output.success {
            bail!(
                "remote transport bootstrap failed: {}",
                output.stderr.trim()
            );
        }
        if output.stdout.len() > 32 * 1024 {
            bail!("remote bootstrap response is too large");
        }
        Ok::<Bootstrap, anyhow::Error>(serde_json::from_str(&output.stdout)?)
    })
    .await??;
    if response.certificate.len() > 16 * 1024 {
        bail!("remote certificate is too large");
    }
    let direct = RemoteLink::connect(
        identity,
        response.certificate.clone().into(),
        response.address,
    );
    let tunneled = async {
        // Give a reachable direct host a head start, without waiting out a blocked UDP dial.
        tokio::time::sleep(Duration::from_millis(100)).await;
        super::tunnel::connect(
            remote,
            identity,
            response.certificate.into(),
            response.tcp_port,
        )
        .await
    };
    tokio::pin!(direct, tunneled);
    tokio::select! {
        link = &mut direct => match link { Ok(link) => Ok(link), Err(_) => tunneled.await },
        link = &mut tunneled => match link { Ok(link) => Ok(link), Err(_) => direct.await },
    }
}

async fn relay(
    stream: tokio::net::TcpStream,
    token: Vec<u8>,
    link: Arc<RemoteLink>,
    acceptor: tokio_rustls::TlsAcceptor,
) -> Result<()> {
    stream.set_nodelay(true)?;
    let mut stream =
        tokio::time::timeout(Duration::from_secs(1), acceptor.accept(stream)).await??;
    let request: LocalRequest =
        tokio::time::timeout(Duration::from_secs(3), protocol::read_json(&mut stream)).await??;
    if request.token != token {
        bail!("remote relay authorization failed");
    }
    request.request.validate()?;
    let (mut send, mut recv) = link.start(&request.request).await?.into_streams();
    // Acknowledgment precedes process data: after this point no request is ever replayed.
    stream.write_u8(1).await?;
    let (mut input, mut output) = tokio::io::split(stream);
    tokio::try_join!(
        tokio::io::copy(&mut input, &mut send),
        tokio::io::copy(&mut recv, &mut output)
    )?;
    _ = send.shutdown().await;
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T> {
    if payload.len() > protocol::MAX_FRAME {
        bail!("remote relay payload is too large");
    }
    Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?)
}

/// # Errors
/// Returns invalid target/request, ambiguous connection loss, or process transport errors.
pub fn run_proxy(config: &str, payload: &str) -> Result<i32> {
    let config: SshRemoteConfig = decode(config)?;
    let mut request: RemoteProcessRequest = decode(payload)?;
    if request.terminal.is_some() {
        request.terminal = Some(current_size());
    }
    request.validate()?;
    let _terminal_mode = TerminalMode::enter(request.terminal.is_some())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(proxy(config, request))
}

async fn proxy(config: SshRemoteConfig, request: RemoteProcessRequest) -> Result<i32> {
    let Ok((mut stream, token)) = connect_broker(&config).await else {
        return fallback(config, &request);
    };
    protocol::write_json(
        &mut stream,
        &LocalRequest {
            token,
            request: request.clone(),
        },
    )
    .await?;
    // Once sent, a request may have started even if its acknowledgment is lost.
    if stream
        .read_u8()
        .await
        .context("remote relay closed before acknowledgment")?
        != 1
    {
        bail!("invalid remote relay acknowledgment");
    }
    let (mut output, mut input) = tokio::io::split(stream);
    let (frames, mut receive) = tokio::sync::mpsc::channel::<(u8, Vec<u8>)>(8);
    start_input(frames, request.terminal.is_some());
    let mut writer = tokio::spawn(async move {
        while let Some((kind, bytes)) = receive.recv().await {
            protocol::write_frame(&mut input, kind, &bytes).await?;
        }
        // EOF closes the child's stdin, not its still-pending stdout or exit status.
        std::future::pending::<Result<()>>().await
    });
    let received = async {
        loop {
            let (kind, bytes) = protocol::read_frame(&mut output).await?;
            match kind {
                protocol::STDOUT => {
                    let mut output = std::io::stdout().lock();
                    output.write_all(&bytes)?;
                    output.flush()?;
                }
                protocol::STDERR => {
                    let mut output = std::io::stderr().lock();
                    output.write_all(&bytes)?;
                    output.flush()?;
                }
                protocol::EXIT if bytes.len() == 4 => {
                    return Ok(i32::from_be_bytes(bytes.as_slice().try_into()?));
                }
                _ => bail!("invalid remote output"),
            }
        }
    };
    let result = tokio::select! {
        result = received => result,
        result = &mut writer => { result??; bail!("remote input stream ended unexpectedly"); }
    };
    writer.abort();
    result
}

fn start_input(frames: tokio::sync::mpsc::Sender<(u8, Vec<u8>)>, terminal: bool) {
    let input = frames.clone();
    std::thread::spawn(move || {
        let mut bytes = vec![0; protocol::CHUNK];
        loop {
            let count = match std::io::stdin().read(&mut bytes) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            let kind = if count == 0 {
                protocol::EOF
            } else {
                protocol::INPUT
            };
            let Some(bytes) = bytes.get(..count) else {
                return;
            };
            if input.blocking_send((kind, bytes.to_vec())).is_err() || count == 0 {
                return;
            }
        }
    });
    if terminal {
        std::thread::spawn(move || {
            let mut size = current_size();
            loop {
                std::thread::sleep(Duration::from_millis(50));
                let next = current_size();
                if next == size {
                    continue;
                }
                size = next;
                let Ok(bytes) = serde_json::to_vec(&size) else {
                    return;
                };
                if frames.blocking_send((protocol::RESIZE, bytes)).is_err() {
                    return;
                }
            }
        });
    }
}

fn current_size() -> RemoteTerminalSize {
    rmux_os::terminal::current_size().map_or(RemoteTerminalSize { cols: 80, rows: 24 }, |size| {
        RemoteTerminalSize {
            cols: size.cols.max(1),
            rows: size.rows.max(1),
        }
    })
}

fn fallback(config: SshRemoteConfig, request: &RemoteProcessRequest) -> Result<i32> {
    let remote = SshRemote::new(config);
    let args = crate::exec::proxy_command_args_in(
        &request.program,
        &request.args,
        request.terminal.is_some(),
        request.cwd.as_deref(),
    )?;
    let line = crate::exec::remote_program_line(&args);
    let mode = if request.terminal.is_some() {
        vec!["-t"]
    } else {
        vec!["-o", "BatchMode=yes"]
    };
    let (program, args) = remote.build_line(line, &mode);
    let status = Command::new(program).args(args).status()?;
    Ok(status.code().unwrap_or(1))
}

fn identity_argument() -> &'static str {
    match ApplicationIdentity::for_process() {
        ApplicationIdentity::Production => "bootty",
        ApplicationIdentity::Development => "bootty-dev",
    }
}

#[cfg(unix)]
struct TerminalMode(Option<rustix::termios::Termios>);
#[cfg(not(unix))]
struct TerminalMode(bool);

impl TerminalMode {
    fn enter(terminal: bool) -> Result<Self> {
        use std::io::IsTerminal as _;
        let enabled = terminal && std::io::stdin().is_terminal();
        #[cfg(unix)]
        {
            use rustix::termios::{OptionalActions, tcgetattr, tcsetattr};
            let original = enabled.then(|| tcgetattr(std::io::stdin())).transpose()?;
            if let Some(original) = &original {
                let mut raw = original.clone();
                raw.make_raw();
                tcsetattr(std::io::stdin(), OptionalActions::Now, &raw)?;
            }
            Ok(Self(original))
        }
        #[cfg(not(unix))]
        {
            if enabled {
                crossterm::terminal::enable_raw_mode()?;
            }
            Ok(Self(enabled))
        }
    }
}

impl Drop for TerminalMode {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(original) = &self.0 {
            _ = rustix::termios::tcsetattr(
                std::io::stdin(),
                rustix::termios::OptionalActions::Now,
                original,
            );
        }
        #[cfg(not(unix))]
        if self.0 {
            _ = crossterm::terminal::disable_raw_mode();
        }
    }
}
