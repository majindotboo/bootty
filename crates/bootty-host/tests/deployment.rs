#![cfg(unix)]

use std::{cell::RefCell, fs, rc::Rc};

use anyhow::Result;
use assert_fs::prelude::*;
use bootty_config::config::SshRemoteConfig as SshTarget;
use bootty_host::ssh::SshRemote;
use bootty_host::{CommandOutput, CommandRunner};
use bootty_os::spawn::CommandSpawn as _;
use pretty_assertions::assert_eq;

#[derive(Clone)]
struct InstallerRunner {
    state: Rc<RefCell<InstallerState>>,
    candidate_ping: CommandOutput,
}

#[derive(Default)]
struct InstallerState {
    installed: bool,
    commands: Vec<String>,
}

impl CommandRunner for InstallerRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let command = args.last().cloned().unwrap_or_default();
        self.state
            .borrow_mut()
            .commands
            .push(format!("{program} {command}"));

        let output = if command == "uname -s && uname -m" {
            output(true, platform_probe())
        } else if command.ends_with("remote-ping") && command.contains(".upload") {
            self.candidate_ping.clone()
        } else if command.ends_with("remote-ping") {
            if self.state.borrow().installed {
                compatible_ping()
            } else {
                output(false, "installed daemon is incompatible")
            }
        } else if command.contains("mv -f") {
            self.state.borrow_mut().installed = true;
            output(true, "")
        } else {
            output(true, "")
        };
        Ok(output)
    }
}

fn output(success: bool, message: &str) -> CommandOutput {
    CommandOutput {
        success,
        stdout: if success {
            message.to_owned()
        } else {
            String::new()
        },
        stderr: if success {
            String::new()
        } else {
            message.to_owned()
        },
    }
}

fn compatible_ping() -> CommandOutput {
    output(
        true,
        &format!(
            "{}:{}",
            bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
            env!("CARGO_PKG_VERSION")
        ),
    )
}

const fn platform_probe() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "Darwin\narm64\n"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "Darwin\nx86_64\n"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "Linux\naarch64\n"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "Linux\nx86_64\n"
    } else {
        "unsupported\nunsupported\n"
    }
}

const PACKAGED_DAEMON: &str = "packaged daemon fixture";

