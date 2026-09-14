#![cfg(test)]

use assert_fs::TempDir;
use bootty_ui::recovery::{ArchiveStore, MAX_ARCHIVES, OutputArchive, fingerprint};
use pretty_assertions::assert_eq;
use rstest::rstest;
fn archive(id: usize, saved: u64) -> OutputArchive {
    OutputArchive {
        id: format!("{id:064x}"),
        run: format!("{:064x}", 1),
        scope: "space".into(),
        session: "session".into(),
        pane: "pane".into(),
        title: format!("title {id}"),
        host: "Local".into(),
        host_fingerprint: "local".into(),
        backend: "native".into(),
        saved_at_ms: saved,
        cols: 80,
        rows: 24,
        omitted_lines: 3,
        text: format!("output {id}"),
        agent: None,
    }
}
#[rstest]
fn archives_replace_atomically_prune_and_export_without_overwrite() -> anyhow::Result<()> {
    let root = TempDir::new()?;
    let store = ArchiveStore::new(root.path().join("archives"));
    for id in 0..MAX_ARCHIVES + 3 {
        store.save(&archive(id, u64::try_from(id)?))?;
    }
    let list = store.list()?;
    anyhow::ensure!(
        list.entries.len() == MAX_ARCHIVES,
        "archive count: {}",
        list.entries.len()
    );
    anyhow::ensure!(
        list.entries[0].saved_at_ms == u64::try_from(MAX_ARCHIVES + 2)?,
        "newest archive timestamp: {}",
        list.entries[0].saved_at_ms
    );
    anyhow::ensure!(store.get(&format!("{:064x}", 0)).is_err());
    let newest = list.entries[0].id.clone();
    let path = root.path().join("output.txt");
    store.export(&newest, &path)?;
    let text = std::fs::read_to_string(&path)?;
    anyhow::ensure!(text.starts_with("Previous session —"));
    anyhow::ensure!(text.contains("Omitted rows: 3"));
    anyhow::ensure!(store.export(&newest, &path).is_err());
    let mut replacement = store.get(&newest)?;
    replacement.text = "new complete output".into();
    store.save(&replacement)?;
    anyhow::ensure!(
        store.get(&newest)?.text == "new complete output",
        "replacement archive text was not saved"
    );
    Ok(())
}
#[rstest]
fn archives_reject_traversal_oversize_and_invalid_metadata() -> anyhow::Result<()> {
    let root = TempDir::new()?;
    let store = ArchiveStore::new(root.path().join("archives"));
    anyhow::ensure!(store.get("../config.toml").is_err());
    let mut value = archive(4, 4);
    value.text = "x".repeat(256 * 1024 + 1);
    anyhow::ensure!(store.save(&value).is_err());
    value.text = "ok".into();
    value.host = "bad\nmetadata".into();
    anyhow::ensure!(store.save(&value).is_err());
    Ok(())
}
#[rstest]
fn fingerprints_are_stable_and_do_not_store_source() {
    let a = fingerprint(b"secret transport args");
    assert_eq!(a.len(), 64);
    assert_eq!(a, fingerprint(b"secret transport args"));
    assert!(!a.contains("secret"));
}

#[cfg(unix)]
#[rstest]
fn archive_save_does_not_follow_a_symlink_to_an_unrelated_file() -> anyhow::Result<()> {
    let root = TempDir::new()?;
    let directory = root.path().join("archives");
    std::fs::create_dir(&directory)?;
    let unrelated = root.path().join("unrelated.txt");
    std::fs::write(&unrelated, "keep this content")?;
    let value = archive(4, 4);
    std::os::unix::fs::symlink(&unrelated, directory.join(format!("{}.json", value.id)))?;

    let store = ArchiveStore::new(directory);
    anyhow::ensure!(store.save(&value).is_err());
    anyhow::ensure!(
        std::fs::read_to_string(unrelated)? == "keep this content",
        "symlink destination was modified"
    );
    Ok(())
}
