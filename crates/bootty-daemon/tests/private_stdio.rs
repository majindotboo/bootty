#![cfg(unix)]
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::json;
use std::{
    hash::{Hash, Hasher as _},
    io::{BufRead as _, BufReader, Read as _, Write as _},
    os::unix::{fs::PermissionsExt as _, net::UnixStream},
    process::{Command, Stdio},
};

#[rstest]
fn private_remote_endpoint_keeps_envelopes_images_and_cleanup_bounded() {
    let root = assert_fs::TempDir::new().unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.path().hash(&mut hasher);
    let name = format!("bt-tool-{:064x}", hasher.finish());
    let directory = std::path::Path::new("/tmp").join(&name);
    let connection = directory.join("connection.json");
    let token = "a".repeat(64);
    let material =
        serde_json::to_vec(&json!({"socket":directory.join("tools.sock"),"token":token})).unwrap();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .args([
            "--application-identity",
            "bootty-dev",
            "private-stdio-relay",
        ])
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = relay.stdin.take().unwrap();
    serde_json::to_writer(
        &mut input,
        &json!({"name":name,"files":[{"name":"connection.json","bytes":material}]}),
    )
    .unwrap();
    input.write_all(b"\n").unwrap();
    input.flush().unwrap();
    let mut output = BufReader::new(relay.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "READY\n");
    for path in [&directory, &connection, &directory.join("tools.sock")] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    let request =
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"computer_snapshot"}});
    let response = json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"image","mimeType":"image/png","data":"x".repeat(2*1024*1024)}]}});
    let mut client = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .args([
            "--application-identity",
            "bootty-dev",
            "--agent-tool-stdio",
            connection.to_str().unwrap(),
        ])
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut client_input = client.stdin.take().unwrap();
    serde_json::to_writer(&mut client_input, &request).unwrap();
    client_input.write_all(b"\n").unwrap();
    drop(client_input);
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        json!({"token":token,"request":request})
    );
    serde_json::to_writer(&mut input, &response).unwrap();
    input.write_all(b"\n").unwrap();
    input.flush().unwrap();
    let result = client.wait_with_output().unwrap();
    assert!(result.status.success(), "{:?}", result.stderr);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap(),
        response
    );
    // EOF is observed at the next accepted request, then removes only this owned directory.
    let mut last = UnixStream::connect(directory.join("tools.sock")).unwrap();
    last.write_all(b"{}\n").unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    drop(input);
    let mut closed = Vec::new();
    last.read_to_end(&mut closed).unwrap();
    assert_eq!(closed, Vec::<u8>::new());
    assert!(relay.wait().unwrap().success());
    assert!(!directory.exists());
}

#[rstest]
fn relay_cleanup_retains_unexpected_files_before_removing_anything() {
    let root = assert_fs::TempDir::new().unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.path().hash(&mut hasher);
    let name = format!("bt-tool-{:064x}", hasher.finish());
    let directory = std::path::Path::new("/tmp").join(&name);
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    for file in ["connection.json", "unexpected"] {
        let path = directory.join(file);
        std::fs::write(&path, b"preserve").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let result = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .args([
            "--application-identity",
            "bootty-dev",
            "private-stdio-cleanup",
            &name,
        ])
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    for file in ["connection.json", "unexpected"] {
        assert_eq!(std::fs::read(directory.join(file)).unwrap(), b"preserve");
    }
    std::fs::remove_dir_all(directory).unwrap();
}
