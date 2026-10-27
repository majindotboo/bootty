#![cfg(test)]
#![cfg(unix)]

use std::{
    fs,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use bootty_agents::{
    AgentKind, TerminalAgentService, TerminalProviderTransport, TerminalQueryOutput,
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

type FixtureResult<T> = Result<T, Box<dyn std::error::Error>>;

struct CoordinatedProvider {
    directory: assert_fs::TempDir,
    program: String,
    started: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    transport: Arc<CoordinatedTransport>,
}

struct CoordinatedTransport {
    requests: AtomicUsize,
    started: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl CoordinatedTransport {
    fn output(&self, command: &Command) -> TerminalQueryOutput {
        let arguments = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        let stdout = if arguments == ["--version"] {
            b"fixture-provider 1\n".to_vec()
        } else {
            assert_eq!(arguments, ["auth", "status", "--json"]);
            let older = self.requests.fetch_add(1, Ordering::Relaxed) == 0;
            if older {
                self.started.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(30))
                    .unwrap();
            }
            let (email, subscription) = if older {
                ("older@example.com", "pro")
            } else {
                ("newer@example.com", "max")
            };
            serde_json::json!({"loggedIn":true,"authMethod":"claude.ai","email":email,"subscriptionType":subscription}).to_string().into_bytes()
        };
        TerminalQueryOutput {
            stdout,
            stderr: vec![],
            exit_code: Some(0),
            successful: true,
        }
    }
}

#[fixture]
fn coordinated_provider() -> FixtureResult<CoordinatedProvider> {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = assert_fs::TempDir::new()?;
    let program = directory.path().join("provider");
    fs::write(&program, "in-process provider")?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    let (started, arrival) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let transport = Arc::new(CoordinatedTransport {
        requests: AtomicUsize::new(0),
        started,
        release: Mutex::new(resume),
    });
    Ok(CoordinatedProvider {
        directory,
        program: program
            .to_str()
            .ok_or("Fixture path is not UTF-8")?
            .to_owned(),
        started: arrival,
        release,
        transport,
    })
}

fn newer_scope(
    fixture: &CoordinatedProvider,
    changed_scope: &str,
) -> FixtureResult<(String, &'static str, &'static str)> {
    let program = if changed_scope == "program" {
        let path = fixture.directory.path().join("other-provider");
        fs::copy(&fixture.program, &path)?;
        path.to_str()
            .ok_or("Fixture program path is not UTF-8")?
            .to_owned()
    } else {
        fixture.program.clone()
    };
    let directory = if changed_scope == "directory" {
        "/fixture/newer-account"
    } else {
        "/fixture/older-account"
    };
    let model_provider = if changed_scope == "model_provider" {
        "newer-model-provider"
    } else {
        "older-model-provider"
    };
    Ok((program, directory, model_provider))
}

#[rstest]
#[case("same")]
#[case("directory")]
#[case("model_provider")]
#[case("program")]
fn older_refresh_cannot_replace_a_newer_request_or_notify_consumers(
    coordinated_provider: FixtureResult<CoordinatedProvider>,
    #[case] changed_scope: &str,
) {
    let fixture = coordinated_provider.unwrap();
    let service = Arc::new(
        TerminalAgentService::open(fixture.directory.path().join("catalog.json"))
            .unwrap()
            .with_provider_transport(TerminalProviderTransport {
                output: {
                    let transport = fixture.transport.clone();
                    Arc::new(move |command, _cancelled| Ok(transport.output(&command)))
                },
                ..TerminalProviderTransport::default()
            }),
    );
    let notifications = Arc::new(AtomicUsize::new(0));
    let observed_notifications = Arc::clone(&notifications);
    service.set_change_handler(Arc::new(move || {
        observed_notifications.fetch_add(1, Ordering::Relaxed);
    }));

    let older_service = Arc::clone(&service);
    let older_program = fixture.program.clone();
    let older = thread::spawn(move || {
        older_service.inspect_provider(
            AgentKind::Claude,
            &older_program,
            Some("/fixture/older-account"),
            Some("older-model-provider"),
        )
    });
    fixture
        .started
        .recv_timeout(Duration::from_secs(30))
        .unwrap();
    let checking = service.provider_status(
        AgentKind::Claude,
        &fixture.program,
        Some("/fixture/older-account"),
        Some("older-model-provider"),
    );

    let (newer_program, newer_directory, newer_model_provider) =
        newer_scope(&fixture, changed_scope).unwrap();
    let newer = service.inspect_provider(
        AgentKind::Claude,
        &newer_program,
        Some(newer_directory),
        Some(newer_model_provider),
    );
    let revision = service.revision();
    let notification_count = notifications.load(Ordering::Relaxed);
    fixture.release.send(()).unwrap();
    let older = older.join().unwrap();

    assert_eq!(checking.unwrap().message.as_deref(), Some("Checking…"));
    assert_eq!(older.account.as_deref(), Some("older@example.com"));
    assert_eq!(older.subscription.as_deref(), Some("pro"));
    assert_eq!(newer.account.as_deref(), Some("newer@example.com"));
    assert_eq!(newer.subscription.as_deref(), Some("max"));
    let retained = service
        .provider_status(
            AgentKind::Claude,
            &newer_program,
            Some(newer_directory),
            Some(newer_model_provider),
        )
        .unwrap();
    assert_eq!(retained.account, newer.account);
    assert_eq!(retained.subscription, newer.subscription);
    assert_eq!(service.revision(), revision);
    assert_eq!(notifications.load(Ordering::Relaxed), notification_count);
    assert_eq!(notification_count, 3);
    if changed_scope != "same" {
        assert!(
            service
                .provider_status(
                    AgentKind::Claude,
                    &fixture.program,
                    Some("/fixture/older-account"),
                    Some("older-model-provider"),
                )
                .is_none()
        );
    }
}
