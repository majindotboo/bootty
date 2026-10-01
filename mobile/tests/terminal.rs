use bootty_mobile::TerminalPresentation;
use gpui_kit::{FontWeight, rgb};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

proptest! {
    #[test]
    fn printable_unicode_is_preserved_with_valid_style_ranges(text in "[[:alnum:] 🥟┃█]{0,300}") {
        let output = TerminalPresentation::parse(&format!("\u{1b}[1m{text}\u{1b}[0m"));
        assert_eq!(&output.text, &text);
        for (range, _) in output.highlights {
            prop_assert!(output.text.is_char_boundary(range.start));
            prop_assert!(output.text.is_char_boundary(range.end));
            prop_assert!(range.start < range.end);
        }
    }
}

#[rstest]
#[case("\u{1b}]0;secret title\u{7}Hello\r\n🥟", "Hello\n🥟")]
#[case("visible\u{1b}[8msecret\u{1b}[28m!", "visible      !")]
#[case("\u{1b}]52;c;c2VjcmV0\u{1b}\\safe", "safe")]
fn control_payloads_do_not_become_visible_text(#[case] capture: &str, #[case] text: &str) {
    assert_eq!(TerminalPresentation::parse(capture).text, text);
}

#[rstest]
#[case("31", 0x00cd_0000)]
#[case("38;5;196", 0x00ff_0000)]
#[case("38;2;12;34;56", 0x000c_2238)]
#[case("38:2:12:34:56", 0x000c_2238)]
#[case("38:2::12:34:56", 0x000c_2238)]
fn colors_and_emphasis_reset_at_unicode_boundaries(#[case] color: &str, #[case] expected: u32) {
    let output = TerminalPresentation::parse(&format!("\u{1b}[{color};1m🥟\u{1b}[0mplain"));
    assert_eq!(output.text, "🥟plain");
    let (range, highlight) = output.highlights.first().unwrap();
    assert_eq!(range, &(0.."🥟".len()));
    assert_eq!(highlight.color, Some(rgb(expected).into()));
    assert_eq!(highlight.font_weight, Some(FontWeight::BOLD));
    let (_, plain) = output.highlights.last().unwrap();
    assert_eq!(plain.color, None);
    assert_eq!(plain.font_weight, None);
}
