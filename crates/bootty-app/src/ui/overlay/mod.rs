//! Shared framework for floating windows (session pickers, prompts, menus).
//!
//! [`FloatingWindow`] draws the normalized chrome — a dimming scrim, a centered
//! rounded panel with an icon+title header and an optional footer — and hands an
//! inner `Ui` to the caller's body, which typically hosts a [`ListView`] (or, in
//! later windows, a text prompt or action menu). The body decides what closes
//! the window via the returned [`OverlayResult`].

pub mod list;
pub mod menu;
pub mod prompt;

use bootty_ui::{Theme, ThemePalette};
use eframe::egui::{self, Color32, CornerRadius, Stroke};

use crate::ui::icons;

pub use list::{ListOutcome, ListRow, ListView};
pub use menu::{ActionItem, ActionMenu, ActionMenuOutcome, ActionRisk, StatusLine};
pub use prompt::{PromptOutcome, TextPrompt};

/// What a [`FloatingWindow`] produced for one frame.
pub struct OverlayResult<R> {
    /// The body closure's return value.
    pub inner: R,
    /// Escape was pressed this frame.
    pub escaped: bool,
    /// The scrim (anywhere outside the panel) was clicked this frame.
    pub clicked_outside: bool,
}

/// A centered modal overlay with normalized chrome. Build it per frame, then
/// [`FloatingWindow::show`] it with a body closure.
pub struct FloatingWindow {
    id: egui::Id,
    title: String,
    icon: Option<String>,
    hint: String,
    footer: Option<String>,
    width: f32,
}

impl FloatingWindow {
    pub fn new(id_source: impl std::hash::Hash, title: impl Into<String>) -> Self {
        Self {
            id: egui::Id::new(id_source),
            title: title.into(),
            icon: None,
            hint: String::new(),
            footer: None,
            width: 720.0,
        }
    }

    /// Leading header icon slug (resolved through `ui::icons`).
    #[must_use]
    pub fn icon(mut self, slug: impl Into<String>) -> Self {
        self.icon = Some(slug.into());
        self
    }

    /// Right-aligned header key hint, e.g. `"Enter select   Esc close"`.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// Footer status line, drawn under a rule below the body.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    #[must_use]
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn show<R>(
        self,
        ctx: &egui::Context,
        theme: Theme,
        add_body: impl FnOnce(&mut egui::Ui, ThemePalette) -> R,
    ) -> OverlayResult<R> {
        let palette = theme.palette;
        let screen = ctx.input(|input| input.content_rect());

        // The scrim sits in its own area below the panel (added first => lower in
        // the same order). A click that reaches it landed outside the panel.
        let clicked_outside = egui::Area::new(self.id.with("scrim"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(
                    screen,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 150),
                );
                response.clicked()
            })
            .inner;

        let mut body = None;
        egui::Area::new(self.id)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                bootty_ui::configure_style(ui.style_mut(), theme);
                egui::Frame::popup(ui.style())
                    .fill(palette.pane)
                    .stroke(Stroke::new(1.0, palette.border))
                    .corner_radius(CornerRadius::same(palette.radius))
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(self.width);
                        self.header(ui, palette);
                        rule(ui, palette);
                        ui.add_space(8.0);
                        body = Some(add_body(ui, palette));
                        if let Some(footer) = &self.footer {
                            ui.add_space(6.0);
                            rule(ui, palette);
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(footer)
                                    .monospace()
                                    .size(12.0)
                                    .color(palette.muted),
                            );
                        }
                    });
            });

        let escaped = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        OverlayResult {
            inner: body.expect("floating-window body always runs"),
            escaped,
            clicked_outside,
        }
    }

    fn header(&self, ui: &mut egui::Ui, palette: ThemePalette) {
        ui.horizontal(|ui| {
            if let Some(slug) = &self.icon
                && let Some(text) = icons::icon_text(slug, 16.0, palette.warning)
            {
                ui.label(text);
                ui.add_space(2.0);
            }
            ui.label(
                egui::RichText::new(&self.title)
                    .monospace()
                    .size(15.0)
                    .color(palette.warning),
            );
            if !self.hint.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(&self.hint)
                            .monospace()
                            .size(12.0)
                            .color(palette.muted),
                    );
                });
            }
        });
    }
}

