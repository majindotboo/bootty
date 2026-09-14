#![cfg(test)]

use std::{
    io::Write,
    process::{Command, Stdio},
};

use bootty_config::config::SshRemoteConfig;
use bootty_host::{REMOTE_DAEMON_PROGRAM, ssh::SshRemote};
use pretty_assertions::assert_eq;
use rstest::rstest;

const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\nclipboard transfer fixture";
const DIGEST: &str = "9986f8439c26ee434b33b968a4181ae2a018e08b4eacc0ba725dba02c61b17d9";

#[rstest]
#[case(false)]
#[case(true)]
fn daemon_proxy_preserves_binary_input_and_rejects_truncation(#[case] truncate: bool) {
    let directory = assert_fs::TempDir::new().unwrap();
    let (_, args) = SshRemote::new(SshRemoteConfig::for_host("fixture-host"))
        .proxy_command(
            REMOTE_DAEMON_PROGRAM,
            &[
                "clipboard-upload".to_owned(),
                IMAGE.len().to_string(),
                DIGEST.to_owned(),
            ],
        )
        .unwrap();
    let payload = args.last().unwrap().split_whitespace().last().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .args(["remote-exec", payload])
        .env(bootty_config::APPLICATION_IDENTITY_ENV, "bootty-dev")
        .env("XDG_CONFIG_HOME", directory.path())
        .env("XDG_STATE_HOME", directory.path())
        .env("TMPDIR", directory.path())
        .env("TMP", directory.path())
        .env("TEMP", directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(if truncate { &IMAGE[..12] } else { IMAGE })
        .unwrap();
    let output = child.wait_with_output().unwrap();
    if truncate {
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("incomplete"));
        assert_eq!(output.stdout, Vec::<u8>::new());
    } else {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path: std::path::PathBuf = serde_json::from_slice(&output.stdout).unwrap();
        assert!(path.starts_with(directory.path()));
        assert_eq!(std::fs::read(path).unwrap(), IMAGE);
    }
}
