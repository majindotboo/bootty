use std::sync::Arc;
use std::time::Instant;

use crate::terminal_text::TerminalTextConfig;
use anyhow::Result;
use bootty_config::settings_schema::SettingsSchema;
use bootty_config::{
    ConfigChange, ConfigRuntime, ModifierRemapSet,
    config::{
        AppearanceVariant, BoottyConfig, ConfigDocument, ConfigWriteOutcome,
        MultiplexerBackendConfig, WindowFullscreen,
    },
};

use crate::terminal_config::terminal_text_config;
use crate::{
    app_actions::{AppKeyBindings, SidebarKeyBindings},
    diagnostics::{StabilityTrace, StabilityTraceSample},
    input::{apply_modifier_remap, resolve_modifier_remaps},
};
use bootty_mux::terminal_config::{terminal_cursor_config, terminal_live_config};
use bootty_terminal::terminal_engine::TerminalLiveConfig;
use bootty_terminal::terminal_input_model::KeyMods;

pub struct AcceptedConfigChange {
    pub(super) config: BoottyConfig,
    pub(super) live_config: Option<TerminalLiveConfig>,
    pub(super) text_config: Option<TerminalTextConfig>,
    pub(super) ui_fonts: Option<Vec<String>>,
    pub(super) ui_font_weights: Option<bootty_config::FontWeightAssignments>,
    pub(super) ui_font_size: Option<f32>,
    pub(super) window_title: Option<String>,
    pub(super) window_fullscreen: Option<WindowFullscreen>,
    pub(super) ssh_profiles_changed: bool,
    pub(super) compatibility_warning: Option<String>,
}

#[allow(clippy::float_cmp)]
fn new_session_only_config_changed(previous: &BoottyConfig, next: &BoottyConfig) -> bool {
    previous.session.shell != next.session.shell
        || previous.session.working_directory != next.session.working_directory
        || previous.session.env != next.session.env
        || previous.session.term != next.session.term
        || previous.session.colorterm != next.session.colorterm
        || previous.session.max_scrollback != next.session.max_scrollback
        || previous.restore_on_startup != next.restore_on_startup
        || previous.on_last_window_closed != next.on_last_window_closed
        || previous.window.width != next.window.width
        || previous.window.height != next.window.height
        || previous.window.window_decoration != next.window.window_decoration
        || previous.window.macos_titlebar_style != next.window.macos_titlebar_style
}

pub struct AppConfigRuntime {
    runtime: ConfigRuntime,
    modifier_remaps: ModifierRemapSet,
    has_new_session_config_changes: bool,
    stability_trace: Option<StabilityTrace>,
}

fn validate_keybindings(config: &BoottyConfig) -> Result<()> {
    for backend in [
        MultiplexerBackendConfig::Herdr,
        MultiplexerBackendConfig::Native,
        MultiplexerBackendConfig::Rmux,
        MultiplexerBackendConfig::Tmux,
    ] {
        AppKeyBindings::from_keybinds(&config.input.keybinds_for_backend(backend))?;
    }
    SidebarKeyBindings::from_keybinds(&config.input.sidebar_keybind)?;
    Ok(())
}

impl AppConfigRuntime {
    pub(super) fn new(config: BoottyConfig) -> Result<Self> {
        let runtime = ConfigRuntime::new(config)?;
        let modifier_remaps = resolve_modifier_remaps(&runtime.current().input.modifier_remap)?;
        validate_keybindings(runtime.current())?;
        let stability_trace = StabilityTrace::from_config(runtime.current());
        Ok(Self {
            runtime,
            modifier_remaps,
            has_new_session_config_changes: false,
            stability_trace,
        })
    }

    pub(super) const fn current(&self) -> &BoottyConfig {
        self.runtime.current()
    }

    pub(super) const fn configured_font_size(&self) -> f32 {
        self.runtime.configured_font_size()
    }

    pub(super) const fn revision(&self) -> u64 {
        self.runtime.revision()
    }

    pub(super) fn settings_schema(&self) -> Arc<SettingsSchema> {
        self.runtime.settings_schema()
    }

    pub(super) const fn document(&self) -> &ConfigDocument {
        self.runtime.document()
    }

