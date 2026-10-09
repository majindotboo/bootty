use bootty_host::remote_link::{
    RemoteLink, RemoteLinkIdentity, RemoteLinkServer, RemoteOutput, RemoteProcessRequest,
    RemoteTerminalSize,
};
use pretty_assertions::assert_eq;
use proptest::{
    prelude::*,
    test_runner::{Config, TestRunner},
};
use rstest::rstest;
use std::net::{Ipv4Addr, SocketAddr};

#[derive(Clone, Copy, Debug)]
enum Transport {
    Quic,
    Tcp,
}

impl Transport {
    async fn connect(
        self,
        identity: &RemoteLinkIdentity,
        certificate: rustls::pki_types::CertificateDer<'static>,
        address: SocketAddr,
    ) -> anyhow::Result<RemoteLink> {
        match self {
            Self::Quic => RemoteLink::connect(identity, certificate, address).await,
            Self::Tcp => RemoteLink::connect_tcp(identity, certificate, address).await,
        }
    }
    fn address(self, server: &RemoteLinkServer) -> anyhow::Result<SocketAddr> {
        match self {
            Self::Quic => server.address(),
            Self::Tcp => server.tcp_address(),
        }
    }
}

async fn connection(
    transport: Transport,
) -> anyhow::Result<(RemoteLink, tokio::task::JoinHandle<anyhow::Result<()>>)> {
    let identity = RemoteLinkIdentity::generate()?;
    let server = RemoteLinkServer::bind(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        identity.cert.clone(),
    )?;
    let address = transport.address(&server)?;
    let certificate = server.certificate();
    let server = tokio::spawn(server.serve());
    let link = transport.connect(&identity, certificate, address).await?;
    Ok((link, server))
}

fn request(program: &str, args: &[&str]) -> RemoteProcessRequest {
    RemoteProcessRequest {
        program: program.into(),
        args: args.iter().map(|value| (*value).into()).collect(),
        cwd: None,
        terminal: None,
    }
}

async fn output(
    process: &mut bootty_host::remote_link::RemoteProcess,
) -> anyhow::Result<(Vec<u8>, Vec<u8>, i32)> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        match process.next().await? {
            RemoteOutput::Stdout(bytes) => stdout.extend(bytes),
            RemoteOutput::Stderr(bytes) => stderr.extend(bytes),
            RemoteOutput::Exit(code) => return Ok((stdout, stderr, code)),
        }
    }
}

#[cfg(unix)]
#[rstest]
#[case(0)]
#[case(7)]
#[tokio::test]
async fn process_stream_preserves_stdout_stderr_exit_and_cwd(
    #[case] code: i32,
    #[values(Transport::Quic, Transport::Tcp)] transport: Transport,
) {
    let (link, server) = connection(transport).await.expect("connection");
    let directory = assert_fs::TempDir::new().expect("cwd");
    let mut request = request(
        "/bin/sh",
        &[
            "-c",
            "printf stdout; printf stderr >&2; pwd; exit \"$1\"",
            "probe",
        ],
    );
    request.args.push(code.to_string());
    request.cwd = Some(directory.path().to_string_lossy().into_owned());
    let mut process = link.start(&request).await.expect("process");
    process.finish_input().await.expect("stdin EOF");
    let (stdout, stderr, exit) = output(&mut process).await.expect("output");
    assert_eq!(
        stdout,
        format!(
            "stdout{}\n",
            std::fs::canonicalize(directory.path())
                .expect("canonical cwd")
                .display()
        )
        .into_bytes()
    );
    assert_eq!(stderr, b"stderr");
    assert_eq!(exit, code);
    drop(process);
    drop(link);
    server.await.expect("server task").expect("server shutdown");
}

