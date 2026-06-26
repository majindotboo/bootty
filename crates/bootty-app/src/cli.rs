use std::{
    fs,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use bootty_config::{
    color::Color,
    config::{
        BoottyConfig, CursorStyleConfig, MacosOptionAsAltConfig, MacosTitlebarStyle,
        MultiplexerBackendConfig, SidebarPosition, WindowDecoration, WindowFullscreen,
        default_config_path, load_config_from_path, resolve_theme,
    },
};
use bootty_render::terminal_text::FontFeature;
use clap::{Args, Parser, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "bootty", version, about = "Bootty terminal emulator")]
pub struct Cli {
    /// Load config from this TOML file instead of the default XDG path.
    #[arg(long, value_name = "PATH", conflicts_with = "defaults")]
    config: Option<PathBuf>,

    /// Ignore user config and start from built-in defaults with isolated temp sidecar state.
    #[arg(long, conflicts_with = "config")]
    defaults: bool,

    #[command(flatten)]
    overrides: ConfigOverrides,
}

impl Cli {
    pub fn load_config(&self) -> Result<BoottyConfig> {
        let path = self.selected_config_path();
        if self.defaults {
            create_parent_dir_for_defaults(&path)?;
        }
        let mut config = load_config_from_path(&path)?;
        self.overrides.apply(&mut config)?;
        Ok(config)
    }

    fn selected_config_path(&self) -> PathBuf {
        if self.defaults {
            return isolated_defaults_config_path();
        }
        self.config.clone().unwrap_or_else(default_config_path)
    }
}

#[derive(Debug, Default, Args)]
struct ConfigOverrides {
    /// Force the multiplexer backend.
    #[arg(long, value_enum, value_name = "BACKEND")]
    backend: Option<CliBackend>,

    /// Force tmux status hiding on.
    #[arg(long, conflicts_with = "show_tmux_status")]
    hide_tmux_status: bool,

    /// Force tmux status hiding off.
    #[arg(long)]
    show_tmux_status: bool,

