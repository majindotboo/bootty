//! Settings draft editing and config-derived native controls.

use crate::{
    ansi_palette::{standard_16_palette, xterm_256_palette},
    gpui::{
        AnsiPalettePreset, EnvironmentVariable, FontFeatureEditorSnapshot, FontFeaturePreset,
        GpuiSettings, ModifierRemap, ModifierRemapField, ModuleIntegrationsSnapshot,
        ModuleSourceIntent, RemoteEditorSnapshot, RemoteProfileFieldSnapshot, RemoteProfileOption,
        RemoteProfileSnapshot, RemoteTestIntent, RemoteTestState, ScalarValue, SettingsCategory,
        SettingsChoice, SettingsContent, SettingsControl, SettingsIntent, SettingsListItem,
        SettingsRow, StatusSegmentAlignment, StatusSegmentColor, StatusSegmentEditorRow,
        StatusSegmentIntent, StatusSegmentsSnapshot,
    },
    gpui_settings_catalog::{
        UnsupportedModuleDiagnostic, advanced_configuration_rows,
        setting_is_visible_in_native_settings, settings_catalog_pages, settings_category_for,
        settings_page, settings_row_order as catalog_row_order,
        settings_section as catalog_section, unsupported_module_rows,
    },
    settings_session::{
        AcceptedSettings, Catalogs, DefaultRemote, FontFeatureDraft, RemoteDraft, RemoteProfile,
        SettingsSession, StatusSegmentEdit, normalize_number, parse_display_number,
    },
    state::AppState,
};
use bootty_config::{
    color::Color,
    config::{
        BoottyConfig, DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, SegmentAlign,
        SshAuthenticationConfig, SshHostKeyPolicyConfig, available_theme_names,
    },
    settings_schema::{NumberControl, SettingEditor, SettingKind, SettingSpec, SettingValue},
};
use gpui_kit::{Context, Window};
use std::sync::Arc;

impl GpuiSettings {
    pub(crate) fn for_app(
        state: &AppState,
        font_families: Arc<[String]>,
        unsupported: &[UnsupportedModuleDiagnostic],
        integrations: &[ModuleIntegrationsSnapshot],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = SettingsSession::new(
            accepted_settings(state),
            settings_catalogs(state, font_families),
        );
        let content = settings_content(&draft, state, unsupported, integrations);
        Self::new_with_window(content, draft, window, cx)
    }

    pub(crate) fn reconcile(
        &mut self,
        state: &AppState,
        unsupported: &[UnsupportedModuleDiagnostic],
        integrations: &[ModuleIntegrationsSnapshot],
        cx: &mut Context<Self>,
    ) -> bool {
        self.draft.reconcile_accepted(accepted_settings(state));
        self.draft.set_catalogs(settings_catalogs(
            state,
            Arc::clone(self.draft.font_families()),
        ));
        let content = settings_content(&self.draft, state, unsupported, integrations);
        self.set_content(content, cx)
    }

    pub(crate) fn apply_edit(&mut self, intent: SettingsIntent, state: &AppState) {
        match intent {
            SettingsIntent::Close | SettingsIntent::Apply => {}
            SettingsIntent::SetValue {
                id,
                value: ScalarValue::Number(value),
            } => {
                let schema = state.settings_schema();
                let Some(spec) = schema.get(&id) else {
                    return;
                };
                if let Some(value) = normalize_setting_number(spec, value) {
                    self.draft.set_value(&id, &SettingValue::Number(value));
                }
            }
            SettingsIntent::SetValue { id, value } => self.set_scalar_value(&id, &value, state),
            SettingsIntent::RemoveValue(id) => {
                if state.settings_schema().get(&id).is_some() {
                    self.draft.remove_value(&id);
                } else {
                    self.draft.remove_custom_value(&id);
                }
            }
            SettingsIntent::SetText { id, value } => self.set_setting_text(&id, value, state),
            SettingsIntent::SetAnsiPaletteColor { id, index, value } => {
                self.set_ansi_color(&id, index, &value);
            }
            SettingsIntent::ReplaceAnsiPalette { id, colors } => {
                self.replace_ansi_palette(&id, colors);
            }
            SettingsIntent::EditStatusSegments { id, edit } => {
                self.edit_status_segments(&id, edit);
            }
            SettingsIntent::SetStringListItem { id, index, value } => {
                let mut values = self.setting_string_list(&id, state);
                if let Some(item) = values.get_mut(index) {
                    *item = value;
                    self.draft.set_string_list(&id, &values);
                }
            }
            SettingsIntent::AddStringListItem(id) => {
                let mut values = self.setting_string_list(&id, state);
                let added = self.new_string_list_item(&id);
                values.push(added);
                self.draft.set_string_list(&id, &values);
            }
            SettingsIntent::RemoveStringListItem { id, index } => {
                self.remove_string_list_item(&id, index, state);
            }
            SettingsIntent::MoveStringListItem { id, index, offset } => {
                self.move_string_list_item(&id, index, offset, state);
            }
            SettingsIntent::SetModifierRemap {
                index,
                field,
                value,
            } => self.set_modifier_remap(index, field, &value, state),
            SettingsIntent::AddModifierRemap => {
                let mut values = self.setting_string_list("input.modifier-remap", state);
                values.push("right_alt=left_ctrl".to_owned());
                self.draft.set_string_list("input.modifier-remap", &values);
            }
            SettingsIntent::RemoveModifierRemap(index) => {
                self.remove_string_list_item("input.modifier-remap", index, state);
            }
            SettingsIntent::MoveModifierRemap { index, offset } => {
                self.move_string_list_item("input.modifier-remap", index, offset, state);
            }
            SettingsIntent::SetEnvironmentName { index, value } => {
                self.draft.set_environment_name(index, value);
            }
            SettingsIntent::SetEnvironmentValue { index, value } => {
                self.draft.set_environment_value(index, value);
            }
            SettingsIntent::AddEnvironmentVariable => {
                self.draft.add_environment_variable();
            }
            SettingsIntent::RemoveEnvironmentVariable(index) => {
                self.draft.remove_environment_variable(index);
            }
            SettingsIntent::MoveEnvironmentVariable { index, offset } => {
                self.draft.move_environment_variable(index, offset);
            }
            SettingsIntent::ReplaceFontFeatures(features) => {
                self.draft.set_font_features(features);
            }
            SettingsIntent::SetRemoteField {
                profile_id,
                field_id,
                value,
            } => self.set_remote_field(&profile_id, &field_id, value),
            SettingsIntent::Module(intent) => self.apply_module_intent(intent),
            SettingsIntent::TestRemote(intent) => {
                self.draft
                    .test_remote_with_fields(&intent.profile_id, intent.fields);
            }
            SettingsIntent::Invoke(id) => self.invoke_remote_editor(&id, state),
        }
    }

    fn set_scalar_value(&mut self, id: &str, value: &ScalarValue, state: &AppState) {
        match (id, value) {
            (_, ScalarValue::Token(token))
                if state
                    .settings_schema()
                    .get(id)
                    .is_some_and(|spec| matches!(spec.kind, SettingKind::FontStyle)) =>
            {
                match token.as_str() {
                    "auto" => {
                        self.draft.remove_value(id);
                    }
                    "disabled" => {
                        self.draft.set_value(id, &SettingValue::Bool(false));
                    }
                    _ => {
                        if let Some(name) = token.strip_prefix("name:") {
                            self.draft
                                .set_value(id, &SettingValue::Text(name.to_owned()));
                        }
                    }
                }
            }
            ("multiplexer.backend", ScalarValue::Token(backend)) => {
                self.draft.set_multiplexer_backend(backend);
            }
            ("cursor.style", ScalarValue::Token(token)) if token == "default" => {
                self.draft.remove_value(id);
            }
            _ => {
                if state.settings_schema().get(id).is_some() {
                    self.draft.set_value(id, value);
                } else {
                    self.draft.set_custom_value(id, value);
                }
            }
        }
    }

