use std::{fs, path::PathBuf};

use anyhow::{Context as _, ensure};
use assert_fs::prelude::*;
use bootty_host::files::{
    FileRequest, FileResponse, FileSnapshot, MAX_DOCUMENT_BYTES, encode_document,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn read(path: &str) -> anyhow::Result<FileSnapshot> {
    let FileResponse::Document(snapshot) = (FileRequest::Read {
        path: path.to_owned(),
    })
    .execute()?
    else {
        anyhow::bail!("expected document response");
    };
    Ok(snapshot)
}

proptest! {
    #[test]
    fn revisions_preserve_utf8_bytes_and_reject_external_edits(contents in "[^\\x00]{0,400}", replacement in "[^\\x00]{0,400}") {
        let directory = assert_fs::TempDir::new().unwrap();
        let path = directory.child("file.txt");
        path.write_str(&contents).unwrap();
        let name = path.path().to_str().unwrap();
        let snapshot = read(name).expect("document snapshot");
        prop_assert_eq!(snapshot.contents().unwrap(), contents);
        let save = FileRequest::Save { path: name.to_owned(), expected_digest: snapshot.digest, content_base64: encode_document(&replacement).unwrap() };
        save.execute().unwrap();
        prop_assert_eq!(fs::read(path.path()).unwrap(), replacement.as_bytes());
        path.write_str("external content\0binary").unwrap();
        prop_assert!(save.execute().is_err());
        prop_assert_eq!(fs::read(path.path()).unwrap(), b"external content\0binary");
    }
}

#[rstest]
#[case(vec![0, 1, 2])]
#[case(vec![0xff, 0xfe])]
#[case(vec![b'x'; MAX_DOCUMENT_BYTES + 1])]
fn unsupported_documents_are_rejected_without_changes(#[case] bytes: Vec<u8>) {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("unsupported");
    file.write_binary(&bytes).unwrap();
    assert!(
        FileRequest::Read {
            path: file.path().to_string_lossy().into_owned()
        }
        .execute()
        .is_err()
    );
    assert_eq!(fs::read(file.path()).unwrap(), bytes);
}

#[rstest]
fn directory_pages_are_complete_and_directories_precede_files() {
    let directory = assert_fs::TempDir::new().unwrap();
    directory.child("z directory").create_dir_all().unwrap();
    for index in 0..220 {
        directory
            .child(format!("file-{index:03}.txt"))
            .write_str("")
            .unwrap();
    }
    let mut offset = 0;
    let mut entries = Vec::new();
    loop {
        let FileResponse::Directory(page) = (FileRequest::List {
            path: directory.path().to_string_lossy().into_owned(),
            offset,
        })
        .execute()
        .unwrap() else {
            panic!("directory response");
        };
        entries.extend(page.entries);
        let Some(next) = page.next_offset else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(entries.len(), 221);
    assert_eq!(entries[0].name, "z directory");
    assert!(entries[0].is_directory);
    assert_eq!(entries[220].name, "file-219.txt");
}

#[rstest]
fn text_revisions_keep_crlf_and_bom_bytes() {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("file.txt");
    file.write_str("\u{feff}first\r\nsecond\r\n").unwrap();
    let name = file.path().to_str().unwrap();
    let snapshot = read(name).expect("document snapshot");
    let replacement = snapshot.contents().unwrap().replace("second", "changed");
    FileRequest::Save {
        path: name.to_owned(),
        expected_digest: snapshot.digest.to_uppercase(),
        content_base64: encode_document(&replacement).unwrap(),
    }
    .execute()
    .unwrap();
    file.assert("\u{feff}first\r\nchanged\r\n");
}

#[cfg(unix)]
#[rstest]
#[case("target")]
#[case("")]
fn directory_links_preserve_the_browsed_branch(#[case] target: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let target = directory.child(target);
    target.create_dir_all().unwrap();
    target.child("document.txt").write_str("contents").unwrap();
    let link = directory.child("alias");
    std::os::unix::fs::symlink(target.path(), link.path()).unwrap();

    let FileResponse::Directory(page) = (FileRequest::List {
        path: link.path().to_str().unwrap().to_owned(),
        offset: 0,
    })
    .execute()
    .unwrap() else {
        panic!("directory response");
    };
    assert_eq!(PathBuf::from(&page.path), link.path());
    assert_eq!(
        page.parent.map(PathBuf::from).as_deref(),
        Some(directory.path())
    );
    let document = page
        .entries
        .iter()
        .find(|entry| entry.name == "document.txt")
        .unwrap();
    assert_eq!(
        PathBuf::from(&document.path),
        link.path().join("document.txt")
    );
    assert_eq!(
        read(&document.path)
            .expect("document snapshot")
            .contents()
            .unwrap(),
        "contents"
    );
    assert_ne!(
        document.path,
        target.path().join("document.txt").to_str().unwrap()
    );
}

#[cfg(unix)]
#[rstest]
fn editing_a_symlink_preserves_the_link_and_target_permissions() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let directory = assert_fs::TempDir::new().unwrap();
    let target = directory.child("target");
    target.write_str("before").unwrap();
    fs::set_permissions(target.path(), fs::Permissions::from_mode(0o640)).unwrap();
    let link = directory.child("link");
    symlink(target.path(), link.path()).unwrap();
    let path = link.path().to_str().unwrap();
    let snapshot = read(path).expect("document snapshot");
    FileRequest::Save {
        path: path.to_owned(),
        expected_digest: snapshot.digest,
        content_base64: encode_document("after").unwrap(),
    }
    .execute()
    .unwrap();
    assert!(fs::symlink_metadata(link.path()).unwrap().is_symlink());
    assert_eq!(
        fs::metadata(target.path()).unwrap().permissions().mode() & 0o777,
        0o640
    );
    target.assert("after");
}

#[rstest]
fn largest_document_fits_control_frames_even_with_json_control_characters() {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("control.txt");
    let contents = "\u{0001}".repeat(MAX_DOCUMENT_BYTES);
    file.write_str(&contents).unwrap();
    let snapshot = read(file.path().to_str().unwrap()).expect("document snapshot");
    let response = serde_json::to_vec(&FileResponse::Document(snapshot.clone())).unwrap();
    assert!(response.len() < bootty_host::files::FILE_WIRE_LIMIT);
    let request = FileRequest::Save {
        path: snapshot.path,
        expected_digest: snapshot.digest,
        content_base64: encode_document(&contents).unwrap(),
    };
    assert!(serde_json::to_vec(&request).unwrap().len() < bootty_host::files::FILE_WIRE_LIMIT);
    request.execute().unwrap();
    file.assert(contents);
}

struct RemoteFiles {
    remote: std::path::PathBuf,
}
impl bootty_host::CommandRunner for &RemoteFiles {
    fn run(&self, _: &str, _: &[String]) -> anyhow::Result<bootty_host::CommandOutput> {
        Ok(bootty_host::CommandOutput {
            success: true,
            stdout: format!(
                "{}:{}",
                bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
                env!("CARGO_PKG_VERSION")
            ),
            stderr: String::new(),
        })
    }
    fn run_with_input(
        &self,
        _: &str,
        args: &[String],
        input: Vec<u8>,
    ) -> anyhow::Result<bootty_host::CommandOutput> {
        use base64::Engine as _;
        let payload = args
            .last()
            .and_then(|line| line.split_whitespace().last())
            .context("remote file payload")?;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload)?;
        let command: serde_json::Value = serde_json::from_slice(&decoded)?;
        ensure!(
            command.get("args") == Some(&serde_json::json!(["file"])),
            "remote file command"
        );
        let mut request: FileRequest = serde_json::from_slice(&input)?;
        let original_path = match &mut request {
            FileRequest::Resolve { path, .. }
            | FileRequest::List { path, .. }
            | FileRequest::Read { path }
            | FileRequest::Save { path, .. } => {
                let original = path.clone();
                self.remote
                    .to_str()
                    .context("remote UTF-8 path")?
                    .clone_into(path);
                original
            }
        };
        let mut response = request.execute()?;
        if let FileResponse::Document(snapshot) = &mut response {
            snapshot.path = original_path;
        }
        Ok(bootty_host::CommandOutput {
            success: true,
            stdout: serde_json::to_string(&response)?,
            stderr: String::new(),
        })
    }
}