    /// Force fullscreen mode. Omitting a value is the same as --fullscreen native.
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "native",
        conflicts_with = "no_fullscreen"
    )]
    fullscreen: Option<CliFullscreen>,

    /// Force fullscreen off, regardless of config.
    #[arg(long)]
    no_fullscreen: bool,

    /// Force non-native fullscreen top offset.
    #[arg(long, value_name = "PX")]
    fullscreen_top_offset: Option<f32>,

    /// Let fullscreen tabs occupy the notch band.
    #[arg(long, conflicts_with = "no_fullscreen_tabs_in_notch")]
    fullscreen_tabs_in_notch: bool,

    /// Keep fullscreen tabs below the notch band.
    #[arg(long)]
    no_fullscreen_tabs_in_notch: bool,

    /// Force native window decoration mode.
    #[arg(long, value_enum, value_name = "MODE")]
    window_decoration: Option<CliWindowDecoration>,

    /// Force macOS titlebar style.
    #[arg(
        long = "titlebar",
        alias = "macos-titlebar-style",
        value_enum,
        value_name = "STYLE"
    )]
    titlebar: Option<CliTitlebarStyle>,

    /// Force the window title.
    #[arg(long, value_name = "TITLE")]
    title: Option<String>,

    /// Force the initial window width.
    #[arg(long, value_name = "PX")]
    width: Option<f32>,

    /// Force the initial window height.
    #[arg(long, value_name = "PX")]
    height: Option<f32>,

    /// Force the active theme name.
    #[arg(long, value_name = "NAME")]
    theme: Option<String>,

    /// Force terminal background color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    background: Option<Color>,

    /// Force terminal foreground color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    foreground: Option<Color>,

    /// Force terminal cursor color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    cursor_color: Option<Color>,

    /// Force text color under the cursor.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    cursor_text: Option<Color>,

    /// Force selection background color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    selection_background: Option<Color>,

    /// Force selection foreground color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    selection_foreground: Option<Color>,

    /// Force the ANSI palette. Repeat the flag or pass a comma-separated list.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color, value_delimiter = ',', num_args = 1..)]
    palette: Vec<Color>,

    /// Enable generated 256-color palette entries.
    #[arg(long, conflicts_with = "no_palette_generate")]
    palette_generate: bool,

    /// Disable generated 256-color palette entries.
    #[arg(long)]
    no_palette_generate: bool,

    /// Enable harmonious generated palette entries.
    #[arg(long, conflicts_with = "no_palette_harmonious")]
    palette_harmonious: bool,

    /// Disable harmonious generated palette entries.
    #[arg(long)]
    no_palette_harmonious: bool,

    /// Force the font size.
    #[arg(long, value_name = "PT")]
    font_size: Option<f32>,

    /// Force font families. Repeat the flag or pass a comma-separated list.
    #[arg(long, value_name = "FAMILY", value_delimiter = ',', num_args = 1..)]
    font_family: Vec<String>,

    /// Add font feature settings such as +liga or ss01.
    #[arg(long, value_name = "FEATURE", value_delimiter = ',', num_args = 1..)]
    font_feature: Vec<String>,

    /// Force fixed terminal cell width.
    #[arg(long, value_name = "PX")]
    font_cell_width: Option<f32>,

    /// Force fixed terminal cell height.
    #[arg(long, value_name = "PX")]
    font_cell_height: Option<f32>,

    /// Stretch row spacing to fit the available terminal height.
    #[arg(long, conflicts_with = "no_fit_cell_height")]
    fit_cell_height: bool,

    /// Disable row spacing stretch-to-fit.
    #[arg(long)]
    no_fit_cell_height: bool,

    /// Stretch column spacing to fit the available terminal width.
    #[arg(long, conflicts_with = "no_fit_cell_width")]
    fit_cell_width: bool,

    /// Disable column spacing stretch-to-fit.
    #[arg(long)]
    no_fit_cell_width: bool,

    /// Force font baseline adjustment.
    #[arg(long, value_name = "PX")]
    font_baseline_adjustment: Option<f32>,

    /// Force underline position adjustment.
    #[arg(long, value_name = "PX")]
    font_underline_position: Option<f32>,

    /// Force underline thickness adjustment.
    #[arg(long, value_name = "PX")]
    font_underline_thickness: Option<f32>,

    /// Force cursor style.
    #[arg(long, value_enum, value_name = "STYLE")]
    cursor_style: Option<CliCursorStyle>,

    /// Force cursor blinking on.
    #[arg(long, conflicts_with = "no_cursor_blink")]
    cursor_blink: bool,

    /// Force cursor blinking off.
    #[arg(long)]
    no_cursor_blink: bool,

    /// Force the shell used for new sessions.
    #[arg(long, value_name = "PATH")]
    shell: Option<String>,

    /// Force the working directory used for new sessions.
    #[arg(long, value_name = "DIR")]
    working_directory: Option<PathBuf>,

    /// Replace session environment with NAME=VALUE entries.
    #[arg(long = "env", value_name = "NAME=VALUE", value_parser = parse_env, num_args = 1..)]
    env: Vec<EnvOverride>,

    /// Force TERM for new sessions.
    #[arg(long, value_name = "TERM")]
    term: Option<String>,

    /// Force COLORTERM for new sessions.
    #[arg(long, value_name = "COLORTERM")]
    colorterm: Option<String>,

    /// Force max scrollback rows.
    #[arg(long, value_name = "ROWS")]
    max_scrollback: Option<usize>,

    /// Enable the terminal glyph protocol.
    #[arg(long, conflicts_with = "no_glyph_protocol")]
    glyph_protocol: bool,

    /// Disable the terminal glyph protocol.
    #[arg(long)]
    no_glyph_protocol: bool,

    /// Force macOS Option-as-Alt mode.
    #[arg(long, value_enum, value_name = "MODE")]
    macos_option_as_alt: Option<CliMacosOptionAsAlt>,

    /// Replace modifier remaps. Repeat the flag or pass a comma-separated list.
    #[arg(long, value_name = "REMAP", value_delimiter = ',', num_args = 1..)]
    modifier_remap: Vec<String>,

    /// Force the sidebar on.
    #[arg(long, conflicts_with = "no_sidebar")]
    sidebar: bool,

    /// Force the sidebar off.
    #[arg(long)]
    no_sidebar: bool,

    /// Force the sidebar position.
    #[arg(long, value_enum, value_name = "POSITION")]
    sidebar_position: Option<CliSidebarPosition>,

    /// Force sidebar width.
    #[arg(long, value_name = "PX")]
    sidebar_width: Option<f32>,

    /// Force sidebar background color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    sidebar_background: Option<Color>,

    /// Force sidebar fullscreen background color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    sidebar_fullscreen_background: Option<Color>,

    /// Force sidebar foreground color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    sidebar_foreground: Option<Color>,

    /// Force selected sidebar row color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    sidebar_selected: Option<Color>,

    /// Force hovered sidebar row color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    sidebar_hover: Option<Color>,

    /// Force sidebar border color.
    #[arg(long, value_name = "#RRGGBB", value_parser = parse_color)]
    sidebar_border: Option<Color>,

    /// Force the status bar on.
    #[arg(long, conflicts_with = "no_status_bar")]
    status_bar: bool,

    /// Force the status bar off.
    #[arg(long)]
    no_status_bar: bool,

    /// Force status bar height.
    #[arg(long, value_name = "PX")]
    status_height: Option<f32>,

    /// Force chrome gap size.
    #[arg(long = "chrome-gap", alias = "gap", value_name = "PX")]
    chrome_gap: Option<f32>,

    /// Force unfocused sidebar dim amount.
    #[arg(long, value_name = "0..1")]
    unfocused_sidebar_dim: Option<f32>,

    /// Force unfocused terminal dim amount.
    #[arg(long, value_name = "0..1")]
    unfocused_terminal_dim: Option<f32>,

    /// Write stability trace CSV to this path.
    #[arg(long, value_name = "PATH")]
    stability_trace: Option<PathBuf>,
}