    fn set_ansi_color(&mut self, id: &str, index: usize, value: &str) {
        let Ok(color) = Color::from_hex(value) else {
            self.draft
                .reject("Color must contain 6 or 8 hexadecimal digits.");
            return;
        };
        let Some(mut colors) = self.draft.ansi_palette(id) else {
            return;
        };
        let Some(slot) = colors.get_mut(index) else {
            return;
        };
        *slot = color_hex(color);
        self.draft.set_ansi_palette(id, &colors);
    }

    fn replace_ansi_palette(&mut self, id: &str, colors: Vec<String>) {
        if colors.len() > 256 {
            self.draft
                .reject("ANSI palettes contain at most 256 colors.");
            return;
        }
        let colors = colors
            .into_iter()
            .map(|value| Color::from_hex(&value).map(color_hex))
            .collect::<std::result::Result<Vec<_>, _>>();
        match colors {
            Ok(colors) => {
                self.draft.set_ansi_palette(id, &colors);
            }
            Err(_) => self
                .draft
                .reject("Color must contain 6 or 8 hexadecimal digits."),
        }
    }

    fn set_modifier_remap(
        &mut self,
        index: usize,
        field: ModifierRemapField,
        value: &str,
        state: &AppState,
    ) {
        if !modifier_remap_token_is_valid(value) {
            self.draft.reject("Choose a supported modifier.");
            return;
        }
        let mut values = self.setting_string_list("input.modifier-remap", state);
        let Some(entry) = values.get_mut(index) else {
            return;
        };
        let Some((source, target)) = entry.split_once('=') else {
            self.draft
                .reject("Modifier remap must contain a source and target.");
            return;
        };
        *entry = match field {
            ModifierRemapField::Source => format!("{value}={target}"),
            ModifierRemapField::Target => format!("{source}={value}"),
        };
        self.draft.set_string_list("input.modifier-remap", &values);
    }

    fn set_remote_field(&mut self, profile_id: &str, field_id: &str, value: String) {
        if profile_id == "default" {
            if let Some(index) = remote_argument_index(field_id) {
                self.draft.edit_remote_argument(profile_id, index, value);
            } else {
                self.draft.edit_default_remote(field_id, value);
            }
            return;
        }
        if let Some(index) = remote_argument_index(field_id) {
            self.draft.edit_remote_argument(profile_id, index, value);
            return;
        }
        let Some(mut draft) = self
            .draft
            .remotes()
            .draft
            .filter(|draft| draft.id == profile_id)
        else {
            return;
        };
        let Some(field) = remote_draft_field(&mut draft, field_id) else {
            return;
        };
        *field = value;
        self.draft.edit_remote(draft);
    }

    fn invoke_remote_editor(&mut self, id: &str, state: &AppState) {
        if id == "remote:new" {
            if let Some(id) = Self::next_remote_profile_id(state) {
                self.draft.new_remote(id);
            } else {
                self.draft.reject("Remote profile limit reached.");
            }
        } else if let Some(id) = id.strip_prefix("remote:select:") {
            self.draft.select_remote(id);
        } else if id == "remote:save" {
            self.draft.save_remote();
        } else if id == "remote:save-default" {
            self.draft.save_default_remote();
        } else if let Some(profile) = id.strip_prefix("remote:add-arg:") {
            self.draft.add_remote_argument(profile);
        } else if let Some((profile, index)) = id
            .strip_prefix("remote:remove-arg:")
            .and_then(|value| value.rsplit_once(':'))
            .and_then(|(profile, index)| Some((profile, index.parse().ok()?)))
        {
            self.draft.remove_remote_argument(profile, index);
        } else if let Some(id) = id.strip_prefix("remote:delete:") {
            self.draft.remove_remote(id.to_owned());
        } else if id == "remote:clear-default" {
            self.draft.clear_default_remote();
        }
    }

    fn remove_string_list_item(&mut self, id: &str, index: usize, state: &AppState) {
        let mut values = self.setting_string_list(id, state);
        if index < values.len() {
            values.remove(index);
            self.draft.set_string_list(id, &values);
        }
    }

    fn move_string_list_item(&mut self, id: &str, index: usize, offset: isize, state: &AppState) {
        let mut values = self.setting_string_list(id, state);
        let Some(target) = index.checked_add_signed(offset) else {
            return;
        };
        if index < values.len() && target < values.len() && index != target {
            let value = values.remove(index);
            values.insert(target, value);
            self.draft.set_string_list(id, &values);
        }
    }

    fn setting_string_list(&self, id: &str, state: &AppState) -> Vec<String> {
        self.draft.string_list(id).unwrap_or_else(|| match id {
            "font.family" => state.config().font.family.clone(),
            "font.ui-family" => state.config().font.ui_family.clone(),
            "input.modifier-remap" => state.config().input.modifier_remap.clone(),
            "sidebar.session-modules" => state.config().sidebar.session_modules.clone(),
            "sidebar.modules" => state.config().sidebar.modules.clone(),
            _ => Vec::new(),
        })
    }

