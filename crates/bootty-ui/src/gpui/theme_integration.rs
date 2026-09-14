//! Project Bootty's palette and font configuration into GPUI Kit.

use super::{UiPalette, UiTheme, chrome::Rgba, settings::ToggleFocusNav};
use gpui_kit::{
    App, Font, FontFeatures, Global, Hsla, KeyBinding, Pixels, SharedString, Window, px,
};
use std::sync::Arc;

const UI_FONT_FAMILY: &str = "IBM Plex Sans";
const UI_FONT_SIZE: f32 = 16.0;
const BUFFER_FONT_FAMILY: &str = "Lilex";
const BUFFER_FONT_SIZE: f32 = 15.0;

pub fn init_theme(palette: UiPalette, cx: &mut App) {
    init_ui_theme(UiTheme { palette }, cx);
}

pub fn init_ui_theme(ui_theme: UiTheme, cx: &mut App) {
    if !cx.has_global::<crate::font_mapping::FontMappings>() {
        cx.set_global(crate::font_mapping::FontMappings::default());
    }
    gpui_kit::init(cx);
    super::config_editor::init(cx);
    cx.set_global(BoottyThemeSettings::default());
    cx.bind_keys([
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
        // Reset every component role before projecting Bootty's overrides. Changing `mode`
        // alone leaves unprojected controls with the previous appearance's colors.
        component.colors = match mode {
            gpui_kit::component::ThemeMode::Light => *gpui_kit::component::ThemeColor::light(),
            gpui_kit::component::ThemeMode::Dark => *gpui_kit::component::ThemeColor::dark(),
        };
        component.font_family = ui_font_family;
        component.font_size = ui_font_size;
        component.mono_font_family = mono_font_family;
        component.mono_font_size = px(BUFFER_FONT_SIZE);
        component.radius = px(f32::from(palette.radius));
        let mut highlight_theme = component_highlight_theme(component.mode).as_ref().clone();
        highlight_theme.style.editor_background = Some(color(palette.base));
        highlight_theme.style.editor_gutter_background = Some(color(palette.base));
        component.highlight_theme = Arc::new(highlight_theme);

        project_component_colors(palette, &mut component.colors);

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

fn project_component_colors(palette: UiPalette, colors: &mut gpui_kit::component::ThemeColor) {
    colors.background = color(palette.base);
    colors.foreground = color(palette.text);
    colors.muted = color(palette.element_disabled);
    colors.muted_foreground = color(palette.muted);
    colors.border = color(palette.border);
    colors.input = color(palette.border_variant);
    colors.ring = color(palette.border_focused);
    colors.caret = color(palette.text_accent);
    colors.selection = color(palette.element_selected);
    colors.list = color(palette.pane);
    colors.list_active = color(palette.element_selected);
    colors.list_active_border = color(palette.border_focused);
    colors.list_hover = color(palette.element_hover);
    colors.list_head = color(palette.surface);
    colors.list_even = color(palette.element_background);
    colors.popover = color(palette.surface);
    colors.popover_foreground = color(palette.text);
    colors.group_box = color(palette.surface);
    colors.group_box_foreground = color(palette.text);
    colors.button = color(palette.element_background);
    colors.button_foreground = color(palette.text);
    colors.button_hover = color(palette.element_hover);
    colors.button_active = color(palette.element_active);
    colors.primary = color(palette.primary);
    colors.primary_foreground = color(palette.base);
    // Ghost controls, including gpui-component's notification close button, use the
    // secondary foreground token. Keep it readable when Bootty starts in dark mode: the
    // component theme is initialized from its light defaults before this palette is applied.
    colors.secondary_foreground = color(palette.text);
    colors.secondary = color(palette.element_background);
    colors.secondary_hover = color(palette.element_hover);
    colors.secondary_active = color(palette.element_active);
    colors.button_secondary = colors.secondary;
    colors.button_secondary_foreground = colors.secondary_foreground;
    colors.button_secondary_hover = colors.secondary_hover;
    colors.button_secondary_active = colors.secondary_active;
    // Sliders are rendered from these legacy component tokens. Project them from the active
    // palette so the track and thumb retain visible contrast for custom dark themes instead
    // of inheriting the gpui-component defaults.
    colors.slider_bar = color(palette.muted);
    colors.slider_thumb = color(palette.text);
    colors.accent = color(palette.element_hover);
    colors.accent_foreground = color(palette.text);

    colors.tab_bar = color(palette.tab_bar);
    colors.tab_bar_segmented = color(palette.tab_bar);
    colors.tab = color(palette.tab_inactive);
    colors.tab_foreground = color(palette.muted);
    colors.tab_active = color(palette.base);
    colors.tab_active_foreground = color(palette.text);
    colors.title_bar = color(palette.tab_bar);
    colors.title_bar_border = color(palette.border);

    // Navigation and the host sidebar share the same surface.
    colors.sidebar = color(palette.pane);
    colors.sidebar_foreground = color(palette.text);
    colors.sidebar_accent = color(palette.element_selected);
    colors.sidebar_accent_foreground = color(palette.text);
    colors.sidebar_border = color(palette.border);
    colors.sidebar_primary = color(palette.primary);
    colors.sidebar_primary_foreground = color(palette.base);

    colors.status_bar = color(palette.mantle);
    colors.status_bar_border = color(palette.border);
    colors.scrollbar = color(palette.base);
    colors.scrollbar_thumb = color(palette.border);
    colors.scrollbar_thumb_hover = color(palette.muted);
    colors.window_border = color(palette.border);

    colors.red = color(palette.destructive);
    colors.red_light = color(palette.destructive);
    colors.green = color(palette.success);
    colors.green_light = color(palette.success);
    colors.blue = color(palette.accent);
    colors.blue_light = color(palette.accent);
    colors.yellow = color(palette.warning);
    colors.yellow_light = color(palette.warning);
    colors.cyan = color(palette.text_accent);
    colors.cyan_light = color(palette.text_accent);
    colors.magenta = color(palette.primary);
    colors.magenta_light = color(palette.primary);
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
