#![cfg(unix)]

use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::PermissionsExt as _,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use bootty_agents::{AgentKind, TerminalAgentService};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

type FixtureResult<T> = Result<T, Box<dyn std::error::Error>>;

struct CoordinatedProvider {
    directory: assert_fs::TempDir,
    program: String,
    started: File,
    release: File,
}

fn quoted(path: &std::path::Path) -> FixtureResult<String> {
    let text = path.to_str().ok_or("Fixture path is not UTF-8")?;
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

#[fixture]
fn coordinated_provider() -> FixtureResult<CoordinatedProvider> {
    let directory = assert_fs::TempDir::new()?;
    let started = directory.path().join("started");
    let release = directory.path().join("release");
    if !Command::new("mkfifo")
        .args([&started, &release])
        .status()?
        .success()
    {
        return Err("Could not create provider coordination pipes".into());
    }
    // Keep both ends open so provider launch and release never depend on open order.
    let started_pipe = OpenOptions::new().read(true).write(true).open(&started)?;
    let release_pipe = OpenOptions::new().read(true).write(true).open(&release)?;
    let program = directory.path().join("provider");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\n\
             if [ \"$*\" = '--version' ]; then printf '%s\\n' 'fixture-provider 1'; exit 0; fi\n\
             [ \"$*\" = 'auth status --json' ] || exit 9\n\
             if mkdir {} 2>/dev/null; then\n\
               printf A > {}\n\
               IFS= read -r release < {}\n\
               printf '%s\\n' '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"older@example.com\",\"subscriptionType\":\"pro\"}}'\n\
             else\n\
               printf '%s\\n' '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"newer@example.com\",\"subscriptionType\":\"max\"}}'\n\
             fi\n",
            quoted(&directory.path().join("first-request"))?,
            quoted(&started)?,
            quoted(&release)?,
        ),
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    let program = program
        .to_str()
        .ok_or("Fixture program path is not UTF-8")?
        .to_owned();
    Ok(CoordinatedProvider {
        directory,
        program,
        started: started_pipe,
        release: release_pipe,
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
    let mut fixture = coordinated_provider.unwrap();
    let service = Arc::new(
        TerminalAgentService::open(fixture.directory.path().join("catalog.json")).unwrap(),
    );
    let notifications = Arc::new(AtomicUsize::new(0));
    let observed_notifications = Arc::clone(&notifications);
    service.set_change_handler(Arc::new(move || {
        observed_notifications.fetch_add(1, Ordering::Relaxed);
    }));

    let mut started = fixture.started.try_clone().unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    let arrival = thread::spawn(move || {
        let _ = sender.send(started.read_exact(&mut [0]));
    });
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
    // The deadline bounds a broken fixture; successful coordination advances by pipe messages.
    receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    arrival.join().unwrap();
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
    writeln!(fixture.release, "continue").unwrap();
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