    fn new_string_list_item(&self, id: &str) -> String {
        match id {
            "input.modifier-remap" => "right_alt=left_ctrl".to_owned(),
            "sidebar.session-modules" | "sidebar.modules" => "module".to_owned(),
            _ => self
                .draft
                .font_families()
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| "monospace".to_owned()),
        }
    }

    fn edit_status_segments(&mut self, id: &str, edit: StatusSegmentIntent) {
        let top = match id {
            "chrome.top-segment" => true,
            "chrome.bottom-segment" => false,
            _ => {
                self.draft.reject(format!("Unknown status bar {id:?}."));
                return;
            }
        };
        let edit = match edit {
            StatusSegmentIntent::Add { module } => StatusSegmentEdit::Add { module },
            StatusSegmentIntent::Remove { index } => StatusSegmentEdit::Remove { index },
            StatusSegmentIntent::Move { index, offset } => {
                StatusSegmentEdit::Move { index, offset }
            }
            StatusSegmentIntent::SetModule { index, module } => {
                StatusSegmentEdit::SetModule { index, module }
            }
            StatusSegmentIntent::SetAlignment { index, alignment } => {
                StatusSegmentEdit::SetAlignment {
                    index,
                    alignment: match alignment {
                        StatusSegmentAlignment::Left => SegmentAlign::Left,
                        StatusSegmentAlignment::Center => SegmentAlign::Center,
                        StatusSegmentAlignment::Right => SegmentAlign::Right,
                    },
                }
            }
            StatusSegmentIntent::SetColor {
                index,
                field,
                value,
            } => {
                let color = match value {
                    Some(value) => {
                        if let Ok(color) = Color::from_hex(&value) {
                            Some(color)
                        } else {
                            self.draft
                                .reject("Color must contain 6 or 8 hexadecimal digits.");
                            return;
                        }
                    }
                    None => None,
                };
                match field {
                    StatusSegmentColor::Foreground => {
                        StatusSegmentEdit::SetForeground { index, color }
                    }
                    StatusSegmentColor::Background => {
                        StatusSegmentEdit::SetBackground { index, color }
                    }
                }
            }
            StatusSegmentIntent::SetIcon { index, icon } => {
                StatusSegmentEdit::SetIcon { index, icon }
            }
        };
        self.draft.edit_status_segments(top, edit);
    }

    fn set_setting_text(&mut self, id: &str, text: String, state: &AppState) {
        if is_scalar_color_setting(id) {
            match Color::from_hex(&text) {
                Ok(color) => {
                    self.draft
                        .set_custom_value(id, &SettingValue::Text(color_hex(color)));
                }
                Err(_) => self
                    .draft
                    .reject("Color must contain 6 or 8 hexadecimal digits."),
            }
            return;
        }
        let schema = state.settings_schema();
        let Some(spec) = schema.get(id) else {
            return;
        };
        match &spec.kind {
            SettingKind::Text { optional: true, .. } if text.is_empty() => {
                self.draft.remove_value(id);
            }
            SettingKind::Text { .. } => {
                self.draft.set_value(id, &SettingValue::Text(text));
            }
            SettingKind::Number { .. } => {
                if let Some(value) = parse_setting_number(spec, &text) {
                    self.draft.set_value(id, &SettingValue::Number(value));
                }
            }
            SettingKind::Choice { .. } => {
                self.draft.set_value(id, &SettingValue::Token(text));
            }
            SettingKind::FontStyle => {
                if text.is_empty() || text == "auto" {
                    self.draft.remove_value(id);
                } else {
                    self.draft.set_value(id, &SettingValue::Text(text));
                }
            }
            SettingKind::Custom(_) if id == "input.prefix" => {
                if text.is_empty() {
                    self.draft.remove_value(id);
                } else {
                    self.draft.set_value(id, &SettingValue::Text(text));
                }
            }
            SettingKind::Custom(_) if id == "session.max-scrollback" => {
                match text.trim().parse::<usize>() {
                    Ok(lines) => {
                        self.draft.set_custom_i64(
                            id,
                            crate::presentation::scrollback::bytes_from_lines(lines),
                        );
                    }
                    _ => self
                        .draft
                        .reject("Scrollback limit must be a whole number of lines."),
                }
            }
            SettingKind::Custom(_)
                if matches!(
                    id,
                    "font.cell-width" | "font.cell-height" | "window.fullscreen-top-offset"
                ) =>
            {
                if text.is_empty() {
                    self.draft.remove_value(id);
                } else if let Some(value) = parse_setting_number(spec, &text) {
                    self.draft.set_value(id, &SettingValue::Number(value));
                }
            }
            SettingKind::Bool | SettingKind::Custom(_) => {}
        }
    }

    fn next_remote_profile_id(state: &AppState) -> Option<String> {
        let configured = &state.config().ssh_profiles;
        (1..=configured.len().saturating_add(1))
            .map(|index| format!("remote-{index}"))
            .find(|id| !configured.contains_key(id))
    }

    fn apply_module_intent(&mut self, intent: ModuleSourceIntent) {
        match intent {
            ModuleSourceIntent::InstallIntegration {
                identity,
                module,
                id,
            } => self.draft.install_integration(identity, module, id),
            ModuleSourceIntent::UninstallIntegration {
                identity,
                module,
                id,
            } => self.draft.uninstall_integration(identity, module, id),
        }
    }
}

fn accepted_settings(state: &AppState) -> AcceptedSettings {
    AcceptedSettings {
        revision: state.config_revision(),
        config: Arc::new(state.config().clone()),
        document: state.config_document(),
        schema: state.settings_schema(),
    }
}

fn settings_catalogs(state: &AppState, font_families: Arc<[String]>) -> Catalogs {
    let status_modules = ["session", "windows", "sysinfo", "clock"]
        .map(str::to_owned)
        .to_vec();
    let remotes = state
        .config()
        .ssh_profiles
        .iter()
        .map(|(id, profile)| RemoteProfile {
            id: id.clone(),
            name: profile.name.clone(),
            host: profile.host.clone(),
            user: profile.user.clone(),
            port: profile.port,
            authentication: authentication_token(profile.authentication).to_owned(),
            host_key_policy: host_key_policy_token(profile.host_key_policy).to_owned(),
            identity_file: profile.identity_file.clone(),
            proxy_jump: profile.proxy_jump.clone(),
            program: profile.program.clone(),
            args: profile.args.clone(),
        })
        .collect::<Vec<_>>();
    let default_remote = state
        .config()
        .multiplexer
        .remote
        .as_ref()
        .and_then(bootty_config::config::RemoteConfig::as_ssh)
        .map(|remote| RemoteProfile {
            id: "default".to_owned(),
            name: "Default remote".to_owned(),
            host: remote.host.clone(),
            user: remote.user.clone(),
            port: remote.port,
            authentication: "auto".to_owned(),
            host_key_policy: "strict".to_owned(),
            identity_file: None,
            proxy_jump: None,
            program: remote.program.clone(),
            args: remote.args.clone(),
        });
    Catalogs {
        font_families,
        status_modules,
        top_status_segments: state.config().chrome.top_segments.clone(),
        bottom_status_segments: state.config().chrome.bottom_segments.clone(),
        environment: state.config().session.env.clone(),
        remotes,
        default_remote: DefaultRemote::from_remote(default_remote.as_ref()),
    }
}

fn settings_content(
    session: &SettingsSession,
    state: &AppState,
    unsupported_sources: &[UnsupportedModuleDiagnostic],
    integration_rows: &[ModuleIntegrationsSnapshot],
) -> SettingsContent {
    let defaults = BoottyConfig::default();
    let schema = state.settings_schema();
    let mut pages: Vec<_> =
        settings_catalog_pages()
            .iter()
            .map(|category| {
                let mut rows = Vec::new();
                let mut section = if category.category == SettingsCategory::Advanced {
                    rows.extend(advanced_configuration_rows(
                        &state.config().config_path,
                        session.write_error(),
                    ));
                    Some("STATE".to_owned())
                } else { None };

                let mut specs = schema
                    .specs()
                    .iter()
                    .filter(|spec| {
                        setting_is_visible_in_native_settings(&spec.id())
                            && settings_category_for(&spec.id(), spec.page.as_ref())
                                == category.category
                    })
                    .collect::<Vec<_>>();
                specs.sort_by_key(|spec| catalog_row_order(category.category, &spec.id()));
                for spec in specs {
                    let spec_rows = match &spec.kind {
                        SettingKind::FontStyle => font_style_row(spec, state.config(), session, &defaults).into_iter().collect(),
                        SettingKind::Custom(editor) => custom_setting_rows(
                            spec,
                            *editor,
                            state.config(),
                            session,
                            session.font_families(),
                        ),
                        kind => {
                            let (label, help) = setting_copy(spec);
                            let value = if spec.id() == "window.fullscreen-enabled" {
                                Some(SettingValue::Bool(state.config().window.fullscreen_enabled))
                            } else {
                                session
                                    .value(&spec.id())
                                    .or_else(|| spec.default_value(&defaults))
                            };
                            let Some(value) = value else { continue };
                            vec![SettingsRow::Value {
                                id: spec.id(),
                                label,
                                help,
                                value,
                                control: settings_control(kind),
                                enabled: true,
                            }]
                        }
                    };
                    if spec_rows.is_empty() {
                        continue;
                    }
                    let display_section =
                        catalog_section(category.category, &spec.id(), spec.section.as_ref());
                    if section.as_deref() != Some(display_section) {
                        section = Some(display_section.to_owned());
                        rows.push(SettingsRow::Section(display_section.to_owned()));
                    }
                    rows.extend(spec_rows);
                }

                if category.category == SettingsCategory::Keymap {
                    rows.push(SettingsRow::Section("KEYMAP EDITOR".to_owned()));
                    rows.push(SettingsRow::Action {
                    id: "keymap:open".to_owned(),
                    label: "Keymap".to_owned(),
                    help:
                        "Search every command and edit keybindings in the dedicated keymap editor."
                            .to_owned(),
                    button: "Open Keymap".to_owned(),
                    enabled: true,
                });
                } else if category.category == SettingsCategory::Remotes {
                    rows = remote_settings_rows(session, state);
                } else if category.category == SettingsCategory::Advanced {
                    rows.push(SettingsRow::Section("AGENT INTEGRATIONS".to_owned()));
                    rows.extend(integration_rows.iter().cloned().map(|mut row| {
                        row.error = session.integration_error(&row.identity).map(str::to_owned);
                        SettingsRow::ModuleIntegrations(row)
                    }));
                    rows.push(SettingsRow::Section("CUSTOM SCRIPTS".to_owned()));
                    rows.push(SettingsRow::Notice {
                        text: "Custom Lua and Luau scripts are no longer executed. Existing source files are preserved.".to_owned(),
                        destructive: false,
                    });
                    rows.extend(unsupported_module_rows(unsupported_sources));
                }
                settings_page(category, rows)
            })
            .collect();
    crate::i18n::localize_settings(&mut pages, &state.localizer);
    SettingsContent {
        pages,
        write_error: session.write_error().map(str::to_owned),
    }
}

