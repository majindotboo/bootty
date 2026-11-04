//! Project Bootty's palette and font configuration into GPUI Kit.

use super::{UiPalette, UiTheme, chrome::Rgba, settings::ToggleFocusNav};
use gpui_kit::component::Colorize as _;
use gpui_kit::{
    App, Font, FontFeatures, Global, Hsla, KeyBinding, Pixels, SharedString, Window, px,
};
use std::sync::Arc;

const UI_FONT_FAMILY: &str = "IBM Plex Sans";
const UI_FONT_SIZE: f32 = 16.0;
const BUFFER_FONT_FAMILY: &str = "Lilex";

pub fn init_theme(palette: UiPalette, cx: &mut App) {
    init_ui_theme(UiTheme { palette }, cx);
}

/// The Control keys macOS text fields take from Emacs, which GPUI Kit leaves to the platform
/// apart from Control-A and Control-E. Moving by line and deleting to the line's end stay in the
/// composer, where lists and menus don't claim those keys.
#[cfg(target_os = "macos")]
fn bind_cocoa_text_keys(cx: &mut App) {
    use gpui_kit::component::input::{
        Backspace, Delete, DeleteToEndOfLine, MoveDown, MoveLeft, MoveRight, MoveUp,
    };
    let composer = Some("BoottyComposer > Input");
    cx.bind_keys([
        KeyBinding::new("ctrl-b", MoveLeft, Some("Input")),
        KeyBinding::new("ctrl-f", MoveRight, Some("Input")),
        KeyBinding::new("ctrl-d", Delete, Some("Input")),
        KeyBinding::new("ctrl-h", Backspace, Some("Input")),
        KeyBinding::new("ctrl-p", MoveUp, composer),
        KeyBinding::new("ctrl-n", MoveDown, composer),
        KeyBinding::new("ctrl-k", DeleteToEndOfLine, composer),
    ]);
}

pub fn init_ui_theme(ui_theme: UiTheme, cx: &mut App) {
    if !cx.has_global::<crate::font_mapping::FontMappings>() {
        cx.set_global(crate::font_mapping::FontMappings::default());
    }
    gpui_kit::init(cx);
    super::init_text_menu(cx);
    super::config_editor::init(cx);
    #[cfg(target_os = "macos")]
    bind_cocoa_text_keys(cx);
    cx.set_global(BoottyThemeSettings::default());
    cx.bind_keys([
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-w"
            } else {
                "ctrl-w"
            },
            crate::gpui_actions::CloseSettingsWindow,
            Some("BoottySettingsWindow"),
        ),
        KeyBinding::new("ctrl-p", gpui_kit::base::actions::SelectUp, Some("Command")),
        KeyBinding::new(
            "ctrl-n",
            gpui_kit::base::actions::SelectDown,
            Some("Command"),
        ),
        KeyBinding::new("left", ToggleFocusNav, Some("SettingsWindow")),
        KeyBinding::new("tab", gpui_kit::NoAction, Some("BoottyThemePicker")),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-shift-e"
            } else {
                "ctrl-shift-e"
            },
            ToggleFocusNav,
            Some("SettingsWindow"),
        ),
    ]);
    update_ui_theme(ui_theme, cx);
}

pub fn update_theme(palette: UiPalette, cx: &mut App) {
    update_ui_theme(UiTheme { palette }, cx);
}

pub fn update_ui_theme(ui_theme: UiTheme, cx: &mut App) {
    update_gpui_component_theme(ui_theme.palette, cx);
    cx.refresh_windows();
}

fn update_gpui_component_theme(palette: UiPalette, cx: &mut App) {
    let settings = cx.global::<BoottyThemeSettings>();
    let ui_font_family = settings.ui_font.family.clone();
    let mono_font_family = settings.mono_font_family.clone();
    let ui_font_size = settings.ui_font_size;
    {
        let component = gpui_kit::component::Theme::global_mut(cx);
        let mode = appearance_for(palette);
        component.mode = mode;
        component.colors = component_colors(palette);
        component.font_family = ui_font_family;
        component.font_size = ui_font_size;
        component.mono_font_family = mono_font_family;
        component.mono_font_size = ui_font_size;
        component.radius = px(f32::from(palette.radius));
        component.shadow = false;
        // Keep keyboard focus visible through the ring-colored border without the outer glow.
        component.focus_ring = false;
        component.list.active_highlight = false;
        let mut highlight_theme = component_highlight_theme(component.mode).as_ref().clone();
        highlight_theme.style.editor_background = Some(color(palette.base));
        highlight_theme.style.editor_foreground = Some(color(palette.text));
        highlight_theme.style.editor_gutter_background = Some(color(palette.base));
        component.highlight_theme = Arc::new(highlight_theme);

        component.tokens = gpui_kit::component::ThemeTokens::from(&component.colors);
    }
    gpui_kit::component::Theme::sync_base(cx);
    let base = gpui_kit::base::Theme::global_mut(cx);
    let scrollbar_styles = base
        .scrollbar
        .styles()
        .clone()
        .track(|style| style.width(px(10.)).bg(gpui_kit::transparent_black()))
        .track_hover(|style| style.width(px(10.)).bg(gpui_kit::transparent_black()))
        .track_active(|style| {
            style
                .width(px(10.))
                .bg(gpui_kit::transparent_black())
                .border_color(gpui_kit::transparent_black())
        })
        .thumb(|style| style.width(px(4.)).inset(px(3.)))
        .thumb_hover(|style| style.width(px(6.)).inset(px(2.)))
        .thumb_active(|style| style.width(px(6.)).inset(px(2.)));
    base.scrollbar = base.scrollbar.clone().with_styles(scrollbar_styles);
}

