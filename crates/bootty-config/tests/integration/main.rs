#![cfg(test)]

#[path = "../background.rs"]
mod background;

#[path = "../config.rs"]
mod config;

#[path = "../config_reload.rs"]
mod config_reload;

#[path = "../config_runtime.rs"]
mod config_runtime;

#[path = "../default_appearance.rs"]
mod default_appearance;

#[path = "../font_features.rs"]
mod font_features;

#[path = "../font_styles.rs"]
mod font_styles;

#[path = "../identity.rs"]
mod identity;

#[path = "../keymap.rs"]
mod keymap;

#[path = "../keymap_file.rs"]
mod keymap_file;

#[path = "../modifier_remap.rs"]
mod modifier_remap;

#[path = "../modifier_remaps.rs"]
mod modifier_remaps;

#[path = "../settings_schema.rs"]
mod settings_schema;

#[path = "../theme_file.rs"]
mod theme_file;

#[path = "../writeback.rs"]
mod writeback;

#[path = "../wsl.rs"]
mod wsl;
