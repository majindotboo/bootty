//! Theme colors retain their original setting identity and config writeback owner.

use gpui_kit::base::StyledExt as _;

use super::{
    GpuiSettings, ScalarValue, SettingsCategory, SettingsControl, SettingsPageItem, SettingsRow,
    ThemeColorGroup,
};
use bootty_config::config::ColorConfig;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::Input,
    label::Label,
    tab::{Tab, TabBar},
};
use gpui_kit::{
    AnyElement, App, Context, IntoElement, ParentElement, SharedString, Styled, Window, div,
    prelude::*, rems,
};

impl GpuiSettings {
    pub(crate) fn set_theme_preview(&mut self, colors: [ColorConfig; 2]) {
        self.theme_preview_colors = colors;
    }

    pub fn show_theme(&mut self, cx: &mut Context<Self>) {
        self.select_category(SettingsCategory::Theme, cx);
    }

    pub fn focus_theme_setting(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(group) = ThemeColorGroup::for_setting(id) {
            self.theme_group = group;
        }
        if id.starts_with("appearance.light.colors.") {
            "appearance.light.colors".clone_into(&mut self.theme_branch);
        } else if id.starts_with("appearance.dark.colors.") {
            "appearance.dark.colors".clone_into(&mut self.theme_branch);
        }
        self.show_theme(cx);
    }

    pub fn show_settings(&mut self, cx: &mut Context<Self>) {
        if self.category == SettingsCategory::Theme {
            self.select_category(SettingsCategory::General, cx);
        }
    }

    fn render_theme_groups(&self, cx: &Context<Self>) -> AnyElement {
        let mut groups = div().flex().flex_wrap().gap_1();
        for group in ThemeColorGroup::ALL {
            groups = groups.child(
                Button::new(SharedString::from(format!("theme-group-{group:?}")))
                    .debug_selector(move || format!("theme-group-{group:?}"))
                    .label(group.label())
                    .small()
                    .ghost()
                    .selected(self.theme_group == group)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.theme_group = group;
                        cx.notify();
                    })),
            );
        }
        let branch = self.theme_branch.clone();
        let mut scopes = div().flex().gap_1();
        if !matches!(
            self.theme_group,
            ThemeColorGroup::Window | ThemeColorGroup::Sidebar
        ) {
            for (prefix, label) in [
                ("appearance.light.colors", "Light"),
                ("appearance.dark.colors", "Dark"),
            ] {
                scopes = scopes.child(
                    Button::new(SharedString::from(format!("theme-scope-{label}")))
                        .label(label)
                        .small()
                        .outline()
                        .selected(branch == prefix)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            prefix.clone_into(&mut this.theme_branch);
                            cx.notify();
                        })),
                );
            }
        }
        div()
            .v_flex()
            .gap_2()
            .child(groups)
            .child(scopes)
            .into_any_element()
    }

    fn render_theme_cards(
        &self,
        rows: &[SettingsRow],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let branch = &self.theme_branch;
        let mut colors = div().flex().flex_wrap().gap_3();
        for row in rows {
            let (SettingsRow::Value { id, .. } | SettingsRow::AnsiPalette { id, .. }) = row else {
                continue;
            };
            if ThemeColorGroup::for_setting(id) != Some(self.theme_group) {
                continue;
            }
            if !matches!(
                self.theme_group,
                ThemeColorGroup::Window | ThemeColorGroup::Sidebar
            ) && !id.starts_with(&format!("{branch}."))
            {
                continue;
            }
            let searchable =
                format!("theme colors {} {id} {row:?}", self.theme_group.label()).to_lowercase();
            if self.has_query
                && !self
                    .search
                    .to_lowercase()
                    .split_whitespace()
                    .all(|token| searchable.contains(token))
            {
                continue;
            }
            let control = if let SettingsRow::Value {
                id,
                label,
                value: ScalarValue::Text(_),
                control: SettingsControl::Color,
                enabled,
                ..
            } = row
            {
                let path: Vec<_> = id.split('.').collect();
                let value = self
                    .draft
                    .draft_document()
                    .str_at(&path)
                    .unwrap_or_default();
                div()
                    .v_flex()
                    .gap_2()
                    .w(rems(13.))
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(Label::new(label.clone()).text_sm())
                    .child(Self::render_color_control(
                        id,
                        label,
                        value,
                        *enabled,
                        !value.is_empty(),
                        window,
                        cx,
                    ))
                    .into_any_element()
            } else {
                div()
                    .w_full()
                    .child(self.render_setting(row, window, cx))
                    .into_any_element()
            };
            colors = colors.child(control);
        }
        colors.into_any_element()
    }

    pub(super) fn render_theme_settings(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The Theme page has no category sidebar. Stack before the palette and preview
        // would compete for the same horizontal space, including at enlarged UI sizes.
        let stacked = f32::from(window.viewport_size().width) < f32::from(window.rem_size()) * 49.0;
        let rows: Vec<_> = self
            .content
            .pages
            .iter()
            .find(|page| page.category == SettingsCategory::Theme)
            .into_iter()
            .flat_map(|page| &page.items)
            .flat_map(|item| match item {
                SettingsPageItem::Setting(row) => vec![row.clone()],
                SettingsPageItem::Dependent { parent, children } => std::iter::once(parent.clone())
                    .chain(children.clone())
                    .collect(),
                SettingsPageItem::SectionHeader { .. } => Vec::new(),
            })
            .collect();
        let controls = rows.iter().filter(|row| matches!(row, SettingsRow::Value{id, ..} if id == "appearance.mode" || id == "appearance.light.theme" || id == "appearance.dark.theme"));
        let mut header = div().v_flex().gap_3();
        if let Some(input) = &self.search_input {
            header = header.child(crate::gpui::focus_input(
                input,
                Input::new(input)
                    .aria_label("Search theme colors")
                    .cleanable(true)
                    .small()
                    .w_full(),
            ));
        }
        let mut themes = div().flex().gap_3();
        for row in controls {
            if matches!(row, SettingsRow::Value { id, .. } if id == "appearance.mode") {
                header = header.child(self.render_theme_selector(row, window, cx));
            } else {
                themes = themes.child(self.render_theme_selector(row, window, cx));
            }
        }
        header = header.child(themes);
        let groups = self.render_theme_groups(cx);
        let colors = self.render_theme_cards(&rows, window, cx);
        let editor = div()
            .v_flex()
            .gap_4()
            .when(!stacked, Styled::flex_1)
            .when(stacked, Styled::flex_shrink_0)
            .min_w_0()
            .child(header)
            .child(groups)
            .child(colors)
            .when_some(self.content.write_error.as_ref(), |this, error| {
                this.child(Label::new(error.clone()).text_color(cx.theme().danger))
            })
            .id("theme-colors-scroll")
            .debug_selector(|| "theme-colors-editor".to_owned());
        let editor = if stacked {
            editor.into_any_element()
        } else {
            editor.overflow_y_scrollbar().into_any_element()
        };
        let base = if self.theme_branch == "appearance.light.colors" {
            &self.theme_preview_colors[0]
        } else {
            &self.theme_preview_colors[1]
        };
        let preview = super::theme_colors_from_document(
            self.draft.draft_document(),
            &self.theme_branch,
            base,
        );
        let layout = div()
            .id("theme-layout-scroll")
            .flex()
            .when(stacked, Styled::flex_col)
            .size_full()
            .min_w_0()
            .min_h_0()
            .gap_4()
            .p_4()
            .child(editor)
            .child(render_theme_preview(
                &preview,
                self.draft.draft_document(),
                cx,
            ));
        if stacked {
            layout.overflow_y_scrollbar().into_any_element()
        } else {
            layout.into_any_element()
        }
    }
}