/// Run the named test again in a copy of this test binary that ships a packaged host daemon. A
/// client finds that daemon beside its own executable, so a copy keeps it from every other test
/// process. Returns true in the launching process, which has nothing left to do.
fn in_packaged_client(test: &str) -> Result<bool> {
    const CHILD: &str = "BOOTTY_TEST_PACKAGED_CLIENT";
    if std::env::var_os(CHILD).is_some() {
        return Ok(false);
    }
    let bundle = assert_fs::TempDir::new()?;
    let client = bundle.child("client");
    fs::copy(std::env::current_exe()?, client.path())?;
    bundle.child("bootty-daemon").write_str(PACKAGED_DAEMON)?;
    let run = std::process::Command::new(client.path())
        .args([test, "--exact", "--include-ignored", "--test-threads=1"])
        .env(CHILD, "1")
        .bootty_output()?;
    let stdout = String::from_utf8_lossy(&run.stdout);
    anyhow::ensure!(
        run.status.success() && stdout.contains("1 passed"),
        "packaged client failed:\n{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    Ok(true)
}

fn remote() -> SshRemote {
    SshRemote::new(SshTarget::for_host("devbox"))
}

#[test]
fn candidate_is_verified_before_publication() -> Result<()> {
    if in_packaged_client("deployment::candidate_is_verified_before_publication")? {
        return Ok(());
    }
    // Readiness is shared by every handle to one host, and only a published daemon sets it, so
    // the rejected candidates go first.
    let failed = Rc::new(RefCell::new(InstallerState::default()));
    let error = remote()
        .ensure_daemon_with(&InstallerRunner {
            state: Rc::clone(&failed),
            candidate_ping: output(false, "candidate is incompatible"),
        })
        .expect_err("an incompatible candidate must not publish");
    let failed = failed.borrow();
    let has = |needle| {
        failed
            .commands
            .iter()
            .any(|command| command.contains(needle))
    };
    let observed = (
        failed.installed,
        has("mv -f"),
        has("rm -f") && has(".upload"),
    );
    assert_eq!(observed, (false, false, true));
    assert_eq!(
        error.to_string(),
        format!(
            "uploaded Bootty daemon on devbox did not start with protocol {}: candidate is incompatible",
            bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION
        )
    );

    let wrong_response = remote()
        .ensure_daemon_with(&InstallerRunner {
            state: Rc::new(RefCell::new(InstallerState::default())),
            candidate_ping: output(true, "a different daemon"),
        })
        .expect_err("the observed response should explain a failed ping");
    assert_eq!(
        wrong_response.to_string(),
        format!(
            "uploaded Bootty daemon on devbox did not start with protocol {}: unexpected response \"a different daemon\"",
            bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION
        )
    );
    let successful = Rc::new(RefCell::new(InstallerState::default()));
    remote().ensure_daemon_with(&InstallerRunner {
        state: Rc::clone(&successful),
        candidate_ping: compatible_ping(),
    })?;
    let successful = successful.borrow();
    let position = |needle| {
        successful
            .commands
            .iter()
            .position(|command| command.contains(needle))
            .expect("observed install step")
    };
    let candidate_ping = position(".upload");
    let publication = position("mv -f");
    let final_ping = successful
        .commands
        .iter()
        .rposition(|command| command.ends_with("remote-ping"))
        .expect("final ping");
    let observed = (
        successful.installed,
        candidate_ping < publication,
        publication < final_ping,
    );
    assert_eq!(observed, (true, true, true));

    Ok(())
}

#[rstest::rstest]
#[case("Permission denied (publickey)")]
#[case("Connection timed out")]
fn failed_remote_probe_preserves_the_connection_error(#[case] reason: &'static str) {
    struct Unreachable(&'static str);
    impl CommandRunner for Unreachable {
        fn run(&self, _: &str, _: &[String]) -> Result<CommandOutput> {
            Ok(output(false, self.0))
        }
    }
    let remote = SshRemote::new(SshTarget::for_host("unreachable"));
    let error = remote.ensure_daemon_with(&Unreachable(reason)).unwrap_err();
    assert!(error.to_string().contains(reason), "{error:#}");
}

/// A remote home: uploads land in it, shell commands run in it, and a daemon answers its ping only
/// when its file exists.
struct RemoteHome(assert_fs::TempDir);
impl CommandRunner for RemoteHome {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        if program.ends_with("scp") {
            let [.., local, remote] = args else {
                anyhow::bail!("scp needs a source and a destination");
            };
            let (_, path) = remote.split_once(':').unwrap_or_default();
            fs::copy(local, self.0.path().join(path))?;
            return Ok(output(true, ""));
        }
        let line = args.last().cloned().unwrap_or_default();
        if let Some(daemon) = line.strip_suffix(" remote-ping") {
            let daemon = self.0.path().join(daemon.trim_matches('\''));
            return Ok(if daemon.is_file() {
                compatible_ping()
            } else {
                output(false, "no daemon at this path")
            });
        }
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &line])
            .current_dir(self.0.path())
            .bootty_output()?;
        Ok(CommandOutput {
            success: status.status.success(),
            stdout: String::from_utf8(status.stdout)?,
            stderr: String::from_utf8(status.stderr)?,
        })
    }
}

/// Builds of one version ship different daemons. Each installs its own beside the others, and an
/// install retires only the daemons nothing has run for a month.
#[test]
fn each_shipped_daemon_installs_beside_others_and_retires_unused_ones() -> Result<()> {
    if in_packaged_client(
        "deployment::each_shipped_daemon_installs_beside_others_and_retires_unused_ones",
    )? {
        return Ok(());
    }
    let home = RemoteHome(assert_fs::TempDir::new()?);
    let daemons = home.0.child(".bootty/bin");
    let protocol = bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION;
    let old_build = daemons.child(format!(
        "bootty-daemon-{protocol}-{}.exe",
        env!("CARGO_PKG_VERSION")
    ));
    let recent_build = daemons.child(format!("bootty-daemon-{protocol}-0123456789abcdef.exe"));
    for daemon in [&old_build, &recent_build] {
        daemon.write_str("another build")?;
    }
    let month_ago = std::time::SystemTime::now() - std::time::Duration::from_hours(31 * 24);
    fs::File::options()
        .write(true)
        .open(old_build.path())?
        .set_modified(month_ago)?;

    SshRemote::new(SshTarget::for_host("build-host")).ensure_daemon_with(&home)?;

    let installed = home
        .0
        .child(bootty_host::remote_exec_program().trim_start_matches("./"));
    installed.assert(PACKAGED_DAEMON);
    let present = |daemon: &assert_fs::fixture::ChildPath| daemon.path().exists();
    assert_eq!(
        (
            present(&old_build),
            present(&recent_build),
            installed.path() != old_build.path()
        ),
        (false, true, true)
    );
    Ok(())
}