fn remote_settings_rows(session: &SettingsSession, state: &AppState) -> Vec<SettingsRow> {
    let remotes = session.remotes();
    let mut rows = Vec::new();
    rows.push(SettingsRow::Section("DEFAULT REMOTE".to_owned()));
    if let Some(bootty_config::config::RemoteConfig::Wsl(remote)) =
        &state.config().multiplexer.remote
    {
        rows.push(SettingsRow::Action {
            id: "remote:clear-default".to_owned(),
            label: format!("WSL: {}", remote.distribution.as_str()),
            help: "Default Linux host. Per-Space host selection can override it.".to_owned(),
            button: "Clear default remote".to_owned(),
            enabled: true,
        });
    } else {
        rows.push(SettingsRow::Remote(default_remote_snapshot(
            &remotes.default,
        )));
    }
    rows.push(SettingsRow::Section("SSH PROFILES".to_owned()));
    rows.push(SettingsRow::Action {
        id: "remote:new".to_owned(),
        label: "SSH profiles".to_owned(),
        help: "Saved SSH connections for remote Spaces.".to_owned(),
        button: "Add remote".to_owned(),
        enabled: true,
    });
    rows.extend(remotes.profiles.iter().map(|profile| {
        let selected = remotes.selected.as_deref() == Some(&profile.id);
        let test_state = if selected && remotes.testing.is_some() {
            RemoteTestState::Testing
        } else if selected {
            match &remotes.message {
                Some(Ok(())) => RemoteTestState::Passed,
                Some(Err(error)) => RemoteTestState::Failed(error.clone()),
                None => RemoteTestState::Idle,
            }
        } else {
            RemoteTestState::Idle
        };
        SettingsRow::Remote(remote_snapshot(
            profile,
            selected.then_some(remotes.draft.as_ref()).flatten(),
            test_state,
            selected,
        ))
    }));
    if remotes.selected.is_none()
        && let Some(draft) = remotes.draft.as_ref()
    {
        rows.push(SettingsRow::Remote(new_remote_snapshot(draft)));
    }
    rows
}

fn font_stack_row(
    spec: &SettingSpec,
    items: Vec<String>,
    options: &[String],
    enabled: bool,
    add_label: &str,
) -> SettingsRow {
    let (label, help) = match spec.id().as_str() {
        "font.family" => (
            "Terminal font stack",
            "Bootty tries the primary font first, followed by each fallback in order.",
        ),
        "font.ui-family" => (
            "UI font stack",
            "Fonts used by settings, sidebar, status, and other application chrome.",
        ),
        _ => ("Values", "Ordered values."),
    };
    SettingsRow::StringList {
        id: spec.id(),
        label: label.to_owned(),
        help: help.to_owned(),
        items,
        options: options.to_vec(),
        add_label: add_label.to_owned(),
        enabled,
    }
}

fn font_feature_options() -> Vec<FontFeaturePreset> {
    [
        (*b"liga", 1),
        (*b"liga", 0),
        (*b"calt", 1),
        (*b"calt", 0),
        (*b"dlig", 1),
        (*b"dlig", 0),
        (*b"kern", 1),
        (*b"kern", 0),
        (*b"zero", 1),
        (*b"tnum", 1),
        (*b"onum", 1),
        (*b"ss01", 1),
        (*b"ss02", 1),
    ]
    .into_iter()
    .map(|(tag, value)| {
        let feature = bootty_config::FontFeature::new(tag, value);
        FontFeaturePreset {
            label: feature.to_string(),
            feature: feature.into(),
        }
    })
    .collect()
}

fn optional_metric_row(
    spec: &SettingSpec,
    value: Option<f32>,
    range: std::ops::RangeInclusive<f32>,
) -> SettingsRow {
    let is_width = spec.id() == "font.cell-width";
    SettingsRow::Value {
        id: spec.id(),
        label: if is_width {
            "Cell width"
        } else {
            "Cell height"
        }
        .to_owned(),
        help: if is_width {
            "Override automatic glyph cell width. Auto follows the selected font."
        } else {
            "Override automatic line height. Auto follows the selected font size."
        }
        .to_owned(),
        value: ScalarValue::Number(value.unwrap_or(if is_width {
            bootty_terminal::geometry::DEFAULT_CELL_WIDTH
        } else {
            bootty_terminal::geometry::DEFAULT_LINE_HEIGHT
        })),
        control: SettingsControl::Number {
            range,
            control: NumberControl::Slider,
            precision: 2,
            suffix: "px".to_owned(),
            display_scale: 1.0,
            optional: true,
        },
        enabled: true,
    }
}

fn font_style_row(
    spec: &SettingSpec,
    config: &BoottyConfig,
    session: &SettingsSession,
    defaults: &BoottyConfig,
) -> Option<SettingsRow> {
    let id = spec.id();
    let families = if id.starts_with("font.ui-weights.") {
        config.font.ui_families()
    } else {
        &config.font.family
    };
    let value = session
        .value(&id)
        .or_else(|| spec.default_value(defaults))?;
    let selected = match &value {
        SettingValue::Bool(false) => "disabled".to_owned(),
        SettingValue::Text(name) | SettingValue::Token(name) if name != "auto" => {
            format!("name:{name}")
        }
        _ => "auto".to_owned(),
    };
    let mut choices = vec![
        SettingsChoice {
            token: "auto".to_owned(),
            label: "Automatic".to_owned(),
            description: None,
        },
        SettingsChoice {
            token: "disabled".to_owned(),
            label: "Use base style".to_owned(),
            description: None,
        },
    ];
    let styles = families.first().map_or_else(Vec::new, |family| {
        crate::font_database::font_style_names(family)
    });
    choices.extend(styles.into_iter().map(|name| SettingsChoice {
        token: format!("name:{name}"),
        label: name,
        description: None,
    }));
    if let Some(name) = selected.strip_prefix("name:")
        && !choices.iter().any(|choice| choice.token == selected)
    {
        choices.push(SettingsChoice {
            token: selected.clone(),
            label: format!("{name} (unavailable)"),
            description: None,
        });
    }
    let (label, help) = setting_copy(spec);
    Some(SettingsRow::Value {
        id,
        label,
        help,
        value: ScalarValue::Token(selected),
        control: SettingsControl::Choice(choices),
        enabled: true,
    })
}

