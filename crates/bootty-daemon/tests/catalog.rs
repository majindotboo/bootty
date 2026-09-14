#![cfg(test)]
#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use assert_fs::prelude::*;
use bootty_config::ApplicationIdentity;
use bootty_mux::rmux::endpoint_path_for;
use bootty_mux::{
    command::MuxCommand,
    snapshot::{MuxSessionTag, MuxSnapshot},
};
use rmux_sdk::{Rmux, RmuxEndpoint};
use rstest::rstest;
use tokio::runtime::Builder;

fn create_fixture_dir(
    path: impl AsRef<Path>,
) -> std::result::Result<(), assert_fs::fixture::FixtureError> {
    assert_fs::fixture::ChildPath::new(path.as_ref().to_path_buf()).create_dir_all()
}

struct DaemonCleanup(Option<PathBuf>);

impl DaemonCleanup {
    fn shutdown(&self) -> Result<()> {
        let Some(endpoint) = self.0.as_ref().filter(|endpoint| endpoint.exists()) else {
            return Ok(());
        };
        Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                Rmux::builder()
                    .endpoint(RmuxEndpoint::UnixSocket(endpoint.clone()))
                    .connect()
                    .await?
                    .shutdown()
                    .await
            })
            .context("shut down the private rmux daemon")?;
        Ok(())
    }
}

impl Drop for DaemonCleanup {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("private rmux daemon cleanup failed: {error}");
        }
    }
}

const REAL_DAEMON_HELPER_ENV: &str = "BOOTTY_DAEMON_CATALOG_RECOVERY_HELPER";
const DEVELOPMENT_NAMESPACE: &str = "bootty-dev-0123456789abcdef";

#[rstest]
#[case::production("bootty")]
#[case::development("bootty-dev")]
fn a_real_daemon_autostarts_rmux_and_round_trips_session_tags(
    #[case] identity: &str,
) -> Result<()> {
    // Unix sockets have a short path limit, including the development namespace.
    let directory = assert_fs::TempDir::new_in("/tmp")?;
    let rmux_root = directory.path().join("rmux");
    create_fixture_dir(&rmux_root)?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "a_real_daemon_round_trips_a_session_tag_through_rmux_helper",
        ])
        .env(REAL_DAEMON_HELPER_ENV, identity)
        .env(
            bootty_config::DEVELOPMENT_NAMESPACE_ENV,
            DEVELOPMENT_NAMESPACE,
        )
        .env("RMUX_TMPDIR", rmux_root)
        .env("BOOTTY_DAEMON_RECOVERY_ROOT", directory.path())
        .status()?;

    anyhow::ensure!(status.success(), "the daemon round-trip helper failed");
    Ok(())
}

