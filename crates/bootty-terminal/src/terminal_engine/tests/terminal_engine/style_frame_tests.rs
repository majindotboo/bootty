use super::super::super::*;
use super::support::*;
use proptest::prelude::*;

#[test]
fn terminal_engine_extracts_color_and_flag_style_state() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let palette_color = engine.terminal.color_palette()?[42];
    engine.write_vt(b"\x1b[38;5;42;48;2;255;128;64;1;4:3mX");
    let frame = engine.extract_frame()?;
    let cell = frame
        .cells
        .iter()
        .find(|cell| frame.cell_text(cell) == ['X'])
        .expect("styled cell");

    assert_eq!(cell.fg, Some(palette_color));
    assert_eq!(
        cell.bg,
        Some(RgbColor {
            r: 255,
            g: 128,
            b: 64,
        }),
    );
    assert!(cell.style.bold);
    assert_eq!(cell.style.underline, Underline::Curly);

    Ok(())
}

#[test]
fn terminal_engine_extracts_sgr_attribute_variants() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let palette_color = engine.terminal.color_palette()?[4];
    engine.write_vt(
        b"\x1b[1mB\x1b[22m\
          \x1b[2mF\x1b[22m\
          \x1b[3mI\x1b[23m\
          \x1b[5mK\x1b[25m\
          \x1b[7mV\x1b[27m\
          \x1b[8mH\x1b[28m\
          \x1b[9mS\x1b[29m\
          \x1b[53mO\x1b[55m\
          \x1b[4:3mU\x1b[24m\
          \x1b[38:2::1:2:3;48:5:4mC",
    );
    let frame = engine.extract_frame()?;

    let styled = |marker| {
        frame
            .cells
            .iter()
            .find(|cell| frame.cell_text(cell) == [marker])
            .unwrap_or_else(|| panic!("missing {marker} cell"))
    };

    assert!(styled('B').style.bold);
    assert!(styled('F').style.faint);
    assert!(styled('I').style.italic);
    assert!(styled('K').style.blink);
    assert!(styled('V').style.inverse);
    assert!(styled('H').style.invisible);
    assert!(styled('S').style.strikethrough);
    assert!(styled('O').style.overline);
    assert_eq!(styled('U').style.underline, Underline::Curly);
    assert_eq!(styled('C').fg, Some(RgbColor { r: 1, g: 2, b: 3 }));
    assert_eq!(styled('C').bg, Some(palette_color));

    Ok(())
}

#[test]
fn extract_frame_repacking_preserves_clean_row_text_after_earlier_row_length_change() -> Result<()>
{
    let mut engine = test_terminal_engine()?;
    engine.write_vt(b"\x1b[1;1Habcdefgh\x1b[2;1Hrow-two");
    let first = engine.extract_frame()?.clone();
    assert_eq!(row_text(&first, 1), "row-two");

    engine.write_vt(b"\x1b[1;1HZ\x1b[K");
    let frame = engine.extract_frame()?;

    assert_eq!(row_text(frame, 0), "Z");
    assert_eq!(row_text(frame, 1), "row-two");
    assert_eq!(frame.row_dirty.len(), usize::from(frame.rows));
    assert_eq!(frame.cells.len(), frame.stats.cells);
    assert_eq!(frame.text.len(), frame.stats.chars);
    Ok(())
}

#[test]
fn clean_extract_frame_reuses_retained_cells_without_stale_dirty_rows() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    engine.write_vt(b"\x1b[1;1Hcached frame\x1b[2;1Hrow two");
    let first = engine.extract_frame()?.clone();

    let frame = engine.extract_frame()?;

    assert_eq!(frame.dirty, libghostty_vt::render::Dirty::Clean);
    assert_eq!(row_text(frame, 0), row_text(&first, 0));
    assert_eq!(row_text(frame, 1), row_text(&first, 1));
    assert_eq!(frame.cells.len(), first.cells.len());
    assert_eq!(frame.text.len(), first.text.len());
    assert_eq!(frame.row_dirty, vec![false; usize::from(frame.rows)]);
    assert_eq!(frame.stats.dirty_rows, 0);
    assert_eq!(frame.stats.cells, frame.cells.len());
    assert_eq!(frame.stats.chars, frame.text.len());
    Ok(())
}