/// These are the same native controls and active color tokens used by the workspace.
pub(super) fn render_theme_preview(
    colors: &ColorConfig,
    document: &bootty_config::config::ConfigDocument,
    cx: &App,
) -> AnyElement {
    let override_color = |path: &[&str], fallback| {
        document
            .str_at(path)
            .and_then(|value| bootty_config::color::Color::from_hex(value).ok())
            .map_or(fallback, color)
    };
    let sidebar = override_color(&["sidebar", "background"], cx.theme().sidebar);
    let sidebar_foreground =
        override_color(&["sidebar", "foreground"], cx.theme().sidebar_foreground);
    let selected = override_color(&["sidebar", "selected"], cx.theme().sidebar_accent);
    let hovered = override_color(&["sidebar", "hover"], cx.theme().list_hover);
    let terminal_background = colors.background.map_or(cx.theme().background, color);
    let terminal_foreground = colors.foreground.map_or(cx.theme().foreground, color);
    let selection = colors
        .selection_background
        .map_or(cx.theme().selection, color);
    let mut palette = div().flex().flex_wrap().gap_1();
    for (index, value) in colors.palette.iter().take(16).enumerate() {
        palette = palette.child(
            div()
                .id(("theme-preview-ansi", index))
                .size_4()
                .rounded_sm()
                .bg(color(*value)),
        );
    }
    div()
        .id("theme-live-preview")
        .debug_selector(|| "theme-live-preview".to_owned())
        .v_flex()
        .gap_3()
        .w(rems(18.))
        .min_w(rems(14.))
        .flex_shrink_0()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(Label::new("Live preview").text_sm())
        .child(
            TabBar::new("theme-preview-tabs")
                .selected_index(0)
                .child(Tab::new().label("Terminal"))
                .child(Tab::new().label("Changes")),
        )
        .child(
            div()
                .v_flex()
                .bg(sidebar)
                .text_color(sidebar_foreground)
                .p_2()
                .gap_1()
                .rounded_md()
                .child(
                    div()
                        .p_2()
                        .rounded_sm()
                        .bg(selected)
                        .child("Improve agent startup"),
                )
                .child(
                    div()
                        .p_2()
                        .rounded_sm()
                        .hover(move |style| style.bg(hovered))
                        .child("Review terminal colors"),
                ),
        )
        .child(
            div()
                .v_flex()
                .gap_2()
                .p_3()
                .rounded_md()
                .bg(terminal_background)
                .text_color(terminal_foreground)
                .font_family("monospace")
                .child("$ cargo check")
                .child("Checking bootty-ui…")
                .child(div().px_1().rounded_sm().bg(selection).child("src/main.rs"))
                .child(palette),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("theme-preview-default")
                        .label("New task")
                        .small(),
                )
                .child(
                    Button::new("theme-preview-outline")
                        .label("Open terminal")
                        .outline()
                        .small(),
                ),
        )
        .into_any_element()
}

fn color(value: bootty_config::color::Color) -> gpui_kit::Hsla {
    gpui_kit::rgba(
        u32::from(value.r) << 24
            | u32::from(value.g) << 16
            | u32::from(value.b) << 8
            | u32::from(value.a),
    )
    .into()
}
