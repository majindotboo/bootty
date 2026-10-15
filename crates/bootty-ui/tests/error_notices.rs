use bootty_ui::error_catalog::ErrorNotice;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(
    "%14: rmux pane output ended: TransportLost",
    "The terminal connection was lost. Try reopening this pane."
)]
// A local rmux daemon is not a remote workspace; only SSH failures say so.
#[case(
    "connect to rmux daemon: connection refused",
    "The connection was lost. Try again."
)]
#[case(
    "ssh: connect to host forge port 22: Operation timed out",
    "Could not reach the remote workspace."
)]
#[case("command deadline expired", "The command took too long. Try again.")]
#[case(
    "the Terminal target is stale",
    "That pane or session no longer exists."
)]
// A refused steer says why instead of failing silently.
#[case(
    "The agent is stopping; queued messages send after it stops",
    "The agent is stopping. Queued messages send after it stops."
)]
#[case(
    "Wait for the previous message to reach the active turn",
    "The previous message hasn't reached the agent yet. Try again in a moment."
)]
// Provider diagnostics are retained without becoming interface copy.
#[case(
    "unknown variant `video`, expected `media`",
    "Could not complete the operation. Try again."
)]
#[case(
    "pane %47 has no running terminal\ncaused by: gone",
    "Could not complete the operation. Try again."
)]
#[case("", "Could not complete the operation. Try again.")]
#[case(
    "Could not read /Users/private/token.json: invalid secret-value",
    "Could not complete the operation. Try again."
)]
#[case(
    r#"{"code":-32600,"message":"thread secret-id already has an active writer"}"#,
    "This conversation is open in another app. Close it there, then retry."
)]
#[case(
    "session 019a is archived. Run `codex unarchive 019a` to unarchive it first.",
    "This conversation is archived. Unarchive it to continue."
)]
#[case(
    "This conversation is archived in Codex.",
    "This conversation is archived. Unarchive it to continue."
)]
#[case(
    "Could not read archived logs",
    "Could not complete the operation. Try again."
)]
#[case(
    "Not logged in · /login",
    "Sign in to the selected provider account, then retry."
)]
// Provider and dictation keys share one neutral instruction.
#[case(
    "Missing OpenAI API key",
    "Add the missing API key in Settings, then retry."
)]
#[case(
    "Invalid API key · Fix external API key",
    "Add the missing API key in Settings, then retry."
)]
fn technical_errors_have_useful_window_messages(#[case] raw: &str, #[case] message: &str) {
    let notice = ErrorNotice::from_text(raw);
    assert_eq!(notice.to_string(), message);
    assert_eq!(notice.raw_message(), raw);
}
