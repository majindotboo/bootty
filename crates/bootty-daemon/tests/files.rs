#![cfg(test)]

use assert_fs::prelude::*;
use bootty_config::config::SshRemoteConfig;
use bootty_host::{
    REMOTE_DAEMON_PROGRAM,
    files::{FileRequest, FileResponse, encode_document},
    ssh::SshRemote,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

fn execute(request: &FileRequest) -> std::process::Output {
    let (_, args) = SshRemote::new(SshRemoteConfig::for_host("fixture-host"))
        .proxy_command(REMOTE_DAEMON_PROGRAM, &["file".to_owned()])
        .unwrap();
    let payload = args.last().unwrap().split_whitespace().last().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .args(["remote-exec", payload])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(request).unwrap())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[rstest]
fn daemon_file_transport_preserves_paths_and_checks_revisions() {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("quotes ' dollar $ file.txt");
    file.write_str("before\r\n").unwrap();
    let path = file.path().to_str().unwrap().to_owned();
    let output = execute(&FileRequest::Read { path: path.clone() });
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let FileResponse::Document(snapshot) = serde_json::from_slice(&output.stdout).unwrap() else {
        panic!("document");
    };
    assert_eq!(snapshot.contents().unwrap(), "before\r\n");
    let save = FileRequest::Save {
        path,
        expected_digest: snapshot.digest,
        content_base64: encode_document("after 中文\r\n").unwrap(),
    };
    let output = execute(&save);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(matches!(
        serde_json::from_slice::<FileResponse>(&output.stdout).unwrap(),
        FileResponse::Saved { .. }
    ));
    file.assert("after 中文\r\n");
    file.write_str("external edit").unwrap();
    assert!(!execute(&save).status.success());
    file.assert("external edit");
}