    pub(super) fn reload(
        &mut self,
        _backend: MultiplexerBackendConfig,
        appearance: AppearanceVariant,
    ) -> Result<AcceptedConfigChange> {
        let (change, modifier_remaps) = self.runtime.reload(|candidate| {
            let modifier_remaps = resolve_modifier_remaps(&candidate.input.modifier_remap)
                .map_err(|error| error.to_string())?;
            validate_keybindings(candidate).map_err(|error| error.to_string())?;
            Ok(modifier_remaps)
        })?;
        self.modifier_remaps = modifier_remaps;
        Ok(self.project_change(&change, appearance))
    }

    pub(super) fn commit_document(
        &mut self,
        document: ConfigDocument,
        _backend: MultiplexerBackendConfig,
        appearance: AppearanceVariant,
    ) -> Result<(AcceptedConfigChange, ConfigDocument, ConfigWriteOutcome)> {
        let (change, document, outcome, modifier_remaps) =
            self.runtime.commit_document(document, |candidate| {
                // App-owned input parsing is part of acceptance, so reject before replacement.
                let modifier_remaps = resolve_modifier_remaps(&candidate.input.modifier_remap)
                    .map_err(|error| error.to_string())?;
                validate_keybindings(candidate).map_err(|error| error.to_string())?;
                Ok(modifier_remaps)
            })?;
        self.modifier_remaps = modifier_remaps;
        Ok((self.project_change(&change, appearance), document, outcome))
    }

    #[expect(
        clippy::float_cmp,
        reason = "Accepted configuration values are compared exactly to detect edits."
    )]
    fn project_change(
        &mut self,
        change: &ConfigChange,
        appearance: AppearanceVariant,
    ) -> AcceptedConfigChange {
        let previous = change.previous();
        let next = change.current();
        let live_config_changed = previous.colors_for_appearance(appearance)
            != next.colors_for_appearance(appearance)
            || terminal_cursor_config(&previous.cursor) != terminal_cursor_config(&next.cursor)
            || previous.session.glyph_protocol != next.session.glyph_protocol;
        let text_config = (previous.font != next.font).then(|| terminal_text_config(&next.font));
        let ui_fonts = (previous.font.ui_families() != next.font.ui_families())
            .then(|| next.font.ui_families().to_vec());
        let ui_font_weights = (previous.font.ui_weights != next.font.ui_weights)
            .then(|| next.font.ui_weights.clone());
        let ui_font_size =
            (previous.font.ui_size != next.font.ui_size).then_some(next.font.ui_size);
        let window_title =
            (previous.window.title != next.window.title).then(|| next.window.title.clone());
        // `fullscreen-enabled` is a launch preference. Editing it must not enter or leave
        // fullscreen in the running window; changing the style does update an active fullscreen.
        let window_fullscreen = (previous.window.fullscreen != next.window.fullscreen)
            .then_some(next.window.fullscreen);
        let ssh_profiles_changed = previous.ssh_profiles != next.ssh_profiles;
        let compatibility_warning = (!next.compatibility_warnings.is_empty())
            .then(|| next.compatibility_warnings.join("; "));
        self.has_new_session_config_changes =
            self.has_new_session_config_changes || new_session_only_config_changed(previous, next);
        if previous.diagnostics != next.diagnostics {
            self.stability_trace = StabilityTrace::from_config(next);
        }
        let live_config = live_config_changed.then(|| terminal_live_config(next, appearance));
        AcceptedConfigChange {
            config: next.clone(),
            live_config,
            text_config,
            ui_fonts,
            ui_font_weights,
            ui_font_size,
            window_title,
            window_fullscreen,
            ssh_profiles_changed,
            compatibility_warning,
        }
    }

    pub(super) fn reload_due(&mut self, now: Instant) -> bool {
        self.runtime.reload_due(now)
    }

    pub(super) const fn has_new_session_config_changes(&self) -> bool {
        self.has_new_session_config_changes
    }

    pub(super) fn record_stability(&mut self, sample: StabilityTraceSample<'_>) {
        if let Some(trace) = &mut self.stability_trace {
            trace.record(sample);
        }
    }

    pub(super) fn remap_mods(&self, mods: KeyMods) -> KeyMods {
        apply_modifier_remap(&self.modifier_remaps, mods)
    }

    pub(super) fn set_sidebar_width(&mut self, width: f32) {
        self.runtime.set_sidebar_width(width);
    }

    pub(super) fn replace_preview_config(&mut self, config: BoottyConfig) {
        self.runtime.replace_preview_config(config);
    }

    pub(super) fn set_font_size(&mut self, size: f32) {
        self.runtime.set_font_size(size);
    }
}