#[cfg(unix)]
#[rstest]
#[tokio::test]
async fn an_undrained_output_stream_cannot_block_another_process(
    #[values(Transport::Quic, Transport::Tcp)] transport: Transport,
) {
    let (link, server) = connection(transport).await.expect("connection");
    let mut noisy = link
        .start(&request("/bin/sh", &["-c", "head -c 4194304 /dev/zero"]))
        .await
        .expect("noisy process");
    let mut echo = link
        .start(&request("/bin/cat", &[]))
        .await
        .expect("interactive process");
    let bytes: Vec<u8> = (0..=255).cycle().take(32 * 1024).collect();
    echo.write(&bytes).await.expect("input");
    echo.finish_input().await.expect("EOF");
    let (stdout, stderr, exit) =
        tokio::time::timeout(std::time::Duration::from_secs(2), output(&mut echo))
            .await
            .expect("independent stream progress")
            .expect("output");
    assert_eq!(stdout, bytes);
    assert_eq!(stderr, Vec::<u8>::new());
    assert_eq!(exit, 0);
    let (stdout, _, exit) = output(&mut noisy).await.expect("output");
    assert_eq!(stdout, vec![0; 4_194_304]);
    assert_eq!(exit, 0);
    drop(noisy);
    drop(echo);
    drop(link);
    server.await.expect("server task").expect("server shutdown");
}

#[cfg(unix)]
#[rstest]
#[tokio::test]
async fn terminal_input_is_immediate_and_resize_reaches_the_remote_pty(
    #[values(Transport::Quic, Transport::Tcp)] transport: Transport,
) {
    let (link, server) = connection(transport).await.expect("connection");
    let mut request = request(
        "/bin/sh",
        &[
            "-c",
            "stty raw -echo; printf READY; dd bs=1 count=1 2>/dev/null; stty size",
        ],
    );
    request.terminal = Some(RemoteTerminalSize { cols: 80, rows: 24 });
    let mut process = link.start(&request).await.expect("terminal");
    let mut initial = Vec::new();
    while !initial.ends_with(b"READY") {
        let RemoteOutput::Stdout(bytes) = process.next().await.expect("ready") else {
            panic!("terminal failed before ready");
        };
        initial.extend(bytes);
    }
    process
        .resize(RemoteTerminalSize {
            cols: 120,
            rows: 40,
        })
        .await
        .expect("resize");
    process.write(b"x").await.expect("single key without Enter");
    let (stdout, stderr, code) = output(&mut process).await.expect("output");
    assert_eq!(stdout, b"x40 120\n");
    assert_eq!(stderr, Vec::<u8>::new());
    assert_eq!(code, 0);
    drop(process);
    drop(link);
    server.await.expect("server task").expect("server shutdown");
}

#[rstest]
#[tokio::test]
async fn an_unknown_client_cannot_execute_or_displace_the_authorized_client(
    #[values(Transport::Quic, Transport::Tcp)] transport: Transport,
) {
    let authorized = RemoteLinkIdentity::generate().expect("authorized identity");
    let stranger = RemoteLinkIdentity::generate().expect("stranger identity");
    let server = RemoteLinkServer::bind(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        authorized.cert.clone(),
    )
    .expect("server");
    let address = transport.address(&server).expect("address");
    let certificate = server.certificate();
    let server = tokio::spawn(server.serve());
    if let Ok(link) = transport
        .connect(&stranger, certificate.clone(), address)
        .await
        && let Ok(mut process) = link
            .start(&request("untrusted-client-must-never-execute", &[]))
            .await
    {
        assert!(
            process.next().await.is_err(),
            "no process stream can reach an unauthenticated peer"
        );
    }
    let link = transport
        .connect(&authorized, certificate, address)
        .await
        .expect("authorized client still connects");
    assert!(!link.is_closed());
    drop(link);
    server.await.expect("server task").expect("server shutdown");
}

