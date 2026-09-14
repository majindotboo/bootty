use bootty_terminal::{
    geometry::{
        CellMetrics, GridPoint, SurfacePoint, TerminalGeometry, TerminalPadding, TerminalSurface,
    },
    terminal_engine::{TerminalEngine, TerminalSelectionEvent, TerminalSelectionFormat},
    terminal_links::{LinkTarget, link_at, parse_location, semantic_selection_at},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn engine(text: &str, cols: u16) -> anyhow::Result<TerminalEngine> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols,
        rows: 12,
        cell_width: 8,
        cell_height: 16,
    })?;
    engine.write_vt(text.as_bytes());
    Ok(engine)
}

proptest! {
    #[test]
    fn wrapped_urls_preserve_text_and_cell_mapping(cols in 12u16..60, name in "[a-z]{3,14}") {
        let url=format!("https://example.com/{name}/nested(foo.rs)?a=1&b=2");
        let text=format!("界e\u{301} ({url}).");
        let mut engine=engine(&text,cols).expect("terminal fixture");
        let frame=engine.extract_frame().unwrap();
        for offset in 5..5usize.checked_add(url.len()).expect("URL length") {
            let point=GridPoint{x:u16::try_from(offset.checked_rem(usize::from(cols)).unwrap()).unwrap(),y:u16::try_from(offset.checked_div(usize::from(cols)).unwrap()).unwrap()};
            let link=link_at(frame,point).expect("URL at each printed cell");
            prop_assert_eq!(&link.target,&LinkTarget::Url(url.clone()));
            prop_assert!(link.segments.iter().any(|segment|segment.row==point.y && segment.start_col<=point.x && segment.end_col>=point.x));
        }
    }
}

#[rstest]
#[case("src/main.rs:12:7", "src/main.rs", Some(12), Some(7))]
#[case("C:\\repo\\main.rs:12", "C:\\repo\\main.rs", Some(12), None)]
#[case("src/main.rs(12,7)", "src/main.rs", Some(12), Some(7))]
#[case("~/notes.md", "~/notes.md", None, None)]
fn file_locations_preserve_host_path_meaning(
    #[case] text: &str,
    #[case] path: &str,
    #[case] line: Option<u32>,
    #[case] column: Option<u32>,
) {
    assert_eq!(
        parse_location(text),
        Some(LinkTarget::File {
            path: path.to_owned(),
            line,
            column
        })
    );
}

#[rstest]
fn explicit_links_have_priority_and_do_not_bridge_unlinked_cells() {
    let mut engine = engine(
        "\x1b]8;;https://first.test\x07src/a.rs\x1b]8;;\x07 gap \x1b]8;;https://first.test\x07tail\x1b]8;;\x07",
        80,
    ).expect("terminal fixture");
    let link = link_at(engine.extract_frame().unwrap(), GridPoint { x: 2, y: 0 }).unwrap();
    assert_eq!(
        link.target,
        LinkTarget::Url("https://first.test".to_owned())
    );
    assert_eq!(link.segments[0].end_col, 7);
    assert!(link_at(engine.extract_frame().unwrap(), GridPoint { x: 10, y: 0 }).is_none());
}

#[rstest]
#[case("\"notes with spaces.md\"", 8, "notes with spaces.md")]
#[case("(hello world)", 4, "hello world")]
#[case("person@example.com", 5, "person@example.com")]
#[case("person@example.com,", 5, "person@example.com")]
#[case("日本語", 2, "本")]
fn semantic_double_click_uses_the_published_text(
    #[case] text: &str,
    #[case] col: u16,
    #[case] expected: &str,
) {
    let mut engine = engine(text, 80).expect("terminal fixture");
    let frame = engine.extract_frame().unwrap();
    assert!(semantic_selection_at(frame, GridPoint { x: col, y: 0 }).is_some());
    let event = TerminalSelectionEvent {
        surface: TerminalSurface::for_logical_size(
            640.,
            192.,
            CellMetrics::new(8., 16.),
            TerminalPadding::default(),
        ),
        position: SurfacePoint {
            x: f32::mul_add(f32::from(col), 8., 4.),
            y: 8.,
        },
        rectangle: false,
    };
    engine.begin_selection(event).unwrap();
    engine.begin_selection(event).unwrap();
    engine.end_selection(Some(event)).unwrap();
    let selected = engine
        .format_selection(TerminalSelectionFormat::PlainText)
        .unwrap()
        .unwrap();
    assert_eq!(String::from_utf8(selected).unwrap(), expected);
}
