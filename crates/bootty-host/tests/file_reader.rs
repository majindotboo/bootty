//! Account-scoped file reads preserve bounds and revisions without loading whole histories.
use assert_fs::prelude::*;
use bootty_host::{
    file_reader::{FileDescriptor, FileReader},
    files::{FileRequest, FileResponse},
};
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
