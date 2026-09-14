use crate::terminal_text::TerminalTextConfig;
use bootty_config::config::FontConfig;

pub fn terminal_text_config(config: &FontConfig) -> TerminalTextConfig {
    TerminalTextConfig {
        families: config.family.clone(),
        style_bold: config.style_bold.clone(),
        style_italic: config.style_italic.clone(),
        style_bold_italic: config.style_bold_italic.clone(),
        font_features: config.features.clone(),
        font_size: config.size,
        cell_width: config.cell_width,
        cell_height: config.cell_height,
        fit_cell_height: config.fit_cell_height,
        fit_cell_width: config.fit_cell_width,
        baseline_adjustment: config.baseline_adjustment,
        underline_position: config.underline_position,
        underline_thickness: config.underline_thickness,
        ..TerminalTextConfig::default()
    }
}
