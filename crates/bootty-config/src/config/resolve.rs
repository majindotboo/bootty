use super::load::{ConfigLoadError, ConfigResult};
use super::model::{
    AppearanceBranchConfig, AppearanceConfig, BackendKeybindConfig, BoottyConfig, ChromeConfig,
    ColorConfig, CursorConfig, DiagnosticsConfig, FontConfig, InputConfig, MultiplexerConfig,
    ResolvedTheme, SessionConfig, SidebarConfig, SshAuthenticationConfig, SshProfileConfig,
    WindowConfig,
};
use super::raw::{
    AppearanceBranchPatch, AppearancePatch, BackendKeybindPatch, ChromePatch, ColorPatch,
    CursorPatch, DiagnosticsPatch, FontPatch, InputPatch, MultiplexerPatch, RawConfig,
    SessionPatch, SidebarPatch, WindowPatch,
};
use super::theme_catalog::{load_builtin_theme, parse_theme_source};
use crate::FontFeature;
use std::{
    fs,
    path::{Path, PathBuf},
};

impl SshProfileConfig {
    fn validate(&self, id: &str) -> ConfigResult<()> {
        if id.trim().is_empty() || self.name.trim().is_empty() || self.host.trim().is_empty() {
            return Err(ConfigLoadError::new(
                "SSH profiles need a stable id, display name, and host",
            ));
        }
        let mode = match self.authentication {
            SshAuthenticationConfig::Auto => return Ok(()),
            SshAuthenticationConfig::Agent => "agent",
            SshAuthenticationConfig::KeyFile => "key-file",
        };
        if self.identity_file.is_none() {
            return Err(ConfigLoadError::new(format!(
                "ssh-profiles.{id}.identity-file is required for {mode} authentication"
            )));
        }
        Ok(())
    }
}
// Both required and optional config values preserve the default when absent.
fn apply_value<T>(target: &mut T, value: Option<impl Into<T>>) {
    if let Some(value) = value {
        *target = value.into();
    }
}

macro_rules! apply_fields {
    ($target:ident, $patch:ident; $($field:ident),+ $(,)?) => {
        $(apply_value(&mut $target.$field, $patch.$field);)+
    };
}

pub(super) struct ConfigResolver<'a> {
    pub(super) path: PathBuf,
    pub(super) config_dir: &'a Path,
}

impl ConfigResolver<'_> {
    pub(super) fn resolve(&self, raw: RawConfig) -> ConfigResult<BoottyConfig> {
        let mut config = BoottyConfig {
            config_path: self.path.clone(),
            ..BoottyConfig::default()
        };
        apply_fields!(config, raw;
            version,
            restore_on_startup,
            cli_default_open_behavior,
            default_open_behavior,
            when_closing_with_no_tabs,
            on_last_window_closed,
            locale,
        );
        config.appearance = resolve_appearance(
            raw.appearance,
            raw.theme.as_deref(),
            raw.colors,
            self.config_dir,
        )?;
        apply_partial_cursor(&mut config.cursor, raw.cursor);
        apply_partial_font(&mut config.font, raw.font)?;
        apply_font_features(&mut config.font, raw.font_feature)?;
        apply_partial_chrome(&mut config.chrome, raw.chrome);
        config.panels = raw.panels;
        apply_partial_sidebar(&mut config.sidebar, raw.sidebar);
        apply_partial_multiplexer(&mut config.multiplexer, raw.multiplexer)?;
        config.ssh_profiles = raw.ssh_profiles;
        config.extensions = raw.extensions;
        for (id, profile) in &config.ssh_profiles {
            profile.validate(id)?;
        }
        apply_partial_input(&mut config.input, raw.input);
        apply_partial_session(&mut config.session, raw.session);
        apply_partial_diagnostics(&mut config.diagnostics, raw.diagnostics);
        apply_partial_window(&mut config.window, raw.window);
        for (name, value, max) in [
            ("background-opacity", config.window.background_opacity, 1.0),
            (
                "background-image-opacity",
                config.window.background_image_opacity,
                1.0,
            ),
            (
                "background-gradient-angle",
                config.window.background_gradient_angle,
                360.0,
            ),
        ] {
            if !value.is_finite() || !(0.0..=max).contains(&value) {
                return Err(ConfigLoadError::new(format!(
                    "window.{name} must be between 0 and {max}"
                )));
            }
        }
        Ok(config)
    }
}