#[test]
fn a_real_daemon_round_trips_a_session_tag_through_rmux_helper() -> Result<()> {
    let Ok(identity) = std::env::var(REAL_DAEMON_HELPER_ENV) else {
        return Ok(());
    };
    let application_identity = ApplicationIdentity::parse(&identity).expect("fixture identity");
    let root = std::path::PathBuf::from(
        std::env::var_os("BOOTTY_DAEMON_RECOVERY_ROOT").expect("recovery root"),
    );
    let state = root.join("daemon.sqlite");
    let config_root = root.join("config");
    let empty_path = root.join("empty-path");
    create_fixture_dir(&config_root)?;
    create_fixture_dir(&empty_path)?;
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");
    let endpoint = endpoint_path_for(application_identity)?;
    let other_endpoint = endpoint_path_for(match application_identity {
        ApplicationIdentity::Production => ApplicationIdentity::Development,
        ApplicationIdentity::Development => ApplicationIdentity::Production,
    })?;
    let rmux_root = root.join("rmux").canonicalize()?;
    anyhow::ensure!(
        endpoint.starts_with(&rmux_root),
        "the selected daemon endpoint stays under the test rmux root"
    );
    anyhow::ensure!(
        other_endpoint.starts_with(&rmux_root),
        "the other daemon endpoint stays under the test rmux root"
    );
    anyhow::ensure!(!endpoint.exists(), "the selected daemon is not running yet");
    anyhow::ensure!(!other_endpoint.exists(), "the other daemon is not running");
    let mut cleanup = DaemonCleanup(Some(endpoint.clone()));

    // Observe the launcher's child environment, then run the actual embedded daemon.
    let launcher = root.join("daemon-launcher");
    assert_fs::fixture::ChildPath::new(&launcher).write_str(
        "#!/bin/sh\nprintf '%s\\n' \"$BOOTTY_APPLICATION_IDENTITY\" \"$BOOTTY_DEVELOPMENT_NAMESPACE\" >> \"$BOOTTY_TEST_DAEMON_LAUNCH_ENV\"\nexec \"$BOOTTY_TEST_DAEMON_BINARY\" \"$@\"\n",
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
    let launch_environment = root.join("daemon-launch-environment");
    let inherited_identity = if identity == "bootty" {
        "bootty-dev"
    } else {
        "bootty"
    };
    let run = |args: &[String]| {
        std::process::Command::new(daemon)
            .args(["--application-identity", &identity])
            .env(bootty_config::APPLICATION_IDENTITY_ENV, inherited_identity)
            .env("BOOTTY_DAEMON_BINARY", &launcher)
            .env("BOOTTY_TEST_DAEMON_BINARY", daemon)
            .env("BOOTTY_TEST_DAEMON_LAUNCH_ENV", &launch_environment)
            .env("BOOTTY_DAEMON_STATE", &state)
            .env("XDG_CONFIG_HOME", &config_root)
            .env("HOME", &root)
            .env("PATH", &empty_path)
            .env("SHELL", "/bin/sh")
            .env("BOOTTY_SHELL", "/bin/sh")
            .args(args)
            .output()
    };
    let remote_space = |command: &str, options: &[(&str, &str)]| {
        let mut args = vec!["remote-space".to_owned(), command.to_owned()];
        args.extend(
            options
                .iter()
                .flat_map(|(name, value)| [(*name).to_owned(), (*value).to_owned()]),
        );
        let output = run(&args).with_context(|| format!("run remote-space {command}"))?;
        anyhow::ensure!(
            output.status.success(),
            "remote-space {command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok::<_, anyhow::Error>(output)
    };

    round_trip_session(
        &remote_space,
        &root,
        &endpoint,
        &other_endpoint,
        &launch_environment,
        &identity,
        &mut cleanup,
    )?;
    Ok(())
}

fn round_trip_session(
    remote_space: &impl Fn(&str, &[(&str, &str)]) -> Result<std::process::Output>,
    root: &Path,
    endpoint: &Path,
    other_endpoint: &Path,
    launch_environment: &Path,
    identity: &str,
    cleanup: &mut DaemonCleanup,
) -> Result<()> {
    let created = remote_space("create", &[("--name", "Recovery"), ("--backend", "rmux")])?;
    let space: serde_json::Value = serde_json::from_slice(&created.stdout)?;
    let space_id = space["id"].as_str().expect("Space id");
    let session_id = format!("bootty-recovery-{}", std::process::id());
    let session_identity = format!("{session_id}-identity");
    let payload = bootty_mux::remote_space::encode_command(&MuxCommand::CreateProjectSession {
        session_id: session_id.clone(),
        cwd: root.to_string_lossy().into_owned(),
        tag: MuxSessionTag {
            identity: Some(session_identity.clone()),
            space: Some(space_id.to_owned()),
        },
    })?;
    remote_space(
        "execute",
        &[
            ("--id", space_id),
            ("--backend", "rmux"),
            ("--payload", &payload),
        ],
    )
    .context("autostart rmux and create the project session")?;

    let snapshot = remote_space("snapshot", &[("--id", space_id), ("--backend", "rmux")])?;
    let snapshot: MuxSnapshot = serde_json::from_slice(&snapshot.stdout)?;
    let created = snapshot
        .sessions
        .iter()
        .find(|session| session.name == session_id)
        .expect("the created session is visible through its Space");
    anyhow::ensure!(
        created.tag.identity.as_deref() == Some(session_identity.as_str()),
        "the created session has the expected identity tag"
    );
    anyhow::ensure!(
        created.tag.space.as_deref() == Some(space_id),
        "the created session has the expected Space tag"
    );

    let renamed_name = format!("{session_id}-renamed");
    let rename = bootty_mux::remote_space::encode_command(&MuxCommand::RenameSession {
        session_id: session_id.clone(),
        name: renamed_name.clone(),
    })?;
    remote_space(
        "execute",
        &[
            ("--id", space_id),
            ("--backend", "rmux"),
            ("--payload", &rename),
        ],
    )
    .context("rename the project session")?;
    let snapshot = remote_space("snapshot", &[("--id", space_id), ("--backend", "rmux")])?;
    let snapshot: MuxSnapshot = serde_json::from_slice(&snapshot.stdout)?;
    let renamed = snapshot
        .sessions
        .iter()
        .find(|session| session.name == renamed_name)
        .expect("the renamed session still belongs to its Space");
    anyhow::ensure!(
        renamed.tag.identity.as_deref() == Some(session_identity.as_str()),
        "the renamed session retains its identity tag"
    );

    anyhow::ensure!(
        endpoint.exists(),
        "the selected identity owns the running daemon"
    );
    anyhow::ensure!(
        !other_endpoint.exists(),
        "the other identity was not started"
    );
    anyhow::ensure!(
        std::fs::read_to_string(launch_environment)?
            == format!("{identity}\n{DEVELOPMENT_NAMESPACE}\n"),
        "all requests reuse one daemon with the selected child identity"
    );
    let ditch = bootty_mux::remote_space::encode_command(&MuxCommand::DitchSession {
        session_id: renamed_name,
    })?;
    remote_space(
        "execute",
        &[
            ("--id", space_id),
            ("--backend", "rmux"),
            ("--payload", &ditch),
        ],
    )
    .context("close the final project session")?;
    // rmux exits after its final session closes. A second shutdown handshake races that exit.
    cleanup.0 = None;
    Ok(())
}
