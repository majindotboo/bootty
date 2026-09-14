use assert_fs::prelude::*;
use bootty_host::text_file::{load_text_file, save_text_file, save_text_file_if_unchanged};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn missing_file_opens_as_an_empty_reusable_document() {
    let directory = assert_fs::TempDir::new().expect("temporary directory");
    let path = directory.path().join("future.txt");

    let loaded = load_text_file(&path).expect("load missing document");

    assert_eq!(loaded.path, path);
    assert_eq!(loaded.contents, "");
    assert_eq!(
        loaded.path.file_name().and_then(|name| name.to_str()),
        Some("future.txt")
    );
}

#[rstest]
fn save_atomically_creates_and_replaces_utf8_text() {
    let directory = assert_fs::TempDir::new().expect("temporary directory");
    let file = directory.child("nested/config.toml");

    let created = save_text_file(file.path(), "version = 1\n").expect("create text file");
    assert_eq!(created.durability_warning, None);
    file.assert("version = 1\n");

    let replaced = save_text_file(file.path(), "version = 2\n# 文\n").expect("replace text file");
    assert_eq!(replaced.durability_warning, None);
    file.assert("version = 2\n# 文\n");
    assert_eq!(
        load_text_file(file.path())
            .expect("reload text file")
            .contents,
        "version = 2\n# 文\n"
    );
}

#[rstest]
fn non_utf8_file_reports_an_open_error_instead_of_losing_bytes() {
    let directory = assert_fs::TempDir::new().expect("temporary directory");
    let file = directory.child("binary.toml");
    file.write_binary(&[0xff, 0xfe])
        .expect("write binary fixture");

    let error = load_text_file(file.path()).expect_err("reject non-UTF-8 text");

    assert!(error.to_string().contains("read text file"));
    file.assert([0xff, 0xfe].as_slice());
}

#[rstest]
fn save_rejects_external_changes_without_clobbering_the_file() {
    let directory = assert_fs::TempDir::new().expect("temporary directory");
    let file = directory.child("config.toml");
    file.write_str("version = 1\n")
        .expect("write original fixture");

    file.write_str("version = 2\n")
        .expect("simulate external edit");
    let error = save_text_file_if_unchanged(file.path(), "version = 1\n", "version = 3\n")
        .expect_err("stale editor must not overwrite external edit");

    assert!(error.to_string().contains("changed on disk"));
    file.assert("version = 2\n");
}

#[rstest]
fn save_accepts_an_unchanged_target_and_advances_the_expected_revision() {
    let directory = assert_fs::TempDir::new().expect("temporary directory");
    let file = directory.child("config.toml");
    file.write_str("version = 1\n")
        .expect("write original fixture");

    save_text_file_if_unchanged(file.path(), "version = 1\n", "version = 2\n")
        .expect("save the first editor revision");
    save_text_file_if_unchanged(file.path(), "version = 2\n", "version = 3\n")
        .expect("save the next editor revision");

    file.assert("version = 3\n");
}
