#![cfg(unix)]
use anyhow::{Context as _, ensure};
use assert_fs::prelude::*;
use bootty_config::config::{WslDistribution, WslRemoteConfig};
use bootty_host::{CommandOutput, CommandRunner, wsl::WslRemote};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

struct LinuxProcess {
    home: PathBuf,
    corrupt: bool,
}
impl LinuxProcess {
    fn execute(&self, args: &[String], input: Option<Vec<u8>>) -> anyhow::Result<CommandOutput> {
        ensure!(
            args.get(4).is_some_and(|arg| arg == "--exec"),
            "WSL execution flag"
        );
        let (program, arguments) = args
            .get(5..)
            .and_then(|args| args.split_first())
            .context("WSL command arguments")?;
        let mut command = Command::new(program);
        command
            .args(arguments)
            .current_dir(&self.home)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.home.join("bin").display()),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if let Some(mut input) = input {
            if self.corrupt {
                input.push(b'x');
            }
            child
                .stdin
                .take()
                .context("piped WSL command input")?
                .write_all(&input)?;
        }
        let output = child.wait_with_output()?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8(output.stdout)?,
            stderr: String::from_utf8(output.stderr)?,
        })
    }
}
impl CommandRunner for LinuxProcess {
    fn run(&self, _: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        self.execute(args, None)
    }
    fn run_with_input(
        &self,
        _: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> anyhow::Result<CommandOutput> {
        self.execute(args, Some(input))
    }
}
#[rstest]
#[case(true, false)]
#[case(false, false)]
#[case(true, true)]
fn installer_publishes_only_verified_complete_candidates(
    #[case] compatible: bool,
    #[case] corrupt: bool,
) {
    use std::os::unix::fs::PermissionsExt;
    let home = assert_fs::TempDir::new().unwrap();
    // macOS supplies shasum; Linux normally supplies sha256sum directly.
    if cfg!(target_os = "macos") {
        let checksum = home.child("bin/sha256sum");
        checksum
            .write_str("#!/bin/sh\nexec /usr/bin/shasum -a 256 \"$@\"\n")
            .unwrap();
        std::fs::set_permissions(checksum.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let installed = home.child(format!(
        ".bootty/bin/bootty-daemon-{}-{}.exe",
        bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
        env!("CARGO_PKG_VERSION")
    ));
    installed.write_str("prior daemon").unwrap();
    let asset = home.child("asset");
    let version = if compatible {
        format!(
            "{}:{}",
            bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
            env!("CARGO_PKG_VERSION")
        )
    } else {
        "incompatible".to_owned()
    };
    let contents = format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n");
    asset.write_str(&contents).unwrap();
    let remote = WslRemote::new(WslRemoteConfig {
        distribution: WslDistribution::new("fixture").unwrap(),
    });
    let result = remote.install_daemon(
        asset.path(),
        &LinuxProcess {
            home: home.path().to_owned(),
            corrupt,
        },
    );
    assert_eq!(result.is_ok(), compatible && !corrupt, "{result:?}");
    installed.assert(if compatible && !corrupt {
        contents.as_str()
    } else {
        "prior daemon"
    });
    assert_eq!(
        std::fs::read_dir(home.child(".bootty/bin").path())
            .unwrap()
            .count(),
        1
    );
}
