#![cfg(test)]

use bootty_terminal::geometry::CellMetrics;
use bootty_ui::{
    terminal_cell_metrics::terminal_text_cell_metrics, terminal_text::TerminalTextConfig,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn default_grid_uses_the_preserved_ghostty_font_metrics() {
    let config = TerminalTextConfig::default();
    let cell = terminal_text_cell_metrics(&config, 1.0);

    assert!(cell.width < config.font_size);
    assert!(cell.height > config.font_size);
}

#[rstest]
fn explicit_cell_metrics_override_the_font_contract() {
    let config = TerminalTextConfig::with_cell_metrics(CellMetrics::new(12.5, 24.5));

    assert_eq!(
        terminal_text_cell_metrics(&config, 2.0),
        CellMetrics::new(12.5, 24.5)
    );
}

#[rstest]
#[case(1.0, 7.0, 23.0)]
#[case(1.5, 11.0, 33.0)]
#[case(2.0, 14.0, 45.0)]
fn maple_grid_rounds_in_device_pixels(#[case] scale: f32, #[case] width: f32, #[case] height: f32) {
    let config = TerminalTextConfig {
        families: vec!["Maple Mono NF".into()],
        font_size: 11.75,
        ..TerminalTextConfig::default()
    };
    let cell = terminal_text_cell_metrics(&config, scale);
    assert_eq!(cell, CellMetrics::new(width / scale, height / scale));
}

#[rstest]
fn fitted_grid_pitch_preserves_the_ghostty_ink_cell() {
    let config = TerminalTextConfig {
        fit_cell_height: true,
        fit_cell_width: true,
        ..TerminalTextConfig::default()
    };
    let base = CellMetrics::new(9.0, 20.0);
    let geometry = bootty_ui::terminal_text::TerminalTextGeometry::fitted(
        &config,
        103.0,
        207.0,
        base,
        bootty_terminal::geometry::TerminalPadding::default(),
    );

    assert_eq!(
        geometry.grid_cell,
        CellMetrics::new(103.0 / 20.0, 207.0 / 10.0),
        "fitting distributes the viewport through the product's minimum grid"
    );
    assert_eq!(
        geometry.ink_cell, base,
        "fitting cannot stretch or shift Ghostty's base glyph cell"
    );
}
