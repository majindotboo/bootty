#![cfg(target_os = "macos")]

use std::{io::Write as _, process::Command, process::Stdio};

use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case::parent_closed_input(b"".as_slice(), 0)]
#[case::malformed_snapshot(b"not-json\n".as_slice(), 1)]
#[case::truncated_snapshot(b"{}".as_slice(), 1)]
fn tray_helper_ends_with_its_parent_input(#[case] input: &[u8], #[case] expected: i32) {
    if expected != 0 {
        assert!(bootty_ui::read_agent_tray_snapshot(&mut std::io::Cursor::new(input)).is_err());
        return;
    }

    let mut helper = Command::new(env!("CARGO_BIN_EXE_bootty"))
        .arg("--agent-tray-stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the stateless tray helper");
    let mut stdin = helper.stdin.take().expect("helper input pipe");
    stdin.write_all(input).expect("write helper input");
    drop(stdin);
    let output = helper.wait_with_output().expect("wait for helper shutdown");
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, Vec::<u8>::new());
}
