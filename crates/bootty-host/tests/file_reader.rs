//! Account-scoped file reads preserve bounds and revisions without loading whole histories.
use assert_fs::prelude::*;
use bootty_host::{
    file_reader::{FileDescriptor, FileReader},
    files::{FileRequest, FileResponse},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use std::io::{Cursor, Read as _, Seek as _, SeekFrom};

fn descriptor(path: &std::path::Path, root: &std::path::Path) -> anyhow::Result<FileDescriptor> {
    let FileResponse::Source(value) = (FileRequest::OpenReader {
        path: path.to_string_lossy().into_owned(),
        root: root.to_string_lossy().into_owned(),
    })
    .execute()?
    else {
        anyhow::bail!("expected source metadata");
    };
    Ok(value)
}

proptest! {
    #[test]
    fn arbitrary_file_ranges_match_an_independent_cursor(bytes in prop::collection::vec(any::<u8>(), 0..4096), operations in prop::collection::vec((0_u64..5000, 0_usize..200), 0..50)) {
        let directory = assert_fs::TempDir::new().unwrap();
        let file = directory.child("session.jsonl"); file.write_binary(&bytes).unwrap();
        let mut source = FileReader::open(&descriptor(file.path(), directory.path()).unwrap(), None).unwrap();
        let mut oracle = Cursor::new(bytes);
        for (offset, length) in operations {
            prop_assert_eq!(source.seek(SeekFrom::Start(offset)).unwrap(), oracle.seek(SeekFrom::Start(offset)).unwrap());
            let mut actual = vec![0; length]; let mut expected = vec![0; length];
            prop_assert_eq!(source.read(&mut actual).unwrap(), oracle.read(&mut expected).unwrap());
            prop_assert_eq!(actual, expected);
        }
    }
}

#[rstest]
fn root_fences_and_changed_cached_sources_are_enforced() {
    let root = assert_fs::TempDir::new().unwrap();
    let other = assert_fs::TempDir::new().unwrap();
    let outside = other.child("session.jsonl");
    outside.write_str("outside").unwrap();
    assert!(descriptor(outside.path(), root.path()).is_err());
    let file = root.child("session.jsonl");
    file.write_str("original").unwrap();
    let captured = descriptor(file.path(), root.path()).unwrap();
    let mut reader = FileReader::open(&captured, None).unwrap();
    reader.read_exact(&mut [0; 1]).unwrap();
    reader.verify_source().unwrap();
    file.write_str("replacement").unwrap();
    assert!(reader.verify_source().is_err());
    assert!(FileReader::open(&captured, None).is_err());
}

#[cfg(unix)]
#[rstest]
fn account_history_rejects_symlinks_and_non_regular_sources() {
    let root = assert_fs::TempDir::new().unwrap();
    let file = root.child("session.jsonl");
    file.write_str("data").unwrap();
    let link = root.child("link");
    std::os::unix::fs::symlink(file.path(), link.path()).unwrap();
    assert!(descriptor(link.path(), root.path()).is_err());
    assert!(descriptor(root.path(), root.path()).is_err());
    let mut captured = descriptor(file.path(), root.path()).unwrap();
    captured.path = link.path().to_string_lossy().into_owned();
    assert!(FileReader::open(&captured, None).is_err());
}

/// Appends a line to `file` the way an agent extends its transcript.
#[expect(
    clippy::unwrap_used,
    reason = "Fixture writes require the temporary file"
)]
fn append(file: &assert_fs::fixture::ChildPath, text: &str) {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(file.path())
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

/// A conversation log its agent keeps writing reads as it was when it opened, even when it grew
/// after its descriptor was taken and again while it was read. A log that shrank or whose end was
/// rewritten fails instead.
#[rstest]
#[case::grown(|file: &assert_fs::fixture::ChildPath| append(file, "{\"third\":3}\n"), true)]
#[case::truncated(|file: &assert_fs::fixture::ChildPath| file.write_str("{\"fi").unwrap(), false)]
#[case::rewritten(
    |file: &assert_fs::fixture::ChildPath| file.write_str("{\"first\":1}\n{\"other\":2}\n{}\n").unwrap(),
    false
)]
fn logs_read_their_opened_length_while_they_grow(
    #[case] change: fn(&assert_fs::fixture::ChildPath),
    #[case] readable: bool,
) {
    let root = assert_fs::TempDir::new().unwrap();
    let file = root.child("session.jsonl");
    file.write_str("{\"first\":1}\n").unwrap();
    let captured = descriptor(file.path(), root.path()).unwrap();
    append(&file, "{\"second\":2}\n");
    let mut reader = FileReader::open_log(&captured, None).unwrap();
    let opened = "{\"first\":1}\n{\"second\":2}\n";
    assert_eq!(reader.len(), u64::try_from(opened.len()).unwrap());
    change(&file);
    let mut text = String::new();
    let read = reader.read_to_string(&mut text).map(|_| text);
    assert_eq!(read.ok().as_deref(), readable.then_some(opened));
    assert_eq!(reader.verify_source().is_ok(), readable);
    // A snapshot read still refuses any change.
    assert!(FileReader::open(&captured, None).is_err());
}

/// The remote daemon serves a log at the length it has when it opens.
#[rstest]
fn remote_log_requests_serve_the_opened_length() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let root = assert_fs::TempDir::new().unwrap();
    let file = root.child("session.jsonl");
    file.write_str("{\"first\":1}\n").unwrap();
    let captured = descriptor(file.path(), root.path()).unwrap();
    append(&file, "{\"second\":2}\n");
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({"descriptor": captured, "changes": "appended"}))
            .unwrap(),
    );
    let mut output = Vec::new();
    bootty_host::file_reader::serve(
        &payload,
        Cursor::new(b"{\"offset\":12,\"length\":13}\n".to_vec()),
        &mut output,
    )
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    let mut lines = output.lines();
    let ready: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(ready["len"], 25);
    assert_eq!(lines.next(), Some("{\"status\":\"data\",\"length\":13}"));
    assert_eq!(lines.next(), Some("{\"second\":2}"));
}

/// A log its agent hasn't written yet is missing, not an error, so its reader shows no history.
#[rstest]
fn unwritten_logs_are_missing() {
    let root = assert_fs::TempDir::new().unwrap();
    let response = FileRequest::OpenReader {
        path: root.child("session.jsonl").to_string_lossy().into_owned(),
        root: root.path().to_string_lossy().into_owned(),
    }
    .execute()
    .unwrap();
    assert_eq!(response, FileResponse::Missing);
}