// Exhaustive construction makes a new GPUI role a compile error until Bootty maps it.
#[expect(
    clippy::too_many_lines,
    reason = "ThemeColor has a field per GPUI role, and only one exhaustive literal keeps every role mapped."
)]
fn component_colors(palette: UiPalette) -> gpui_kit::component::ThemeColor {
    let background = color(palette.base);
    let foreground = color(palette.text);
    let primary = color(palette.primary);
    let primary_foreground = color(super::readable_color(palette.primary, palette.base));
    let primary_hover = primary.mix_oklab(foreground, 0.9);
    let primary_active = primary.mix_oklab(background, 0.9);
    let secondary = color(palette.element_background);
    let secondary_foreground = foreground;
    let secondary_hover = color(palette.element_hover);
    let secondary_active = color(palette.element_active);
    let link = color(super::readable_color(palette.base, palette.accent));
    let danger_color = super::readable_color(palette.base, palette.destructive);
    let danger = color(danger_color);
    let danger_foreground = color(super::readable_color(danger_color, palette.base));
    let danger_hover = danger.mix_oklab(foreground, 0.9);
    let danger_active = danger.mix_oklab(background, 0.9);
    let warning_color = super::readable_color(palette.base, palette.warning);
    let warning = color(warning_color);
    let warning_foreground = color(super::readable_color(warning_color, palette.base));
    let warning_hover = warning.mix_oklab(foreground, 0.9);
    let warning_active = warning.mix_oklab(background, 0.9);
    let success_color = super::readable_color(palette.base, palette.success);
    let success = color(success_color);
    let success_foreground = color(super::readable_color(success_color, palette.base));
    let success_hover = success.mix_oklab(foreground, 0.9);
    let success_active = success.mix_oklab(background, 0.9);
    let info_color = super::readable_color(palette.base, palette.accent);
    let info = color(info_color);
    let info_foreground = color(super::readable_color(info_color, palette.base));
    let info_hover = info.mix_oklab(foreground, 0.9);
    let info_active = info.mix_oklab(background, 0.9);

    let mut colors = gpui_kit::component::ThemeColor::default();
    colors.accent = color(palette.element_hover);
    colors.accent_foreground = color(palette.text);
    colors.accordion = color(palette.pane);
    colors.background = color(palette.base);
    colors.border = color(palette.border);
    colors.button = color(palette.element_background);
    colors.button_active = color(palette.element_active);
    colors.button_foreground = color(palette.text);
    colors.button_hover = color(palette.element_hover);
    colors.button_danger = danger;
    colors.button_danger_active = danger_active;
    colors.button_danger_foreground = danger_foreground;
    colors.button_danger_hover = danger_hover;
    colors.button_info = info;
    colors.button_info_active = info_active;
    colors.button_info_foreground = info_foreground;
    colors.button_info_hover = info_hover;
    colors.button_primary = primary;
    colors.button_primary_active = primary_active;
    colors.button_primary_foreground = primary_foreground;
    colors.button_primary_hover = primary_hover;
    colors.button_secondary = secondary;
    colors.button_secondary_active = secondary_active;
    colors.button_secondary_foreground = secondary_foreground;
    colors.button_secondary_hover = secondary_hover;
    colors.button_success = success;
    colors.button_success_active = success_active;
    colors.button_success_foreground = success_foreground;
    colors.button_success_hover = success_hover;
    colors.button_warning = warning;
    colors.button_warning_active = warning_active;
    colors.button_warning_foreground = warning_foreground;
    colors.button_warning_hover = warning_hover;
    colors.group_box = color(palette.surface);
    colors.group_box_foreground = color(palette.text);
    colors.caret = color(palette.text_accent);
    colors.chart_1 = color(palette.accent);
    colors.chart_2 = color(palette.success);
    colors.chart_3 = color(palette.warning);
    colors.chart_4 = color(palette.primary);
    colors.chart_5 = color(palette.destructive);
    colors.chart_bullish = color(palette.success);
    colors.chart_bearish = color(palette.destructive);
    colors.chart_grid = color(palette.border);
    colors.danger = danger;
    colors.danger_active = danger_active;
    colors.danger_foreground = danger_foreground;
    colors.danger_hover = danger_hover;
    colors.description_list_label = color(palette.pane);
    colors.description_list_label_foreground = color(palette.subtext);
    colors.drag_border = color(palette.border_focused);
    colors.drop_target = color(palette.element_selected).opacity(0.35);
    colors.foreground = color(palette.text);
    colors.info = info;
    colors.info_active = info_active;
    colors.info_foreground = info_foreground;
    colors.info_hover = info_hover;
    colors.input = color(palette.border_variant);
    colors.link = color(super::readable_color(palette.base, palette.accent));
    colors.link_active = link.mix_oklab(foreground, 0.8);
    colors.link_hover = link.mix_oklab(foreground, 0.9);
    colors.list = color(palette.pane);
    colors.list_active = color(palette.element_selected);
    colors.list_active_border = color(palette.border_focused);
    colors.list_even = color(palette.element_background);
    colors.list_head = color(palette.surface);
    colors.list_hover = color(palette.element_hover);
    colors.muted = color(palette.element_disabled);
    colors.muted_foreground = color(palette.muted);
    // Tooltips and menus lift toward the theme's own text color, which keeps its tint; white
    // would wash them gray.
    colors.popover = color(super::mix(palette.base, palette.text, 0.1));
    colors.popover_foreground = color(palette.text);
    colors.primary = primary;
    colors.primary_active = primary_active;
    colors.primary_foreground = primary_foreground;
    colors.primary_hover = primary_hover;
    colors.progress_bar = color(palette.accent);
    colors.ring = color(palette.border_strong);
    colors.scrollbar = color(palette.base);
    colors.scrollbar_thumb = color(palette.border);
    colors.scrollbar_thumb_hover = color(palette.muted);
    colors.secondary = color(palette.element_background);
    colors.secondary_active = color(palette.element_active);
    colors.secondary_foreground = color(palette.text);
    colors.secondary_hover = color(palette.element_hover);
    colors.selection = color(palette.element_selected).opacity(0.35);
    colors.sidebar = color(palette.pane);
    colors.sidebar_accent = color(palette.element_selected);
    colors.sidebar_accent_foreground = color(palette.text);
    colors.sidebar_border = color(palette.border);
    colors.sidebar_foreground = color(palette.text);
    colors.sidebar_primary = color(palette.primary);
    colors.sidebar_primary_foreground = primary_foreground;
    colors.skeleton = color(palette.element_background);
    colors.slider_bar = color(palette.muted);
    colors.slider_thumb = color(palette.text);
    colors.success = success;
    colors.success_foreground = success_foreground;
    colors.success_hover = success_hover;
    colors.success_active = success_active;
    colors.switch = color(palette.element_background);
    colors.switch_thumb = color(palette.text);
    colors.tab = color(palette.tab_inactive);
    colors.tab_active = color(palette.base);
    colors.tab_active_foreground = color(palette.text);
    colors.tab_bar = color(palette.tab_bar);
    colors.tab_bar_segmented = color(palette.tab_bar);
    colors.tab_foreground = color(palette.muted);
    colors.table = color(palette.base);
    colors.table_active = color(palette.element_selected);
    colors.table_active_border = color(palette.border_focused);
    colors.table_even = color(palette.pane);
    colors.table_head = color(palette.pane);
    colors.table_head_foreground = color(palette.text);
    colors.table_foot = color(palette.pane);
    colors.table_foot_foreground = color(palette.subtext);
    colors.table_hover = color(palette.element_hover);
    colors.table_row_border = color(palette.border_variant);
    colors.title_bar = color(palette.tab_bar);
    colors.title_bar_border = color(palette.border);
    colors.status_bar = color(palette.mantle);
    colors.status_bar_border = color(palette.border);
    colors.warning = warning;
    colors.warning_active = warning_active;
    colors.warning_hover = warning_hover;
    colors.warning_foreground = warning_foreground;
    colors.overlay = color(palette.mantle).opacity(0.6);
    colors.window_border = color(palette.border);
    colors.red = color(palette.destructive);
    colors.red_light = color(palette.destructive);
    colors.green = color(palette.success);
    colors.green_light = color(palette.success);
    colors.blue = color(palette.accent);
    colors.blue_light = color(palette.accent);
    colors.yellow = color(palette.warning);
    colors.yellow_light = color(palette.warning);
    colors.magenta = color(palette.primary);
    colors.magenta_light = color(palette.primary);
    colors.cyan = color(palette.text_accent);
    colors.cyan_light = color(palette.text_accent);
    colors
}