/// A flat, borderless single-line filter field. Returns the text-edit response
/// so callers can request focus on the first frame.
pub fn filter_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    buf: &mut String,
    theme: Theme,
    hint: &str,
) -> egui::Response {
    bootty_ui::flat_text_edit_singleline(ui, buf, theme, |edit| {
        edit.id(id).desired_width(f32::INFINITY).hint_text(hint)
    })
}

fn rule(ui: &mut egui::Ui, palette: ThemePalette) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, palette.border);
}

/// Width for a centered overlay panel: `preferred`, shrunk to keep a fixed screen
/// margin, but never below `min`. Shared so every overlay sizes the same way.
#[must_use]
pub fn panel_width(ctx: &egui::Context, preferred: f32, min: f32) -> f32 {
    let available = ctx.input(|input| input.content_rect().width());
    preferred.min((available - 72.0).max(min))
}

/// Height cap for an overlay's scrolling list: the viewport height minus chrome,
/// clamped to `[min, max]`.
#[must_use]
pub fn list_max_height(ctx: &egui::Context, min: f32, max: f32) -> f32 {
    let available = ctx.input(|input| input.content_rect().height());
    (available - 200.0).clamp(min, max)
}

/// Parse a `chord=action` keybind string into `(chord, action)`, dropping any
/// scope/flag prefixes (`all:`, `global:`, `unconsumed:`, `performable:`) from the
/// chord. The action name never contains `=`, so the *last* `=` is the divider —
/// this keeps a literal `=` key (e.g. `cmd+=`) intact.
#[must_use]
pub fn parse_keybind(raw: &str) -> Option<(String, String)> {
    let (mut chord, action) = raw.rsplit_once('=')?;
    chord = chord.trim();
    while let Some(("all" | "global" | "unconsumed" | "performable", rest)) = chord.split_once(':')
    {
        chord = rest;
    }
    let action = action.trim();
    (!chord.is_empty() && !action.is_empty()).then(|| (chord.to_owned(), action.to_owned()))
}

/// Case-insensitive subsequence match — the picker filter shared across overlays.
#[must_use]
pub fn fuzzy_match(candidate: &str, pattern: &str) -> bool {
    let mut remaining = pattern.chars().flat_map(char::to_lowercase);
    let mut current = remaining.next();
    if current.is_none() {
        return true;
    }
    for ch in candidate.chars().flat_map(char::to_lowercase) {
        if Some(ch) == current {
            current = remaining.next();
            if current.is_none() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keybind_splits_on_last_equals_and_strips_flags() {
        assert_eq!(
            parse_keybind("cmd+p=command_palette"),
            Some(("cmd+p".to_owned(), "command_palette".to_owned()))
        );
        // The literal `=` key survives: the action name never contains `=`, so the
        // last `=` is the divider (a first-`=` split would garble `cmd+=`).
        assert_eq!(
            parse_keybind("cmd+==increase_font_size:1"),
            Some(("cmd+=".to_owned(), "increase_font_size:1".to_owned()))
        );
        // Scope/flag prefixes are dropped from the chord.
        assert_eq!(
            parse_keybind("performable:cmd+v=paste_from_clipboard"),
            Some(("cmd+v".to_owned(), "paste_from_clipboard".to_owned()))
        );
        // Leader sequences keep their `>` chain.
        assert_eq!(
            parse_keybind("ctrl+space>r=rename_session"),
            Some(("ctrl+space>r".to_owned(), "rename_session".to_owned()))
        );
        assert_eq!(parse_keybind("no-equals"), None);
        assert_eq!(parse_keybind("cmd+x="), None);
    }

    #[test]
    fn fuzzy_match_is_case_insensitive_subsequence() {
        assert!(fuzzy_match("bootty", "bty"));
        assert!(fuzzy_match("Dotfiles", "df"));
        assert!(fuzzy_match("anything", ""));
        assert!(!fuzzy_match("bootty", "xyz"));
        assert!(!fuzzy_match("ab", "abc"));
    }
}
