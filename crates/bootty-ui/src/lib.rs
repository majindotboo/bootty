pub mod action_catalog;
mod agent_tray;
pub mod ansi_palette;
pub mod app_actions;
pub mod assets;
mod chrome_frame;
mod chrome_projection;
pub mod clock;
pub mod commands;
mod config_runtime;
pub mod diagnostics;
pub mod error_catalog;
pub mod file_paths;
pub mod font_database;
pub mod font_mapping;
pub mod frame_facts;
pub mod gpui;
pub mod gpui_actions;
mod gpui_agents_panel;
mod gpui_background;
mod gpui_dock;
mod gpui_dock_skin;
mod gpui_document_panel;
mod gpui_files_panel;
mod gpui_git_panel;
mod gpui_input;
mod gpui_keymap_editor;
mod gpui_settings;
mod gpui_settings_catalog;
mod gpui_terminal_panel;
mod gpui_terminal_view;
mod gpui_workspace;
pub mod i18n;
pub mod input;
pub mod keymap;
pub mod keymap_runtime;
pub mod menu;
pub mod metrics;
pub mod native_host;
mod native_platform;
mod new_session;
pub mod paint_plan;
pub mod platform;
pub mod presentation;
pub mod product_dialogs;
mod remote_catalog;
mod settings_runtime;
pub mod settings_session;
mod state;
pub mod strings;
mod switch_benchmark;
pub mod terminal_cell_metrics;
mod terminal_config;
pub mod terminal_font_face;
mod terminal_interaction;
pub mod terminal_render;
pub mod terminal_sprite;
pub mod terminal_text;
pub mod terminal_text_atlas;
pub mod theme;
pub mod usage;
pub mod window;
pub mod workspace_composition;

pub use gpui_keymap_editor::{load_keymap_text_file, reload_saved_keymap_text};
pub use gpui_settings_catalog::{
    SettingsCatalogPage, SettingsDependency, UnsupportedModuleDiagnostic,
    advanced_configuration_rows, scan_unsupported_module_sources,
    setting_is_visible_in_native_settings, settings_catalog_pages, settings_category_for,
    settings_dependency_for, unsupported_module_rows,
};
pub use state::{
    AppEffect, AppState, CursorIcon, FrameInputs, ModalDialog, OpenFilesRequest, ViewportSnapshot,
    WindowChromeFacts,
};

pub mod recovery;

mod gpui_sidebar_panel;
