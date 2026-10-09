#![cfg(test)]

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

fn transfer(request: &serde_json::Value, input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .env(bootty_config::APPLICATION_IDENTITY_ENV, "bootty-dev")
        .args([
            "--application-identity",
            "bootty-dev",
            "transfer",
            &URL_SAFE_NO_PAD.encode(serde_json::to_vec(request).unwrap()),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn upload_input(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let mut sha256 = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(&mut sha256, "{byte:02x}").unwrap();
    }
    let mut input = bytes.to_vec();
    serde_json::to_writer(
        &mut input,
        &serde_json::json!({"bytes": bytes.len(), "sha256": sha256}),
    )
    .unwrap();
    input.push(b'\n');
    input
}

#[rstest]
#[case(0, false)]
#[case(3 * 1024 * 1024 + 13, true)]
#[cfg(unix)]
fn private_uploads_survive_retries_without_replacing_existing_bytes(
    #[case] size: usize,
    #[case] fresh_account: bool,
) {
    let root = assert_fs::TempDir::new().unwrap();
    let account = if fresh_account {
        root.path().join("fresh-account")
    } else {
        root.path().to_path_buf()
    };
    let owner = "b".repeat(64);
    let directory = serde_json::json!({"root": account, "owner": owner});
    let prepared = transfer(
        &serde_json::json!({"operation":"prepare_private", "directory": directory}),
        &[],
    );
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let path: std::path::PathBuf = serde_json::from_slice(&prepared.stdout).unwrap();
    assert_eq!(
        path,
        account
            .canonicalize()
            .unwrap()
            .join("bootty-attachments/bootty-dev")
            .join(&owner)
    );
    let bytes = (0..251_u8).cycle().take(size).collect::<Vec<_>>();
    let request = serde_json::json!({"operation":"upload_private", "directory": directory, "name":"native-att-identity.bin", "bytes": size});
    let file = path.join("native-att-identity.bin");
    for _ in 0..2 {
        let result = transfer(&request, &upload_input(&bytes));
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let receipt: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(receipt["path"], serde_json::json!(file));
        assert_eq!(receipt["bytes"], size);
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for private in [&path, &file] {
            assert_eq!(
                std::fs::metadata(private).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
    let mut changed = bytes.clone();
    if let Some(byte) = changed.first_mut() {
        *byte = 252;
    } else {
        changed.push(252);
    }
    let mut conflict = request;
    conflict["bytes"] = serde_json::json!(changed.len());
    assert!(
        !transfer(&conflict, &upload_input(&changed))
            .status
            .success()
    );
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
    // A disconnect before receipt must not publish even an empty destination.
    let interrupted = serde_json::json!({"operation":"upload_private", "directory": directory, "name":"interrupted.bin", "bytes":5});
    assert!(!transfer(&interrupted, b"hel").status.success());
    assert!(!path.join("interrupted.bin").exists());
    assert_eq!(std::fs::read_dir(path).unwrap().count(), 1);
}

#[rstest]
#[case("../escape", "safe.bin")]
#[case("b", "safe.bin")]
#[case(
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "../escape"
)]
fn private_uploads_reject_caller_path_components(#[case] owner: &str, #[case] name: &str) {
    let root = assert_fs::TempDir::new().unwrap();
    let request = serde_json::json!({"operation":"upload_private", "directory":{"root":root.path(),"owner":owner}, "name":name, "bytes":0});
    assert!(!transfer(&request, &upload_input(&[])).status.success());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn private_uploads_reject_symlink_or_public_storage(#[case] symlink: bool) {
    use std::os::unix::fs::{PermissionsExt as _, symlink as link};
    let root = assert_fs::TempDir::new().unwrap();
    let external = assert_fs::TempDir::new().unwrap();
    let storage = root.path().join("bootty-attachments");
    if symlink {
        link(external.path(), &storage).unwrap();
    } else {
        std::fs::create_dir(&storage).unwrap();
        std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let request = serde_json::json!({"operation":"prepare_private", "directory":{"root":root.path(),"owner":"b".repeat(64)}});
    assert!(!transfer(&request, &[]).status.success());
    assert_eq!(std::fs::read_dir(external.path()).unwrap().count(), 0);
    if !symlink {
        assert_eq!(
            std::fs::metadata(storage).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
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