#[rstest]
#[tokio::test]
async fn a_server_certificate_not_received_from_the_authenticated_bootstrap_is_rejected(
    #[values(Transport::Quic, Transport::Tcp)] transport: Transport,
) {
    let authorized = RemoteLinkIdentity::generate().expect("client");
    let other = RemoteLinkIdentity::generate().expect("unrelated certificate");
    let server = RemoteLinkServer::bind(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        authorized.cert.clone(),
    )
    .expect("server");
    let address = transport.address(&server).expect("address");
    let certificate = server.certificate();
    let server = tokio::spawn(server.serve());
    assert!(
        transport
            .connect(&authorized, other.cert, address)
            .await
            .is_err()
    );
    let link = transport
        .connect(&authorized, certificate, address)
        .await
        .expect("pinned server");
    drop(link);
    server.await.expect("server task").expect("server shutdown");
}

#[cfg(unix)]
#[rstest]
#[tokio::test]
async fn cancel_reaps_the_remote_process_before_returning_its_exit_status(
    #[values(Transport::Quic, Transport::Tcp)] transport: Transport,
    #[values(false, true)] terminal: bool,
) {
    let (link, server) = connection(transport).await.expect("connection");
    let mut execution = request("/bin/sh", &["-c", "trap '' HUP; printf '%s\\n' $$; cat"]);
    if terminal {
        execution.terminal = Some(RemoteTerminalSize { cols: 80, rows: 24 });
    }
    let mut process = link.start(&execution).await.expect("process");
    let mut pid = Vec::new();
    while !pid.ends_with(b"\n") {
        let RemoteOutput::Stdout(bytes) = process.next().await.expect("pid") else {
            panic!("missing process identity");
        };
        pid.extend(bytes);
    }
    let pid: u32 = String::from_utf8(pid)
        .expect("pid string")
        .trim()
        .parse()
        .expect("pid number");
    process.cancel().await.expect("cancel");
    let (_, _, status) =
        tokio::time::timeout(std::time::Duration::from_secs(2), output(&mut process))
            .await
            .expect("the entire captured process tree exits")
            .expect("output");
    assert_ne!(status, 0);
    let mut check = link
        .start(&request(
            "/bin/sh",
            &["-c", &format!("kill -0 {pid} 2>/dev/null")],
        ))
        .await
        .expect("liveness check");
    let (_, _, status) = output(&mut check).await.expect("output");
    assert_ne!(status, 0, "the canceled process has been reaped");
    drop(check);
    drop(process);
    drop(link);
    server.await.expect("server task").expect("server shutdown");
}

#[rstest]
#[case("", vec![], None)]
#[case("bad\0program", vec![], None)]
#[case("valid", vec!["bad\0argument".into()], None)]
#[case("valid", vec![], Some(RemoteTerminalSize { cols: 0, rows: 24 }))]
#[tokio::test]
async fn invalid_execution_requests_fail_before_opening_a_process_stream(
    #[values(Transport::Quic, Transport::Tcp)] transport: Transport,
    #[case] program: &str,
    #[case] args: Vec<String>,
    #[case] terminal: Option<RemoteTerminalSize>,
) {
    let (link, server) = connection(transport).await.expect("connection");
    let request = RemoteProcessRequest {
        program: program.into(),
        args,
        cwd: None,
        terminal,
    };
    assert!(link.start(&request).await.is_err());
    drop(link);
    server.await.expect("server task").expect("server shutdown");
}

#[cfg(unix)]
#[rstest]
fn arbitrary_binary_input_round_trips_without_text_conversion(
    #[values(Transport::Quic, Transport::Tcp)] transport: Transport,
) {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (link, server) = runtime.block_on(connection(transport)).expect("connection");
    let mut runner = TestRunner::new(Config {
        cases: 16,
        ..Config::default()
    });
    runner
        .run(
            &proptest::collection::vec(any::<u8>(), 0..100_000),
            |bytes| {
                let (stdout, stderr, code) = runtime.block_on(async {
                    let mut process = link
                        .start(&request("/bin/cat", &[]))
                        .await
                        .expect("process");
                    process.write(&bytes).await.expect("input");
                    process.finish_input().await.expect("EOF");
                    output(&mut process).await.expect("output")
                });
                prop_assert_eq!(stdout, bytes);
                prop_assert_eq!(stderr, Vec::<u8>::new());
                prop_assert_eq!(code, 0);
                Ok(())
            },
        )
        .expect("binary round-trip property");
    drop(link);
    runtime
        .block_on(server)
        .expect("server task")
        .expect("server shutdown");
}