fn custom_setting_rows(
    spec: &SettingSpec,
    editor: SettingEditor,
    config: &BoottyConfig,
    session: &SettingsSession,
    font_families: &[String],
) -> Vec<SettingsRow> {
    let id = spec.id();
    // The Remotes page is one editor: its default target and profile lifecycle
    // must be projected together so every field follows the same typed command
    // path. Do not emit one read-only row per schema leaf.
    if editor == SettingEditor::Remotes {
        return Vec::new();
    }
    if let Some(row) = editable_custom_setting(spec, config) {
        return vec![row];
    }
    if let Some(row) = structured_custom_setting(spec, config, session) {
        return vec![row];
    }
    match (editor, id.as_str()) {
        (SettingEditor::Colors, _) => custom_color_rows(spec, config, session),
        (_, "font.family" | "font.ui-family") => {
            let terminal = id == "font.family";
            let families = if terminal {
                &config.font.family
            } else {
                &config.font.ui_family
            };
            vec![font_stack_row(
                spec,
                session.string_list(&id).unwrap_or_else(|| families.clone()),
                font_families,
                terminal || !config.font.ui_use_terminal_family,
                if terminal {
                    "Add terminal fallback"
                } else {
                    "Add UI fallback"
                },
            )]
        }
        (_, "font.cell-width") => vec![optional_metric_row(
            spec,
            config.font.cell_width,
            1.0..=64.0,
        )],
        (_, "font.cell-height") => vec![optional_metric_row(
            spec,
            config.font.cell_height,
            1.0..=128.0,
        )],
        (_, "extensions.*") => vec![read_only_setting(
            spec,
            format!("{} configured module(s)", config.extensions.len()),
        )],
        _ => Vec::new(),
    }
}

fn editable_custom_setting(spec: &SettingSpec, config: &BoottyConfig) -> Option<SettingsRow> {
    let id = spec.id();
    let (value, control) = match id.as_str() {
        "cursor.style" => cursor_style_control(config),
        "chrome.notched-fullscreen-black-chrome" => (
            ScalarValue::Bool(config.chrome.notched_fullscreen_black_chrome),
            SettingsControl::Toggle,
        ),
        "input.hide-mouse-pointer-while-typing" => (
            ScalarValue::Bool(config.input.hide_mouse_pointer_while_typing),
            SettingsControl::Toggle,
        ),
        "appearance.mode" => (
            ScalarValue::Token(
                match config.appearance.mode {
                    bootty_config::config::AppearanceMode::System => "system",
                    bootty_config::config::AppearanceMode::Light => "light",
                    bootty_config::config::AppearanceMode::Dark => "dark",
                }
                .to_owned(),
            ),
            choice_control(&[("system", "System"), ("light", "Light"), ("dark", "Dark")]),
        ),
        "font.ui-use-terminal-family" => (
            ScalarValue::Bool(config.font.ui_use_terminal_family),
            SettingsControl::Toggle,
        ),
        "multiplexer.backend" => (
            ScalarValue::Token(config.multiplexer.backend.to_string().to_ascii_lowercase()),
            choice_control(&[
                ("native", "Native"),
                ("herdr", "Herdr"),
                ("rmux", "rmux"),
                ("tmux", "tmux"),
            ]),
        ),
        "chrome.top-bar" => (
            ScalarValue::Bool(config.chrome.top_bar),
            SettingsControl::Toggle,
        ),
        "input.macos-option-as-alt" => (
            ScalarValue::Token(
                match config.input.macos_option_as_alt {
                    bootty_config::config::MacosOptionAsAltConfig::None => "none",
                    bootty_config::config::MacosOptionAsAltConfig::Left => "left",
                    bootty_config::config::MacosOptionAsAltConfig::Right => "right",
                    bootty_config::config::MacosOptionAsAltConfig::Both => "both",
                }
                .to_owned(),
            ),
            choice_control(&[
                ("none", "None"),
                ("left", "Left"),
                ("right", "Right"),
                ("both", "Both"),
            ]),
        ),
        "input.copy-on-select" => (
            ScalarValue::Bool(config.input.copy_on_select),
            SettingsControl::Toggle,
        ),
        "input.preset" => (
            ScalarValue::Token(config.input.preset.as_str().to_owned()),
            choice_control(&[
                ("ghostty", "Ghostty"),
                ("bootty", "Bootty"),
                ("tmux", "Tmux"),
            ]),
        ),
        "input.prefix" => (
            ScalarValue::Text(config.input.prefix.clone().unwrap_or_default()),
            SettingsControl::Text {
                placeholder: "Preset default".to_owned(),
                optional: true,
            },
        ),
        "session.max-scrollback" => (
            ScalarValue::Text(
                crate::presentation::scrollback::lines_from_bytes(config.session.max_scrollback)
                    .to_string(),
            ),
            SettingsControl::Text {
                placeholder: "1000000".to_owned(),
                optional: false,
            },
        ),
        "window.fullscreen-top-offset" => fullscreen_offset_control(config),
        _ => return None,
    };
    let (label, help) = setting_copy(spec);
    Some(SettingsRow::Value {
        id,
        label,
        help,
        value,
        control,
        enabled: true,
    })
}

fn fullscreen_offset_control(config: &BoottyConfig) -> (ScalarValue, SettingsControl) {
    (
        ScalarValue::Number(config.window.fullscreen_top_offset.unwrap_or(0.0)),
        SettingsControl::Number {
            range: 0.0..=160.0,
            control: NumberControl::Edit,
            precision: 0,
            suffix: "px".to_owned(),
            display_scale: 1.0,
            optional: true,
        },
    )
}

fn cursor_style_control(config: &BoottyConfig) -> (ScalarValue, SettingsControl) {
    (
        ScalarValue::Token(config.cursor.style.map_or_else(
            || "default".to_owned(),
            |style| match style {
                bootty_config::config::CursorStyleConfig::Bar => "bar".to_owned(),
                bootty_config::config::CursorStyleConfig::Block => "block".to_owned(),
                bootty_config::config::CursorStyleConfig::Underline => "underline".to_owned(),
                bootty_config::config::CursorStyleConfig::HollowBlock => "hollow-block".to_owned(),
            },
        )),
        choice_control(&[
            ("default", "Default"),
            ("bar", "Bar"),
            ("block", "Block"),
            ("underline", "Underline"),
            ("hollow-block", "Hollow block"),
        ]),
    )
}

fn structured_custom_setting(
    spec: &SettingSpec,
    config: &BoottyConfig,
    session: &SettingsSession,
) -> Option<SettingsRow> {
    if spec.id() == "font.features" {
        return Some(SettingsRow::FontFeatures {
            id: spec.id(),
            label: "Font features".to_owned(),
            help: "Ordered OpenType feature tags and numeric values.".to_owned(),
            editor: FontFeatureEditorSnapshot {
                features: session.font_features().unwrap_or_else(|| {
                    config
                        .font
                        .features
                        .iter()
                        .copied()
                        .map(FontFeatureDraft::from)
                        .collect()
                }),
                presets: font_feature_options(),
                enabled: true,
            },
        });
    }
    if spec.id() == "session.env" {
        return Some(SettingsRow::Environment {
            id: spec.id(),
            label: "Environment variables".to_owned(),
            help: "Variables added to every new terminal session.".to_owned(),
            items: session
                .environment()
                .iter()
                .map(|entry| EnvironmentVariable {
                    name: entry.name.clone(),
                    value: entry.value.clone(),
                })
                .collect(),
            enabled: true,
        });
    }
    if matches!(
        spec.id().as_str(),
        "chrome.top-segment" | "chrome.bottom-segment"
    ) {
        return Some(status_segments_setting(spec, session));
    }
    let (label, help, items, add_label) = match spec.id().as_str() {
        "sidebar.session-modules" => (
            "Session details",
            "Modules shown for the selected session. Add a module name, then reorder it here.",
            config.sidebar.session_modules.clone(),
            "Add session module",
        ),
        "sidebar.modules" => (
            "Sidebar footer",
            "Modules shown at the bottom of the sidebar. Add a module name, then reorder it here.",
            config.sidebar.modules.clone(),
            "Add sidebar module",
        ),
        "input.modifier-remap" => {
            let mappings = config
                .input
                .modifier_remap
                .iter()
                .filter_map(|entry| entry.split_once('='))
                .map(|(source, target)| ModifierRemap {
                    source: source.to_owned(),
                    target: target.to_owned(),
                })
                .collect();
            return Some(SettingsRow::ModifierRemaps {
                id: spec.id(),
                label: "Modifier remapping".to_owned(),
                help: "Remap physical modifiers before Bootty resolves shortcuts.".to_owned(),
                mappings,
                choices: modifier_remap_choices(),
                enabled: true,
            });
        }
        _ => return None,
    };
    Some(SettingsRow::StringList {
        id: spec.id(),
        label: label.to_owned(),
        help: help.to_owned(),
        items,
        options: Vec::new(),
        add_label: add_label.to_owned(),
        enabled: true,
    })
}