#[rstest]
fn remote_reads_and_saves_never_touch_a_local_namesake() {
    let directory = assert_fs::TempDir::new().unwrap();
    let local = directory.child("local");
    local.write_str("local contents").unwrap();
    let remote = directory.child("remote");
    remote.write_str("remote contents").unwrap();
    let transport = RemoteFiles {
        remote: remote.path().to_owned(),
    };
    let config = bootty_config::config::SshRemoteConfig::for_host("fixture-host");
    let host = bootty_host::remote::RemoteHost::new(config.clone());
    let FileResponse::Document(snapshot) = FileRequest::Read {
        path: local.path().to_str().unwrap().to_owned(),
    }
    .execute_remote(&host, &transport)
    .unwrap() else {
        panic!("document");
    };
    assert_eq!(snapshot.contents().unwrap(), "remote contents");
    FileRequest::Save {
        path: snapshot.path,
        expected_digest: snapshot.digest,
        content_base64: encode_document("remote replacement").unwrap(),
    }
    .execute_remote(&host, &transport)
    .unwrap();
    local.assert("local contents");
    remote.assert("remote replacement");
    let identity = bootty_host::files::host_identity(Some(&config.clone().into()))
        .expect("file host namespace");
    assert_ne!(
        identity,
        bootty_host::files::host_identity(None).expect("file host namespace")
    );
    let mut changed = config;
    changed.args.push("-Fcustom".to_owned());
    assert_ne!(
        identity,
        bootty_host::files::host_identity(Some(&changed.into())).expect("file host namespace")
    );
}