impl ConfigOverrides {
    fn apply(&self, config: &mut BoottyConfig) -> Result<()> {
        self.apply_multiplexer(config);
        self.apply_window(config);
        self.apply_theme_and_colors(config)?;
        self.apply_font(config)?;
        self.apply_cursor(config);
        self.apply_session(config);
        self.apply_input(config);
        self.apply_chrome(config);
        self.apply_sidebar(config);
        self.apply_diagnostics(config);
        Ok(())
    }

    fn apply_multiplexer(&self, config: &mut BoottyConfig) {
        if let Some(backend) = self.backend {
            config.multiplexer.backend = backend.into();
        }
        if let Some(hide_tmux_status) = bool_override(self.hide_tmux_status, self.show_tmux_status)
        {
            config.multiplexer.hide_tmux_status = hide_tmux_status;
        }
    }

    fn apply_window(&self, config: &mut BoottyConfig) {
        if let Some(fullscreen) = self.fullscreen {
            config.window.fullscreen = fullscreen.into();
        }
        if self.no_fullscreen {
            config.window.fullscreen = WindowFullscreen::Disabled;
        }
        if let Some(offset) = self.fullscreen_top_offset {
            config.window.fullscreen_top_offset = Some(offset);
        }
        if let Some(tabs_in_notch) = bool_override(
            self.fullscreen_tabs_in_notch,
            self.no_fullscreen_tabs_in_notch,
        ) {
            config.window.fullscreen_tabs_in_notch = tabs_in_notch;
        }
        if let Some(decoration) = self.window_decoration {
            config.window.window_decoration = decoration.into();
        }
        if let Some(titlebar) = self.titlebar {
            config.window.macos_titlebar_style = titlebar.into();
        }
        if let Some(title) = &self.title {
            config.window.title.clone_from(title);
        }
        if let Some(width) = self.width {
            config.window.width = width;
        }
        if let Some(height) = self.height {
            config.window.height = height;
        }
    }