#[test]
fn hidden_hardware_cursor_is_not_extracted() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    })?;

    engine.write_vt(b"\x1b[?25l");
    let frame = engine.extract_frame()?;

    assert!(frame.cursor.is_none());
    Ok(())
}

#[test]
fn native_terminal_scrollback_retains_more_than_old_ten_thousand_row_cap() -> Result<()> {
    let geometry = TerminalGeometry {
        cols: 16,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    };
    let mut engine = TerminalEngine::new_with_scrollback(
        geometry,
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )?;

    for row in 0..20_000 {
        engine.write_vt(format!("row-{row:05}\r\n").as_bytes());
    }

    engine.scroll_viewport_delta(-1_000_000);
    let frame = engine.extract_frame()?;

    assert_eq!(row_text(frame, 0), "row-00000");
    Ok(())
}
#[test]
fn terminal_frame_exposes_scrollbar_state_for_native_scrollbar_ui() -> Result<()> {
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 8,
            rows: 2,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )?;

    engine.write_vt(b"one\r\ntwo\r\nthree\r\nfour");
    let frame = engine.extract_frame()?;
    let scrollbar = frame.scrollbar.expect("scrollbar state");

    assert!(scrollbar.total > scrollbar.len);
    assert_eq!(scrollbar.len, 2);
    Ok(())
}

#[test]
fn terminal_engine_scroll_viewport_bottom_returns_to_cursor() -> Result<()> {
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 8,
            rows: 2,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )?;

    engine.write_vt(b"one\r\ntwo\r\nthree\r\nfour");
    engine.scroll_viewport_delta(-2);
    assert_eq!(row_text(engine.extract_frame()?, 0), "one");

    engine.scroll_viewport_bottom();
    assert_eq!(row_text(engine.extract_frame()?, 0), "three");
    Ok(())
}

#[test]
fn native_terminal_scrolls_viewport_through_owned_scrollback() -> Result<()> {
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 8,
            rows: 2,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )?;

    engine.write_vt(b"one\r\ntwo\r\nthree\r\nfour");
    assert_eq!(row_text(engine.extract_frame()?, 0), "three");

    engine.scroll_viewport_delta(-2);
    assert_eq!(row_text(engine.extract_frame()?, 0), "one");

    engine.scroll_viewport_delta(2);
    assert_eq!(row_text(engine.extract_frame()?, 0), "three");
    Ok(())
}

#[test]
fn terminal_engine_projects_active_selection_into_render_frame() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 8,
        rows: 2,
        cell_width: 10,
        cell_height: 20,
    })?;
    let surface = TerminalSurface::for_logical_size(
        80.0,
        40.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let event = |x, y| TerminalSelectionEvent {
        surface,
        position: SurfacePoint { x, y },
        rectangle: false,
    };

    engine.write_vt(b"abcdefgh");
    engine.begin_selection(event(15.0, 10.0))?;
    engine.update_selection(event(45.0, 10.0))?;
    engine.end_selection(Some(event(45.0, 10.0)))?;

    assert_eq!(
        engine.extract_frame()?.selections,
        vec![FrameSelection {
            row: 0,
            start_col: 1,
            end_col: 3,
        }]
    );
    Ok(())
}

