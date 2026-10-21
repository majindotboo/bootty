pub mod action_catalog;
mod agent_cursor;
mod agent_tray;
#[cfg(target_os = "macos")]
pub use agent_tray::run_helper as run_agent_tray_helper;
pub mod ansi_palette;
pub mod app_actions;
pub mod assets;
pub mod attachment_source;
mod chrome_frame;
mod chrome_projection;
pub mod clock;
pub mod commands;
mod config_runtime;
pub mod diagnostics;
mod editors;
pub mod error_catalog;
pub mod file_paths;
pub mod font_database;
pub mod font_mapping;
pub mod frame_facts;
pub mod gpui;
pub mod gpui_actions;
mod gpui_agent_session;
mod gpui_agents_panel;
mod gpui_application_icons;
mod gpui_background;
mod gpui_composer_completion;
pub use gpui_composer_completion::{CompletionScope, MentionableConversation};
pub mod completion;
pub mod composer_commands;
mod gpui_browser_panel;
mod gpui_dock;
mod gpui_dock_skin;
mod gpui_document_panel;
mod gpui_files_panel;
mod gpui_git_panel;
pub mod gpui_image_viewer;
mod gpui_input;
mod gpui_keymap_editor;
mod gpui_mcp_app;
mod gpui_model_picker;
mod gpui_prompt_attachments;
mod gpui_settings;
mod gpui_settings_catalog;
mod gpui_speech;
mod gpui_subagent_panel;
mod gpui_surface_chooser;
mod gpui_terminal_panel;
mod gpui_terminal_view;
#[cfg(target_os = "macos")]
mod gpui_video;
mod gpui_workspace;
pub mod i18n;
pub mod input;
pub mod keymap;
pub mod keymap_runtime;
pub mod menu;
pub mod metrics;
mod motion;
pub mod native_host;
mod native_platform;
mod new_session;
pub mod paint_plan;
pub mod platform;
pub mod presentation;
pub mod product_dialogs;
pub mod project_artwork;
pub mod prompt_stash;
mod remote_catalog;
mod settings_runtime;
pub mod settings_session;
mod state;
pub mod strings;
pub mod surface_creation;
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
pub mod transcript_links;
pub mod usage;
pub mod window;
pub mod window_state;
pub mod workspace_composition;

pub use gpui_keymap_editor::{load_keymap_text_file, reload_saved_keymap_text};
pub use gpui_settings_catalog::{
    SettingsCatalogPage, SettingsDependency, advanced_configuration_rows,
    setting_is_visible_in_native_settings, settings_catalog_pages, settings_category_for,
    settings_dependency_for,
};
pub use state::{
    AppEffect, AppState, CursorIcon, FrameInputs, ModalDialog, NavigationStep, OpenFilesRequest,
    ViewportSnapshot, WindowChromeFacts,
};

pub mod recovery;

mod gpui_sidebar_panel;

pub use gpui_surface_chooser::{SurfaceCommand, SurfacePanel};

pub use gpui_agent_session::{ConversationContext, NativeAgentSessionView, OpenNativeSession};
pub use gpui_git_panel::DiffComment;
pub use gpui_subagent_panel::SubagentPanel;
