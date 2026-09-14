//! Renderer-neutral palette used by Bootty's GPUI presentation.

use super::chrome::Rgba;
use num_traits::ToPrimitive as _;

/// The shared one-pixel separator used by the GPUI chrome.
pub const UI_BORDER_WIDTH: f32 = 1.0;
/// The keyboard-focus outline is intentionally thin, like the rest of the chrome separators.
pub const UI_FOCUS_RING_WIDTH: f32 = 1.0;

/// Small radius for controls, list rows, and icon buttons.
pub const UI_RADIUS_SM: f32 = 4.0;
/// Medium radius for popovers and contained surfaces.
pub const UI_RADIUS_MD: f32 = 6.0;
/// Large radius for top-level overlays. Most chrome should use [`UI_RADIUS_SM`].
pub const UI_RADIUS_LG: f32 = 8.0;

/// Height of a regular compact control in the application chrome.
pub const UI_CONTROL_HEIGHT: f32 = 28.0;
/// Height of the tab bar container. Zed keeps the active tab one pixel above its bar.
pub const UI_TAB_BAR_HEIGHT: f32 = 32.0;
/// Height of a standard list row.
pub const UI_ROW_HEIGHT: f32 = 28.0;

/// Standard icon boxes used by chrome controls.
pub const UI_ICON_XSMALL: f32 = 12.0;
pub const UI_ICON_SMALL: f32 = 14.0;
pub const UI_ICON_MEDIUM: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiPalette {
    pub mantle: Rgba,
    pub base: Rgba,
    pub pane: Rgba,
    pub surface: Rgba,
    pub hover: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub subtext: Rgba,
    pub muted: Rgba,
    pub primary: Rgba,
    pub accent: Rgba,
    pub warning: Rgba,
    pub success: Rgba,
    pub destructive: Rgba,

    // These names mirror the semantic roles used by Zed's UI components. They are derived from
    // the same terminal palette as the legacy names above so a theme changes the whole chrome,
    // not only the background and foreground.
    pub border_variant: Rgba,
    pub border_focused: Rgba,
    pub border_selected: Rgba,
    pub border_disabled: Rgba,
    pub element_background: Rgba,
    pub element_hover: Rgba,
    pub element_active: Rgba,
    pub element_selected: Rgba,
    pub element_disabled: Rgba,
    pub ghost_element_background: Rgba,
    pub ghost_element_hover: Rgba,
    pub ghost_element_active: Rgba,
    pub ghost_element_selected: Rgba,
    pub ghost_element_disabled: Rgba,
    pub text_placeholder: Rgba,
    pub text_disabled: Rgba,
    pub text_accent: Rgba,
    pub icon: Rgba,
    pub icon_muted: Rgba,
    pub icon_disabled: Rgba,
    pub icon_accent: Rgba,
    pub tab_bar: Rgba,
    pub tab_inactive: Rgba,
    pub tab_active: Rgba,
    pub radius: u8,
}

impl Default for UiPalette {
    fn default() -> Self {
        Self {
            mantle: Rgba::rgb(0x13, 0x15, 0x18),
            base: Rgba::rgb(0x1b, 0x1d, 0x21),
            pane: Rgba::rgb(0x24, 0x27, 0x2c),
            surface: Rgba::rgb(0x2c, 0x2f, 0x35),
            hover: Rgba::rgb(0x37, 0x3b, 0x43),
            border: Rgba::rgb(0x4a, 0x4f, 0x59),
            text: Rgba::rgb(0xed, 0xf0, 0xf2),
            subtext: Rgba::rgb(0xce, 0xd3, 0xd8),
            muted: Rgba::rgb(0x93, 0x9a, 0xa3),
            primary: Rgba::rgb(0xd2, 0xd6, 0xdb),
            accent: Rgba::rgb(0x77, 0xb7, 0xdf),
            warning: Rgba::rgb(0xe8, 0xb2, 0x6f),
            success: Rgba::rgb(0x79, 0xca, 0x9b),
            destructive: Rgba::rgb(0xe6, 0x8d, 0x91),
            border_variant: Rgba::rgb(0x37, 0x3b, 0x43),
            border_focused: Rgba::rgb(0x77, 0xb7, 0xdf),
            border_selected: Rgba::rgb(0x77, 0xb7, 0xdf),
            border_disabled: Rgba::rgb(0x2d, 0x30, 0x35),
            element_background: Rgba::rgb(0x2c, 0x2f, 0x35),
            element_hover: Rgba::rgb(0x37, 0x3b, 0x43),
            element_active: Rgba::rgb(0x42, 0x47, 0x50),
            element_selected: Rgba::rgb(0x31, 0x4a, 0x5b),
            element_disabled: Rgba::rgb(0x1f, 0x21, 0x25),
            ghost_element_background: Rgba::rgb(0x1b, 0x1d, 0x21),
            ghost_element_hover: Rgba::rgb(0x37, 0x3b, 0x43),
            ghost_element_active: Rgba::rgb(0x42, 0x47, 0x50),
            ghost_element_selected: Rgba::rgb(0x31, 0x4a, 0x5b),
            ghost_element_disabled: Rgba::rgb(0x1b, 0x1d, 0x21),
            text_placeholder: Rgba::rgb(0x93, 0x9a, 0xa3),
            text_disabled: Rgba::rgb(0x6d, 0x74, 0x7d),
            text_accent: Rgba::rgb(0x77, 0xb7, 0xdf),
            icon: Rgba::rgb(0xed, 0xf0, 0xf2),
            icon_muted: Rgba::rgb(0x93, 0x9a, 0xa3),
            icon_disabled: Rgba::rgb(0x6d, 0x74, 0x7d),
            icon_accent: Rgba::rgb(0x77, 0xb7, 0xdf),
            tab_bar: Rgba::rgb(0x13, 0x15, 0x18),
            tab_inactive: Rgba::rgb(0x1b, 0x1d, 0x21),
            tab_active: Rgba::rgb(0x1b, 0x1d, 0x21),
            radius: UI_RADIUS_SM.to_u8().unwrap_or(0),
        }
    }
}

