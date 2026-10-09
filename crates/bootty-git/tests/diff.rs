use bootty_git::diff::{DiffAnchor, DiffSide, FileDiff};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use std::fmt::Write as _;

proptest! {
    #[test]
    fn changed_lines_keep_independent_source_numbers(old in 1_u32..10000, new in 1_u32..10000, removed in 1_usize..20, added in 1_usize..20) {
        let mut patch = format!("@@ -{old},{removed} +{new},{added} @@\n");
        for n in 0..removed { _ = writeln!(patch,"-old {n}"); }
        for n in 0..added { _ = writeln!(patch,"+new {n}"); }
        let diff = FileDiff::parse("file.rs".into(), None, Some(&patch)).unwrap();
        for (side, start, count, prefix) in [(DiffSide::Left, old, removed, "old"), (DiffSide::Right, new, added, "new")] {
            let end = start.saturating_add(u32::try_from(count).unwrap()).saturating_sub(1);
            let anchor = DiffAnchor { path:"file.rs".into(), side, start_line:start, line:end };
            let expected = (0..count).map(|n| format!("{prefix} {n}")).collect::<Vec<_>>().join("\n");
            prop_assert_eq!(diff.quote(&anchor).unwrap(), expected);
        }
    }
}

#[rstest]
#[case("@@ -0,0 +1 @@\n+added\n", DiffSide::Right, 1, "added")]
#[case("@@ -1 +0,0 @@\n-deleted\n", DiffSide::Left, 1, "deleted")]
#[case(
    "@@ -1 +1 @@\n é🥟\n\\ No newline at end of file\n",
    DiffSide::Right,
    1,
    "é🥟"
)]
fn additions_deletions_and_unicode_are_source_anchored(
    #[case] patch: &str,
    #[case] side: DiffSide,
    #[case] line: u32,
    #[case] quote: &str,
) {
    let diff = FileDiff::parse("new.rs".into(), Some("old.rs".into()), Some(patch)).unwrap();
    assert_eq!(
        diff.quote(&DiffAnchor {
            path: "new.rs".into(),
            side,
            start_line: line,
            line
        })
        .unwrap(),
        quote
    );
    assert_eq!(diff.previous_path.as_deref(), Some("old.rs"));
}

#[rstest]
#[case("@@ -1,2 +1 @@\n-one\n+new\n")]
#[case("@@ -0 +1 @@\n-old\n+new\n")]
#[case("@@ -1 +1 @@\n?invalid\n")]
fn malformed_hunks_cannot_be_reviewed(#[case] patch: &str) {
    assert!(FileDiff::parse("file.rs".into(), None, Some(patch)).is_err());
}

#[rstest]
fn missing_deleted_and_cross_hunk_selections_are_rejected() {
    let diff = FileDiff::parse(
        "file.rs".into(),
        None,
        Some("@@ -1 +1 @@\n-old\n+new\n@@ -3 +3 @@\n same\n"),
    )
    .unwrap();
    for (path, start_line, line) in [
        ("other.rs", 1, 1),
        ("file.rs", 1, 3),
        ("file.rs", 2, 2),
        ("file.rs", 0, 1),
    ] {
        assert!(
            diff.quote(&DiffAnchor {
                path: path.into(),
                side: DiffSide::Right,
                start_line,
                line
            })
            .is_err()
        );
    }
    assert!(
        FileDiff::parse("binary.bin".into(), None, None)
            .unwrap()
            .patch_unavailable
    );
}
