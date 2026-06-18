use eframe::egui;

use super::SettingsWindow;
use crate::config::{
    MacosTitlebarStyle, MultiplexerBackendConfig, SidebarPosition, WindowDecoration,
    WindowFullscreen,
};

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;

    super::section(ui, palette, "MULTIPLEXER");
    egui::Grid::new("settings_multiplexer_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Backend");
            let mut backend = win.config.multiplexer.backend;
            if enum_combo(
                ui,
                palette,
                "settings_backend",
                &mut backend,
                &[
                    (MultiplexerBackendConfig::Native, "native"),
                    (MultiplexerBackendConfig::Rmux, "rmux"),
                    (MultiplexerBackendConfig::Tmux, "tmux"),
                    (MultiplexerBackendConfig::Zellij, "zellij"),
                ],
            ) {
                win.config.multiplexer.backend = backend;
                win.set_str(&["multiplexer", "backend"], backend_token(backend));
            }
            ui.end_row();
        });
    ui.label(
        egui::RichText::new("Applies to new sessions.")
            .color(palette.muted)
            .size(12.0),
    );

    super::section(ui, palette, "WINDOW");

    egui::Grid::new("settings_window_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Title");
            let mut title = win.config.window.title.clone();
            if ui
                .add_sized(
                    [300.0, 26.0],
                    egui::TextEdit::singleline(&mut title).vertical_align(egui::Align::Center),
                )
                .changed()
            {
                win.config.window.title = title.clone();
                win.set_str(&["window", "title"], &title);
            }
            ui.end_row();

            ui.label("Titlebar (macOS)");
            let mut style = win.config.window.macos_titlebar_style;
            if enum_combo(
                ui,
                palette,
                "settings_titlebar",
                &mut style,
                &[
                    (MacosTitlebarStyle::Native, "native"),
                    (MacosTitlebarStyle::Transparent, "transparent"),
                    (MacosTitlebarStyle::Tabs, "tabs"),
                    (MacosTitlebarStyle::Hidden, "hidden"),
                ],
            ) {
                win.config.window.macos_titlebar_style = style;
                win.set_str(&["window", "macos-titlebar-style"], titlebar_token(style));
            }
            ui.end_row();

            ui.label("Decoration");
            let mut decoration = win.config.window.window_decoration;
            if enum_combo(
                ui,
                palette,
                "settings_decoration",
                &mut decoration,
                &[
                    (WindowDecoration::Auto, "auto"),
                    (WindowDecoration::None, "none"),
                    (WindowDecoration::Client, "client"),
                    (WindowDecoration::Server, "server"),
                ],
            ) {
                win.config.window.window_decoration = decoration;
                win.set_str(
                    &["window", "window-decoration"],
                    decoration_token(decoration),
                );
            }
            ui.end_row();

            ui.label("Fullscreen");
            let mut fullscreen = win.config.window.fullscreen;
            if enum_combo(
                ui,
                palette,
                "settings_fullscreen",
                &mut fullscreen,
                &[
                    (WindowFullscreen::Disabled, "disabled"),
                    (WindowFullscreen::Native, "native"),
                    (WindowFullscreen::NonNative, "non_native"),
                    (
                        WindowFullscreen::NonNativeVisibleMenu,
                        "non_native_visible_menu",
                    ),
                    (
                        WindowFullscreen::NonNativePaddedNotch,
                        "non_native_padded_notch",
                    ),
                ],
            ) {
                win.config.window.fullscreen = fullscreen;
                win.set_str(&["window", "fullscreen"], fullscreen_token(fullscreen));
            }
            ui.end_row();
        });

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Default size applies to new windows.")
            .color(palette.muted)
            .size(12.0),
    );
    egui::Grid::new("settings_window_size_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Default width");
            let mut width = win.config.window.width;
            if ui
                .add(
                    egui::DragValue::new(&mut width)
                        .speed(4.0)
                        .range(400.0..=6000.0),
                )
                .changed()
            {
                win.config.window.width = width;
                win.set_f32(&["window", "width"], width);
            }
            ui.end_row();

            ui.label("Default height");
            let mut height = win.config.window.height;
            if ui
                .add(
                    egui::DragValue::new(&mut height)
                        .speed(4.0)
                        .range(300.0..=4000.0),
                )
                .changed()
            {
                win.config.window.height = height;
                win.set_f32(&["window", "height"], height);
            }
            ui.end_row();
        });

    super::section(ui, palette, "CHROME");
    egui::Grid::new("settings_chrome_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            checkbox(ui, win, "Sidebar", &["chrome", "sidebar"], |chrome| {
                &mut chrome.sidebar
            });
            checkbox(ui, win, "Status bar", &["chrome", "status-bar"], |chrome| {
                &mut chrome.status_bar
            });
            checkbox(
                ui,
                win,
                "Window tabs",
                &["chrome", "window-tabs"],
                |chrome| &mut chrome.window_tabs,
            );
            slider(
                ui,
                win,
                "Sidebar width",
                &["chrome", "sidebar-width"],
                120.0..=600.0,
                " px",
                |chrome| &mut chrome.sidebar_width,
            );
            ui.label("Sidebar position");
            let mut position = win.config.sidebar.position;
            if enum_combo(
                ui,
                palette,
                "settings_sidebar_position",
                &mut position,
                &[
                    (SidebarPosition::Left, "left"),
                    (SidebarPosition::Right, "right"),
                ],
            ) {
                win.config.sidebar.position = position;
                win.set_str(&["sidebar", "position"], position_token(position));
            }
            ui.end_row();
            slider(
                ui,
                win,
                "Status height",
                &["chrome", "status-height"],
                0.0..=80.0,
                " px",
                |chrome| &mut chrome.status_height,
            );
            slider(
                ui,
                win,
                "Gap",
                &["chrome", "gap"],
                0.0..=24.0,
                " px",
                |chrome| &mut chrome.gap,
            );
            slider(
                ui,
                win,
                "Unfocused sidebar dim",
                &["chrome", "unfocused-sidebar-dim"],
                0.0..=1.0,
                "",
                |chrome| &mut chrome.unfocused_sidebar_dim,
            );
            slider(
                ui,
                win,
                "Unfocused terminal dim",
                &["chrome", "unfocused-terminal-dim"],
                0.0..=1.0,
                "",
                |chrome| &mut chrome.unfocused_terminal_dim,
            );
        });
}

