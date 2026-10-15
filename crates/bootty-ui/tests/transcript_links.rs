use bootty_ui::transcript_links::link_code_paths;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case::path_and_line(
    "Edit `src/main.rs:12` now",
    "Edit [`src/main.rs:12`](<src/main.rs:12>) now"
)]
#[case::known_file("See `Cargo.toml`.", "See [`Cargo.toml`](<Cargo.toml>).")]
#[case::directory("In `crates/bootty-ui`", "In [`crates/bootty-ui`](<crates/bootty-ui>)")]
#[case::field_access("Read `snapshot.status`", "Read `snapshot.status`")]
#[case::host_port("Open `localhost:3000`", "Open `localhost:3000`")]
#[case::address("Bind `127.0.0.1:8080`", "Bind `127.0.0.1:8080`")]
#[case::path_rule("Use `std::fmt`", "Use `std::fmt`")]
#[case::already_linked("[`src/a.rs`](src/a.rs)", "[`src/a.rs`](src/a.rs)")]
#[case::double_ticks("`` `src/a.rs` ``", "`` `src/a.rs` ``")]
#[case::fenced(
    "```\n`src/a.rs`\n```\n`src/b.rs`",
    "```\n`src/a.rs`\n```\n[`src/b.rs`](<src/b.rs>)"
)]
#[case::unclosed("Streaming `src/ma", "Streaming `src/ma")]
fn inline_code_links_only_file_paths(#[case] reply: &str, #[case] linked: &str) {
    assert_eq!(link_code_paths(reply).0, linked);
}

#[rstest]
fn selections_inside_links_map_to_the_reply_text() {
    let reply = "Edit `src/main.rs:12` now";
    let (linked, marks) = link_code_paths(reply);
    let start = linked.find("main.rs").unwrap();
    assert_eq!(
        reply.get(marks.source_range(start..start.saturating_add("main.rs".len()))),
        Some("main.rs")
    );
    let now = linked.find("now").unwrap();
    assert_eq!(
        reply.get(marks.source_range(now..linked.len())),
        Some("now")
    );
}

proptest! {
    /// Link marks never change a reply's own text: the whole linked text maps to the whole reply,
    /// and every mapped edge lands on one of its character boundaries.
    #[test]
    fn link_marks_map_back_to_the_reply(reply in "[a-z./:`\n 1-9]{0,80}") {
        let (linked, marks) = link_code_paths(&reply);
        prop_assert_eq!(reply.get(marks.source_range(0..linked.len())), Some(reply.as_str()));
        for offset in 0..=linked.len() {
            let mapped = marks.source_range(offset..linked.len());
            prop_assert!(reply.is_char_boundary(mapped.start));
            prop_assert!(mapped.start <= reply.len());
        }
    }
}