const MODIFIER_REMAP_CHOICES: &[(&str, &str)] = &[
    ("ctrl", "Control"),
    ("alt", "Alt"),
    ("shift", "Shift"),
    ("super", "Command / Super"),
    ("left_ctrl", "Left Control"),
    ("left_alt", "Left Alt"),
    ("left_shift", "Left Shift"),
    ("left_super", "Left Command / Super"),
    ("right_ctrl", "Right Control"),
    ("right_alt", "Right Alt"),
    ("right_shift", "Right Shift"),
    ("right_super", "Right Command / Super"),
];

fn modifier_remap_token_is_valid(value: &str) -> bool {
    MODIFIER_REMAP_CHOICES
        .iter()
        .any(|(token, _)| *token == value)
}

fn modifier_remap_choices() -> Vec<SettingsChoice> {
    MODIFIER_REMAP_CHOICES
        .iter()
        .map(|(token, label)| SettingsChoice {
            token: (*token).to_owned(),
            label: (*label).to_owned(),
            description: None,
        })
        .collect()
}

fn status_segments_setting(spec: &SettingSpec, session: &SettingsSession) -> SettingsRow {
    let top = spec.id() == "chrome.top-segment";
    let segments = session.status_segments(top);
    let mut modules = session.status_modules().to_vec();
    modules.extend(segments.iter().map(|segment| segment.module.clone()));
    modules.sort_unstable_by_key(|module| module.to_ascii_lowercase());
    modules.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    let (label, help) = setting_copy(spec);
    SettingsRow::StatusSegments(StatusSegmentsSnapshot {
        id: spec.id(),
        label,
        help,
        modules: modules
            .into_iter()
            .map(|module| SettingsChoice {
                label: title_case(&module),
                token: module,
                description: None,
            })
            .collect(),
        segments: segments
            .iter()
            .map(|segment| StatusSegmentEditorRow {
                module: segment.module.clone(),
                alignment: match segment.align {
                    SegmentAlign::Left => StatusSegmentAlignment::Left,
                    SegmentAlign::Center => StatusSegmentAlignment::Center,
                    SegmentAlign::Right => StatusSegmentAlignment::Right,
                },
                foreground: segment.fg.map(color_hex),
                background: segment.bg.map(color_hex),
                icon: segment.icon.clone(),
            })
            .collect(),
        add_label: if top {
            "Add top module".to_owned()
        } else {
            "Add bottom module".to_owned()
        },
    })
}

fn choice_control(options: &[(&str, &str)]) -> SettingsControl {
    SettingsControl::Choice(
        options
            .iter()
            .map(|(token, label)| SettingsChoice {
                token: (*token).to_owned(),
                label: (*label).to_owned(),
                description: None,
            })
            .collect(),
    )
}

fn setting_label(spec: &SettingSpec) -> String {
    spec.path
        .iter()
        .rev()
        .find(|part| part.as_ref() != "*")
        .map_or_else(|| spec.id(), |part| title_case(part))
}

fn setting_copy(spec: &SettingSpec) -> (String, String) {
    let label = if spec.label.is_empty() {
        setting_label(spec)
    } else {
        spec.label.to_string()
    };
    let help = if spec.help.is_empty() {
        "Configured in Bootty settings.".to_owned()
    } else {
        spec.help.to_string()
    };
    (label, help)
}

fn custom_color_rows(
    spec: &SettingSpec,
    config: &BoottyConfig,
    session: &SettingsSession,
) -> Vec<SettingsRow> {
    match spec.id().as_str() {
        // The old combined summary made the theme setting look read-only and hid the actual
        // branch controls. Zed presents one compact picker per appearance branch instead.
        "appearance.light.theme" => theme_choice_row(
            spec,
            "Light theme",
            config
                .appearance
                .light
                .theme
                .as_deref()
                .unwrap_or(DEFAULT_LIGHT_THEME),
            config,
        ),
        "appearance.dark.theme" => theme_choice_row(
            spec,
            "Dark theme",
            config
                .appearance
                .dark
                .theme
                .as_deref()
                .unwrap_or(DEFAULT_DARK_THEME),
            config,
        ),
        "appearance.light.colors.*" => color_config_rows(
            "appearance.light.colors",
            spec,
            &config.appearance.light.colors,
            session,
        ),
        "appearance.dark.colors.*" => color_config_rows(
            "appearance.dark.colors",
            spec,
            &config.appearance.dark.colors,
            session,
        ),
        "chrome.status-background" => editable_color_row(
            spec,
            "Status bar background",
            "Status strip background; unset uses the sidebar background default.",
            config.chrome.status_background,
        ),
        "chrome.pane-divider-color" => editable_color_row(
            spec,
            "Pane divider",
            "Color of the gap between split panes; unset uses the window background.",
            config.chrome.pane_divider_color,
        ),
        "chrome.pane-focus-border-color" => editable_color_row(
            spec,
            "Pane focus border",
            "Border around the focused split pane; unset uses the theme accent.",
            config.chrome.pane_focus_border_color,
        ),
        "sidebar.background" => editable_color_row(
            spec,
            "Sidebar background",
            "Sidebar background override; unset uses the active theme.",
            config.sidebar.background,
        ),
        "sidebar.foreground" => editable_color_row(
            spec,
            "Sidebar foreground",
            "Sidebar text override; unset uses the active theme.",
            config.sidebar.foreground,
        ),
        "sidebar.selected" => editable_color_row(
            spec,
            "Sidebar selected",
            "Selected sidebar item background override.",
            config.sidebar.selected,
        ),
        "sidebar.hover" => editable_color_row(
            spec,
            "Sidebar hover",
            "Hovered sidebar item background override.",
            config.sidebar.hover,
        ),
        "sidebar.border" => editable_color_row(
            spec,
            "Sidebar border",
            "Sidebar border override; unset uses the active theme.",
            config.sidebar.border,
        ),
        _ => Vec::new(),
    }
}

fn is_scalar_color_setting(id: &str) -> bool {
    // Palette arrays need indexed editing and are intentionally outside this scalar picker seam.
    let parts = id.split('.').collect::<Vec<_>>();
    match parts.as_slice() {
        ["colors", leaf] | ["appearance", "light" | "dark", "colors", leaf] => {
            is_color_config_leaf(leaf)
        }
        [
            "chrome",
            "status-background" | "pane-divider-color" | "pane-focus-border-color",
        ]
        | [
            "sidebar",
            "background" | "foreground" | "selected" | "hover" | "border",
        ] => true,
        _ => false,
    }
}

fn is_color_config_leaf(leaf: &str) -> bool {
    matches!(
        leaf,
        "background"
            | "foreground"
            | "cursor"
            | "cursor-text"
            | "pointer-foreground"
            | "pointer-background"
            | "tektronix-foreground"
            | "tektronix-background"
            | "highlight-background"
            | "tektronix-cursor"
            | "highlight-foreground"
            | "selection-background"
            | "selection-foreground"
    )
}