fn apply_partial_window(window: &mut WindowConfig, partial: WindowPatch) {
    apply_fields!(window, partial;
        background_opacity,
        background_image,
        background_image_opacity,
        background_gradient_start,
        background_gradient_end,
        background_gradient_angle,
        background_material,
        title,
        width,
        height,
    );
    // `fullscreen = false` is the legacy spelling for inactive native fullscreen. Normalize it
    // to the valid restore style now that active state is persisted separately.
    let fullscreen = partial.fullscreen.map(|mode| match mode {
        super::model::WindowFullscreen::Disabled => super::model::WindowFullscreen::Native,
        mode => mode,
    });
    let fullscreen_enabled = partial.fullscreen_enabled.or_else(|| {
        partial
            .fullscreen
            .map(|mode| mode != super::model::WindowFullscreen::Disabled)
    });
    apply_value(&mut window.fullscreen, fullscreen);
    apply_value(&mut window.fullscreen_enabled, fullscreen_enabled);
    apply_fields!(window, partial;
        fullscreen_top_offset,
        fullscreen_tabs_in_notch,
        window_decoration,
        macos_titlebar_style,
    );
}

fn apply_partial_font(font: &mut FontConfig, partial: FontPatch) -> ConfigResult<()> {
    apply_fields!(font, partial;
        family,
        style_bold,
        style_italic,
        style_bold_italic,
        ui_family,
        ui_size,
        ui_use_terminal_family,
        size,
        cell_width,
        cell_height,
        fit_cell_height,
        fit_cell_width,
        baseline_adjustment,
        underline_position,
        underline_thickness,
    );
    if let Some(weights) = partial.ui_weights {
        font.ui_weights.extend(weights);
    }
    if let Some(features) = partial.features {
        apply_font_features(font, features)?;
    }
    Ok(())
}

fn apply_font_features(font: &mut FontConfig, features: Vec<String>) -> ConfigResult<()> {
    for feature in features {
        let parsed = FontFeature::parse(&feature)
            .ok_or_else(|| ConfigLoadError::new(format!("invalid font feature: {feature}")))?;
        font.features.push(parsed);
    }
    Ok(())
}

fn apply_partial_chrome(chrome: &mut ChromeConfig, partial: ChromePatch) {
    apply_fields!(chrome, partial;
        left_dock_toggle,
        right_dock_toggle,
        panel_tab_style,
        panel_tabs,
    );
    for (tabs, patch) in [
        (&mut chrome.dock_tabs, partial.dock_tabs),
        (&mut chrome.terminal_tabs, partial.terminal_tabs),
    ] {
        if let Some(patch) = patch {
            apply_fields!(tabs, patch;
                appearance,
                close_position,
                close_button,
            );
        }
    }

    apply_fields!(chrome, partial;
        sidebar,
        top_bar,
        bottom_bar,
        sidebar_width,
        status_height,
        status_background,
        gap,
        pane_divider_width,
        pane_divider_color,
        notched_fullscreen_black_chrome,
        pane_focus_border_width,
        pane_focus_border_color,
        pane_corner_radius,
        unfocused_sidebar_dim,
        unfocused_terminal_dim,
    );
    if let Some(segments) = partial.top_segment {
        chrome.top_segments = segments;
    }
    if let Some(segments) = partial.bottom_segment {
        chrome.bottom_segments = segments;
    }
}

fn apply_partial_sidebar(sidebar: &mut SidebarConfig, partial: SidebarPatch) {
    apply_fields!(sidebar, partial;
        position,
        background,
        foreground,
        selected,
        hover,
        border,
    );
    // An empty module list keeps the defaults. A sidebar with no modules has no session list at
    // all, and a session list with no components is a row of bare names — neither is a state anyone
    // configures on purpose, and the editor refuses to write one. Reaching it means a file was
    // damaged, so repair it rather than rendering an unusable sidebar.
    // Limit: an explicit "show nothing" needs its own affordance if anyone ever wants it.
    if let Some(modules) = partial
        .session_modules
        .filter(|modules| !modules.is_empty())
    {
        sidebar.session_modules = modules;
        sidebar.session_modules_configured = true;
    }
    apply_value(
        &mut sidebar.modules,
        partial.modules.filter(|modules| !modules.is_empty()),
    );
}

