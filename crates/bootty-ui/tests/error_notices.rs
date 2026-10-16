use bootty_ui::error_catalog::ErrorNotice;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(
    "Wait for the current voice call to end before starting another",
    Some("Wait for the current voice call to end before starting another.")
)]
#[case(
    "Voice connection failed. Check your network and try again.",
    Some("Voice connection failed. Check your network and try again.")
)]
#[case(
    "Voice connection was lost. Check your network and try again.",
    Some("Voice connection was lost. Check your network and try again.")
)]
#[case(
    "%14: rmux pane output ended: TransportLost",
    Some("The terminal connection was lost. Try reopening this pane.")
)]
// A local rmux daemon is not a remote workspace; only SSH failures say so.
#[case(
    "connect to rmux daemon: connection refused",
    Some("The connection was lost. Try again.")
)]
#[case(
    "ssh: connect to host forge port 22: Operation timed out",
    Some("Could not reach the remote workspace.")
)]
#[case(
    "command deadline expired",
    Some("The command took too long. Try again.")
)]
#[case(
    "the Terminal target is stale",
    Some("That pane or session no longer exists.")
)]
// Provider diagnostics stay out of interface copy.
#[case("unknown variant `video`, expected `media`", None)]
#[case("pane %47 has no running terminal\ncaused by: gone", None)]
#[case("", None)]
#[case("Could not read /Users/private/token.json: invalid secret-value", None)]
#[case(
    r#"{"code":-32600,"message":"thread secret-id already has an active writer"}"#,
    Some("This conversation is open in another app. Close it there, then retry.")
)]
#[case(
    "session 019a is archived. Run `codex unarchive 019a` to unarchive it first.",
    Some("This conversation is archived. Unarchive it to continue.")
)]
#[case(
    "This conversation is archived in Codex.",
    Some("This conversation is archived. Unarchive it to continue.")
)]
#[case("Could not read archived logs", None)]
#[case(
    "Not logged in · /login",
    Some("Sign in to the selected provider account, then retry.")
)]
// Provider and dictation keys share one neutral instruction.
#[case(
    "Missing OpenAI API key",
    Some("Add the missing API key in Settings, then retry.")
)]
#[case(
    "Invalid API key · Fix external API key",
    Some("Add the missing API key in Settings, then retry.")
)]
/// A failure says what failed. A cause the catalog knows follows it; the raw text, which can hold
/// paths and provider output, stays in the details.
fn failures_name_their_action_and_a_known_cause(#[case] raw: &str, #[case] cause: Option<&str>) {
    let notice = ErrorNotice::failed("Could not send the message", raw);
    let expected = cause.map_or_else(
        || "Could not send the message.".to_owned(),
        |cause| format!("Could not send the message. {cause}"),
    );
    assert_eq!(notice.to_string(), expected);
    assert_eq!(notice.raw_message(), raw);
}
