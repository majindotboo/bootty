use bootty_ui::error_catalog::ErrorNotice;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
// A local connection is not a remote workspace; only SSH failures say so.
#[case("connection refused", Some("The connection was lost. Try again."))]
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
    "/tmp/notes/doc.txt: No such file or directory (os error 2)",
    Some("It no longer exists.")
)]
#[case(
    "Permission denied (os error 13)",
    Some("Permission to it was denied.")
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
