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
    "Connect to rmux daemon: connection refused."
)]
#[case("command deadline expired", "The command took too long. Try again.")]
#[case(
    "the Terminal target is stale",
    "That pane or session no longer exists."
)]
// Unrecognized failures keep their reason rather than a sentence that explains nothing.
#[case(
    "unknown variant `video`, expected `media`",
    "Unknown variant `video`, expected `media`."
)]
#[case(
    "pane %47 has no running terminal\ncaused by: gone",
    "Pane %47 has no running terminal."
)]
#[case("", "The operation failed without a reason.")]
fn technical_errors_have_useful_window_messages(#[case] raw: &str, #[case] message: &str) {
    let notice = ErrorNotice::from_text(raw);
    assert_eq!(notice.to_string(), message);
    assert_eq!(notice.raw_message(), raw);
}