impl UiPalette {
    #[must_use]
    pub fn from_terminal_colors(
        background: Option<Rgba>,
        foreground: Option<Rgba>,
        terminal: [Option<Rgba>; 16],
    ) -> Self {
        let mut palette = Self::default();
        palette.base = background.unwrap_or(palette.base);
        palette.text = foreground.unwrap_or_else(|| default_text_for(palette.base));
        palette.primary = terminal[5].unwrap_or(palette.primary);
        palette.accent = terminal[4].unwrap_or(palette.accent);
        palette.warning = terminal[3].unwrap_or(palette.warning);
        palette.success = terminal[2].unwrap_or(palette.success);
        palette.destructive = terminal[1].unwrap_or(palette.destructive);
        palette.derive_roles();
        palette
    }

    /// Recompute semantic UI roles after applying the terminal's background, foreground, and
    /// ANSI accents. The steps are relative to the active base color so light and dark themes
    /// retain the same restrained hierarchy.
    fn derive_roles(&mut self) {
        let dark = is_dark(self.base);
        let step = |amount| surface_step(self.base, dark, amount);

        self.mantle = surface_step(self.base, !dark, 0.12);
        self.pane = step(0.04);
        self.surface = step(0.08);
        self.hover = step(0.14);
        self.border = step(0.21);
        self.border_variant = step(0.14);
        self.border_focused = readable_color(self.base, self.accent);
        self.border_selected = self.border_focused;
        self.border_disabled = mix(self.base, self.border, 0.45);

        self.element_background = self.surface;
        self.element_hover = self.hover;
        self.element_active = step(0.18);
        self.element_selected = mix(self.surface, self.accent, 0.18);
        self.element_disabled = step(0.02);
        self.ghost_element_background = self.base;
        self.ghost_element_hover = self.hover;
        self.ghost_element_active = self.element_active;
        self.ghost_element_selected = self.element_selected;
        self.ghost_element_disabled = self.base;

        self.subtext = mix(self.text, self.base, 0.20);
        self.muted = mix(self.text, self.base, 0.52);
        self.text_placeholder = self.muted;
        self.text_disabled = mix(self.text, self.base, 0.62);
        self.text_accent = self.accent;
        self.icon = self.text;
        self.icon_muted = self.muted;
        self.icon_disabled = self.text_disabled;
        self.icon_accent = self.accent;

        self.tab_bar = self.mantle;
        self.tab_inactive = self.base;
        self.tab_active = self.base;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiTheme {
    pub palette: UiPalette,
}

fn mix(a: Rgba, b: Rgba, b_weight: f32) -> Rgba {
    let weight = b_weight.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| {
        f32::mul_add(f32::from(b), weight, f32::from(a) * (1.0 - weight))
            .round()
            .to_u8()
            .unwrap_or(0)
    };
    Rgba {
        red: channel(a.red, b.red),
        green: channel(a.green, b.green),
        blue: channel(a.blue, b.blue),
        alpha: channel(a.alpha, b.alpha),
    }
}

fn surface_step(base: Rgba, dark: bool, amount: f32) -> Rgba {
    let target = if dark {
        Rgba::rgb(u8::MAX, u8::MAX, u8::MAX)
    } else {
        Rgba::rgb(0, 0, 0)
    };
    mix(base, target, amount)
}

/// Pick the nearest readable version of a preferred color while retaining its hue where possible.
/// This is used for extension-owned primitives that may provide arbitrary RGB values.
#[must_use]
pub fn readable_color(background: Rgba, preferred: Rgba) -> Rgba {
    const MIN_CONTRAST: f32 = 4.5;
    if contrast_ratio(background, preferred) >= MIN_CONTRAST {
        return preferred;
    }

    let target = if is_dark(background) {
        Rgba::rgb(u8::MAX, u8::MAX, u8::MAX)
    } else {
        Rgba::rgb(0, 0, 0)
    };
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..8 {
        let weight = f32::midpoint(low, high);
        if contrast_ratio(background, mix(preferred, target, weight)) >= MIN_CONTRAST {
            high = weight;
        } else {
            low = weight;
        }
    }
    mix(preferred, target, high)
}

fn contrast_ratio(a: Rgba, b: Rgba) -> f32 {
    let (bright, dark) = if luminance(a) >= luminance(b) {
        (luminance(a), luminance(b))
    } else {
        (luminance(b), luminance(a))
    };
    (bright + 0.05) / (dark + 0.05)
}

fn luminance(value: Rgba) -> f32 {
    let channel = |value: u8| {
        let value = f32::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.0722f32.mul_add(
        channel(value.blue),
        0.7152f32.mul_add(channel(value.green), 0.2126 * channel(value.red)),
    )
}

fn default_text_for(background: Rgba) -> Rgba {
    if is_dark(background) {
        Rgba::rgb(0xf4, 0xf4, 0xf5)
    } else {
        Rgba::rgb(0x18, 0x18, 0x1b)
    }
}

fn is_dark(color: Rgba) -> bool {
    luminance(color) < 0.5
}