fn theme_choice_row(
    spec: &SettingSpec,
    label: &str,
    current: &str,
    config: &BoottyConfig,
) -> Vec<SettingsRow> {
    let mut options = available_theme_names(&config.config_path)
        .into_iter()
        .map(|theme| SettingsChoice {
            label: theme.clone(),
            token: theme,
            description: None,
        })
        .collect::<Vec<_>>();
    if !options
        .iter()
        .any(|option| option.token.eq_ignore_ascii_case(current))
    {
        options.push(SettingsChoice {
            label: current.to_owned(),
            token: current.to_owned(),
            description: Some("Current configured theme".to_owned()),
        });
    }
    vec![SettingsRow::Value {
        id: spec.id(),
        label: label.to_owned(),
        help: "Search the built-in and user theme catalog.".to_owned(),
        value: ScalarValue::Token(current.to_owned()),
        control: SettingsControl::Theme(options),
        enabled: true,
    }]
}

fn editable_color_row(
    spec: &SettingSpec,
    label: &str,
    help: &str,
    value: Option<Color>,
) -> Vec<SettingsRow> {
    vec![SettingsRow::Value {
        id: spec.id(),
        label: label.to_owned(),
        help: help.to_owned(),
        value: ScalarValue::Text(value.map_or_else(String::new, color_hex)),
        control: SettingsControl::Color,
        enabled: true,
    }]
}

fn color_config_rows(
    prefix: &str,
    _spec: &SettingSpec,
    colors: &bootty_config::config::ColorConfig,
    session: &SettingsSession,
) -> Vec<SettingsRow> {
    [
        ("background", "Background", colors.background),
        ("foreground", "Foreground", colors.foreground),
        ("cursor", "Cursor", colors.cursor),
        ("cursor-text", "Cursor text", colors.cursor_text),
        (
            "selection-background",
            "Selection background",
            colors.selection_background,
        ),
        (
            "selection-foreground",
            "Selection foreground",
            colors.selection_foreground,
        ),
        (
            "highlight-background",
            "Highlight background",
            colors.highlight_background,
        ),
        (
            "highlight-foreground",
            "Highlight foreground",
            colors.highlight_foreground,
        ),
        (
            "pointer-foreground",
            "Pointer foreground",
            colors.pointer_foreground,
        ),
        (
            "pointer-background",
            "Pointer background",
            colors.pointer_background,
        ),
        (
            "tektronix-foreground",
            "Tektronix foreground",
            colors.tektronix_foreground,
        ),
        (
            "tektronix-background",
            "Tektronix background",
            colors.tektronix_background,
        ),
        (
            "tektronix-cursor",
            "Tektronix cursor",
            colors.tektronix_cursor,
        ),
    ]
    .into_iter()
    .map(|(name, label, color)| SettingsRow::Value {
        id: format!("{prefix}.{name}"),
        // The branch selector above the page already supplies the Light/Dark context. Repeating
        // it on every row turns the page into a wall of redundant labels.
        label: label.to_owned(),
        help: "Uses the selected theme when unset.".to_owned(),
        value: ScalarValue::Text(color.map_or_else(String::new, color_hex)),
        control: SettingsControl::Color,
        enabled: true,
    })
    .chain(ansi_palette_rows(prefix, colors, session))
    .collect()
}

fn ansi_palette_rows(
    prefix: &str,
    colors: &bootty_config::config::ColorConfig,
    session: &SettingsSession,
) -> [SettingsRow; 3] {
    let branch_label = match prefix {
        "appearance.light.colors" => "Light",
        "appearance.dark.colors" => "Dark",
        _ => "",
    };
    let palette_id = format!("{prefix}.palette");
    let palette_values = session.ansi_palette(&palette_id).unwrap_or_default();
    let palette_overrides = palette_values
        .iter()
        .filter_map(|value| Color::from_hex(value).ok())
        .collect::<Vec<_>>();
    let standard_16 = standard_16_palette(&palette_overrides, colors)
        .into_iter()
        .map(color_hex)
        .collect();
    let xterm_256 = xterm_256_palette(&palette_overrides, colors)
        .into_iter()
        .map(color_hex)
        .collect();
    [
        SettingsRow::AnsiPalette {
            id: palette_id,
            label: format!("{branch_label} ANSI palette"),
            help: "Override indexed terminal colors or start from the active standard 16 / xterm 256 palette.".to_owned(),
            colors: palette_values,
            presets: vec![
                AnsiPalettePreset {
                    label: "Use standard 16".to_owned(),
                    colors: standard_16,
                },
                AnsiPalettePreset {
                    label: "Use xterm 256".to_owned(),
                    colors: xterm_256,
                },
            ],
        },
        SettingsRow::Value {
            id: format!("{prefix}.palette-generate"),
            label: "Generate 256-color cube".to_owned(),
            help: "Generate the indexed ANSI palette from the theme colors.".to_owned(),
            value: ScalarValue::Bool(colors.palette_generate),
            control: SettingsControl::Toggle,
            enabled: true,
        },
        SettingsRow::Value {
            id: format!("{prefix}.palette-harmonious"),
            label: "Harmonious palette".to_owned(),
            help: "Blend generated colors toward a harmonious theme palette.".to_owned(),
            value: ScalarValue::Bool(colors.palette_harmonious),
            control: SettingsControl::Toggle,
            enabled: true,
        },
    ]
}

fn read_only_setting(spec: &SettingSpec, value: String) -> SettingsRow {
    let (label, help) = setting_copy(spec);
    SettingsRow::Value {
        id: spec.id(),
        label,
        help,
        value: ScalarValue::Text(if value.is_empty() {
            "None".to_owned()
        } else {
            value
        }),
        control: SettingsControl::ReadOnly,
        enabled: true,
    }
}

fn color_hex(color: Color) -> String {
    if color.a == u8::MAX {
        format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            color.r, color.g, color.b, color.a
        )
    }
}

fn title_case(value: &str) -> String {
    let mut title = value.replace('-', " ");
    if let Some(first) = title.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    title
}

fn normalize_setting_number(spec: &SettingSpec, value: f32) -> Option<f32> {
    let (range, _) = setting_number_constraints(spec)?;
    normalize_number(value, &range)
}

fn parse_setting_number(spec: &SettingSpec, text: &str) -> Option<f32> {
    let (range, display_scale) = setting_number_constraints(spec)?;
    parse_display_number(text, &range, display_scale)
}

fn setting_number_constraints(spec: &SettingSpec) -> Option<(std::ops::RangeInclusive<f32>, f32)> {
    match &spec.kind {
        SettingKind::Number {
            range,
            display_scale,
            ..
        } => Some((range.clone(), *display_scale)),
        SettingKind::Custom(_) if spec.id() == "font.cell-width" => Some((1.0..=64.0, 1.0)),
        SettingKind::Custom(_) if spec.id() == "font.cell-height" => Some((1.0..=128.0, 1.0)),
        SettingKind::Custom(_) if spec.id() == "window.fullscreen-top-offset" => {
            Some((0.0..=160.0, 1.0))
        }
        _ => None,
    }
}

fn settings_control(kind: &SettingKind) -> SettingsControl {
    match kind {
        SettingKind::Bool => SettingsControl::Toggle,
        SettingKind::Text {
            placeholder,
            optional,
        } => SettingsControl::Text {
            placeholder: placeholder.to_string(),
            optional: *optional,
        },
        SettingKind::Number {
            range,
            control,
            precision,
            suffix,
            display_scale,
        } => SettingsControl::Number {
            range: range.clone(),
            control: *control,
            precision: *precision,
            suffix: suffix.to_string(),
            display_scale: *display_scale,
            optional: false,
        },
        SettingKind::Choice { options } => SettingsControl::Choice(
            options
                .iter()
                .map(|option| SettingsChoice {
                    token: option.token.to_string(),
                    label: option.label.to_string(),
                    description: option.description.as_ref().map(ToString::to_string),
                })
                .collect(),
        ),
        SettingKind::FontStyle | SettingKind::Custom(_) => SettingsControl::ReadOnly,
    }
}