#[cfg(unix)]
#[rstest]
#[tokio::test]
async fn packet_loss_preserves_bytes_and_interactive_progress_beside_blocked_output() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let identity = RemoteLinkIdentity::generate().expect("client");
    let server = RemoteLinkServer::bind(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        identity.cert.clone(),
    )
    .expect("server");
    let address = server.address().expect("server address");
    let certificate = server.certificate();
    let server = tokio::spawn(server.serve());
    let front = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("client-facing proxy");
    let proxy_address = front.local_addr().expect("proxy address");
    let back = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("server-facing proxy");
    back.connect(address).await.expect("proxy destination");
    let dropped = Arc::new(AtomicUsize::new(0));
    let loss = dropped.clone();
    let remaining = Arc::new(AtomicUsize::new(0));
    let loss_remaining = remaining.clone();
    // Drop one bounded burst after authentication. Periodic losses can repeatedly drop
    // sparse recovery/flow-control packets and turn this integrity test into a timing test.
    let proxy = tokio::spawn(async move {
        let mut client = None;
        let mut incoming = vec![0; 65_536];
        let mut outgoing = vec![0; 65_536];
        loop {
            tokio::select! {
                received = front.recv_from(&mut incoming) => {
                    let (count, source) = received.expect("client datagram");
                    client = Some(source);
                    if loss_remaining.try_update(Ordering::Relaxed, Ordering::Relaxed, |count| count.checked_sub(1)).is_ok() { loss.fetch_add(1, Ordering::Relaxed); continue; }
                    back.send(&incoming[..count]).await.expect("forward input");
                }
                received = back.recv(&mut outgoing) => {
                    let count = received.expect("server datagram");
                    if loss_remaining.try_update(Ordering::Relaxed, Ordering::Relaxed, |count| count.checked_sub(1)).is_ok() { loss.fetch_add(1, Ordering::Relaxed); continue; }
                    if let Some(client) = client { front.send_to(&outgoing[..count], client).await.expect("forward output"); }
                }
            }
        }
    });
    let link = RemoteLink::connect(&identity, certificate, proxy_address)
        .await
        .expect("authenticated lossy connection");
    remaining.store(3, Ordering::Relaxed);
    let mut noisy = link
        .start(&request("/bin/sh", &["-c", "head -c 4194304 /dev/zero"]))
        .await
        .expect("blocked bulk stream");
    let mut echo = link
        .start(&request("/bin/cat", &[]))
        .await
        .expect("interactive stream");
    // This caller writes before reading its echo. Keep that interactive burst bounded;
    // the separate 4 MiB stream supplies output pressure and verifies loss recovery.
    let bytes: Vec<u8> = (0..=255).cycle().take(32 * 1024).collect();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        echo.write(&bytes).await.expect("input during packet loss");
        echo.finish_input().await.expect("EOF");
        let (stdout, stderr, code) = output(&mut echo).await.expect("loss recovery");
        assert_eq!(stdout, bytes);
        assert_eq!(stderr, Vec::<u8>::new());
        assert_eq!(code, 0);
        let (stdout, stderr, code) = output(&mut noisy).await.expect("bulk stream recovery");
        assert_eq!(stdout, vec![0; 4_194_304]);
        assert_eq!(stderr, Vec::<u8>::new());
        assert_eq!(code, 0);
    })
    .await
    .expect("both streams recover without application replay");
    assert_eq!(
        dropped.load(Ordering::Relaxed),
        3,
        "proxy consumed the complete loss burst"
    );
    drop(echo);
    drop(noisy);
    drop(link);
    // Keep forwarding the close packet until the server has released its streams.
    server.await.expect("server task").expect("server shutdown");
    proxy.abort();
}
