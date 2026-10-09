use assert_fs::{TempDir, prelude::*};
use bootty_write::{CommitError, CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

proptest! {
    #[test]
    fn create_preserves_existing_file_contents(original in prop::collection::vec(any::<u8>(), 0..4096)) {
        let directory = TempDir::new()?;
        let path = directory.child("snapshot.png");
        path.write_binary(&original)?;
        let result = WriteTarget::resolve(path.path()).map_err(ResolveTargetError::into_io)?.lock()?.create(b"replacement", NewFileMode::Private);
        let error = result.err().ok_or_else(|| std::io::Error::other("create must refuse an existing file"))?;
        prop_assert_eq!(error.into_io().kind(), std::io::ErrorKind::AlreadyExists);
        prop_assert_eq!(std::fs::read(path.path())?, original);
    }
}

#[rstest]
fn concurrent_creators_publish_exactly_one_complete_file() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = TempDir::new()?;
    let path = directory.child("snapshot.png").path().to_owned();
    let mut creators = Vec::new();
    for bytes in [
        b"first complete image".to_vec(),
        b"second complete image".to_vec(),
    ] {
        let path = path.clone();
        creators.push(std::thread::spawn(move || -> Result<_, std::io::Error> {
            let result = WriteTarget::resolve(&path)
                .map_err(ResolveTargetError::into_io)?
                .lock()?
                .create(&bytes, NewFileMode::Private)
                .map_err(CommitError::into_io);
            Ok((bytes, result))
        }));
    }
    let mut winner = None;
    let mut rejected = 0_u8;
    for creator in creators {
        let (bytes, result) = creator.join().map_err(|_| "creator failed")??;
        match result {
            Ok(CommitOutcome::Confirmed | CommitOutcome::CommittedWithDurabilityWarning(_)) => {
                if winner.replace(bytes).is_some() {
                    return Err("multiple creators published".into());
                }
            }
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
                rejected = rejected.saturating_add(1);
            }
        }
    }
    assert_eq!(rejected, 1);
    assert_eq!(Some(std::fs::read(&path)?), winner);
    Ok(())
}

#[rstest]
fn create_refuses_an_external_writer_after_target_resolution()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TempDir::new()?;
    let path = directory.child("snapshot.png");
    let target = WriteTarget::resolve(path.path())
        .map_err(ResolveTargetError::into_io)?
        .lock()?;
    path.write_binary(b"external writer")?;
    let error = target
        .create(b"capture", NewFileMode::Private)
        .err()
        .ok_or("must refuse overwrite")?;
    drop(target);
    assert_eq!(error.into_io().kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(path.path())?, b"external writer");
    Ok(())
}