#[cfg(unix)]
#[rstest]
fn a_document_replaced_by_a_fifo_is_rejected_before_opening_it() {
    use std::os::unix::fs::FileTypeExt as _;
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("document");
    file.write_str("before").unwrap();
    let snapshot = read(file.path().to_str().unwrap()).expect("document snapshot");
    fs::remove_file(file.path()).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(file.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        FileRequest::Save {
            path: snapshot.path.clone(),
            expected_digest: snapshot.digest,
            content_base64: encode_document("after").unwrap()
        }
        .execute()
        .is_err()
    );
    assert!(
        FileRequest::Read {
            path: snapshot.path
        }
        .execute()
        .is_err()
    );
    assert!(fs::metadata(file.path()).unwrap().file_type().is_fifo());
}

#[rstest]
fn file_links_resolve_on_the_host_with_an_explicit_directory() {
    let directory = assert_fs::TempDir::new().unwrap();
    directory
        .child("some file.rs")
        .write_str("fn main() {}\n")
        .unwrap();
    let expected = fs::canonicalize(directory.child("some file.rs").path()).unwrap();
    for path in [
        "some file.rs".to_owned(),
        url::Url::from_file_path(&expected).unwrap().to_string(),
    ] {
        let FileResponse::Location { path, is_directory } = (FileRequest::Resolve {
            path,
            base: Some(directory.path().to_string_lossy().into_owned()),
        })
        .execute()
        .unwrap() else {
            panic!("location")
        };
        assert_eq!(PathBuf::from(path), expected);
        assert!(!is_directory);
    }
    for (path, base) in [
        ("some file.rs", None),
        ("file://other-host/tmp/example.rs", None),
        ("", Some("/tmp")),
        ("bad\npath", Some("/tmp")),
        ("some file.rs", Some("relative")),
    ] {
        assert!(
            FileRequest::Resolve {
                path: path.to_owned(),
                base: base.map(str::to_owned)
            }
            .execute()
            .is_err()
        );
    }
}