    fn apply_theme_and_colors(&self, config: &mut BoottyConfig) -> Result<()> {
        if let Some(theme) = &self.theme {
            let config_dir = config
                .config_path
                .parent()
                .unwrap_or_else(|| Path::new("."));
            config.colors = resolve_theme(theme, config_dir)?.colors;
            config.theme = Some(theme.clone());
        }
        if let Some(background) = self.background {
            config.colors.background = Some(background);
        }
        if let Some(foreground) = self.foreground {
            config.colors.foreground = Some(foreground);
        }
        if let Some(cursor) = self.cursor_color {
            config.colors.cursor = Some(cursor);
        }
        if let Some(cursor_text) = self.cursor_text {
            config.colors.cursor_text = Some(cursor_text);
        }
        if let Some(selection_background) = self.selection_background {
            config.colors.selection_background = Some(selection_background);
        }
        if let Some(selection_foreground) = self.selection_foreground {
            config.colors.selection_foreground = Some(selection_foreground);
        }
        if !self.palette.is_empty() {
            config.colors.palette.clone_from(&self.palette);
        }
        if let Some(palette_generate) =
            bool_override(self.palette_generate, self.no_palette_generate)
        {
            config.colors.palette_generate = palette_generate;
        }
        if let Some(palette_harmonious) =
            bool_override(self.palette_harmonious, self.no_palette_harmonious)
        {
            config.colors.palette_harmonious = palette_harmonious;
        }
        Ok(())
    }

    fn apply_font(&self, config: &mut BoottyConfig) -> Result<()> {
        if let Some(font_size) = self.font_size {
            config.font.size = font_size;
        }
        if !self.font_family.is_empty() {
            config.font.family.clone_from(&self.font_family);
        }
        for feature in &self.font_feature {
            let parsed = FontFeature::parse(feature)
                .ok_or_else(|| anyhow!("invalid font feature: {feature}"))?;
            config.font.features.push(parsed);
        }
        if let Some(cell_width) = self.font_cell_width {
            config.font.cell_width = Some(cell_width);
        }
        if let Some(cell_height) = self.font_cell_height {
            config.font.cell_height = Some(cell_height);
        }
        if let Some(fit_cell_height) = bool_override(self.fit_cell_height, self.no_fit_cell_height)
        {
            config.font.fit_cell_height = fit_cell_height;
        }
        if let Some(fit_cell_width) = bool_override(self.fit_cell_width, self.no_fit_cell_width) {
            config.font.fit_cell_width = fit_cell_width;
        }
        if let Some(adjustment) = self.font_baseline_adjustment {
            config.font.baseline_adjustment = adjustment;
        }
        if let Some(position) = self.font_underline_position {
            config.font.underline_position = position;
        }
        if let Some(thickness) = self.font_underline_thickness {
            config.font.underline_thickness = thickness;
        }
        Ok(())
    }

    fn apply_cursor(&self, config: &mut BoottyConfig) {
        if let Some(style) = self.cursor_style {
            config.cursor.style = Some(style.into());
        }
        if let Some(blink) = bool_override(self.cursor_blink, self.no_cursor_blink) {
            config.cursor.blink = Some(blink);
        }
    }

    fn apply_session(&self, config: &mut BoottyConfig) {
        if let Some(shell) = &self.shell {
            config.session.shell = Some(shell.clone());
        }
        if let Some(working_directory) = &self.working_directory {
            config.session.working_directory = Some(working_directory.clone());
        }
        if !self.env.is_empty() {
            config.session.env = self
                .env
                .iter()
                .map(|entry| (entry.name.clone(), entry.value.clone()))
                .collect();
        }
        if let Some(term) = &self.term {
            config.session.term.clone_from(term);
        }
        if let Some(colorterm) = &self.colorterm {
            config.session.colorterm.clone_from(colorterm);
        }
        if let Some(max_scrollback) = self.max_scrollback {
            config.session.max_scrollback = max_scrollback;
        }
        if let Some(glyph_protocol) = bool_override(self.glyph_protocol, self.no_glyph_protocol) {
            config.session.glyph_protocol = glyph_protocol;
        }
    }

    fn apply_input(&self, config: &mut BoottyConfig) {
        if let Some(mode) = self.macos_option_as_alt {
            config.input.macos_option_as_alt = mode.into();
        }
        if !self.modifier_remap.is_empty() {
            config.input.modifier_remap.clone_from(&self.modifier_remap);
        }
    }

