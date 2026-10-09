use std::ffi::OsString;

use assert_fs::{TempDir, prelude::*};
use bootty_git::project::{
    WorktreePickerEntry, add_favorite_project_path, discover_project_picker_entries,
    discover_worktree_picker_entries, home_dir_from, mark_occupied_worktrees,
    toggle_favorite_project_path,
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

#[fixture]
fn home() -> Result<TempDir, assert_fs::fixture::FixtureError> {
    TempDir::new()
}

#[rstest]
#[case(None)]
#[case(Some(OsString::new()))]
fn absent_or_empty_home_is_not_a_project_root(#[case] value: Option<OsString>) {
    assert_eq!(
        home_dir_from(|name| (name == "HOME").then(|| value.clone()).flatten()),
        None
    );
}

#[rstest]
fn discovery_includes_visible_project_roots_and_excludes_hidden_entries(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let home = home.expect("home fixture");
    home.child("src/project").create_dir_all().expect("project");
    home.child("src/.hidden").create_dir_all().expect("hidden");
    home.child("dotfiles").create_dir_all().expect("dotfiles");

    let actual = discover_project_picker_entries(Some(home.path()));

    assert!(
        actual
            .iter()
            .any(|entry| entry.path.ends_with("src/project"))
            && actual.iter().any(|entry| entry.path.ends_with("dotfiles"))
            && !actual.iter().any(|entry| entry.path.ends_with(".hidden")),
        "{actual:#?}"
    );
}

#[rstest]
fn favorite_toggle_and_discovery_share_the_same_file(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let home = home.expect("home fixture");
    let project = home.child("projects/bootty");
    project.create_dir_all().expect("project");
    let project_path = project.path().to_string_lossy().into_owned();

    assert!(toggle_favorite_project_path(Some(home.path()), &project_path).expect("favorite"));
    let discovered = discover_project_picker_entries(Some(home.path()));
    assert!(
        discovered
            .iter()
            .any(|entry| entry.path == project_path && entry.favorite),
        "{discovered:#?}"
    );
    assert!(!toggle_favorite_project_path(Some(home.path()), &project_path).expect("unfavorite"));
    home.child(".config/tmux/.session-favorites").assert("");
}

#[rstest]
fn adding_a_favorite_is_idempotent_and_does_not_rewrite_existing_bookmarks(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let home = home.expect("home fixture");
    let project = home.child("projects/bootty");
    project.create_dir_all().expect("project");
    let project_path = project.path().to_string_lossy().into_owned();

    assert!(add_favorite_project_path(Some(home.path()), &project_path).expect("add project"));
    let favorites = home.child(".config/tmux/.session-favorites");
    let content = std::fs::read(favorites.path()).expect("favorite bytes");

    assert!(
        !add_favorite_project_path(Some(home.path()), &project_path).expect("existing project")
    );
    assert_eq!(
        std::fs::read(favorites.path()).expect("favorite bytes"),
        content
    );
    assert!(
        discover_project_picker_entries(Some(home.path()))
            .iter()
            .any(|entry| entry.path == project_path && entry.favorite)
    );
}

#[rstest]
fn adding_a_missing_directory_fails_without_creating_a_bookmark_file(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let home = home.expect("home fixture");
    let project_path = home.path().join("missing").to_string_lossy().into_owned();

    assert_eq!(
        add_favorite_project_path(Some(home.path()), &project_path)
            .expect_err("missing project must fail")
            .kind(),
        std::io::ErrorKind::NotFound
    );
    assert!(!home.path().join(".config/tmux/.session-favorites").exists());
}

#[rstest]
fn adding_a_project_reports_favorite_storage_failures(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let home = home.expect("home fixture");
    let project = home.child("projects/bootty");
    project.create_dir_all().expect("project");
    home.child(".config/tmux").create_dir_all().expect("config");
    home.child(".config/tmux/.session-favorites")
        .create_dir_all()
        .expect("invalid favorites file");

    assert!(
        add_favorite_project_path(Some(home.path()), &project.path().to_string_lossy()).is_err()
    );
}

#[rstest]
fn adding_a_project_rejects_ambiguous_paths_without_changing_existing_favorites(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let home = home.expect("home fixture");
    let project = home.child("projects/bootty");
    project.create_dir_all().expect("project");
    let path = project.path().to_string_lossy().into_owned();
    let favorites = home.child(".config/tmux/.session-favorites");
    home.child(".config/tmux").create_dir_all().expect("config");
    favorites
        .write_str(&format!("{path}\n"))
        .expect("existing favorite");
    let original = std::fs::read(favorites.path()).expect("favorite bytes");

    for invalid_path in [
        "relative/path".to_owned(),
        format!("{path}\n{path}"),
        format!("{path}\r"),
        format!("{path}\t"),
    ] {
        assert_eq!(
            add_favorite_project_path(Some(home.path()), &invalid_path)
                .expect_err("favorite paths must be absolute and unambiguous")
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            std::fs::read(favorites.path()).expect("favorite bytes"),
            original
        );
    }
}

#[rstest]
fn adding_a_project_preserves_spaces_and_quotes_in_absolute_paths(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let home = home.expect("home fixture");
    let project = home.child("projects/project with \"quotes\"");
    project.create_dir_all().expect("project");
    let path = project.path().to_string_lossy().into_owned();

    assert!(add_favorite_project_path(Some(home.path()), &path).expect("add project"));
    home.child(".config/tmux/.session-favorites")
        .assert(format!("{path}\n").as_str());
}

#[rstest]
fn canonical_path_aliases_mark_the_same_worktree_occupied(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let home = home.expect("home fixture");
    let project = home.child("project");
    project.create_dir_all().expect("project");
    let path = project.path().to_string_lossy().into_owned();
    let alias = project
        .path()
        .join("..")
        .join("project")
        .to_string_lossy()
        .into_owned();
    let mut entries = vec![WorktreePickerEntry {
        label: "project (main)".to_owned(),
        path: Some(path),
        is_new: false,
        occupied: false,
    }];

    mark_occupied_worktrees(&mut entries, &[alias]);

    assert!(entries[0].occupied, "{entries:#?}");
}

#[rstest]
fn non_git_directory_offers_only_its_main_entry(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let home = home.expect("home fixture");
    let path = home.path().to_string_lossy().into_owned();
    let directory_name = home
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("directory name");

    assert_eq!(
        discover_worktree_picker_entries(&path),
        vec![WorktreePickerEntry {
            label: format!("{directory_name} (main)"),
            path: Some(path),
            is_new: false,
            occupied: false,
        }]
    );
}

#[cfg(unix)]
#[rstest]
fn favorite_replacement_preserves_file_permissions(
    home: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    use std::{fs, os::unix::fs::PermissionsExt};
    let home = home.expect("home fixture");

    let favorites = home.child(".config/tmux/.session-favorites");
    home.child(".config/tmux").create_dir_all().unwrap();
    favorites.write_str("~/projects/old\n").unwrap();
    fs::set_permissions(favorites.path(), fs::Permissions::from_mode(0o444)).unwrap();
    let project = home.child("projects/new");
    project.create_dir_all().unwrap();

    assert!(
        toggle_favorite_project_path(Some(home.path()), &project.path().to_string_lossy()).unwrap()
    );
    favorites.assert(format!("~/projects/old\n{}\n", project.path().display()).as_str());
    assert_eq!(
        fs::metadata(favorites.path()).unwrap().permissions().mode() & 0o777,
        0o444
    );
}
