#![cfg(test)]

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};
#[rstest]
#[case(b"hel".as_slice())]
#[case(b"hello{\"bytes\":5,\"sha256\":\"wrong\"}\n".as_slice())]
fn incomplete_or_corrupt_uploads_never_publish_and_remove_staging(#[case] input: &[u8]) {
    let directory = assert_fs::TempDir::new().unwrap();
    let destination = directory.path().join("destination");
    let request =
        serde_json::json!({"operation":"upload","path":destination.to_string_lossy(),"bytes":5});
    let mut child = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .env(bootty_config::APPLICATION_IDENTITY_ENV, "bootty-dev")
        .args([
            "transfer",
            &URL_SAFE_NO_PAD.encode(serde_json::to_vec(&request).unwrap()),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