fn component_highlight_theme(
    mode: gpui_kit::component::ThemeMode,
) -> Arc<gpui_kit::component::highlighter::HighlightTheme> {
    match mode {
        gpui_kit::component::ThemeMode::Light => {
            gpui_kit::component::highlighter::HighlightTheme::default_light()
        }
        gpui_kit::component::ThemeMode::Dark => {
            gpui_kit::component::highlighter::HighlightTheme::default_dark()
        }
    }
}

/// Apply the complete configured font, including its weight and fallbacks, to a window root.
pub fn setup_ui_font(window: &mut Window, cx: &mut App) -> Font {
    let settings = cx.global::<BoottyThemeSettings>();
    window.set_rem_size(settings.ui_font_size);
    settings.ui_font.clone()
}

pub fn ui_rem_size(cx: &App) -> Pixels {
    cx.global::<BoottyThemeSettings>().ui_font_size
}

pub fn update_ui_font(families: &[String], size: f32, cx: &mut App) {
    let weights = cx.global::<BoottyThemeSettings>().ui_weights.clone();
    let font = crate::font_mapping::ui_font(families, &weights, cx.global());
    let settings = cx.global_mut::<BoottyThemeSettings>();
    settings.ui_font = font;
    settings.ui_families = families.to_vec();
    settings.ui_font_size = px(size);
    sync_component_ui_font(cx);
}