#[test]
fn terminal_engine_formats_active_selection_as_plain_text() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 8,
        rows: 2,
        cell_width: 10,
        cell_height: 20,
    })?;
    let surface = TerminalSurface::for_logical_size(
        80.0,
        40.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let event = |x, y| TerminalSelectionEvent {
        surface,
        position: SurfacePoint { x, y },
        rectangle: false,
    };

    engine.write_vt(b"abcdefgh");
    engine.begin_selection(event(15.0, 10.0))?;
    engine.update_selection(event(45.0, 10.0))?;
    engine.end_selection(Some(event(45.0, 10.0)))?;

    let text = engine
        .format_selection(TerminalSelectionFormat::PlainText)?
        .expect("active selection");
    assert_eq!(String::from_utf8_lossy(&text), "bcd");
    Ok(())
}

#[test]
fn terminal_engine_double_click_selects_word() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 2,
        cell_width: 10,
        cell_height: 20,
    })?;
    let surface = TerminalSurface::for_logical_size(
        120.0,
        40.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let event = |x, y| TerminalSelectionEvent {
        surface,
        position: SurfacePoint { x, y },
        rectangle: false,
    };

    engine.write_vt(b"abc def");
    engine.begin_selection(event(15.0, 10.0))?;
    engine.end_selection(Some(event(15.0, 10.0)))?;
    engine.begin_selection(event(15.0, 10.0))?;
    engine.end_selection(Some(event(15.0, 10.0)))?;

    let text = engine
        .format_selection(TerminalSelectionFormat::PlainText)?
        .expect("active selection");
    assert_eq!(String::from_utf8_lossy(&text), "abc");
    Ok(())
}

#[test]
fn terminal_engine_triple_click_selects_line() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 2,
        cell_width: 10,
        cell_height: 20,
    })?;
    let surface = TerminalSurface::for_logical_size(
        120.0,
        40.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let event = |x, y| TerminalSelectionEvent {
        surface,
        position: SurfacePoint { x, y },
        rectangle: false,
    };

    engine.write_vt(b"abc def");
    for _ in 0..3 {
        engine.begin_selection(event(15.0, 10.0))?;
        engine.end_selection(Some(event(15.0, 10.0)))?;
    }

    let text = engine
        .format_selection(TerminalSelectionFormat::PlainText)?
        .expect("active selection");
    assert_eq!(String::from_utf8_lossy(&text), "abc def");
    Ok(())
}

#[test]
fn terminal_engine_applies_configured_default_cursor_style_and_blink() -> Result<()> {
    let mut engine = TerminalEngine::new_with_cursor_options(
        TerminalGeometry {
            cols: 8,
            rows: 2,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        TerminalCursorConfig {
            style: Some(TerminalCursorStyle::Underline),
            blink: Some(true),
        },
        DEFAULT_MAX_SCROLLBACK,
        MacosOptionAsAlt::default(),
    )?;

    engine.write_vt(b"\x1b[0 q");
    let cursor = engine.extract_frame()?.cursor.expect("visible cursor");

    assert_eq!(cursor.style, CursorVisualStyle::Underline);
    assert!(cursor.blinking);
    Ok(())
}

proptest! {
    #[test]
    fn terminal_engine_extracts_truecolor_sgr_cells(
        fg_r in any::<u8>(),
        fg_g in any::<u8>(),
        fg_b in any::<u8>(),
        bg_r in any::<u8>(),
        bg_g in any::<u8>(),
        bg_b in any::<u8>(),
    ) {
        let mut engine = test_terminal_engine().expect("terminal engine");
        engine.write_vt(
            format!(
                "\x1b[38;2;{fg_r};{fg_g};{fg_b};48;2;{bg_r};{bg_g};{bg_b}mX"
            )
            .as_bytes(),
        );

        let frame = engine.extract_frame().expect("frame");
        let cell = frame
            .cells
            .iter()
            .find(|cell| frame.cell_text(cell) == ['X'])
            .expect("styled cell");

        prop_assert_eq!(cell.fg, Some(RgbColor { r: fg_r, g: fg_g, b: fg_b }));
        prop_assert_eq!(cell.bg, Some(RgbColor { r: bg_r, g: bg_g, b: bg_b }));
    }
}
