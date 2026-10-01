#![cfg(test)]

// Settings behavior and GPUI acceptance share one integration executable.
// Keep each scenario module independently filterable with nextest.

#[path = "../support/settings.rs"]
mod settings_support;

mod gpui_settings_ansi_palette;
mod gpui_settings_choice;
mod gpui_settings_color;
mod gpui_settings_dependent;
mod gpui_settings_environment;
mod gpui_settings_font_features;
mod gpui_settings_inline_inputs;
mod gpui_settings_module_editor;
mod gpui_settings_remote;
mod gpui_settings_scrollbar;
mod gpui_settings_search;
mod gpui_settings_slider;
mod gpui_settings_status_segments;
mod gpui_settings_text;
mod gpui_settings_toggle;
mod gpui_settings_window;
mod settings_agent_adapters;
mod settings_ansi_palette;
mod settings_boolean_values;
mod settings_catalog;
mod settings_custom_values;
mod settings_default_values;
mod settings_environment_drafts;
mod settings_font_features;
mod settings_numeric_values;
mod settings_remote_drafts;
mod settings_string_lists;
mod settings_structured_values;
