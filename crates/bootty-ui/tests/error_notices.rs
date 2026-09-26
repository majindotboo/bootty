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
    "The terminal is unavailable. Try reconnecting."
)]
#[case("command deadline expired", "The command took too long. Try again.")]
#[case(
    "unknown variant `video`, expected `media`",
    "The operation could not be completed."
)]
fn technical_errors_have_useful_window_messages(#[case] raw: &str, #[case] message: &str) {
    let notice = ErrorNotice::from_text(raw);
    assert_eq!(notice.to_string(), message);
    assert_eq!(notice.raw_message(), raw);
}
