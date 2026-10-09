use assert_fs::{TempDir, prelude::*};
use bootty_host::files::{FileCompletionPage, FileRequest, FileResponse};
fn search(cwd: &str, query: &str) -> anyhow::Result<FileCompletionPage> {
    let FileResponse::Completions(page) = FileRequest::Complete {
        base: cwd.into(),
        query: query.into(),
    }
    .execute()?
    else {
        anyhow::bail!("Expected completion page")
    };
    Ok(page)
}
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::process::Command;

#[rstest]
#[case("", vec!["nested/", "notes.md"])]
#[case("nes", vec!["nested/", "notes.md"])]
#[case("nested/", vec!["image.png"])]
fn ordinary_directories_complete_without_git(#[case] query: &str, #[case] expected: Vec<&str>) {
    let root = TempDir::new().unwrap();
    root.child("notes.md").write_str("notes").unwrap();
    root.child("nested/image.png").write_str("image").unwrap();
    let cwd = root.path().to_str().unwrap();
    let page = search(cwd, query).unwrap();
    assert_eq!(page.files, expected);
    let absolute = search(cwd, &format!("{cwd}/nested/ima")).unwrap();
    assert_eq!(absolute.files, ["image.png"]);
    assert_eq!(
        Path::new(&absolute.root),
        root.child("nested").path().canonicalize().unwrap()
    );
}

use std::path::Path;

#[rstest]
fn project_search_includes_tracked_and_untracked_but_honors_gitignore() {
    let root = TempDir::new().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );
    root.child(".gitignore").write_str("ignored\n").unwrap();
    root.child("ignored").write_str("ignored").unwrap();
    root.child("src/main.rs").write_str("main").unwrap();
    root.child("notes.txt").write_str("notes").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "src/main.rs"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );
    let cwd = root.path().to_str().unwrap();
    let page = search(cwd, "").unwrap();
    assert_eq!(page.files.len(), 4);
    assert!(page.files.contains(&"notes.txt".to_owned()));
    assert!(!page.files.contains(&"ignored".to_owned()));
    assert_eq!(search(cwd, "smr").unwrap().files, ["src/main.rs"]);
    assert!(search(cwd, "\n").is_err());
}

#[rstest]
#[case("", "nes", true)]
#[case("n", "nes", true)]
#[case("notes", "n", false)]
#[case("nested/", "nested/i", false)]
#[case("", "~/", false)]
fn narrowing_a_complete_snapshot_matches_a_fresh_host_search(
    #[case] previous: &str,
    #[case] query: &str,
    #[case] reusable: bool,
) {
    let root = TempDir::new().unwrap();
    root.child("notes.md").write_str("notes").unwrap();
    root.child("nested/image.png").write_str("image").unwrap();
    let cwd = root.path().to_str().unwrap();
    let snapshot = search(cwd, previous).unwrap();
    let narrowed = snapshot.narrow(previous, query);
    assert_eq!(narrowed.is_some(), reusable);
    if let Some(narrowed) = narrowed {
        assert_eq!(narrowed, search(cwd, query).unwrap());
    }
}

use proptest::prelude::*;
proptest! {
    #[test]
    fn partial_results_are_never_used_as_a_complete_index(omitted in 1_usize..100_000, query in "[a-z]{0,30}") {
        let snapshot = FileCompletionPage {root:"/project".into(),files:vec!["notes.md".into()],omitted};
        prop_assert!(snapshot.narrow("", &query).is_none());
    }
}