/// Publish an accepted live UI family change while retaining the active size.
pub fn update_ui_font_families(families: &[String], cx: &mut App) {
    let size = cx.global::<BoottyThemeSettings>().ui_font_size;
    update_ui_font(families, size.into(), cx);
}

pub fn update_ui_font_weights(weights: &bootty_config::FontWeightAssignments, cx: &mut App) {
    let families = cx.global::<BoottyThemeSettings>().ui_families.clone();
    let font = crate::font_mapping::ui_font(&families, weights, cx.global());
    let mono = crate::font_mapping::ui_font(&[BUFFER_FONT_FAMILY.to_owned()], weights, cx.global());
    let settings = cx.global_mut::<BoottyThemeSettings>();
    settings.ui_font = font;
    settings.mono_font_family = mono.family;
    settings.ui_weights.clone_from(weights);
    sync_component_ui_font(cx);
}

/// Publish an accepted live UI size change while retaining the active family stack.
pub fn update_ui_font_size(size: f32, cx: &mut App) {
    cx.global_mut::<BoottyThemeSettings>().ui_font_size = px(size);
    sync_component_ui_font(cx);
}

fn sync_component_ui_font(cx: &mut App) {
    let settings = cx.global::<BoottyThemeSettings>();
    let family = settings.ui_font.family.clone();
    let mono_family = settings.mono_font_family.clone();
    let size = settings.ui_font_size;
    let component = gpui_kit::component::Theme::global_mut(cx);
    component.font_family = family;
    component.mono_font_family = mono_family;
    component.font_size = size;
    component.mono_font_size = size;
    gpui_kit::component::Theme::sync_base(cx);
    cx.refresh_windows();
}

struct BoottyThemeSettings {
    ui_font: Font,
    ui_families: Vec<String>,
    ui_weights: bootty_config::FontWeightAssignments,
    mono_font_family: SharedString,
    ui_font_size: Pixels,
}
impl Global for BoottyThemeSettings {}
impl Default for BoottyThemeSettings {
    fn default() -> Self {
        Self {
            ui_font: Font {
                features: FontFeatures::disable_ligatures(),
                ..gpui_kit::font(UI_FONT_FAMILY)
            },
            ui_families: Vec::new(),
            ui_weights: bootty_config::FontWeightAssignments::new(),
            mono_font_family: BUFFER_FONT_FAMILY.into(),
            ui_font_size: px(UI_FONT_SIZE),
        }
    }
}

fn appearance_for(palette: UiPalette) -> gpui_kit::component::ThemeMode {
    let channel = |value: u8| {
        let value = f32::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.0722f32.mul_add(
        channel(palette.base.blue),
        0.7152f32.mul_add(
            channel(palette.base.green),
            0.2126 * channel(palette.base.red),
        ),
    );
    if luminance > 0.5 {
        gpui_kit::component::ThemeMode::Light
    } else {
        gpui_kit::component::ThemeMode::Dark
    }
}

fn color(value: Rgba) -> Hsla {
    gpui_kit::rgba(
        u32::from(value.red) << 24
            | u32::from(value.green) << 16
            | u32::from(value.blue) << 8
            | u32::from(value.alpha),
    )
    .into()
}