    fn apply_chrome(&self, config: &mut BoottyConfig) {
        if let Some(sidebar) = bool_override(self.sidebar, self.no_sidebar) {
            config.chrome.sidebar = sidebar;
        }
        if let Some(status_bar) = bool_override(self.status_bar, self.no_status_bar) {
            config.chrome.status_bar = status_bar;
        }
        if let Some(sidebar_width) = self.sidebar_width {
            config.chrome.sidebar_width = sidebar_width;
        }
        if let Some(status_height) = self.status_height {
            config.chrome.status_height = status_height;
        }
        if let Some(gap) = self.chrome_gap {
            config.chrome.gap = gap;
        }
        if let Some(dim) = self.unfocused_sidebar_dim {
            config.chrome.unfocused_sidebar_dim = dim;
        }
        if let Some(dim) = self.unfocused_terminal_dim {
            config.chrome.unfocused_terminal_dim = dim;
        }
    }

    fn apply_sidebar(&self, config: &mut BoottyConfig) {
        if let Some(position) = self.sidebar_position {
            config.sidebar.position = position.into();
        }
        if let Some(background) = self.sidebar_background {
            config.sidebar.background = Some(background);
        }
        if let Some(background) = self.sidebar_fullscreen_background {
            config.sidebar.fullscreen_background = Some(background);
        }
        if let Some(foreground) = self.sidebar_foreground {
            config.sidebar.foreground = Some(foreground);
        }
        if let Some(selected) = self.sidebar_selected {
            config.sidebar.selected = Some(selected);
        }
        if let Some(hover) = self.sidebar_hover {
            config.sidebar.hover = Some(hover);
        }
        if let Some(border) = self.sidebar_border {
            config.sidebar.border = Some(border);
        }
    }

