//! Keyboard copy mode's cursor and selections over a conversation's shown text.
#![cfg(test)]
use bootty_ui::presentation::transcript_copy::{
    CopyMotion, CopySelection, apply_motion, cursor_at, selected_ranges, selected_text,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

const TEXT: &str = "one two, three\n\nfour five\nsix";

/// Where the cursor lands, shown as the text after it.
fn after(motion: CopyMotion, from: &str, count: usize) -> &'static str {
    let start = TEXT.find(from).expect("the start is in the text");
    let cursor = apply_motion(TEXT, cursor_at(TEXT, start), motion, count);
    TEXT.get(cursor.offset..).unwrap_or_default()
}

#[rstest]
#[case::word(CopyMotion::WordStart, "one two", 1, "two, three\n\nfour five\nsix")]
#[case::word_punctuation(CopyMotion::WordStart, "two, three", 1, ", three\n\nfour five\nsix")]
#[case::words_across_lines(CopyMotion::WordStart, "three", 1, "four five\nsix")]
#[case::counted_words(CopyMotion::WordStart, "one", 3, "three\n\nfour five\nsix")]
#[case::back(CopyMotion::WordBack, "four", 1, "three\n\nfour five\nsix")]
#[case::end(CopyMotion::WordEnd, "one", 1, "e two, three\n\nfour five\nsix")]
#[case::right_stops_at_the_line_end(CopyMotion::Right, "three", 9, "e\n\nfour five\nsix")]
#[case::left_stops_at_the_line_start(CopyMotion::Left, "five", 9, "four five\nsix")]
#[case::line_end(CopyMotion::LineEnd, "four", 1, "e\nsix")]
#[case::first_non_blank(CopyMotion::FirstNonBlank, "five", 1, "four five\nsix")]
#[case::text_end(CopyMotion::TextEnd, "one", 1, "six")]
#[case::text_start(CopyMotion::TextStart, "six", 1, TEXT)]
#[case::find(CopyMotion::Find { target: 'e', forward: true, till: false }, "one", 1, "e two, three\n\nfour five\nsix")]
#[case::find_counted(CopyMotion::Find { target: 'e', forward: true, till: false }, "one", 2, "ee\n\nfour five\nsix")]
#[case::till(CopyMotion::Find { target: ',', forward: true, till: true }, "one", 1, "o, three\n\nfour five\nsix")]
#[case::find_back(CopyMotion::Find { target: 'o', forward: false, till: false }, "three", 1, "o, three\n\nfour five\nsix")]
#[case::till_back(CopyMotion::Find { target: 'o', forward: false, till: true }, "three", 1, ", three\n\nfour five\nsix")]
#[case::find_stays_on_its_line(CopyMotion::Find { target: 's', forward: true, till: false }, "four", 1, "four five\nsix")]
fn motions_move_the_way_vim_does(
    #[case] motion: CopyMotion,
    #[case] from: &str,
    #[case] count: usize,
    #[case] expected: &str,
) {
    assert_eq!(after(motion, from, count), expected);
}

/// Up and down keep aiming for the column they started in, past shorter and empty lines.
#[test]
fn vertical_motions_keep_their_column() {
    let start = TEXT.find("three").unwrap();
    let down = apply_motion(TEXT, cursor_at(TEXT, start), CopyMotion::Down, 1);
    assert_eq!(down.offset, TEXT.find("\n\nfour").unwrap() + 1);
    let down = apply_motion(TEXT, down, CopyMotion::Down, 1);
    assert_eq!(TEXT.get(down.offset..), Some("e\nsix"));
    let up = apply_motion(TEXT, down, CopyMotion::Up, 2);
    assert_eq!(up.offset, start);
}

#[rstest]
#[case::characters(CopySelection::Character, "two", "three", "two, t")]
#[case::lines(CopySelection::Line, "two", "five", "one two, three\n\nfour five")]
#[case::block(CopySelection::Block, "two", "ive", "two\n\n fi")]
fn selections_copy_characters_lines_or_columns(
    #[case] selection: CopySelection,
    #[case] from: &str,
    #[case] to: &str,
    #[case] copied: &str,
) {
    let ranges = selected_ranges(
        TEXT,
        TEXT.find(from).unwrap(),
        TEXT.find(to).unwrap(),
        selection,
    );
    assert_eq!(selected_text(TEXT, &ranges), copied);
}

proptest! {
    /// Every motion keeps the cursor on a character of the text, and every selection covers
    /// whole characters of it.
    #[test]
    fn the_cursor_and_selections_stay_on_characters(
        text in "[a-c é\n,]{0,40}",
        start in 0usize..48,
        motion in 0usize..14,
        count in 0usize..5,
        anchor in 0usize..48,
    ) {
        let motions = [
            CopyMotion::Left, CopyMotion::Right, CopyMotion::Up, CopyMotion::Down,
            CopyMotion::WordStart, CopyMotion::WordBack, CopyMotion::WordEnd,
            CopyMotion::LineStart, CopyMotion::FirstNonBlank, CopyMotion::LineEnd,
            CopyMotion::TextStart, CopyMotion::TextEnd,
            CopyMotion::Find { target: 'é', forward: true, till: true },
            CopyMotion::Find { target: 'a', forward: false, till: false },
        ];
        let boundary = |offset: usize| (0..=offset.min(text.len())).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0);
        let cursor = apply_motion(&text, cursor_at(&text, boundary(start)), *motions.get(motion).unwrap(), count);
        prop_assert!(text.is_char_boundary(cursor.offset));
        for selection in [CopySelection::Character, CopySelection::Line, CopySelection::Block] {
            for range in selected_ranges(&text, boundary(anchor), cursor.offset, selection) {
                prop_assert!(range.start <= range.end && text.get(range).is_some());
            }
        }
    }
}
