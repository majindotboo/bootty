use anyhow::Result;
use bootty_terminal::{
    geometry::TerminalGeometry,
    terminal_engine::{
        NATIVE_MAX_SCROLLBACK, TerminalColorConfig, TerminalCopyModeAction, TerminalEngine,
        TerminalSearchDirection,
    },
    terminal_search::TerminalSearchOptions,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[fixture]
fn engine() -> Result<TerminalEngine> {
    TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 12,
            rows: 6,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )
}

#[rstest]
#[case(false, false, "foo", 3)]
#[case(false, true, "foo", 2)]
#[case(true, true, "[Ff]oo", 3)]
#[case(false, false, "f.o", 0)]
#[case(true, false, "f.o", 3)]
#[case(true, false, "^", 0)]
#[case(true, false, "o+", 3)]
fn search_options_apply_to_viewport_and_copy_mode(
    engine: Result<TerminalEngine>,
    #[case] regex: bool,
    #[case] case_sensitive: bool,
    #[case] query: &str,
    #[case] count: usize,
    #[values(false, true)] copy_mode: bool,
) {
    let mut engine = engine.expect("terminal fixture");
    engine.write_vt(b"Foo foo foo");
    let options = TerminalSearchOptions {
        regex,
        case_sensitive,
    };
    let found = if copy_mode {
        engine.enter_copy_mode().expect("test operation succeeds");
        engine
            .handle_copy_mode_action(TerminalCopyModeAction::SearchWithOptions {
                query: query.to_owned(),
                direction: TerminalSearchDirection::Current,
                options,
            })
            .expect("test operation succeeds")
            .search
            .expect("search result")
            .found
    } else {
        engine
            .search_viewport_with_options(query, TerminalSearchDirection::Current, options)
            .expect("test operation succeeds")
    };
    assert_eq!(found, count > 0);
    assert_eq!(
        engine
            .extract_frame()
            .expect("test operation succeeds")
            .search_match_count,
        count
    );
}

#[rstest]
fn invalid_regex_preserves_last_search_and_viewport(engine: Result<TerminalEngine>) {
    let mut engine = engine.expect("terminal fixture");
    engine.write_vt(b"one\r\ntarget\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven");
    assert!(
        engine
            .search_viewport("target", TerminalSearchDirection::Previous)
            .expect("test operation succeeds")
    );
    let before = engine
        .extract_frame()
        .expect("test operation succeeds")
        .clone();
    assert!(
        engine
            .search_viewport_with_options(
                "[",
                TerminalSearchDirection::Next,
                TerminalSearchOptions {
                    regex: true,
                    case_sensitive: false
                }
            )
            .is_err()
    );
    let after = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(after.scrollbar, before.scrollbar);
    assert_eq!(after.search_matches, before.search_matches);
}

#[rstest]
#[case("abcdefghijkXYZ", "kXY", vec![(0, 10, 11), (1, 0, 0)])]
#[case("界e\u{301}ABC", "e\u{301}AB", vec![(0, 2, 4)])]
#[case("界ABC", "界A", vec![(0, 0, 2)])]
fn search_uses_cell_coordinates_and_counts_logical_matches(
    engine: Result<TerminalEngine>,
    #[case] text: &str,
    #[case] query: &str,
    #[case] segments: Vec<(u16, u16, u16)>,
    #[values(false, true)] copy_mode: bool,
) {
    let mut engine = engine.expect("terminal fixture");
    engine.write_vt(text.as_bytes());
    if copy_mode {
        engine.enter_copy_mode().expect("test operation succeeds");
        assert!(
            engine
                .handle_copy_mode_action(TerminalCopyModeAction::Search {
                    query: query.to_owned(),
                    direction: TerminalSearchDirection::Current,
                })
                .expect("test operation succeeds")
                .search
                .expect("search")
                .found
        );
    } else {
        assert!(
            engine
                .search_viewport(query, TerminalSearchDirection::Current)
                .expect("test operation succeeds")
        );
    }
    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(frame.search_match_count, 1);
    assert_eq!(
        frame
            .active_search_segments
            .iter()
            .map(|s| (s.row, s.start_col, s.end_col))
            .collect::<Vec<_>>(),
        segments
    );
}

proptest! {
    #[test]
    fn literal_search_escapes_regex_syntax(query in "[a-z.*+?\\[\\](){}^$|]{1,10}") {
        let mut terminal = engine().expect("terminal fixture");
        terminal.write_vt(query.as_bytes());
        prop_assert!(terminal.search_viewport(&query, TerminalSearchDirection::Current).unwrap());
        prop_assert_eq!(terminal.extract_frame().unwrap().search_match_count, 1);
    }
}
