use std::{fmt::Write as _, io::Cursor, path::PathBuf};

use anyhow::{Context as _, Result, ensure};
use assert_fs::prelude::*;
use bootty_config::config::SshRemoteConfig;
use bootty_host::{
    CommandOutput, CommandRunner, REMOTE_DAEMON_PROTOCOL_VERSION,
    clipboard_image::{MAX_CLIPBOARD_IMAGE_BYTES, receive_clipboard_image, upload_clipboard_image},
    remote::RemoteHost,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use sha2::{Digest, Sha256};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nclipboard transfer fixture";

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}

proptest! {
    #[test]
    fn complete_uploads_preserve_bytes_in_private_unique_files(tail in prop::collection::vec(any::<u8>(), 0..2048)) {
        let directory = assert_fs::TempDir::new().unwrap();
        let mut bytes = PNG.to_vec(); bytes.extend(tail);
        let first = receive_clipboard_image(Cursor::new(&bytes), u64::try_from(bytes.len()).unwrap(), &digest(&bytes), directory.path()).unwrap();
        let second = receive_clipboard_image(Cursor::new(&bytes), u64::try_from(bytes.len()).unwrap(), &digest(&bytes), directory.path()).unwrap();
        prop_assert_ne!(&first, &second);
        prop_assert_eq!(std::fs::read(&first).unwrap(), bytes);
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            prop_assert_eq!(std::fs::metadata(first).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}

#[rstest]
#[case(u64::try_from(PNG.len()).unwrap().saturating_add(1), digest(PNG))]
#[case(u64::try_from(PNG.len()).unwrap().saturating_sub(1), digest(PNG))]
#[case(u64::try_from(PNG.len()).unwrap(), "0".repeat(64))]
#[case(MAX_CLIPBOARD_IMAGE_BYTES + 1, digest(PNG))]
#[case(u64::try_from(PNG.len()).unwrap(), "invalid".to_owned())]
fn rejected_transfers_leave_no_file(#[case] length: u64, #[case] hash: String) {
    let directory = assert_fs::TempDir::new().unwrap();
    assert!(receive_clipboard_image(Cursor::new(PNG), length, &hash, directory.path()).is_err());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[derive(serde::Deserialize)]
struct Request {
    program: String,
    args: Vec<String>,
}

struct RemoteReceiver {
    directory: PathBuf,
    reject: bool,
}

impl CommandRunner for RemoteReceiver {
    fn run(&self, _program: &str, _args: &[String]) -> Result<CommandOutput> {
        Ok(CommandOutput {
            success: true,
            stdout: format!(
                "{REMOTE_DAEMON_PROTOCOL_VERSION}:{}",
                env!("CARGO_PKG_VERSION")
            ),
            stderr: String::new(),
        })
    }

    fn run_with_input(
        &self,
        _program: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> Result<CommandOutput> {
        use base64::Engine as _;
        let line = args.last().context("remote command line")?;
        let payload = line.split_whitespace().last().context("remote payload")?;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload)?;
        let request: Request = serde_json::from_slice(&decoded)?;
        let [operation, length, digest] = request.args.as_slice() else {
            anyhow::bail!("clipboard upload arguments");
        };
        ensure!(
            request.program == "bootty-daemon" && operation == "clipboard-upload",
            "clipboard upload command"
        );
        if self.reject {
            return Ok(CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "upload denied".to_owned(),
            });
        }
        let length = length.parse()?;
        let path = receive_clipboard_image(Cursor::new(input), length, digest, &self.directory)?;
        Ok(CommandOutput {
            success: true,
            stdout: serde_json::to_string(&path)?,
            stderr: String::new(),
        })
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn upload_only_returns_a_remote_path_after_success(#[case] reject: bool) {
    let local = assert_fs::TempDir::new().expect("test directory");
    let remote = assert_fs::TempDir::new().expect("test directory");
    let source = local.child("local image.png");
    source.write_binary(PNG).expect("PNG fixture");
    let runner = RemoteReceiver {
        directory: remote.path().to_path_buf(),
        reject,
    };
    let outcome = upload_clipboard_image(
        &RemoteHost::new(SshRemoteConfig::for_host("fixture-host")),
        source.path(),
        &runner,
    );
    if reject {
        assert!(outcome.is_err());
        assert_eq!(
            std::fs::read_dir(remote.path())
                .expect("remote directory")
                .count(),
            0
        );
    } else {
        let path = PathBuf::from(outcome.expect("uploaded path"));
        assert!(path.starts_with(remote.path()));
        assert_eq!(std::fs::read(path).expect("uploaded image"), PNG);
    }
}