fn enum_combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    id: &str,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    let labels: Vec<&str> = options.iter().map(|(_, label)| *label).collect();
    let current_index = options.iter().position(|(value, _)| *value == *current);
    let selected_text = current_index.map_or("", |index| labels[index]);
    if let Some(index) = super::searchable_combo(
        ui,
        palette,
        id,
        selected_text,
        220.0,
        &labels,
        current_index,
    ) {
        *current = options[index].0;
        return true;
    }
    false
}

fn checkbox(
    ui: &mut egui::Ui,
    win: &mut SettingsWindow,
    label: &str,
    path: &[&str],
    field: fn(&mut crate::config::ChromeConfig) -> &mut bool,
) {
    ui.label(label);
    let mut value = *field(&mut win.config.chrome);
    if ui.checkbox(&mut value, "").changed() {
        *field(&mut win.config.chrome) = value;
        win.set_bool(path, value);
    }
    ui.end_row();
}

fn slider(
    ui: &mut egui::Ui,
    win: &mut SettingsWindow,
    label: &str,
    path: &[&str],
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    field: fn(&mut crate::config::ChromeConfig) -> &mut f32,
) {
    ui.label(label);
    let mut value = *field(&mut win.config.chrome);
    if ui
        .add(egui::Slider::new(&mut value, range).suffix(suffix))
        .changed()
    {
        *field(&mut win.config.chrome) = value;
        win.set_f32(path, value);
    }
    ui.end_row();
}

fn titlebar_token(style: MacosTitlebarStyle) -> &'static str {
    match style {
        MacosTitlebarStyle::Native => "native",
        MacosTitlebarStyle::Transparent => "transparent",
        MacosTitlebarStyle::Tabs => "tabs",
        MacosTitlebarStyle::Hidden => "hidden",
    }
}

fn decoration_token(decoration: WindowDecoration) -> &'static str {
    match decoration {
        WindowDecoration::None => "none",
        WindowDecoration::Auto => "auto",
        WindowDecoration::Client => "client",
        WindowDecoration::Server => "server",
    }
}

fn backend_token(backend: MultiplexerBackendConfig) -> &'static str {
    match backend {
        MultiplexerBackendConfig::Native => "native",
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
        MultiplexerBackendConfig::Zellij => "zellij",
    }
}

fn position_token(position: SidebarPosition) -> &'static str {
    match position {
        SidebarPosition::Left => "left",
        SidebarPosition::Right => "right",
    }
}

fn fullscreen_token(fullscreen: WindowFullscreen) -> &'static str {
    match fullscreen {
        WindowFullscreen::Disabled => "disabled",
        WindowFullscreen::Native => "native",
        WindowFullscreen::NonNative => "non_native",
        WindowFullscreen::NonNativeVisibleMenu => "non_native_visible_menu",
        WindowFullscreen::NonNativePaddedNotch => "non_native_padded_notch",
    }
}
