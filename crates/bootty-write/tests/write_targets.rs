use std::{fs, io, path::Path};

use assert_fs::{TempDir, prelude::*};
use bootty_write::{CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::{fixture, rstest};

#[derive(Arbitrary, Debug)]
struct PathCase {
    #[proptest(regex = "[a-z][a-z0-9-]{0,31}\\.bin")]
    name: String,
}

#[fixture]
fn directory() -> Result<TempDir, assert_fs::fixture::FixtureError> {
    TempDir::new()
}

#[cfg(unix)]
#[test]
fn relative_symlink_alias_resolves_to_one_target_and_keeps_the_link() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().expect("temporary directory");
    let target = directory.child("target.txt");
    let alias = directory.child("alias.txt");
    target.write_binary(b"old").expect("original target");
    symlink(Path::new("target.txt"), alias.path()).expect("relative alias");

    let resolved = WriteTarget::resolve(alias.path()).expect("resolve alias");
    assert_eq!(
        resolved.path(),
        fs::canonicalize(target.path()).expect("canonical target")
    );
    resolved
        .lock()
        .expect("lock target")
        .replace(b"new", NewFileMode::Private)
        .expect("replace alias target");

    assert!(
        fs::symlink_metadata(alias.path())
            .expect("alias metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(target.path()).expect("replacement contents"),
        b"new"
    );
}

#[cfg(unix)]
#[test]
fn symlink_cycle_is_a_typed_resolution_error() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().expect("temporary directory");
    directory
        .child("nested")
        .create_dir_all()
        .expect("nested directory");
    let first = directory.child("first");
    let second = directory.child("second");
    symlink(Path::new("nested/../second"), first.path()).expect("first link");
    symlink(Path::new("nested/../first"), second.path()).expect("second link");

    assert!(matches!(
        WriteTarget::resolve(first.path()),
        Err(ResolveTargetError::SymlinkCycle)
    ));
}

#[cfg(unix)]
#[test]
fn parent_components_after_symlinks_follow_os_target_resolution() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().expect("temporary directory");
    let real = directory.child("real");
    real.create_dir_all().expect("real directory");
    real.child("nested")
        .create_dir_all()
        .expect("nested directory");
    let target = real.child("target.txt");
    target.write_binary(b"old").expect("original target");
    let alias = directory.child("alias");
    symlink(Path::new("real/nested"), alias.path()).expect("directory alias");

    let requested = alias.path().join("../target.txt");
    let resolved = WriteTarget::resolve(&requested).expect("resolve target through symlink");
    assert_eq!(
        resolved.path(),
        fs::canonicalize(target.path()).expect("canonical target")
    );
    resolved
        .lock()
        .expect("lock target")
        .replace(b"new", NewFileMode::Private)
        .expect("replace target through symlink");

    assert_eq!(
        fs::read(target.path()).expect("replacement contents"),
        b"new"
    );
    assert!(!directory.child("target.txt").exists());
}

#[cfg(unix)]
#[test]
fn dangling_symlink_keeps_its_link_and_creates_the_resolved_target() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().expect("temporary directory");
    let real = directory.child("real");
    real.create_dir_all().expect("real directory");
    let alias = directory.child("alias.txt");
    symlink(Path::new("real/missing.txt"), alias.path()).expect("dangling alias");

    let resolved = WriteTarget::resolve(alias.path()).expect("resolve dangling alias");
    assert_eq!(
        resolved.path(),
        fs::canonicalize(real.path())
            .expect("canonical directory")
            .join("missing.txt")
    );
    resolved
        .lock()
        .expect("lock target")
        .replace(b"new", NewFileMode::Private)
        .expect("create target through dangling alias");

    assert!(
        fs::symlink_metadata(alias.path())
            .expect("alias metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(real.child("missing.txt").path()).expect("created contents"),
        b"new"
    );
}

#[test]
fn writes_remove_legacy_locks_and_leave_no_new_lock_beside_the_target() {
    let directory = TempDir::new().expect("temporary directory");
    let target = directory.child("hooks.json");
    target.write_binary(b"{}").expect("original target");
    let legacy = directory.child(".hooks.json.bootty-write.lock");
    legacy.touch().expect("legacy lock");

    WriteTarget::resolve(target.path())
        .expect("resolve target")
        .lock()
        .expect("lock target")
        .replace(b"{\"a\":1}", NewFileMode::UmaskWritable)
        .expect("replace target");

    let left_behind = fs::read_dir(directory.path())
        .expect("directory entries")
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<_>>>()
        .expect("entry names");
    assert_eq!(left_behind, ["hooks.json"]);
}

proptest! {
    /// Property: lexical current-directory components never change the resolved write target.
    #[test]
    fn lexical_current_directory_components_preserve_the_target(case in any::<PathCase>()) {
        let directory = TempDir::new().expect("temporary directory");
        let canonical = fs::canonicalize(directory.path()).expect("canonical temporary directory");
        let direct = WriteTarget::resolve(&directory.path().join(&case.name))
            .expect("direct target");
        let dotted = WriteTarget::resolve(&directory.path().join(".").join(&case.name))
            .expect("target containing current-directory component");

        prop_assert_eq!(direct.path(), canonical.join(&case.name));
        prop_assert_eq!(dotted.path(), direct.path());
    }
}

#[rstest]
fn a_filesystem_root_is_rejected_before_replacement() {
    let current = std::env::current_dir().expect("current directory");
    let root = current.ancestors().last().expect("filesystem root");
    let target = WriteTarget::resolve(root).expect("resolve root");
    let locked = target.lock().expect("lock root target");
    let error = locked
        .replace(b"must not be written", NewFileMode::Private)
        .expect_err("a filesystem root cannot be replaced");
    drop(locked);
    assert_eq!(error.phase(), "prepare");
    assert_eq!(error.into_io().kind(), io::ErrorKind::InvalidInput);
}

#[rstest]
fn commits_exact_bytes(directory: Result<TempDir, assert_fs::fixture::FixtureError>) {
    let directory = directory.expect("temporary directory");
    let target = directory.child("state.bin");
    let locked = WriteTarget::resolve(target.path())
        .expect("resolve target")
        .lock()
        .expect("lock target");
    let first = b"\0Bootty\xff";
    let second = (u8::MIN..=u8::MAX).collect::<Vec<_>>();

    let outcome = locked
        .replace(first, NewFileMode::Private)
        .expect("first commit");
    assert!(matches!(outcome, CommitOutcome::Confirmed));
    assert_eq!(fs::read(target.path()).expect("read first commit"), first);

    locked
        .replace(&second, NewFileMode::Private)
        .expect("replacement commit");
    drop(locked);
    assert_eq!(
        fs::read(target.path()).expect("read replacement commit"),
        second
    );
}
