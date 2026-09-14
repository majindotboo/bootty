#![cfg(test)]

use bootty_terminal::geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface};
use bootty_terminal::terminal_engine::TerminalEngine;
use bootty_ui::paint_plan::PaintPlanner;
use pretty_assertions::assert_eq;
use rstest::rstest;

fn surface(cols: u16, rows: u16) -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::from(cols) * 9.0,
        f32::from(rows) * 22.0,
        CellMetrics::new(9.0, 22.0),
        TerminalPadding::default(),
    )
}

fn populated_engine(cols: u16, rows: u16) -> TerminalEngine {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
    .expect("terminal engine");
    for row in 0..rows {
        engine.write_vt(
            format!(
                "\x1b[{};1H\x1b[38;5;{}mrow {row:03} styled text\x1b[0m",
                row.checked_add(1).expect("row fits"),
                (row % 216).checked_add(16).expect("palette index fits"),
            )
            .as_bytes(),
        );
    }
    engine
}

#[rstest]
#[case(120, 40)]
#[case(240, 90)]
fn localized_plan_matches_full_plan_and_rebuilds_only_dirty_rows(
    #[case] cols: u16,
    #[case] rows: u16,
) {
    let surface = surface(cols, rows);
    let mut engine = populated_engine(cols, rows);
    let initial = engine.extract_frame().expect("initial frame").clone();
    let mut incremental = PaintPlanner::default();
    let (_, initial_rows) = incremental.plan_incremental(surface, &initial, 16.0, 22.0);
    assert_eq!(initial_rows, usize::from(rows));

    engine.write_vt(
        format!(
            "\x1b[{};1Hedited",
            (rows / 2).checked_add(1).expect("middle row fits")
        )
        .as_bytes(),
    );
    let edited = engine.extract_frame().expect("edited frame").clone();
    let dirty_rows = edited.row_dirty.iter().filter(|dirty| **dirty).count();
    assert!(dirty_rows > 0);
    assert!(dirty_rows < usize::from(rows));

    let (incremental_plan, rebuilt_rows) =
        incremental.plan_incremental(surface, &edited, 16.0, 22.0);
    let incremental_plan = incremental_plan.clone();
    let full_plan = PaintPlanner::default().plan(surface, &edited, 16.0).clone();

    assert_eq!(rebuilt_rows, dirty_rows);
    assert_eq!(incremental_plan, full_plan);

    let clean = engine.extract_frame().expect("clean frame").clone();
    let (_, clean_rebuilt_rows) = incremental.plan_incremental(surface, &clean, 16.0, 22.0);
    assert_eq!(clean_rebuilt_rows, 0);
}

#[rstest]
fn tui_redraw_loop_keeps_incremental_plan_equal_to_full_plan() {
    let cols = 80;
    let rows = 24;
    let surface = surface(cols, rows);
    let mut engine = populated_engine(cols, rows);
    let mut incremental = PaintPlanner::default();

    // A fullscreen TUI loop: scroll the conversation region, rewrite the addressed
    // status row, restore the cursor. Every intermediate frame must plan identically
    // whether rows are reused or rebuilt, or rows visibly jump and overlap.
    let script = [
        "\x1b[1;20r".to_owned(),
        "\x1b[20;1Hnew message line one".to_owned(),
        "\n".to_owned(),
        "\x1b[20;1Hnew message line two".to_owned(),
        "\n".to_owned(),
        "\x1b[24;1H\x1b[2K\x1b[38;5;11mWorking (1s)\x1b[0m".to_owned(),
        "\x1b[20;1Hnew message line three".to_owned(),
        "\n".to_owned(),
        "\x1b[24;1H\x1b[2K\x1b[38;5;11mWorking (2s)\x1b[0m".to_owned(),
        "\x1b[24;1H\x1b[2K\x1b[38;5;10mDone\x1b[0m".to_owned(),
        "\x1b[r".to_owned(),
    ];
    for step in &script {
        engine.write_vt(step.as_bytes());
        let frame = engine.extract_frame().expect("step frame").clone();
        let (incremental_plan, _) = incremental.plan_incremental(surface, &frame, 16.0, 22.0);
        let full_plan = PaintPlanner::default().plan(surface, &frame, 16.0).clone();
        assert_eq!(incremental_plan.clone(), full_plan, "step {step:?}");
    }
}

#[rstest]
#[case::later_row_update(b"\x1b[4;1Hnext".as_slice())]
#[case::clean_publication(b"".as_slice())]
fn skipped_publication_does_not_leave_erased_text(#[case] later_update: &[u8]) {
    let surface = surface(80, 24);
    let mut engine = populated_engine(80, 24);
    let mut incremental = PaintPlanner::default();
    incremental.plan_incremental(surface, engine.extract_frame().unwrap(), 16.0, 22.0);

    engine.write_vt(b"\x1b[2;1H\x1b[2K");
    // The terminal worker publishes this erase while the UI is still painting.
    engine.extract_frame().unwrap();
    engine.write_vt(later_update);
    let latest = engine.extract_frame().unwrap();
    if later_update.is_empty() {
        assert!(!latest.row_dirty[1]);
    }

    let actual = incremental
        .plan_incremental(surface, latest, 16.0, 22.0)
        .0
        .clone();
    let expected = PaintPlanner::default().plan(surface, latest, 16.0).clone();
    assert_eq!(actual, expected);
}

#[rstest]
fn explicit_background_equal_to_default_remains_a_cell_fill() {
    let surface = surface(4, 1);
    let mut engine = TerminalEngine::new(surface.geometry()).unwrap();
    engine.write_vt(b"\x1b]11;#123456\x07\x1b[48;2;18;52;86mA\x1b[49mB");
    let frame = engine.extract_frame().unwrap();
    let mut planner = PaintPlanner::default();
    let plan = planner.plan(surface, frame, 16.0);
    assert_eq!(plan.backgrounds.len(), 1);
    assert_eq!(plan.backgrounds[0].color, plan.default_background);
    assert_eq!(plan.backgrounds[0].rect, surface.cell_rect(0, 0));
}

#[rstest]
#[case(18.0, 20.0)]
#[case(20.0, 16.0)]
fn changed_font_underline_metrics_repaint_clean_rows(#[case] before: f32, #[case] after: f32) {
    let mut engine = populated_engine(40, 3);
    engine.write_vt(b"\x1b[1;1H\x1b[4munderlined\x1b[0m");
    let initial = engine.extract_frame().unwrap().clone();
    let mut planner = PaintPlanner::default();
    planner.set_underline_offset(before);
    let old_y = planner
        .plan_incremental(surface(40, 3), &initial, 16.0, 22.0)
        .0
        .decorations[0]
        .start_y;
    let clean = engine.extract_frame().unwrap().clone();
    planner.set_underline_offset(after);
    let (plan, rebuilt) = planner.plan_incremental(surface(40, 3), &clean, 16.0, 22.0);
    assert_eq!(rebuilt, 3);
    assert_eq!(
        (plan.decorations[0].start_y - old_y).to_bits(),
        (after - before).to_bits()
    );
    let mut full = PaintPlanner::default();
    full.set_underline_offset(after);
    assert_eq!(plan, full.plan(surface(40, 3), &clean, 16.0));
}
