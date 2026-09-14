use bootty_terminal::{
    shell_lifecycle::ShellEvent,
    shell_prompt::{PromptReport, ShellPrompt},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
fn ready(state: &mut ShellPrompt) {
    state.lifecycle(ShellEvent::PromptStart);
    state.report(
        &PromptReport::Ready {
            shell: "bash".to_owned(),
            history_file: "/history".to_owned(),
            editable: true,
        },
        "/cwd",
        1,
    );
}
proptest! {
    #[test]
    fn any_input_or_command_invalidates_a_captured_prompt(inputs in 1..64usize) {
        let mut state = ShellPrompt::default(); ready(&mut state);
        let lease = state.snapshot("/cwd".to_owned(), true).revision;
        for _ in 0..inputs { state.invalidate(); }
        ready(&mut state); // Redraws do not prove an empty input buffer.
        prop_assert!(state.claim(lease,true).is_err());
        prop_assert!(!state.snapshot("/cwd".to_owned(),true).editable);
        state.lifecycle(ShellEvent::CommandFinish { exit_code: Some(0) }); ready(&mut state);
        let next = state.snapshot("/cwd".to_owned(), true);
        prop_assert!(next.editable);
        prop_assert!(state.claim(next.revision, true).is_ok());
        prop_assert!(state.claim(next.revision, true).is_err());
    }
}
#[rstest]
fn reported_commands_keep_their_execution_directory_and_exit_status() {
    let mut state = ShellPrompt::default();
    ready(&mut state);
    state.lifecycle(ShellEvent::CommandStart);
    state.report(&PromptReport::Command("false".to_owned()), "/source", 42);
    state.lifecycle(ShellEvent::CommandFinish { exit_code: Some(1) });
    let snapshot = state.snapshot("/next".to_owned(), true);
    assert_eq!(snapshot.recent[0].cwd, "/source");
    assert_eq!(snapshot.recent[0].timestamp, 42);
    assert_eq!(snapshot.recent[0].exit_code, Some(1));
}
#[rstest]
#[case("E;IHNlY3JldA==")]
#[case("E;YQBi")]
#[case("P;sh;;1")]
#[case("P;bash;!!!!;1")]
fn invalid_or_private_reports_are_ignored(#[case] value: &str) {
    assert!(PromptReport::parse(value).is_none());
}
#[rstest]
fn typeahead_cannot_be_mistaken_for_an_empty_prompt() {
    let mut state = ShellPrompt::default();
    ready(&mut state);
    state.input(false);
    state.input(true);
    state.lifecycle(ShellEvent::CommandStart);
    state.input(false); // Queued while the process still runs.
    state.lifecycle(ShellEvent::CommandFinish { exit_code: Some(0) });
    ready(&mut state);
    assert!(!state.snapshot("/cwd".to_owned(), true).editable);
    // Redraws and another command already in the queued batch remain conservative.
    state.lifecycle(ShellEvent::CommandStart);
    state.lifecycle(ShellEvent::CommandFinish { exit_code: Some(0) });
    ready(&mut state);
    assert!(!state.snapshot("/cwd".to_owned(), true).editable);
    // A fresh Enter at that prompt and observed completion start a clean cycle.
    state.input(true);
    state.lifecycle(ShellEvent::CommandStart);
    state.lifecycle(ShellEvent::CommandFinish { exit_code: Some(0) });
    ready(&mut state);
    assert!(state.snapshot("/cwd".to_owned(), true).editable);
}