    fn apply_diagnostics(&self, config: &mut BoottyConfig) {
        if let Some(path) = &self.stability_trace {
            config.diagnostics.stability_trace = Some(path.clone());
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum CliBackend {
    Native,
    Rmux,
    Tmux,
    Zellij,
}

impl From<CliBackend> for MultiplexerBackendConfig {
    fn from(value: CliBackend) -> Self {
        match value {
            CliBackend::Native => Self::Native,
            CliBackend::Rmux => Self::Rmux,
            CliBackend::Tmux => Self::Tmux,
            CliBackend::Zellij => Self::Zellij,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum CliFullscreen {
    Disabled,
    Native,
    NonNative,
    NonNativeVisibleMenu,
    NonNativePaddedNotch,
}

impl From<CliFullscreen> for WindowFullscreen {
    fn from(value: CliFullscreen) -> Self {
        match value {
            CliFullscreen::Disabled => Self::Disabled,
            CliFullscreen::Native => Self::Native,
            CliFullscreen::NonNative => Self::NonNative,
            CliFullscreen::NonNativeVisibleMenu => Self::NonNativeVisibleMenu,
            CliFullscreen::NonNativePaddedNotch => Self::NonNativePaddedNotch,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum CliWindowDecoration {
    None,
    Auto,
    Client,
    Server,
}

impl From<CliWindowDecoration> for WindowDecoration {
    fn from(value: CliWindowDecoration) -> Self {
        match value {
            CliWindowDecoration::None => Self::None,
            CliWindowDecoration::Auto => Self::Auto,
            CliWindowDecoration::Client => Self::Client,
            CliWindowDecoration::Server => Self::Server,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum CliTitlebarStyle {
    Native,
    Transparent,
    Hidden,
}

impl From<CliTitlebarStyle> for MacosTitlebarStyle {
    fn from(value: CliTitlebarStyle) -> Self {
        match value {
            CliTitlebarStyle::Native => Self::Native,
            CliTitlebarStyle::Transparent => Self::Transparent,
            CliTitlebarStyle::Hidden => Self::Hidden,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum CliCursorStyle {
    Bar,
    Block,
    Underline,
    HollowBlock,
}

impl From<CliCursorStyle> for CursorStyleConfig {
    fn from(value: CliCursorStyle) -> Self {
        match value {
            CliCursorStyle::Bar => Self::Bar,
            CliCursorStyle::Block => Self::Block,
            CliCursorStyle::Underline => Self::Underline,
            CliCursorStyle::HollowBlock => Self::HollowBlock,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum CliMacosOptionAsAlt {
    None,
    Left,
    Right,
    Both,
}

impl From<CliMacosOptionAsAlt> for MacosOptionAsAltConfig {
    fn from(value: CliMacosOptionAsAlt) -> Self {
        match value {
            CliMacosOptionAsAlt::None => Self::None,
            CliMacosOptionAsAlt::Left => Self::Left,
            CliMacosOptionAsAlt::Right => Self::Right,
            CliMacosOptionAsAlt::Both => Self::Both,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum CliSidebarPosition {
    Left,
    Right,
}

impl From<CliSidebarPosition> for SidebarPosition {
    fn from(value: CliSidebarPosition) -> Self {
        match value {
            CliSidebarPosition::Left => Self::Left,
            CliSidebarPosition::Right => Self::Right,
        }
    }
}

#[derive(Clone, Debug)]
struct EnvOverride {
    name: String,
    value: String,
}

fn parse_env(input: &str) -> Result<EnvOverride, String> {
    let (name, value) = input
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=VALUE, got {input:?}"))?;
    if name.is_empty() {
        return Err(format!(
            "environment variable name cannot be empty in {input:?}"
        ));
    }
    Ok(EnvOverride {
        name: name.to_owned(),
        value: value.to_owned(),
    })
}

fn parse_color(input: &str) -> Result<Color, String> {
    Color::from_hex(input)
}

fn bool_override(enable: bool, disable: bool) -> Option<bool> {
    if enable {
        Some(true)
    } else if disable {
        Some(false)
    } else {
        None
    }
}

fn isolated_defaults_config_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir()
        .join(format!("bootty-defaults-{}-{nanos}", process::id()))
        .join("config.toml")
}

fn create_parent_dir_for_defaults(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create isolated defaults directory {}",
                parent.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use bootty_config::{
        color::Color,
        config::{
            CursorStyleConfig, MacosOptionAsAltConfig, MacosTitlebarStyle,
            MultiplexerBackendConfig, SidebarPosition, WindowDecoration, WindowFullscreen,
        },
    };
    use clap::{CommandFactory, Parser};
    use indoc::indoc;

    use super::Cli;

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn config_flag_selects_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-config.toml");
        fs::write(&path, "version = 1\n[multiplexer]\nbackend = \"tmux\"\n").unwrap();

        let cli = Cli::try_parse_from(["bootty", "--config", path.to_str().unwrap()]).unwrap();
        let config = cli.load_config().unwrap();

        assert_eq!(config.config_path, path);
        assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Tmux);
    }

    #[test]
    fn explicit_flags_override_loaded_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            indoc! {r#"
                version = 1

                [multiplexer]
                backend = "tmux"
                hide-tmux-status = true

                [window]
                title = "from config"
                width = 900
                height = 500
                fullscreen = false

                [chrome]
                sidebar = false
                status-bar = false
                [font]
                family = ["Config Font"]
                size = 11

                [session]
                shell = "/bin/zsh"
                working-directory = "/tmp/config"
            "#},
        )
        .unwrap();

        let cli = Cli::try_parse_from([
            "bootty",
            "--config",
            config_path.to_str().unwrap(),
            "--backend",
            "rmux",
            "--fullscreen",
            "non-native",
            "--title",
            "from cli",
            "--width",
            "800",
            "--height",
            "600",
            "--sidebar",
            "--status-bar",
            "--font-size",
            "14",
            "--font-family",
            "Mono A,Mono B",
            "--shell",
            "/bin/bash",
            "--working-directory",
            "/tmp/cli",
            "--show-tmux-status",
        ])
        .unwrap();

        let config = cli.load_config().unwrap();

        assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Rmux);
        assert!(!config.multiplexer.hide_tmux_status);
        assert_eq!(config.window.fullscreen, WindowFullscreen::NonNative);
        assert_eq!(config.window.title, "from cli");
        assert_eq!(config.window.width, 800.0);
        assert_eq!(config.window.height, 600.0);
        assert!(config.chrome.sidebar);
        assert!(config.chrome.status_bar);
        assert_eq!(config.font.size, 14.0);
        assert_eq!(config.font.family, ["Mono A", "Mono B"]);
        assert_eq!(config.session.shell.as_deref(), Some("/bin/bash"));
        assert_eq!(
            config.session.working_directory,
            Some(PathBuf::from("/tmp/cli"))
        );
    }

    #[test]
    fn expanded_flags_override_loaded_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "version = 1\n").unwrap();

        let cli = Cli::try_parse_from([
            "bootty",
            "--config",
            config_path.to_str().unwrap(),
            "--titlebar",
            "hidden",
            "--window-decoration",
            "none",
            "--fullscreen-top-offset",
            "22",
            "--no-fullscreen-tabs-in-notch",
            "--background",
            "#010203",
            "--foreground",
            "#040506",
            "--cursor-color",
            "#070809",
            "--cursor-text",
            "#0a0b0c",
            "--selection-background",
            "#111213",
            "--selection-foreground",
            "#141516",
            "--palette",
            "#000000,#ffffff",
            "--no-palette-generate",
            "--palette-harmonious",
            "--font-cell-width",
            "9",
            "--font-cell-height",
            "20",
            "--no-fit-cell-height",
            "--font-baseline-adjustment",
            "1.5",
            "--font-underline-position",
            "2.5",
            "--font-underline-thickness",
            "1.25",
            "--font-feature",
            "+liga,ss01",
            "--cursor-style",
            "hollow-block",
            "--no-cursor-blink",
            "--env",
            "EDITOR=nvim",
            "--env",
            "BOOTTY_TEST=1",
            "--term",
            "xterm-test",
            "--colorterm",
            "24bit",
            "--max-scrollback",
            "1234",
            "--no-glyph-protocol",
            "--macos-option-as-alt",
            "left",
            "--modifier-remap",
            "right_alt=left_ctrl,right_super=left_alt",
            "--sidebar-position",
            "right",
            "--sidebar-width",
            "244",
            "--sidebar-background",
            "#202122",
            "--sidebar-fullscreen-background",
            "#232425",
            "--sidebar-foreground",
            "#262728",
            "--sidebar-selected",
            "#292a2b",
            "--sidebar-hover",
            "#2c2d2e",
            "--sidebar-border",
            "#2f3031",
            "--status-height",
            "28",
            "--chrome-gap",
            "3",
            "--unfocused-sidebar-dim",
            "0.2",
            "--unfocused-terminal-dim",
            "0.3",
            "--stability-trace",
            "/tmp/bootty-trace.csv",
        ])
        .unwrap();

        let config = cli.load_config().unwrap();

        assert_eq!(
            config.window.macos_titlebar_style,
            MacosTitlebarStyle::Hidden
        );
        assert_eq!(config.window.window_decoration, WindowDecoration::None);
        assert_eq!(config.window.fullscreen_top_offset, Some(22.0));
        assert!(!config.window.fullscreen_tabs_in_notch);
        assert_eq!(
            config.colors.background,
            Some(Color::from_hex("#010203").unwrap())
        );
        assert_eq!(
            config.colors.foreground,
            Some(Color::from_hex("#040506").unwrap())
        );
        assert_eq!(
            config.colors.cursor,
            Some(Color::from_hex("#070809").unwrap())
        );
        assert_eq!(
            config.colors.cursor_text,
            Some(Color::from_hex("#0a0b0c").unwrap())
        );
        assert_eq!(
            config.colors.selection_background,
            Some(Color::from_hex("#111213").unwrap())
        );
        assert_eq!(
            config.colors.selection_foreground,
            Some(Color::from_hex("#141516").unwrap())
        );
        assert_eq!(
            config.colors.palette,
            [
                Color::from_hex("#000000").unwrap(),
                Color::from_hex("#ffffff").unwrap()
            ]
        );
        assert!(!config.colors.palette_generate);
        assert!(config.colors.palette_harmonious);
        assert_eq!(config.font.cell_width, Some(9.0));
        assert_eq!(config.font.cell_height, Some(20.0));
        assert!(!config.font.fit_cell_height);
        assert_eq!(config.font.baseline_adjustment, 1.5);
        assert_eq!(config.font.underline_position, 2.5);
        assert_eq!(config.font.underline_thickness, 1.25);
        assert_eq!(config.font.features.len(), 3);
        assert_eq!(config.cursor.style, Some(CursorStyleConfig::HollowBlock));
        assert_eq!(config.cursor.blink, Some(false));
        assert_eq!(
            config.session.env,
            [
                ("EDITOR".to_owned(), "nvim".to_owned()),
                ("BOOTTY_TEST".to_owned(), "1".to_owned())
            ]
        );
        assert_eq!(config.session.term, "xterm-test");
        assert_eq!(config.session.colorterm, "24bit");
        assert_eq!(config.session.max_scrollback, 1234);
        assert!(!config.session.glyph_protocol);
        assert_eq!(
            config.input.macos_option_as_alt,
            MacosOptionAsAltConfig::Left
        );
        assert_eq!(
            config.input.modifier_remap,
            ["right_alt=left_ctrl", "right_super=left_alt"]
        );
        assert_eq!(config.sidebar.position, SidebarPosition::Right);
        assert_eq!(
            config.sidebar.background,
            Some(Color::from_hex("#202122").unwrap())
        );
        assert_eq!(
            config.sidebar.fullscreen_background,
            Some(Color::from_hex("#232425").unwrap())
        );
        assert_eq!(
            config.sidebar.foreground,
            Some(Color::from_hex("#262728").unwrap())
        );
        assert_eq!(
            config.sidebar.selected,
            Some(Color::from_hex("#292a2b").unwrap())
        );
        assert_eq!(
            config.sidebar.hover,
            Some(Color::from_hex("#2c2d2e").unwrap())
        );
        assert_eq!(
            config.sidebar.border,
            Some(Color::from_hex("#2f3031").unwrap())
        );
        assert_eq!(config.chrome.sidebar_width, 244.0);
        assert_eq!(config.chrome.status_height, 28.0);
        assert_eq!(config.chrome.gap, 3.0);
        assert_eq!(config.chrome.unfocused_sidebar_dim, 0.2);
        assert_eq!(config.chrome.unfocused_terminal_dim, 0.3);
        assert_eq!(
            config.diagnostics.stability_trace,
            Some(PathBuf::from("/tmp/bootty-trace.csv"))
        );
    }

    #[test]
    fn theme_flag_resolves_theme_colors_after_config_load() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            indoc! {r##"
                version = 1

                [colors]
                background = "#101112"
            "##},
        )
        .unwrap();

        let cli = Cli::try_parse_from([
            "bootty",
            "--config",
            config_path.to_str().unwrap(),
            "--theme",
            "Catppuccin Mocha",
        ])
        .unwrap();
        let config = cli.load_config().unwrap();

        assert_eq!(config.theme.as_deref(), Some("Catppuccin Mocha"));
        assert_eq!(
            config.colors.background,
            Some(Color::from_hex("#1e1e2e").unwrap())
        );
    }

    #[test]
    fn fullscreen_flag_without_value_uses_native_fullscreen() {
        let cli = Cli::try_parse_from(["bootty", "--fullscreen"]).unwrap();
        let config = cli.load_config().unwrap();

        assert_eq!(config.window.fullscreen, WindowFullscreen::Native);
    }

    #[test]
    fn defaults_mode_uses_temp_config_path_instead_of_xdg_config() {
        let cli = Cli::try_parse_from(["bootty", "--defaults"]).unwrap();
        let config = cli.load_config().unwrap();

        assert!(config.config_path.starts_with(std::env::temp_dir()));
        assert!(config.config_path.ends_with("config.toml"));
        assert_eq!(
            config,
            bootty_config::config::BoottyConfig {
                config_path: config.config_path.clone(),
                ..Default::default()
            }
        );
    }
}
