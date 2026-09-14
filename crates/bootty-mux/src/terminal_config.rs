use std::sync::mpsc;

use bootty_config::color::Color;
use bootty_config::config::{
    AppearanceVariant, BoottyConfig, ColorConfig, CursorConfig, CursorStyleConfig,
    MacosOptionAsAltConfig, SessionConfig, default_working_directory,
};
use bootty_terminal::RgbColor;
use bootty_terminal::terminal_session::{SessionLaunchConfig, TerminalSessionConfig};
use bootty_terminal::{
    terminal_engine::{
        TerminalColorConfig, TerminalCursorConfig, TerminalCursorStyle, TerminalFeatureConfig,
        TerminalLiveConfig, TerminalSideEffectEvent,
    },
    terminal_input_model::MacosOptionAsAlt,
};

pub fn terminal_session_config_with_side_effects(
    config: &BoottyConfig,
    variant: AppearanceVariant,
    side_effect_tx: &mpsc::Sender<TerminalSideEffectEvent>,
) -> TerminalSessionConfig {
    let TerminalLiveConfig {
        colors,
        cursor,
        features,
    } = terminal_live_config(config, variant);
    TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell_integration: config.session.shell_integration,
            shell: config.session.shell.clone(),
            args: Vec::new(),
            working_directory: config
                .session
                .working_directory
                .clone()
                .or_else(default_working_directory),
            // Filled in by the backend that starts the pane, which is the one that knows it.
            pane_id: None,
            env: config.session.env.clone(),
            env_remove: Vec::new(),
            term: config.session.term.clone(),
            colorterm: config.session.colorterm.clone(),
            term_program: None,
        },
        colors,
        cursor,
        features,
        max_scrollback: config.session.max_scrollback,
        macos_option_as_alt: terminal_macos_option_as_alt(config.input.macos_option_as_alt),
        side_effect_tx: Some(side_effect_tx.clone()),
        side_effect_pane_id: None,
        super_key_input_tx: None,
        benchmark_trace: None,
    }
}

pub fn terminal_color_config(config: &ColorConfig) -> TerminalColorConfig {
    let mut terminal = TerminalColorConfig::default();
    if let Some(background) = config.background {
        terminal.background = terminal_rgb_color(background);
    }
    if let Some(foreground) = config.foreground {
        terminal.foreground = terminal_rgb_color(foreground);
    }
    if let Some(cursor) = config.cursor {
        terminal.cursor = Some(terminal_rgb_color(cursor));
    }
    terminal.cursor_text = config.cursor_text.map(terminal_rgb_color);
    terminal.pointer_foreground = config.pointer_foreground.map(terminal_rgb_color);
    terminal.pointer_background = config.pointer_background.map(terminal_rgb_color);
    terminal.tektronix_foreground = config.tektronix_foreground.map(terminal_rgb_color);
    terminal.tektronix_background = config.tektronix_background.map(terminal_rgb_color);
    terminal.highlight_background = config.highlight_background.map(terminal_rgb_color);
    terminal.tektronix_cursor = config.tektronix_cursor.map(terminal_rgb_color);
    terminal.highlight_foreground = config.highlight_foreground.map(terminal_rgb_color);
    terminal.selection_background = config.selection_background.map(terminal_rgb_color);
    terminal.selection_foreground = config.selection_foreground.map(terminal_rgb_color);
    if !config.palette.is_empty() {
        terminal.palette = config
            .palette
            .iter()
            .take(256)
            .copied()
            .map(terminal_rgb_color)
            .collect();
    }
    terminal.palette_generate = config.palette_generate;
    terminal.palette_harmonious = config.palette_harmonious;
    terminal
}

const fn terminal_rgb_color(color: Color) -> RgbColor {
    RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

pub fn terminal_cursor_config(config: &CursorConfig) -> TerminalCursorConfig {
    TerminalCursorConfig {
        style: config.style.map(terminal_cursor_style),
        blink: config.blink,
    }
}

#[must_use]
pub const fn terminal_feature_config(config: &SessionConfig) -> TerminalFeatureConfig {
    TerminalFeatureConfig {
        glyph_protocol: config.glyph_protocol,
    }
}

#[must_use]
pub fn terminal_live_config(
    config: &BoottyConfig,
    variant: AppearanceVariant,
) -> TerminalLiveConfig {
    TerminalLiveConfig {
        colors: terminal_color_config(config.colors_for_appearance(variant)),
        cursor: terminal_cursor_config(&config.cursor),
        features: terminal_feature_config(&config.session),
    }
}

#[must_use]
pub const fn terminal_macos_option_as_alt(config: MacosOptionAsAltConfig) -> MacosOptionAsAlt {
    match config {
        MacosOptionAsAltConfig::None => MacosOptionAsAlt::None,
        MacosOptionAsAltConfig::Left => MacosOptionAsAlt::Left,
        MacosOptionAsAltConfig::Right => MacosOptionAsAlt::Right,
        MacosOptionAsAltConfig::Both => MacosOptionAsAlt::Both,
    }
}

const fn terminal_cursor_style(config: CursorStyleConfig) -> TerminalCursorStyle {
    match config {
        CursorStyleConfig::Bar => TerminalCursorStyle::Bar,
        CursorStyleConfig::Block => TerminalCursorStyle::Block,
        CursorStyleConfig::Underline => TerminalCursorStyle::Underline,
        CursorStyleConfig::HollowBlock => TerminalCursorStyle::HollowBlock,
    }
}