fn apply_partial_multiplexer(
    multiplexer: &mut MultiplexerConfig,
    partial: MultiplexerPatch,
) -> ConfigResult<()> {
    apply_fields!(multiplexer, partial;
        backend,
        hide_tmux_status,
        remote,
    );
    multiplexer
        .validate_remote()
        .map_err(|error| ConfigLoadError::new(error.to_string()))
}

fn apply_partial_input(input: &mut InputConfig, partial: InputPatch) {
    apply_fields!(input, partial;
        modifier_remap,
        macos_option_as_alt,
        hide_mouse_pointer_while_typing,
        copy_on_select,
        preset,
        prefix,
    );
    // Preset and prefix select which built-in default arrays the user's keybind rows layer
    // onto, so the defaults must be rebuilt before the merges below.
    input.reset_default_keybinds();
    if let Some(value) = partial.keybind {
        input.keybind = merge_keybind_entries(&input.keybind, value);
    }
    if let Some(value) = partial.sidebar_keybind {
        input.sidebar_keybind = merge_keybind_entries(&input.sidebar_keybind, value);
    }
    if let Some(value) = partial.backend_keybind {
        apply_partial_backend_keybind(&mut input.backend_keybinds, value);
    }
}

fn apply_partial_backend_keybind(
    keybinds: &mut BackendKeybindConfig,
    partial: BackendKeybindPatch,
) {
    if let Some(value) = partial.herdr {
        keybinds.herdr = merge_keybind_entries(&keybinds.herdr, value);
    }
    if let Some(value) = partial.native {
        keybinds.native = merge_keybind_entries(&keybinds.native, value);
    }
    if let Some(value) = partial.rmux {
        keybinds.rmux = merge_keybind_entries(&keybinds.rmux, value);
    }
    if let Some(value) = partial.tmux {
        keybinds.tmux = merge_keybind_entries(&keybinds.tmux, value);
    }
}

// User keybinds layer on top of the defaults so new default bindings reach existing configs;
// later entries override earlier ones for the same trigger. A "clear" entry opts out of the
// defaults entirely, keeping only the user's bindings. Use keymap.json's named unbinds to suppress
// individual defaults.
fn merge_keybind_entries(defaults: &[String], entries: Vec<String>) -> Vec<String> {
    if entries.iter().any(|entry| entry == "clear") {
        return entries
            .into_iter()
            .filter(|entry| entry != "clear")
            .collect();
    }
    let mut merged = defaults.to_vec();
    merged.extend(entries);
    merged
}

fn apply_partial_session(session: &mut SessionConfig, partial: SessionPatch) {
    apply_fields!(session, partial;
        output_archives,
        clipboard_write_hosts,
        bell,
        agent_notifications,
        command_notifications,
        command_notification_min_seconds,
        shell_integration,
        shell,
        working_directory,
    );
    if let Some(value) = partial.env {
        session.env = value
            .into_iter()
            .map(|entry| (entry.name, entry.value))
            .collect();
    }
    apply_fields!(session, partial;
        term,
        colorterm,
        max_scrollback,
        scrollbar,
        glyph_protocol,
    );
}

fn apply_partial_diagnostics(diagnostics: &mut DiagnosticsConfig, partial: DiagnosticsPatch) {
    apply_value(&mut diagnostics.stability_trace, partial.stability_trace);
}

pub(super) fn apply_partial_colors(colors: &mut ColorConfig, partial: ColorPatch) {
    apply_fields!(colors, partial;
        background,
        foreground,
        cursor,
        cursor_text,
        pointer_foreground,
        pointer_background,
        tektronix_foreground,
        tektronix_background,
        highlight_background,
        tektronix_cursor,
        highlight_foreground,
        selection_background,
        selection_foreground,
        palette,
        palette_generate,
        palette_harmonious,
    );
}

fn apply_partial_cursor(cursor: &mut CursorConfig, partial: CursorPatch) {
    apply_fields!(cursor, partial;
        style,
        blink,
        dim_inactive_pane,
    );
}