fn remote_draft_field<'a>(draft: &'a mut RemoteDraft, field: &str) -> Option<&'a mut String> {
    match field {
        "name" => Some(&mut draft.name),
        "host" => Some(&mut draft.host),
        "user" => Some(&mut draft.user),
        "port" => Some(&mut draft.port),
        "authentication" => Some(&mut draft.authentication),
        "host-key-policy" => Some(&mut draft.host_key_policy),
        "identity-file" => Some(&mut draft.identity_file),
        "proxy-jump" => Some(&mut draft.proxy_jump),
        "program" => Some(&mut draft.program),
        _ => None,
    }
}

fn remote_argument_index(field: &str) -> Option<usize> {
    field.strip_prefix("args.")?.parse().ok()
}

fn remote_snapshot(
    profile: &RemoteProfile,
    draft: Option<&RemoteDraft>,
    test_state: RemoteTestState,
    selected: bool,
) -> RemoteEditorSnapshot {
    let fields = remote_profile_fields(profile, draft);
    RemoteEditorSnapshot {
        id: profile.id.clone(),
        label: draft.map_or_else(|| profile.name.clone(), |draft| draft.name.clone()),
        detail: format!(
            "{}{}",
            draft.map_or_else(
                || profile
                    .user
                    .as_deref()
                    .map_or(String::new(), |user| format!("{user}@")),
                |draft| {
                    if draft.user.is_empty() {
                        String::new()
                    } else {
                        format!("{}@", draft.user)
                    }
                },
            ),
            draft.map_or(profile.host.as_str(), |draft| draft.host.as_str())
        ),
        error: draft.and_then(|draft| draft.error.clone()),
        profile: selected.then(|| RemoteProfileSnapshot {
            id: profile.id.clone(),
            arguments: draft.map_or_else(|| profile.args.clone(), |draft| draft.args.clone()),
            test: Some(RemoteTestIntent {
                profile_id: profile.id.clone(),
                fields: fields
                    .iter()
                    .map(|field| (field.id.clone(), field.value.clone()))
                    .collect(),
            }),
            fields,
        }),
        test_state,
        actions: if selected {
            vec![
                SettingsListItem {
                    id: "remote:save".to_owned(),
                    label: "Save".to_owned(),
                    detail: None,
                    selected: false,
                },
                SettingsListItem {
                    id: format!("remote:delete:{}", profile.id),
                    label: "Delete".to_owned(),
                    detail: None,
                    selected: false,
                },
            ]
        } else {
            vec![SettingsListItem {
                id: format!("remote:select:{}", profile.id),
                label: "Edit".to_owned(),
                detail: None,
                selected: false,
            }]
        },
    }
}

fn remote_profile_fields(
    profile: &RemoteProfile,
    draft: Option<&RemoteDraft>,
) -> Vec<RemoteProfileFieldSnapshot> {
    let authentication = vec![
        remote_option("auto", "Automatic"),
        remote_option("agent", "SSH agent"),
        remote_option("key-file", "Key file"),
    ];
    let host_key_policy = vec![
        remote_option("strict", "Strict"),
        remote_option("accept-new", "Accept new"),
    ];
    vec![
        remote_field(
            "name",
            "Name",
            draft.map_or(profile.name.as_str(), |draft| draft.name.as_str()),
            Vec::new(),
        ),
        remote_field(
            "host",
            "Host",
            draft.map_or(profile.host.as_str(), |draft| draft.host.as_str()),
            Vec::new(),
        ),
        remote_field(
            "user",
            "User",
            draft.map_or_else(
                || profile.user.as_deref().unwrap_or(""),
                |draft| draft.user.as_str(),
            ),
            Vec::new(),
        ),
        remote_field(
            "port",
            "Port",
            &draft.map_or_else(
                || {
                    profile
                        .port
                        .map(|port| port.to_string())
                        .unwrap_or_default()
                },
                |draft| draft.port.clone(),
            ),
            Vec::new(),
        ),
        remote_field(
            "authentication",
            "Authentication",
            draft.map_or(profile.authentication.as_str(), |draft| {
                draft.authentication.as_str()
            }),
            authentication,
        ),
        remote_field(
            "host-key-policy",
            "Host key policy",
            draft.map_or(profile.host_key_policy.as_str(), |draft| {
                draft.host_key_policy.as_str()
            }),
            host_key_policy,
        ),
        remote_field(
            "identity-file",
            "Identity file",
            &draft.map_or_else(
                || {
                    profile
                        .identity_file
                        .as_ref()
                        .map_or_else(String::new, |path| path.display().to_string())
                },
                |draft| draft.identity_file.clone(),
            ),
            Vec::new(),
        ),
        remote_field(
            "proxy-jump",
            "Proxy / jump host",
            draft.map_or_else(
                || profile.proxy_jump.as_deref().unwrap_or(""),
                |draft| draft.proxy_jump.as_str(),
            ),
            Vec::new(),
        ),
        remote_field(
            "program",
            "SSH client",
            draft.map_or(profile.program.as_str(), |draft| draft.program.as_str()),
            Vec::new(),
        ),
    ]
}

fn new_remote_snapshot(draft: &RemoteDraft) -> RemoteEditorSnapshot {
    let profile = RemoteProfile {
        id: draft.id.clone(),
        ..RemoteProfile::default()
    };
    remote_snapshot(&profile, Some(draft), RemoteTestState::Idle, true)
}

fn default_remote_snapshot(remote: &DefaultRemote) -> RemoteEditorSnapshot {
    let fields = vec![
        remote_field("host", "Host", &remote.host, Vec::new()),
        remote_field("user", "User", &remote.user, Vec::new()),
        remote_field("port", "Port", &remote.port, Vec::new()),
        remote_field("program", "SSH client", &remote.program, Vec::new()),
    ];
    RemoteEditorSnapshot {
        id: "default-remote".to_owned(),
        label: "Default remote".to_owned(),
        detail: "Used by new remote Spaces when no per-Space override is selected.".to_owned(),
        error: remote.error.clone(),
        profile: Some(RemoteProfileSnapshot {
            id: "default".to_owned(),
            fields,
            arguments: remote.args.clone(),
            test: None,
        }),
        test_state: RemoteTestState::Idle,
        actions: std::iter::once(SettingsListItem {
            id: "remote:save-default".to_owned(),
            label: "Save default remote".to_owned(),
            detail: None,
            selected: false,
        })
        .chain((!remote.host.is_empty()).then(|| SettingsListItem {
            id: "remote:clear-default".to_owned(),
            label: "Clear default remote".to_owned(),
            detail: None,
            selected: false,
        }))
        .collect(),
    }
}

fn remote_field(
    id: &str,
    label: &str,
    value: &str,
    options: Vec<RemoteProfileOption>,
) -> RemoteProfileFieldSnapshot {
    RemoteProfileFieldSnapshot {
        id: id.to_owned(),
        label: label.to_owned(),
        value: value.to_owned(),
        options,
    }
}

fn remote_option(id: &str, label: &str) -> RemoteProfileOption {
    RemoteProfileOption {
        id: id.to_owned(),
        label: label.to_owned(),
    }
}

const fn authentication_token(value: SshAuthenticationConfig) -> &'static str {
    match value {
        SshAuthenticationConfig::Auto => "auto",
        SshAuthenticationConfig::Agent => "agent",
        SshAuthenticationConfig::KeyFile => "key-file",
    }
}

const fn host_key_policy_token(value: SshHostKeyPolicyConfig) -> &'static str {
    match value {
        SshHostKeyPolicyConfig::Strict => "strict",
        SshHostKeyPolicyConfig::AcceptNew => "accept-new",
    }
}
