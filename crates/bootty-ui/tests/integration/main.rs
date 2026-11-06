#![cfg(test)]

#[path = "../support/mod.rs"]
mod support;

#[path = "../ansi_palette.rs"]
mod ansi_palette;

#[path = "../application_values.rs"]
mod application_values;

#[path = "../assets.rs"]
mod assets;

#[path = "../capture_dialog.rs"]
mod capture_dialog;

#[path = "../clipboard_images.rs"]
mod clipboard_images;

#[path = "../commands.rs"]
mod commands;

#[path = "../config_editor.rs"]
mod config_editor;

#[path = "../error_notices.rs"]
mod error_notices;

#[path = "../file_paths.rs"]
mod file_paths;

#[path = "../font_config.rs"]
mod font_config;

#[path = "../font_database.rs"]
mod font_database;

#[path = "../font_system_boundaries.rs"]
mod font_system_boundaries;

#[path = "../gpui_actions.rs"]
mod gpui_actions;

#[path = "../gpui_chrome.rs"]
mod gpui_chrome;

#[path = "../gpui_dialog_command.rs"]
mod gpui_dialog_command;

#[path = "../gpui_editor_keybindings.rs"]
mod gpui_editor_keybindings;

#[path = "../gpui_input.rs"]
mod gpui_input;

#[path = "../gpui_interactions.rs"]
mod gpui_interactions;

#[path = "../gpui_keybinding_context.rs"]
mod gpui_keybinding_context;

#[path = "../gpui_keymap_editor.rs"]
mod gpui_keymap_editor;

#[path = "../gpui_keymap_text_editor.rs"]
mod gpui_keymap_text_editor;

#[path = "../gpui_models.rs"]
mod gpui_models;

#[path = "../gpui_overlay_host.rs"]
mod gpui_overlay_host;

#[path = "../gpui_panes.rs"]
mod gpui_panes;

#[path = "../gpui_prompts.rs"]
mod gpui_prompts;

#[path = "../gpui_root.rs"]
mod gpui_root;

#[path = "../gpui_settings_ansi_palette.rs"]
mod gpui_settings_ansi_palette;

#[path = "../gpui_settings_choice.rs"]
mod gpui_settings_choice;

#[path = "../gpui_settings_color.rs"]
mod gpui_settings_color;

#[path = "../gpui_settings_environment.rs"]
mod gpui_settings_environment;

#[path = "../gpui_settings_font_features.rs"]
mod gpui_settings_font_features;

#[path = "../gpui_settings_inline_inputs.rs"]
mod gpui_settings_inline_inputs;

#[path = "../gpui_settings_remote.rs"]
mod gpui_settings_remote;

#[path = "../gpui_settings_scrollbar.rs"]
mod gpui_settings_scrollbar;

#[path = "../gpui_settings_search.rs"]
mod gpui_settings_search;

#[path = "../gpui_settings_slider.rs"]
mod gpui_settings_slider;

#[path = "../gpui_settings_text.rs"]
mod gpui_settings_text;

#[path = "../gpui_settings_theme.rs"]
mod gpui_settings_theme;

#[path = "../gpui_settings_toggle.rs"]
mod gpui_settings_toggle;

#[path = "../gpui_settings_window.rs"]
mod gpui_settings_window;

#[path = "../gpui_space_editor.rs"]
mod gpui_space_editor;

#[path = "../gpui_terminal_images.rs"]
mod gpui_terminal_images;

#[path = "../gpui_terminal_metrics.rs"]
mod gpui_terminal_metrics;

#[path = "../gpui_terminal_sprite.rs"]
mod gpui_terminal_sprite;

#[path = "../gpui_terminal_zoom.rs"]
mod gpui_terminal_zoom;

#[path = "../gpui_theme.rs"]
mod gpui_theme;

#[path = "../i18n.rs"]
mod i18n;

#[path = "../input_config.rs"]
mod input_config;

#[path = "../keymap_bindings.rs"]
mod keymap_bindings;

#[path = "../keymap_runtime.rs"]
mod keymap_runtime;

#[path = "../live_config_reload.rs"]
mod live_config_reload;

#[path = "../native_font_system.rs"]
mod native_font_system;

#[path = "../native_gpui_acceptance.rs"]
mod native_gpui_acceptance;

#[path = "../new_session_dialog.rs"]
mod new_session_dialog;

#[path = "../new_session_form.rs"]
mod new_session_form;

#[path = "../paint_plan_contrast.rs"]
mod paint_plan_contrast;

#[path = "../paint_plan_incremental.rs"]
mod paint_plan_incremental;

#[path = "../project_artwork.rs"]
mod project_artwork;

#[path = "../recovery.rs"]
mod recovery;

#[path = "../scrollback_units.rs"]
mod scrollback_units;

#[path = "../settings_ansi_palette.rs"]
mod settings_ansi_palette;

#[path = "../settings_boolean_values.rs"]
mod settings_boolean_values;

#[path = "../settings_catalog.rs"]
mod settings_catalog;

#[path = "../settings_custom_values.rs"]
mod settings_custom_values;

#[path = "../settings_default_values.rs"]
mod settings_default_values;

#[path = "../settings_environment_drafts.rs"]
mod settings_environment_drafts;

#[path = "../settings_font_features.rs"]
mod settings_font_features;

#[path = "../settings_numeric_values.rs"]
mod settings_numeric_values;

#[path = "../settings_remote_drafts.rs"]
mod settings_remote_drafts;

#[path = "../settings_string_lists.rs"]
mod settings_string_lists;

#[path = "../settings_structured_values.rs"]
mod settings_structured_values;

#[path = "../surface_creation.rs"]
mod surface_creation;

#[path = "../terminal_cell_metrics.rs"]
mod terminal_cell_metrics;

#[path = "../terminal_font_face.rs"]
mod terminal_font_face;

#[path = "../terminal_interaction.rs"]
mod terminal_interaction;

#[path = "../terminal_platform_raster.rs"]
mod terminal_platform_raster;

#[path = "../terminal_render_commands.rs"]
mod terminal_render_commands;

#[path = "../terminal_sprite.rs"]
mod terminal_sprite;

#[path = "../terminal_text_atlas.rs"]
mod terminal_text_atlas;

#[path = "../theme_appearance.rs"]
mod theme_appearance;

#[path = "../theme_editor.rs"]
mod theme_editor;

#[path = "../window_platform.rs"]
mod window_platform;

#[path = "../window_state.rs"]
mod window_state;

#[path = "../workspace_composition.rs"]
mod workspace_composition;

#[path = "../workspace_runtime.rs"]
mod workspace_runtime;