fn resolve_appearance(
    partial: AppearancePatch,
    legacy_theme: Option<&str>,
    legacy_colors: ColorPatch,
    config_dir: &Path,
) -> ConfigResult<AppearanceConfig> {
    let mut appearance = AppearanceConfig::default();
    if legacy_theme.is_some() || legacy_colors != ColorPatch::default() {
        appearance.apply_global_override(legacy_theme, config_dir, |colors| {
            apply_partial_colors(colors, legacy_colors);
        })?;
    }
    apply_value(&mut appearance.mode, partial.mode);
    apply_appearance_branch(&mut appearance.light, partial.light, config_dir)?;
    apply_appearance_branch(&mut appearance.dark, partial.dark, config_dir)?;
    Ok(appearance)
}

fn apply_appearance_branch(
    branch: &mut AppearanceBranchConfig,
    partial: AppearanceBranchPatch,
    config_dir: &Path,
) -> ConfigResult<()> {
    if let Some(theme) = partial.theme {
        branch.colors = resolve_theme_colors(&theme, config_dir)?;
        branch.theme_colors = branch.colors.clone();
        branch.theme = Some(theme);
    }
    apply_partial_colors(&mut branch.colors, partial.colors);
    Ok(())
}

impl AppearanceConfig {
    /// Apply one process-wide override to both appearance branches. The theme resolves first so
    /// explicit color overrides take precedence, matching legacy top-level config semantics.
    ///
    /// # Errors
    /// Returns a theme loading or parsing error before either appearance branch changes.
    pub fn apply_global_override(
        &mut self,
        theme: Option<&str>,
        config_dir: &Path,
        override_colors: impl FnOnce(&mut ColorConfig),
    ) -> ConfigResult<()> {
        let mut branch = self.dark.clone();
        if let Some(theme) = theme {
            branch.theme = Some(theme.to_owned());
            branch.colors = resolve_theme_colors(theme, config_dir)?;
            branch.theme_colors = branch.colors.clone();
        }
        override_colors(&mut branch.colors);
        self.light = branch.clone();
        self.dark = branch;
        Ok(())
    }
}

fn resolve_theme_colors(theme: &str, config_dir: &Path) -> ConfigResult<ColorConfig> {
    resolve_theme(theme, config_dir).map(|theme| theme.colors)
}

///
/// # Errors
/// Returns an error when the theme cannot be found, read, or parsed.
pub fn resolve_theme(theme: &str, config_dir: &Path) -> ConfigResult<ResolvedTheme> {
    if let Some(theme) = load_user_theme(theme, config_dir)? {
        return Ok(theme);
    }
    load_builtin_theme(theme).ok_or_else(|| {
        ConfigLoadError::new(format!(
            "theme {theme:?} not found in {} or built-in catalog",
            config_dir.join("themes").display()
        ))
    })
}

fn load_user_theme(theme: &str, config_dir: &Path) -> ConfigResult<Option<ResolvedTheme>> {
    for path in user_theme_candidates(theme, config_dir) {
        if !path.exists() {
            continue;
        }
        let source = fs::read_to_string(&path).map_err(|error| {
            ConfigLoadError::new(format!(
                "failed to read theme file {}: {error}",
                path.display()
            ))
        })?;
        return parse_theme_source(&source, &path.display().to_string()).map(Some);
    }
    Ok(None)
}

/// Every selectable theme from the built-in catalog and `themes/*.toml` beside the config.
///
/// Ordered case-insensitively with case-duplicates collapsed, so a user copy of a
/// built-in theme replaces it in the list instead of appearing twice.
pub fn available_theme_names(config_path: &Path) -> Vec<String> {
    let mut names: Vec<String> = super::theme_catalog::builtin_theme_names()
        .map(str::to_owned)
        .collect();
    if let Some(config_dir) = config_path.parent()
        && let Ok(entries) = fs::read_dir(config_dir.join("themes"))
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "toml")
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                names.push(stem.to_owned());
            }
        }
    }
    names.sort_unstable_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
}

fn user_theme_candidates(theme: &str, config_dir: &Path) -> [PathBuf; 2] {
    let theme_dir = config_dir.join("themes");
    [
        theme_dir.join(theme),
        theme_dir.join(format!("{theme}.toml")),
    ]
}
