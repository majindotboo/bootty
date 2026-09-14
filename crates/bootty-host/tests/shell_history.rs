use assert_fs::prelude::*;
use bootty_host::shell_history::{HistoryEntry, HistoryRequest, parse_history, rank_history};
use pretty_assertions::assert_eq;
use rstest::rstest;
#[rstest]
#[case("bash", "#123\necho hello\n", "echo hello", Some(123))]
#[case("zsh", ": 123:4;echo hello\n", "echo hello", Some(123))]
#[case("zsh", ": 123:4;echo a\\\nb\n", "echo a\nb", Some(123))]
#[case(
    "fish",
    "- cmd: echo a\\nb\\\\c\n  when: 123\n",
    "echo a\nb\\c",
    Some(123)
)]
fn reads_shell_formats_without_interpreting_commands(
    #[case] shell: &str,
    #[case] text: &str,
    #[case] command: &str,
    #[case] timestamp: Option<u64>,
) {
    let entries = parse_history(shell, text);
    assert_eq!(entries.len(), 1);
    assert_eq!(&entries[0].command, command);
    assert_eq!(entries[0].timestamp, timestamp);
}
#[rstest]
fn ranking_merges_metadata_and_prefers_directory_and_frequency() {
    let entry = |command: &str, cwd: Option<&str>| HistoryEntry {
        command: command.to_owned(),
        cwd: cwd.map(str::to_owned),
        timestamp: Some(42),
        exit_code: Some(7),
        frequency: 1,
    };
    let entries = rank_history(
        vec![
            entry("git status", Some("/work")),
            entry("git status", None),
            entry("git stash", Some("/other")),
        ],
        "gts",
        "/work",
    );
    assert_eq!(entries[0].command, "git status");
    assert_eq!(entries[0].frequency, 2);
    assert_eq!(entries[0].cwd.as_deref(), Some("/work"));
    assert_eq!(entries[0].exit_code, Some(7));
}
#[rstest]
fn history_is_read_only_and_custom_paths_are_preserved() {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("custom history");
    file.write_str("echo one\necho two\n").unwrap();
    let request = HistoryRequest {
        shell: "bash".to_owned(),
        path: file.path().to_string_lossy().into_owned(),
        query: "two".to_owned(),
        cwd: "/".to_owned(),
        recent: Vec::new(),
    };
    let result = request.execute().unwrap();
    assert_eq!(result.entries[0].command, "echo two");
    file.assert("echo one\necho two\n");
}
#[rstest]
fn zsh_metafied_utf8_is_decoded_before_parsing() {
    let bytes = b": 12:0;echo \x83\xe3\x83\x89\n";
    let entries = bootty_host::shell_history::parse_history_bytes("zsh", bytes);
    assert_eq!(entries[0].command, "echo é");
}
