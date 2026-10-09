use bootty_ui::error_catalog::ErrorNotice;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(
    "%14: rmux pane output ended: TransportLost",
    "The terminal connection was lost. Try reopening this pane."
)]
#[case(
    "connect to rmux daemon: connection refused",
    "Could not reach the remote workspace."
)]
#[case("command deadline expired", "The command took too long. Try again.")]
#[case(
    "the Terminal target is stale",
    "That pane or session no longer exists."
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
    "Not logged in · /login",
    "Sign in to the selected provider account, then retry."
)]
#[case(
    "Missing OpenAI API key",
    "Configure an OpenAI API key to use dictation and voice."
)]
fn technical_errors_have_useful_window_messages(#[case] raw: &str, #[case] message: &str) {
    let notice = ErrorNotice::from_text(raw);
    assert_eq!(notice.to_string(), message);
    assert_eq!(notice.raw_message(), raw);
}
