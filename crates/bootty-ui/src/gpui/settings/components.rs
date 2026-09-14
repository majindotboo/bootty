//! Settings page, navigation, and Kit control composition.
//!
//! The page/list/navbar structure is ported from Zed's `settings_ui` at commit
//! `1662f5f3f6` (`settings_ui.rs:1046-1452,2092-2186,2991-3286,3433-3646`).
//! Bootty's DTOs are the narrow adapter at the control callbacks.

use gpui_kit::component::{
    Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    label::Label,
};

use crate::gpui::color_picker::{ColorPickerParams, ColorPickerUpdate, render_color_picker};

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    ops::Range,
    rc::Rc,
};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    Size,
    input::{
        Input as ComponentInput, InputEvent as ComponentInputEvent,
        InputState as ComponentInputState, MaskPattern, NumberInput,
    },
    kbd::Kbd,
    slider::{Slider as ComponentSlider, SliderEvent, SliderState},
    switch::Switch as ComponentSwitch,
};
use gpui_kit::{
    AnyElement, Context, Entity, FocusHandle, Focusable as _, IntoElement, KeyContext,
    KeyDownEvent, ParentElement, Render, Role, SharedString, Styled, Subscription, Window, div,
    prelude::*, rems, uniform_list,
};

use super::{
    ToggleFocusNav,
    inline_inputs::InlineInputTarget,
    model::{
        AnsiPalettePreset, ModifierRemap, ModifierRemapField, ModuleIntegrationStatus,
        ModuleIntegrationsSnapshot, ModuleSourceIntent, NumberControl, RemoteEditorSnapshot,
        RemoteTestState, ScalarValue, SettingsCategory, SettingsChoice, SettingsControl,
        SettingsIntent, SettingsPageItem, SettingsRow, StatusSegmentsSnapshot,
    },
    picker::{
        render_compact_dropdown, render_dropdown, render_searchable_picker, uses_searchable_picker,
    },
    window::{GpuiSettings, NAVBAR_GROUP_TAB_INDEX},
};

/// Sidebar width in the UI's zoom-aware unit. The pixel alias remains for geometry assertions in
/// host-neutral tests and should not be used for styling.
pub const SETTINGS_SIDEBAR_WIDTH_REMS: f32 = 14.125;
pub const SETTINGS_SIDEBAR_WIDTH_PX: f32 = 226.0;
/// Width of a number editor, including its two stepper buttons, in the UI's zoom-aware unit.
///
/// The component reserves two `2rem` buttons before laying out the editable text region. Keep
/// enough rems for the value and suffix at the largest supported UI font size instead of letting
/// flexbox squeeze the text into a clipped sliver.
const SETTINGS_NUMBER_INPUT_WIDTH_REMS: f32 = 10.0;

struct NumberControlState {
    id: String,
    range: std::ops::RangeInclusive<f32>,
    precision: usize,
    display_scale: f32,
    optional: bool,
    owner: Entity<GpuiSettings>,
    input: Entity<ComponentInputState>,
    slider: Option<Entity<SliderState>>,
    external_value: f32,
    current_value: f32,
    slider_preview: Option<f32>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TextControlTarget {
    Setting(String),
    Remote {
        profile_id: String,
        field_id: String,
    },
}

struct TextControlState {
    target: TextControlTarget,
    owner: Entity<GpuiSettings>,
    input: Entity<ComponentInputState>,
    external_value: String,
    current_value: String,
    _subscription: Subscription,
}

impl TextControlState {
    fn on_input_event(
        &mut self,
        input: &Entity<ComponentInputState>,
        event: &ComponentInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComponentInputEvent::Change => {
                let value = input.read(cx).value().to_string();
                if self.current_value == value {
                    return;
                }
                self.current_value.clone_from(&value);
                let intent = match &self.target {
                    TextControlTarget::Setting(id) => SettingsIntent::SetText {
                        id: id.clone(),
                        value,
                    },
                    TextControlTarget::Remote {
                        profile_id,
                        field_id,
                    } => SettingsIntent::SetRemoteField {
                        profile_id: profile_id.clone(),
                        field_id: field_id.clone(),
                        value,
                    },
                };
                self.owner
                    .update(cx, |settings, cx| settings.emit(intent, cx));
            }
            ComponentInputEvent::PressEnter { .. } => {
                if matches!(&self.target, TextControlTarget::Remote { .. }) {
                    self.owner.update(cx, |settings, cx| {
                        settings.finish_inline_input(window, cx);
                    });
                }
            }
            ComponentInputEvent::Focus => {
                if let TextControlTarget::Remote {
                    profile_id,
                    field_id,
                } = &self.target
                {
                    let editor = format!("remote-field:{profile_id}:{field_id}");
                    self.owner.update(cx, |settings, cx| {
                        settings.focus_editor(Some(editor), cx);
                    });
                }
            }
            ComponentInputEvent::Blur => {
                if matches!(&self.target, TextControlTarget::Remote { .. }) {
                    self.owner.update(cx, |settings, cx| {
                        settings.focus_editor(None, cx);
                    });
                }
            }
        }
    }

    fn sync_external_value(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.external_value == value {
            return;
        }
        self.external_value.clear();
        self.external_value.push_str(value);
        if self.current_value == value {
            return;
        }
        self.current_value.clear();
        self.current_value.push_str(value);
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

impl NumberControlState {
    fn on_input_event(
        &mut self,
        input: &Entity<ComponentInputState>,
        event: &ComponentInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, ComponentInputEvent::Change) && self.slider_preview.is_some() {
            return;
        }
        let commit = matches!(
            event,
            ComponentInputEvent::Blur | ComponentInputEvent::PressEnter { .. }
        );
        if !commit && !matches!(event, ComponentInputEvent::Change) {
            return;
        }

        let text = input.read(cx).value();
        if text.trim().is_empty() {
            if commit && self.optional {
                self.owner.update(cx, |settings, cx| {
                    settings.emit(SettingsIntent::RemoveValue(self.id.clone()), cx);
                });
            } else if commit {
                self.sync_input(self.current_value, window, cx);
            }
            return;
        }

        let Some(raw_value) = parse_number_input(&text, self.display_scale) else {
            if commit {
                self.sync_input(self.current_value, window, cx);
            }
            return;
        };
        if !commit && !self.range.contains(&raw_value) {
            return;
        }
        let Some(value) =
            normalize_number_input(raw_value, &self.range, self.precision, self.display_scale)
        else {
            return;
        };

        if commit {
            self.sync_input(value, window, cx);
        }
        if let Some(slider) = &self.slider {
            slider.update(cx, |slider, cx| slider.set_value(value, window, cx));
        }
        self.emit_value(value, cx);
    }

    fn on_slider_event(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (value, commit) = match event {
            SliderEvent::Change(value) => (value, false),
            SliderEvent::Release(value) => (value, true),
        };
        let Some(value) = normalize_number_input(
            value.start(),
            &self.range,
            self.precision,
            self.display_scale,
        ) else {
            return;
        };
        // Dragging previews the control locally; persist and rebuild the terminal once on release.
        self.slider_preview = Some(value);
        self.sync_input(value, window, cx);
        if commit {
            self.emit_value(value, cx);
            self.slider_preview = None;
        }
    }

    #[expect(
        clippy::float_cmp,
        reason = "Exact accepted values deduplicate control events and prevent feedback loops."
    )]
    fn emit_value(&mut self, value: f32, cx: &mut Context<Self>) {
        if self.current_value == value {
            return;
        }
        self.current_value = value;
        self.owner.update(cx, |settings, cx| {
            settings.emit(
                SettingsIntent::SetValue {
                    id: self.id.clone(),
                    value: ScalarValue::Number(value),
                },
                cx,
            );
        });
    }

    #[expect(
        clippy::float_cmp,
        reason = "Exact accepted values deduplicate control events and prevent feedback loops."
    )]
    fn sync_external_value(&mut self, value: f32, window: &mut Window, cx: &mut Context<Self>) {
        if self.external_value == value {
            return;
        }
        self.external_value = value;
        self.slider_preview = None;
        self.current_value = value;
        self.sync_input(value, window, cx);
        if let Some(slider) = &self.slider {
            slider.update(cx, |slider, cx| slider.set_value(value, window, cx));
        }
    }

    fn sync_input(&self, value: f32, window: &mut Window, cx: &mut Context<Self>) {
        let value = format_number(value * self.display_scale, self.precision);
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

#[derive(Debug)]
pub(super) struct NavBarEntry {
    pub(super) title: String,
    pub(super) category: SettingsCategory,
    pub(super) is_root: bool,
    pub(super) expanded: bool,
    pub(super) page_index: usize,
    pub(super) item_index: Option<usize>,
    pub(super) section_id: Option<String>,
    pub(super) focus_handle: FocusHandle,
}

impl GpuiSettings {
    pub(super) fn rebuild_navbar(&mut self, cx: &gpui_kit::App) {
        let prior = self
            .navbar_entries
            .iter()
            .map(|entry| {
                (
                    (entry.category, entry.section_id.clone()),
                    (entry.expanded, entry.focus_handle.clone()),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut entries = Vec::new();

        for (page_index, page) in self.content.pages.iter().enumerate() {
            let prior_root = prior.get(&(page.category, None));
            entries.push(NavBarEntry {
                title: page.title.clone(),
                category: page.category,
                is_root: true,
                expanded: prior_root
                    .map_or(page.category == self.category, |(expanded, _)| *expanded),
                page_index,
                item_index: None,
                section_id: None,
                focus_handle: prior_root.map_or_else(
                    || cx.focus_handle().tab_index(0).tab_stop(true),
                    |(_, focus_handle)| focus_handle.clone(),
                ),
            });
            for (item_index, item) in page.items.iter().enumerate() {
                let SettingsPageItem::SectionHeader { id, title, .. } = item else {
                    continue;
                };
                let section_id = Some(id.clone());
                let prior_section = prior.get(&(page.category, section_id.clone()));
                entries.push(NavBarEntry {
                    title: title.clone(),
                    category: page.category,
                    is_root: false,
                    expanded: false,
                    page_index,
                    item_index: Some(item_index),
                    section_id,
                    focus_handle: prior_section.map_or_else(
                        || cx.focus_handle().tab_index(0).tab_stop(true),
                        |(_, focus_handle)| focus_handle.clone(),
                    ),
                });
            }
        }
        self.navbar_entries = entries;
    }

    pub(super) fn visible_navbar_indices(&self) -> Vec<usize> {
        let mut visible = Vec::new();
        let mut index = 0_usize;
        while let Some(entry) = self.navbar_entries.get(index) {
            let included = entry.item_index.map_or_else(
                || {
                    self.filter_table
                        .get(entry.page_index)
                        .is_some_and(|items| items.is_empty() || items.iter().any(|item| *item))
                },
                |item_index| {
                    self.filter_table
                        .get(entry.page_index)
                        .and_then(|items| items.get(item_index))
                        .copied()
                        .unwrap_or(false)
                },
            );
            if included {
                visible.push(index);
            }
            index = index.saturating_add(1);
            if included && entry.is_root && !entry.expanded && !self.has_query {
                while self
                    .navbar_entries
                    .get(index)
                    .is_some_and(|entry| !entry.is_root)
                {
                    index = index.saturating_add(1);
                }
            }
        }
        visible
    }

    fn render_search(input: &Entity<ComponentInputState>, cx: &Context<Self>) -> AnyElement {
        // Copied from Zed `SettingsWindow::render_search` (`settings_ui.rs:2991-3030`).
        div()
            .id("settings-ui-search")
            .debug_selector(|| "settings-search".to_owned())
            .mb_3()
            .child(crate::gpui::focus_input(
                input,
                ComponentInput::new(input)
                    .aria_label(crate::i18n::t(cx, "settings-search"))
                    .role(Role::SearchInput)
                    .prefix(Icon::new(IconName::Search).text_color(cx.theme().muted_foreground))
                    .cleanable(true)
                    .with_size(Size::Medium)
                    .w_full(),
            ))
            .into_any_element()
    }

    pub(super) fn render_nav(
        &self,
        input: &Entity<ComponentInputState>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let visible_count = self.visible_navbar_indices().len();
        let navigation_focused = self.navbar_focus.contains_focused(window, cx);
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("NavigationMenu");
        key_context.add("menu");
        if input.focus_handle(cx).is_focused(window) {
            key_context.add("search");
        }

        // Copied from Zed `SettingsWindow::render_nav` (`settings_ui.rs:3032-3286`).
        gpui_kit::div()
            .flex()
            .flex_col()
            .id("settings-sidebar")
            .debug_selector(|| "settings-sidebar".to_owned())
            .key_context(key_context)
            .on_key_down(cx.listener(|this, event, window, cx| {
                this.on_nav_key_down(event, window, cx);
            }))
            .w(rems(SETTINGS_SIDEBAR_WIDTH_REMS))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(gpui_kit::component::Theme::global(cx).colors.sidebar)
            .child(
                div()
                    .px_2p5()
                    .pt_2p5()
                    .child(Self::render_search(input, cx)),
            )
            .child(
                gpui_kit::div()
                    .flex()
                    .flex_col()
                    .id("settings-ui-nav")
                    .debug_selector(|| "settings-navigation".to_owned())
                    .role(Role::Tree)
                    .aria_label(crate::i18n::t(cx, "settings-navigation"))
                    .flex_1()
                    .overflow_hidden()
                    .track_focus(&self.navbar_focus)
                    .tab_group()
                    .tab_index(NAVBAR_GROUP_TAB_INDEX)
                    .child(
                        div().px_2p5().size_full().child(
                            uniform_list(
                                "settings-ui-nav-bar",
                                visible_count,
                                cx.processor(move |this, range: Range<usize>, _, cx| {
                                    let visible = this.visible_navbar_indices();
                                    visible
                                        .into_iter()
                                        .skip(range.start)
                                        .take(range.len())
                                        .filter_map(|entry_index| {
                                            this.render_navbar_entry(entry_index, cx)
                                        })
                                        .collect()
                                }),
                            )
                            .size_full()
                            .track_scroll(&self.navbar_scroll_handle),
                        ),
                    )
                    .vertical_scrollbar(&self.navbar_scroll_handle),
            )
            .child(self.render_focus_hint(navigation_focused, window, cx))
            .into_any_element()
    }

    fn render_navbar_entry(&self, entry_index: usize, cx: &Context<Self>) -> Option<AnyElement> {
        let entry = self.navbar_entries.get(entry_index)?;
        let page = self.content.pages.get(entry.page_index)?;
        let selected = !entry.is_root
            && page.category == self.category
            && self.active_section.as_deref() == entry.section_id.as_deref();
        let selector = if entry.is_root {
            format!("settings-category-{}", page.category.id())
        } else {
            format!(
                "settings-section-{}",
                entry.section_id.as_deref().unwrap_or_default()
            )
        };
        let debug_selector = selector.clone();
        let entity = cx.entity();
        let expanded = entry.expanded || self.has_query;
        let is_root = entry.is_root;

        Some(
            div()
                .relative()
                .w_full()
                .py_0p5()
                .child(
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || debug_selector)
                        .absolute()
                        .inset_0(),
                )
                .child(
                    gpui_kit::div()
                        .flex()
                        .items_center()
                        .w_full()
                        .when(!is_root, gpui_kit::Styled::pl_8)
                        .when(is_root, |row| {
                            let entity = entity.clone();
                            row.child(
                                Button::new(SharedString::from(format!("expand-{selector}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(if expanded {
                                        Icon::new(IconName::ChevronDown)
                                    } else {
                                        Icon::new(IconName::ChevronRight)
                                    })
                                    .accessibility_label(if expanded {
                                        "Collapse"
                                    } else {
                                        "Expand"
                                    })
                                    .on_click(move |_, window, app| {
                                        entity.update(app, |this, cx| {
                                            this.toggle_and_focus_navbar_entry(
                                                entry_index,
                                                window,
                                                cx,
                                            );
                                        });
                                    }),
                            )
                        })
                        .child(
                            gpui_kit::base::Button::new(SharedString::from(format!(
                                "nav-{selector}"
                            )))
                            .child(Label::new(entry.title.clone()))
                            .accessibility_label(entry.title.clone())
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .when(selected, |button| {
                                let selector = format!(
                                    "settings-active-section-{}",
                                    entry.section_id.as_deref().unwrap_or_default()
                                );
                                button
                                    .bg(cx.theme().list_active)
                                    .debug_selector(move || selector)
                            })
                            .hover(|style| style.bg(cx.theme().list_hover))
                            .flex_1()
                            .justify_start()
                            .track_focus(&entry.focus_handle)
                            .selected(selected)
                            .on_click(move |_, window, app| {
                                entity.update(app, |this, cx| {
                                    this.activate_navbar_entry(entry_index, true, true, window, cx);
                                });
                            }),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_page_item(
        &self,
        item: &SettingsPageItem,
        item_index: usize,
        bottom_border: bool,
        extra_bottom_padding: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match item {
            SettingsPageItem::SectionHeader { id, title, .. } => {
                Self::render_section_header(id, title, cx)
            }
            SettingsPageItem::Setting(row) => {
                let row_id = settings_row_identity(row)
                    .map_or_else(|| format!("index-{item_index}"), str::to_owned);
                gpui_kit::div()
                    .flex()
                    .flex_col()
                    .group("setting-item")
                    .px_8()
                    .child(
                        gpui_kit::div()
                            .flex()
                            .flex_col()
                            .id(SharedString::from(format!("settings-item-field-{row_id}")))
                            .pt_4()
                            .when(extra_bottom_padding, gpui_kit::Styled::pb_10)
                            .when(!extra_bottom_padding, gpui_kit::Styled::pb_4)
                            .child(self.render_setting(row, window, cx)),
                    )
                    .when(bottom_border, |this| {
                        this.child(
                            gpui_kit::div()
                                .w_full()
                                .h(gpui_kit::px(1.0))
                                .bg(cx.theme().border),
                        )
                    })
                    .into_any_element()
            }
            // Direct port of Zed's `SettingsPageItem::DynamicItem` treatment
            // (`settings_ui.rs:1270-1323`): the discriminating setting remains a normal row;
            // the active children form one indented, dashed group rather than independent rows.
            SettingsPageItem::Dependent { parent, children } => {
                let has_children = !children.is_empty();
                let parent_id = settings_row_identity(parent)
                    .map_or_else(|| format!("index-{item_index}"), str::to_owned);
                let dependent_selector = format!("settings-dependent-{parent_id}");
                let parent_selector = format!("settings-dependent-parent-{parent_id}");
                let dependent_debug_selector = dependent_selector.clone();
                let mut content = gpui_kit::div()
                    .flex()
                    .flex_col()
                    .id(SharedString::from(dependent_selector))
                    .debug_selector(move || dependent_debug_selector)
                    .child(
                        div()
                            .group("setting-item")
                            .px_8()
                            .id(SharedString::from(parent_selector.clone()))
                            .debug_selector(move || parent_selector)
                            .pt_4()
                            .when(extra_bottom_padding, gpui_kit::Styled::pb_10)
                            .when(!extra_bottom_padding, gpui_kit::Styled::pb_4)
                            .child(self.render_setting(parent, window, cx))
                            .when(has_children, gpui_kit::Styled::pb_4),
                    );

                if !has_children && bottom_border {
                    content = content.child(
                        gpui_kit::div().flex().items_center().px_8().child(
                            gpui_kit::div()
                                .w_full()
                                .h(gpui_kit::px(1.0))
                                .bg(cx.theme().border),
                        ),
                    );
                }

                for (index, child) in children.iter().enumerate() {
                    let is_last = index.saturating_add(1) == children.len();
                    let child_id = settings_row_identity(child)
                        .map_or_else(|| format!("index-{index}"), str::to_owned);
                    let child_selector = format!("settings-dependent-child-{parent_id}-{child_id}");
                    content = content.child(
                        div()
                            .id(SharedString::from(child_selector.clone()))
                            .debug_selector(move || child_selector)
                            .group("setting-sub-item")
                            .mx_8()
                            .p_4()
                            .border_t_1()
                            .when(is_last, gpui_kit::Styled::border_b_1)
                            .when(is_last && extra_bottom_padding, gpui_kit::Styled::mb_8)
                            .border_dashed()
                            .border_color(cx.theme().input)
                            .bg(cx.theme().button.opacity(0.2))
                            .child(self.render_setting(child, window, cx)),
                    );
                }

                content.into_any_element()
            }
        }
    }

    fn render_section_header(id: &str, title: &str, cx: &Context<Self>) -> AnyElement {
        let selector = format!("settings-content-section-{id}");
        let debug_selector = selector.clone();
        let label: SharedString = title.to_owned().into();

        // Copied from Zed `SettingsSectionHeader`
        // (`settings_ui/src/components/section_items.rs:31-60`).
        gpui_kit::div()
            .flex()
            .flex_col()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .role(Role::Heading)
            .aria_level(2)
            .aria_label(label.clone())
            .w_full()
            .px_8()
            .gap_1p5()
            .child(
                Label::new(label)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(cx.theme().mono_font_family.clone()),
            )
            .child(
                gpui_kit::div()
                    .w_full()
                    .h(gpui_kit::px(1.0))
                    .bg(cx.theme().border),
            )
            .into_any_element()
    }

    fn render_setting(
        &self,
        row: &SettingsRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let control = self.render_setting_control(row, window, cx);
        let (id, label, help, structured) = match row {
            SettingsRow::Section(_)
            | SettingsRow::Notice { .. }
            | SettingsRow::ModuleIntegrations(_)
            | SettingsRow::Remote(_) => return control,
            SettingsRow::Value {
                id, label, help, ..
            }
            | SettingsRow::AnsiPalette {
                id, label, help, ..
            }
            | SettingsRow::Action {
                id, label, help, ..
            } => (id, label, help, false),
            SettingsRow::StringList {
                id, label, help, ..
            } => (
                id,
                label,
                help,
                matches!(id.as_str(), "font.family" | "font.ui-family"),
            ),
            SettingsRow::ModifierRemaps {
                id, label, help, ..
            }
            | SettingsRow::Environment {
                id, label, help, ..
            }
            | SettingsRow::FontFeatures {
                id, label, help, ..
            } => (id, label, help, true),
            SettingsRow::StatusSegments(editor) => (&editor.id, &editor.label, &editor.help, true),
        };
        if structured {
            render_structured_settings_item_layout(id, label, help, control, Some(&self.draft), cx)
        } else {
            render_settings_item_layout(id, label, help, control, Some(&self.draft), cx)
        }
    }

    fn render_setting_control(
        &self,
        row: &SettingsRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            SettingsRow::Section(label) => Self::setting_section(label, cx),
            SettingsRow::Notice { text, destructive } => gpui_kit::component::alert::Alert::new(
                SharedString::from(format!("settings-notice-{text}")),
                text.clone(),
            )
            .with_variant(if *destructive {
                gpui_kit::component::alert::AlertVariant::Error
            } else {
                gpui_kit::component::alert::AlertVariant::Info
            })
            .banner()
            .into_any_element(),
            SettingsRow::Value {
                id,
                label,
                help,
                value,
                control,
                enabled,
            } => self.render_value_control(id, label, help, value, control, *enabled, window, cx),
            SettingsRow::AnsiPalette {
                id,
                colors,
                presets,
                ..
            } => Self::render_ansi_palette(
                id,
                colors,
                presets,
                !self.draft.is_default(id),
                window,
                cx,
            ),
            SettingsRow::Action {
                id,
                label,
                help,
                button,
                enabled,
            } => Self::setting_action(id, label, help, button, *enabled, cx).into_any_element(),
            SettingsRow::StringList {
                id,
                items,
                options,
                add_label,
                enabled,
                ..
            } => Self::render_string_list(id, items, options, add_label, *enabled, window, cx),
            SettingsRow::ModifierRemaps {
                mappings,
                choices,
                enabled,
                ..
            } => Self::render_modifier_remaps(mappings, choices, *enabled, window, cx),
            SettingsRow::Environment {
                id, items, enabled, ..
            } => Self::render_environment(id, items, *enabled, window, cx),
            SettingsRow::FontFeatures { .. } => self.font_feature_editor.as_ref().map_or_else(
                || gpui_kit::Empty.into_any_element(),
                |host| host.editor.clone().into_any_element(),
            ),
            SettingsRow::StatusSegments(editor) => Self::render_status_segments(editor, window, cx),
            SettingsRow::ModuleIntegrations(editor) => Self::render_module_integrations(editor, cx),
            SettingsRow::Remote(remote) => Self::render_remote_editor(remote, window, cx),
        }
    }

    fn setting_section(label: &str, cx: &Context<Self>) -> AnyElement {
        gpui_kit::div()
            .flex()
            .flex_col()
            .w_full()
            .gap_1p5()
            .child(
                Label::new(label.to_owned())
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                gpui_kit::div()
                    .w_full()
                    .h(gpui_kit::px(1.0))
                    .bg(cx.theme().border),
            )
            .into_any_element()
    }

    fn setting_action(
        id: &str,
        label: &str,
        help: &str,
        button: &str,
        enabled: bool,
        cx: &Context<Self>,
    ) -> Button {
        let entity = cx.entity();
        let intent = id.to_owned();
        Button::new(SharedString::from(format!("settings-action-{id}")))
            .label(button.to_owned())
            .outline()
            .small()
            .tab_index(0_isize)
            .accessibility_label(format!("{button} {label}"))
            .tooltip(help.to_owned())
            .disabled(!enabled)
            .on_click(move |_, _, app| {
                entity.update(app, |this, cx| {
                    this.emit(SettingsIntent::Invoke(intent.clone()), cx);
                });
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_value_control(
        &self,
        id: &str,
        label: &str,
        help: &str,
        value: &ScalarValue,
        control: &SettingsControl,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match control {
            SettingsControl::Toggle => Self::render_toggle(
                id,
                label,
                help,
                value.as_bool().unwrap_or(false),
                enabled,
                cx,
            ),
            SettingsControl::Text { placeholder, .. } => Self::render_setting_text_control(
                id,
                label,
                help,
                value.as_str().unwrap_or_default(),
                placeholder,
                enabled,
                window,
                cx,
            ),
            SettingsControl::Number {
                range,
                control,
                precision,
                suffix,
                display_scale,
                optional,
            } => self.render_number_control(
                id,
                label,
                value.as_number().unwrap_or_else(|| *range.start()),
                range.clone(),
                *control,
                *precision,
                suffix,
                *display_scale,
                *optional,
                enabled,
                window,
                cx,
            ),
            SettingsControl::Choice(choices) => {
                let current = value.as_str().unwrap_or_default();
                let id_for_intent = id.to_owned();
                let selector = format!("settings-choice-{id}");
                let intent = Rc::new(move |token| SettingsIntent::SetValue {
                    id: id_for_intent.clone(),
                    value: ScalarValue::Token(token),
                });
                if uses_searchable_picker(choices.len()) {
                    Self::render_searchable_picker(
                        selector, label, help, "options", current, choices, enabled, intent,
                        window, cx,
                    )
                } else {
                    Self::render_dropdown(
                        selector, label, help, current, choices, enabled, intent, window, cx,
                    )
                }
            }
            SettingsControl::Theme(choices) => {
                let current = value.as_str().unwrap_or_default();
                let id_for_intent = id.to_owned();
                Self::render_searchable_picker(
                    format!("settings-choice-{id}"),
                    label,
                    help,
                    "themes",
                    current,
                    choices,
                    enabled,
                    Rc::new(move |token| SettingsIntent::SetValue {
                        id: id_for_intent.clone(),
                        value: ScalarValue::Token(token),
                    }),
                    window,
                    cx,
                )
            }
            SettingsControl::Color => Self::render_color_control(
                id,
                &format!("{label}. {help}"),
                value.as_str().unwrap_or_default(),
                enabled,
                !self.draft.is_default(id),
                window,
                cx,
            ),
            SettingsControl::ReadOnly => Label::new(display_value(value))
                .text_color(cx.theme().muted_foreground)
                .into_any_element(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_dropdown(
        selector: String,
        label: &str,
        help: &str,
        current: &str,
        choices: &[SettingsChoice],
        enabled: bool,
        intent: Rc<dyn Fn(String) -> SettingsIntent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let on_change = Rc::new(move |token: String, app: &mut gpui_kit::App| {
            entity.update(app, |this, cx| {
                this.emit(intent(token), cx);
            });
        });
        render_dropdown(
            selector, label, help, current, choices, enabled, on_change, window, cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_compact_dropdown(
        selector: String,
        label: &str,
        help: &str,
        current: &str,
        choices: &[SettingsChoice],
        enabled: bool,
        intent: Rc<dyn Fn(String) -> SettingsIntent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let on_change = Rc::new(move |token: String, app: &mut gpui_kit::App| {
            entity.update(app, |this, cx| {
                this.emit(intent(token), cx);
            });
        });
        render_compact_dropdown(
            selector, label, help, current, choices, enabled, on_change, window, cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_searchable_picker(
        selector: String,
        label: &str,
        help: &str,
        catalog_name: &str,
        current: &str,
        choices: &[SettingsChoice],
        enabled: bool,
        intent: Rc<dyn Fn(String) -> SettingsIntent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let on_change = Rc::new(move |token: String, app: &mut gpui_kit::App| {
            entity.update(app, |this, cx| {
                this.emit(intent(token), cx);
            });
        });
        render_searchable_picker(
            selector,
            label,
            help,
            catalog_name,
            current,
            choices,
            enabled,
            on_change,
            window,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(
        clippy::float_cmp,
        reason = "Exact accepted values deduplicate slider notifications."
    )]
    fn render_number_control(
        &self,
        id: &str,
        label: &str,
        value: f32,
        range: std::ops::RangeInclusive<f32>,
        control: NumberControl,
        precision: usize,
        suffix: &str,
        display_scale: f32,
        optional: bool,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let display_scale = valid_display_scale(display_scale);
        let display_step = precision_step(precision);
        let value_step = (display_step / display_scale).max(f32::EPSILON);
        let range_start = *range.start();
        let range_end = *range.end();
        let state_key = SharedString::from(format!(
            "settings-number-state-{id}-{control:?}-{range_start}-{range_end}-{precision}-{display_scale}-{suffix}"
        ));
        let owner = cx.entity();
        let state_id = id.to_owned();
        let state_range = range;
        let state = window.use_keyed_state(state_key, cx, move |window, cx| {
            let input = cx.new(|cx| {
                ComponentInputState::new(window, cx)
                    .default_value(format_number(value * display_scale, precision))
                    .mask_pattern(MaskPattern::Number {
                        separator: None,
                        fraction: Some(precision),
                    })
                    .step(f64::from(display_step))
                    .min(f64::from(range_start * display_scale))
                    .max(f64::from(range_end * display_scale))
            });
            let slider = (control == NumberControl::Slider).then(|| {
                cx.new(|_| {
                    SliderState::new()
                        .max(range_end)
                        // Set max before min: SliderState starts with max=100, and updating
                        // thumb position while min is above that default panics for ranges such
                        // as sidebar width (120..=600).
                        .min(range_start)
                        .step(value_step)
                        .default_value(value)
                })
            });
            let mut subscriptions =
                vec![cx.subscribe_in(&input, window, NumberControlState::on_input_event)];
            if let Some(slider) = &slider {
                subscriptions.push(cx.subscribe_in(
                    slider,
                    window,
                    NumberControlState::on_slider_event,
                ));
                // The pinned Slider's accessibility actions notify without Change/Release.
                // Observe those discrete changes; drag events already mark a local preview.
                subscriptions.push(cx.observe_in(slider, window, |this, slider, window, cx| {
                    let value = slider.read(cx).value().start();
                    if this.slider_preview.is_none() && value != this.current_value {
                        this.sync_input(value, window, cx);
                        this.emit_value(value, cx);
                    }
                }));
            }
            NumberControlState {
                id: state_id,
                range: state_range,
                precision,
                display_scale,
                optional,
                owner,
                input,
                slider,
                external_value: value,
                current_value: value,
                slider_preview: None,
                _subscriptions: subscriptions,
            }
        });

        state.update(cx, |state, cx| {
            state.sync_external_value(value, window, cx);
        });
        let input = state.read(cx).input.clone();
        let slider = state.read(cx).slider.clone();
        let selector = format!("settings-number-{id}");
        let debug_selector = selector.clone();
        let input = Self::number_input(id, &input, suffix, enabled, cx);

        let mut content = gpui_kit::div()
            .flex()
            .items_center()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .items_center()
            .gap_2();
        if let Some(slider) = slider {
            let slider_selector = format!("settings-slider-{id}");
            let slider_debug_selector = slider_selector.clone();
            content = content.child(
                div()
                    .id(SharedString::from(slider_selector))
                    .debug_selector(move || slider_debug_selector)
                    .w_32()
                    .child(ComponentSlider::new(&slider).disabled(!enabled)),
            );
        }
        content = content.child(input);
        if optional && !self.draft.is_default(id) {
            content = content.child(Self::number_auto_button(id, label, enabled, cx));
        }
        content.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_setting_text_control(
        id: &str,
        label: &str,
        help: &str,
        value: &str,
        placeholder: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state_key = SharedString::from(format!("settings-text-state-{id}"));
        let state_id = id.to_owned();
        let initial_value = value.to_owned();
        let initial_placeholder = placeholder.to_owned();
        let owner = cx.entity();
        let state = window.use_keyed_state(state_key, cx, move |window, cx| {
            let input = cx.new(|cx| {
                ComponentInputState::new(window, cx)
                    .default_value(initial_value.clone())
                    .placeholder(initial_placeholder.clone())
            });
            let subscription = cx.subscribe_in(&input, window, TextControlState::on_input_event);
            TextControlState {
                target: TextControlTarget::Setting(state_id),
                owner,
                input,
                external_value: initial_value.clone(),
                current_value: initial_value,
                _subscription: subscription,
            }
        });
        state.update(cx, |state, cx| {
            state.sync_external_value(value, window, cx);
        });
        let input = state.read(cx).input.clone();
        let selector = format!("settings-input-{id}");
        let debug_selector = selector.clone();
        div()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .w(rems(22.5))
            .max_w_full()
            .child(crate::gpui::focus_input(
                &input,
                ComponentInput::new(&input)
                    // gpui-component exposes the editor's accessible name but not a separate
                    // description for this control. Keep the setting help in the real input's
                    // announced name until the component forwards aria_description.
                    .aria_label(format!("{label}. {help}"))
                    .disabled(!enabled)
                    .with_size(Size::Medium)
                    .w_full(),
            ))
            .into_any_element()
    }

    fn render_color_control(
        id: &str,
        label: &str,
        value: &str,
        enabled: bool,
        resettable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let id = id.to_owned();
        render_color_picker(
            ColorPickerParams {
                selector: format!("settings-color-control-{id}"),
                // ColorPicker currently exposes only an accessible name, not a description.
                // Include the setting help in that name rather than attaching it to a
                // non-semantic wrapper around the picker trigger.
                label,
                value,
                default_label: "Theme default",
                enabled,
                resettable,
            },
            Rc::new(move |update, app| {
                entity.update(app, |this, cx| match update {
                    ColorPickerUpdate::Set(value) => {
                        this.emit(
                            SettingsIntent::SetText {
                                id: id.clone(),
                                value,
                            },
                            cx,
                        );
                    }
                    ColorPickerUpdate::Reset => {
                        this.emit(SettingsIntent::RemoveValue(id.clone()), cx);
                        this.focus_editor(None, cx);
                    }
                });
            }),
            window,
            cx,
        )
    }

    fn render_ansi_palette(
        id: &str,
        colors: &[String],
        presets: &[AnsiPalettePreset],
        resettable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut content = gpui_kit::div()
            .flex()
            .flex_col()
            .w(rems(22.5))
            .max_w_full()
            .gap_2()
            .child(Self::ansi_palette_toolbar(
                id, colors, presets, resettable, cx,
            ));

        if colors.is_empty() {
            content = content.child(
                Label::new("Uses the active theme palette").text_color(cx.theme().muted_foreground),
            );
        } else {
            let controls = colors
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let entity = cx.entity();
                    let palette_id = id.to_owned();
                    let current_colors = colors.to_vec();
                    let label = format!("ANSI color {index}");
                    render_color_picker(
                        ColorPickerParams {
                            selector: format!("settings-ansi-palette-{id}-{index}"),
                            label: &label,
                            value,
                            default_label: "Theme",
                            enabled: true,
                            // Palette overrides are dense; only the whole palette can reset.
                            resettable: false,
                        },
                        Rc::new(move |update, app| {
                            let colors = match update {
                                ColorPickerUpdate::Set(value) => {
                                    let mut colors = current_colors.clone();
                                    let Some(color) = colors.get_mut(index) else {
                                        return;
                                    };
                                    *color = value;
                                    colors
                                }
                                ColorPickerUpdate::Reset => return,
                            };
                            entity.update(app, |this, cx| {
                                this.emit(
                                    SettingsIntent::ReplaceAnsiPalette {
                                        id: palette_id.clone(),
                                        colors,
                                    },
                                    cx,
                                );
                            });
                        }),
                        window,
                        cx,
                    )
                })
                .collect::<Vec<_>>();
            content = content.child(
                gpui_kit::div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_1()
                    .children(controls),
            );
        }

        content.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_font_stack_entry(
        id: &str,
        index: usize,
        selection: &crate::font_database::FontSelection,
        families: &[String],
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut families = families.to_vec();
        if !families.contains(&selection.family) {
            families.push(selection.family.clone());
            families.sort();
        }
        let choices = families
            .into_iter()
            .map(|family| SettingsChoice {
                token: family.clone(),
                label: family,
                description: None,
            })
            .collect::<Vec<_>>();
        let list = id.to_owned();
        let weight = selection.weight;
        let family = Self::render_searchable_picker(
            format!("settings-font-family-{id}-{index}"),
            "Font family",
            "",
            "fonts",
            &selection.family,
            &choices,
            enabled,
            Rc::new(move |family| SettingsIntent::SetStringListItem {
                id: list.clone(),
                index,
                value: crate::font_database::font_with_weight(&family, weight),
            }),
            window,
            cx,
        );
        let weights = selection
            .weights
            .iter()
            .cloned()
            .map(|(label, token)| SettingsChoice {
                label,
                token,
                description: None,
            })
            .collect::<Vec<_>>();
        let list = id.to_owned();
        let weight = Self::render_compact_dropdown(
            format!("settings-font-weight-{id}-{index}"),
            "Font weight",
            "",
            &selection.selected,
            &weights,
            enabled,
            Rc::new(move |value| SettingsIntent::SetStringListItem {
                id: list.clone(),
                index,
                value,
            }),
            window,
            cx,
        );
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .w_full()
            .child(div().flex_1().min_w_0().child(family))
            .child(div().w_32().flex_none().child(weight))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_string_list(
        id: &str,
        items: &[String],
        options: &[String],
        add_label: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = items.len();
        let is_font_stack = matches!(id, "font.family" | "font.ui-family");
        let mut add_button = Some(Self::string_list_add_button(
            id,
            add_label,
            is_font_stack,
            enabled,
            cx,
        ));
        let mut rows = Vec::new();
        for (index, value) in items.iter().enumerate() {
            let (control, row_label) =
                Self::string_list_control(id, index, value, options, enabled, window, cx);
            let row_selector = format!("settings-string-list-{id}-{index}");
            let drag_scope = ordered_collection_scope(&format!("string-list:{id}"), items);
            let entity = cx.entity();
            let move_id = id.to_owned();
            let focus_border = cx.theme().ring;
            let mut row = div()
                .id(SharedString::from(row_selector.clone()))
                .debug_selector({
                    let selector = row_selector.clone();
                    move || selector
                })
                .relative()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .gap_0p5()
                .focusable()
                .tab_stop(false)
                .focus_visible(move |style| style.border_1().border_color(focus_border));
            if enabled {
                row = row.child(ordered_row_drag_handle(
                    row_selector.clone(),
                    drag_scope.clone(),
                    index.to_string(),
                    row_label.clone(),
                    cx.theme().muted_foreground,
                ));
            }
            row = row.child(div().flex_1().min_w_0().child(control)).child(
                ordered_row_remove_button(row_selector.clone(), row_label, enabled, move |app| {
                    entity.update(app, |this, cx| {
                        this.emit(
                            SettingsIntent::RemoveStringListItem {
                                id: move_id.clone(),
                                index,
                            },
                            cx,
                        );
                    });
                }),
            );
            let movement_id = id.to_owned();
            row = OrderedRowMovement {
                selector: row_selector,
                scope: drag_scope,
                index,
                count,
                enabled,
            }
            .apply(
                row,
                move |index, offset| SettingsIntent::MoveStringListItem {
                    id: movement_id.clone(),
                    index,
                    offset,
                },
                cx,
            );
            if is_font_stack {
                row = row.child(div().w_7().flex_none().flex().justify_center().children(
                    if index.saturating_add(1) == count {
                        add_button.take()
                    } else {
                        None
                    },
                ));
            }
            rows.push(row.into_any_element());
        }
        gpui_kit::div()
            .flex()
            .flex_col()
            .w(rems(27.5))
            .max_w_full()
            .gap_1()
            .children(rows)
            .children(add_button.map(|button| div().flex().justify_end().child(button)))
            .into_any_element()
    }

    fn render_modifier_remaps(
        mappings: &[ModifierRemap],
        choices: &[SettingsChoice],
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = mappings.len();
        let drag_scope = ordered_collection_scope(
            "modifier-remap",
            mappings
                .iter()
                .map(|mapping| (&mapping.source, &mapping.target))
                .collect::<Vec<_>>(),
        );
        let rows = mappings
            .iter()
            .enumerate()
            .map(|(index, mapping)| {
                Self::modifier_remap_row(
                    mapping,
                    choices,
                    OrderedRowMovement {
                        selector: format!("settings-modifier-remap-{index}"),
                        scope: drag_scope.clone(),
                        index,
                        count,
                        enabled,
                    },
                    window,
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let add_entity = cx.entity();
        gpui_kit::div()
            .flex()
            .flex_col()
            .w_full()
            .gap_1()
            .children(rows)
            .child(
                div()
                    .id("settings-modifier-remap-add")
                    .debug_selector(|| "settings-modifier-remap-add".to_owned())
                    .child(
                        Button::new("settings-modifier-remap-add-button")
                            .label("+ Add modifier remap")
                            .outline()
                            .small()
                            .disabled(!enabled)
                            .on_click(move |_, _, app| {
                                add_entity.update(app, |this, cx| {
                                    this.emit(SettingsIntent::AddModifierRemap, cx);
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_module_integrations(
        editor: &ModuleIntegrationsSnapshot,
        cx: &Context<Self>,
    ) -> AnyElement {
        let integrations = editor
            .integrations
            .iter()
            .map(|integration| Self::module_integration_row(&editor.identity, integration, cx))
            .collect::<Vec<_>>();
        gpui_kit::div()
            .flex()
            .flex_col()
            .w_full()
            .gap_2()
            .when_some(editor.error.clone(), |this, error| {
                this.child(
                    gpui_kit::component::alert::Alert::error("module-integration-error", error)
                        .banner(),
                )
            })
            .children(integrations)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_remote_text_field(
        profile_id: String,
        field_id: String,
        selector: String,
        aria_label: String,
        value: &str,
        placeholder: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state_key = SharedString::from(format!(
            "settings-remote-input-state-{profile_id}-{field_id}"
        ));
        let initial_value = value.to_owned();
        let initial_placeholder = placeholder.to_owned();
        let owner = cx.entity();
        let state = window.use_keyed_state(state_key, cx, move |window, cx| {
            let input = cx.new(|cx| {
                ComponentInputState::new(window, cx)
                    .default_value(initial_value.clone())
                    .placeholder(initial_placeholder.clone())
            });
            let subscription = cx.subscribe_in(&input, window, TextControlState::on_input_event);
            TextControlState {
                target: TextControlTarget::Remote {
                    profile_id,
                    field_id,
                },
                owner,
                input,
                external_value: initial_value.clone(),
                current_value: initial_value,
                _subscription: subscription,
            }
        });
        state.update(cx, |state, cx| {
            state.sync_external_value(value, window, cx);
        });

        let input = state.read(cx).input.clone();
        let debug_selector = selector.clone();
        let escape_owner = cx.entity();
        div()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .w(rems(22.5))
            .max_w_full()
            .on_key_down(move |event, window, app| {
                if event.keystroke.key != "escape" {
                    return;
                }
                app.stop_propagation();
                escape_owner.update(app, |settings, cx| {
                    settings.finish_inline_input(window, cx);
                });
            })
            .child(crate::gpui::focus_input(
                &input,
                ComponentInput::new(&input)
                    .aria_label(aria_label)
                    .role(Role::TextInput)
                    .disabled(!enabled)
                    .with_size(Size::Medium)
                    .w_full(),
            ))
            .into_any_element()
    }

    fn render_remote_editor(
        remote: &RemoteEditorSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut fields = Vec::new();
        if let Some(profile) = &remote.profile {
            fields.extend(
                profile
                    .fields
                    .iter()
                    .map(|field| Self::remote_profile_field(&profile.id, field, window, cx)),
            );
            fields.extend(Self::remote_arguments(profile, window, cx));
        }

        let test = remote.profile.as_ref().and_then(|profile| {
            profile.test.clone().map(|intent| {
                let entity = cx.entity();
                Button::new(SharedString::from(format!("remote-test-{}", remote.id)))
                    .label("Test connection")
                    .outline()
                    .disabled(remote.test_state == RemoteTestState::Testing)
                    .on_click(move |_, _, app| {
                        entity.update(app, |this, cx| {
                            this.emit(SettingsIntent::TestRemote(intent.clone()), cx);
                        });
                    })
            })
        });
        let actions = remote.actions.iter().enumerate().map(|(index, action)| {
            let entity = cx.entity();
            let intent = action.id.clone();
            Button::new(SharedString::from(format!(
                "remote-action-{}-{index}",
                remote.id
            )))
            .label(action.label.clone())
            .outline()
            .on_click(move |_, _, app| {
                entity.update(app, |this, cx| {
                    this.emit(SettingsIntent::Invoke(intent.clone()), cx);
                });
            })
        });

        gpui_kit::div()
            .flex()
            .flex_col()
            .w_full()
            .gap_2()
            .child(Label::new(remote.label.clone()))
            .child(
                Label::new(remote.detail.clone())
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
            .when_some(remote.error.clone(), |this, error| {
                this.child(
                    gpui_kit::component::alert::Alert::error("remote-settings-error", error)
                        .banner(),
                )
            })
            .children(fields)
            .child(
                gpui_kit::div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_1()
                    .child(
                        Label::new(remote_test_label(&remote.test_state))
                            .text_sm()
                            .text_color(remote_test_color(&remote.test_state, cx)),
                    )
                    .children(test)
                    .children(actions),
            )
            .into_any_element()
    }
}

/// Wrap an element with the stable debug selector used by settings UI tests and diagnostics.
pub(super) fn debug_wrapper(selector: String, child: impl IntoElement) -> AnyElement {
    let debug_selector = selector.clone();
    div()
        .id(SharedString::from(selector))
        .debug_selector(move || debug_selector)
        .child(child)
        .into_any_element()
}

impl GpuiSettings {
    fn render_focus_hint(
        &self,
        navigation_focused: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        gpui_kit::div()
            .flex()
            .items_center()
            .px_2p5()
            .id("settings-focus-toggle")
            .debug_selector(move || {
                if navigation_focused {
                    "settings-focus-toggle-to-content".to_owned()
                } else {
                    "settings-focus-toggle-to-navbar".to_owned()
                }
            })
            .w_full()
            .h_8()
            .px_2()
            .py_1()
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().input)
            .child(
                div()
                    .id("settings-focus-hint")
                    .debug_selector(|| "settings-focus-hint".to_owned())
                    .h_full()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        gpui_kit::div()
                            .flex()
                            .items_center()
                            .id("settings-focus-hint-content")
                            .gap_1()
                            .debug_selector(|| "settings-focus-hint-content".to_owned())
                            .when_some(
                                Kbd::binding_for_action_in(
                                    &ToggleFocusNav,
                                    &self.navbar_focus,
                                    window,
                                ),
                                |this, key| this.child(crate::gpui::raised_kbd(key)),
                            )
                            .child(Label::new(if navigation_focused {
                                "Focus Content"
                            } else {
                                "Focus Navbar"
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_toggle(
        id: &str,
        label: &str,
        help: &str,
        selected: bool,
        enabled: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let intent_id = id.to_owned();
        let selector = format!("settings-toggle-{id}");
        let debug_selector = selector.clone();
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || debug_selector)
            .child(
                ComponentSwitch::new(SharedString::from(format!("kit-{selector}")))
                    .checked(selected)
                    // Switch exposes only an accessible name. Include the setting help
                    // there until gpui-component forwards a separate description.
                    .accessibility_label(format!("{label}. {help}"))
                    .tooltip(help.to_owned())
                    .disabled(!enabled)
                    .on_click(move |value, _, app| {
                        entity.update(app, |this, cx| {
                            this.emit(
                                SettingsIntent::SetValue {
                                    id: intent_id.clone(),
                                    value: ScalarValue::Bool(*value),
                                },
                                cx,
                            );
                        });
                    }),
            )
            .into_any_element()
    }

    fn number_auto_button(id: &str, label: &str, enabled: bool, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let intent_id = id.to_owned();
        let auto_selector = format!("settings-number-auto-{id}");
        let auto_debug_selector = auto_selector.clone();
        div()
            .id(SharedString::from(auto_selector.clone()))
            .debug_selector(move || auto_debug_selector)
            .child(
                Button::new(SharedString::from(format!("kit-{auto_selector}")))
                    .label("Auto")
                    .outline()
                    .small()
                    .tab_index(0_isize)
                    .tab_stop(false)
                    .accessibility_label(format!("Use automatic {label}"))
                    .disabled(!enabled)
                    .on_click(move |_, _, app| {
                        entity.update(app, |settings, cx| {
                            settings.emit(SettingsIntent::RemoveValue(intent_id.clone()), cx);
                        });
                    }),
            )
            .into_any_element()
    }

    fn string_list_control(
        id: &str,
        index: usize,
        value: &str,
        options: &[String],
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, String) {
        let target = InlineInputTarget::StringListItem {
            list: id.to_owned(),
            index,
        };
        let font_selection = matches!(id, "font.family" | "font.ui-family")
            .then(|| crate::font_database::font_selection(value));
        let control = if let Some(selection) = &font_selection {
            Self::render_font_stack_entry(id, index, selection, options, enabled, window, cx)
        } else if options.is_empty() {
            Self::render_inline_input(
                format!("settings-string-list-{id}-{index}"),
                target,
                value,
                "Enter a value",
                "List value",
                enabled,
                window,
                cx,
            )
        } else {
            let choices = options
                .iter()
                .map(|option| SettingsChoice {
                    token: option.clone(),
                    label: option.clone(),
                    description: None,
                })
                .collect::<Vec<_>>();
            let list_id = id.to_owned();
            let selector = format!("settings-string-list-{id}-{index}");
            let intent = Rc::new(move |value| SettingsIntent::SetStringListItem {
                id: list_id.clone(),
                index,
                value,
            });
            if uses_searchable_picker(choices.len()) {
                let catalog_name = if id.starts_with("font.") {
                    "fonts"
                } else {
                    "options"
                };
                Self::render_searchable_picker(
                    selector,
                    "Value",
                    "",
                    catalog_name,
                    value,
                    &choices,
                    enabled,
                    intent,
                    window,
                    cx,
                )
            } else {
                Self::render_dropdown(
                    selector, "Value", "", value, &choices, enabled, intent, window, cx,
                )
            }
        };
        let row_label =
            font_selection.map_or_else(|| value.to_owned(), |selection| selection.family);
        (control, row_label)
    }

    fn module_integration_row(
        identity: &str,
        integration: &super::model::ModuleIntegrationSnapshot,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (status, status_color, installed) = match integration.status {
            ModuleIntegrationStatus::Installed => ("Installed", cx.theme().success, true),
            ModuleIntegrationStatus::Partial => ("Partly installed", cx.theme().warning, false),
            ModuleIntegrationStatus::Missing => {
                ("Not installed", cx.theme().muted_foreground, false)
            }
        };
        let request = if installed {
            ModuleSourceIntent::UninstallIntegration {
                identity: identity.to_owned(),
                module: integration.module.clone(),
                id: integration.id.clone(),
            }
        } else {
            ModuleSourceIntent::InstallIntegration {
                identity: identity.to_owned(),
                module: integration.module.clone(),
                id: integration.id.clone(),
            }
        };
        let entity = cx.entity();
        let button_id = format!(
            "module-integration-{}-{}",
            integration.module, integration.id
        );
        gpui_kit::div()
            .flex()
            .flex_col()
            .w_full()
            .gap_0p5()
            .child(
                gpui_kit::div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(
                        gpui_kit::div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Label::new(integration.title.clone()))
                            .child(Label::new(status).text_sm().text_color(status_color)),
                    )
                    .child({
                        let debug_selector = button_id.clone();
                        div()
                            .id(SharedString::from(button_id.clone()))
                            .debug_selector(move || debug_selector)
                            .child(
                                Button::new(SharedString::from(format!("kit-{button_id}")))
                                    .label(if installed {
                                        format!("Remove {}", integration.title)
                                    } else {
                                        format!("Install {}", integration.title)
                                    })
                                    .outline()
                                    .small()
                                    .accessibility_label(if installed {
                                        format!("Remove integration {}", integration.title)
                                    } else {
                                        format!("Install integration {}", integration.title)
                                    })
                                    .on_click(move |_, _, app| {
                                        entity.update(app, |this, cx| {
                                            this.emit(SettingsIntent::Module(request.clone()), cx);
                                        });
                                    }),
                            )
                    }),
            )
            .when(!integration.summary.is_empty(), |this| {
                this.child(
                    Label::new(integration.summary.clone())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
            })
            .into_any_element()
    }

    fn remote_profile_field(
        profile_id: &str,
        field: &super::model::RemoteProfileFieldSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let control = if field.options.is_empty() {
            Self::render_remote_text_field(
                profile_id.to_owned(),
                field.id.clone(),
                format!("remote-field-{}-{}", profile_id, field.id),
                field.label.clone(),
                &field.value,
                &field.label,
                true,
                window,
                cx,
            )
        } else {
            let choices = field
                .options
                .iter()
                .map(|option| SettingsChoice {
                    token: option.id.clone(),
                    label: option.label.clone(),
                    description: None,
                })
                .collect::<Vec<_>>();
            let profile_id = profile_id.to_owned();
            let field_id = field.id.clone();
            Self::render_dropdown(
                format!("remote-field-{}-{}", profile_id, field.id),
                &field.label,
                "",
                &field.value,
                &choices,
                true,
                Rc::new(move |value| SettingsIntent::SetRemoteField {
                    profile_id: profile_id.clone(),
                    field_id: field_id.clone(),
                    value,
                }),
                window,
                cx,
            )
        };
        render_settings_item_layout(
            &format!("remote-field-{}-{}", profile_id, field.id),
            &field.label,
            "",
            control,
            None,
            cx,
        )
    }
}

impl GpuiSettings {
    fn number_input(
        id: &str,
        input: &Entity<ComponentInputState>,
        suffix: &str,
        enabled: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let input_selector = format!("settings-number-input-{id}");
        let input_debug_selector = input_selector.clone();
        // NumberInput owns its semantic spinbutton root, but gpui-component does not currently
        // expose an aria-label/aria-description builder for that root. Do not put a misleading
        // name on this layout wrapper; this needs an upstream component API addition.
        div()
            .id(SharedString::from(input_selector))
            .debug_selector(move || input_debug_selector)
            .w(rems(SETTINGS_NUMBER_INPUT_WIDTH_REMS))
            .flex_shrink_0()
            .child(crate::gpui::focus_input(
                input,
                NumberInput::new(input)
                    .disabled(!enabled)
                    .when(!suffix.is_empty(), |input| {
                        input.suffix(
                            Label::new(suffix.to_owned())
                                .text_sm()
                                .text_color(cx.theme().muted_foreground),
                        )
                    }),
            ))
    }

    fn string_list_add_button(
        id: &str,
        add_label: &str,
        is_font_stack: bool,
        enabled: bool,
        cx: &Context<Self>,
    ) -> Button {
        let add_entity = cx.entity();
        let add_id = id.to_owned();
        Button::new(SharedString::from(format!("settings-string-list-{id}-add")))
            .icon(Icon::new(IconName::Plus).small())
            .small()
            .accessibility_label(add_label.to_owned())
            .tooltip(add_label.to_owned())
            .when(is_font_stack, ButtonVariants::ghost)
            .when(!is_font_stack, |button| {
                button.label(add_label.to_owned()).outline()
            })
            .disabled(!enabled)
            .on_click(move |_, _, app| {
                add_entity.update(app, |this, cx| {
                    this.emit(SettingsIntent::AddStringListItem(add_id.clone()), cx);
                });
            })
    }

    fn modifier_remap_row(
        mapping: &ModifierRemap,
        choices: &[SettingsChoice],
        movement: OrderedRowMovement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = movement.index;
        let enabled = movement.enabled;
        let drag_scope = &movement.scope;
        let source_intent = Rc::new(move |value| SettingsIntent::SetModifierRemap {
            index,
            field: ModifierRemapField::Source,
            value,
        });
        let target_intent = Rc::new(move |value| SettingsIntent::SetModifierRemap {
            index,
            field: ModifierRemapField::Target,
            value,
        });
        let row_selector = format!("settings-modifier-remap-{index}");
        let row_label = format!("{} = {}", mapping.source, mapping.target);
        let entity = cx.entity();
        let focus_border = cx.theme().ring;
        let mut row = gpui_kit::div()
            .flex()
            .items_center()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector({
                let selector = row_selector.clone();
                move || selector
            })
            .w_full()
            .min_w_0()
            .gap_1()
            .focusable()
            .tab_index(0_isize)
            .focus_visible(move |style| style.border_1().border_color(focus_border));
        if enabled {
            row = row.child(ordered_row_drag_handle(
                row_selector.clone(),
                drag_scope.clone(),
                index.to_string(),
                row_label.clone(),
                cx.theme().muted_foreground,
            ));
        }
        row = row
            .child(Self::render_compact_dropdown(
                format!("settings-modifier-remap-{index}-source"),
                "Source modifier",
                "The physical modifier to remap.",
                &mapping.source,
                choices,
                enabled,
                source_intent,
                window,
                cx,
            ))
            .child(Icon::new(IconName::ArrowRight).small())
            .child(Self::render_compact_dropdown(
                format!("settings-modifier-remap-{index}-target"),
                "Target modifier",
                "The modifier Bootty should receive.",
                &mapping.target,
                choices,
                enabled,
                target_intent,
                window,
                cx,
            ));
        row = row.child(ordered_row_remove_button(
            row_selector,
            row_label,
            enabled,
            move |app| {
                entity.update(app, |this, cx| {
                    this.emit(SettingsIntent::RemoveModifierRemap(index), cx);
                });
            },
        ));
        row = movement.apply(
            row,
            |index, offset| SettingsIntent::MoveModifierRemap { index, offset },
            cx,
        );
        row.into_any_element()
    }

    fn remote_arguments(
        profile: &super::model::RemoteProfileSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut fields = Vec::new();
        for (index, argument) in profile.arguments.iter().enumerate() {
            let remove_entity = cx.entity();
            let remove_id = format!("remote:remove-arg:{}:{index}", profile.id);
            fields.push(
                gpui_kit::div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Self::render_remote_text_field(
                        profile.id.clone(),
                        format!("args.{index}"),
                        format!("remote-argument-{}-{index}", profile.id),
                        format!("SSH argument {}", index.saturating_add(1)),
                        argument,
                        "-o BatchMode=yes",
                        true,
                        window,
                        cx,
                    ))
                    .child(
                        Button::new(SharedString::from(format!(
                            "remote-remove-argument-{}-{index}",
                            profile.id
                        )))
                        .icon(Icon::new(IconName::Delete))
                        .accessibility_label(format!(
                            "Remove SSH argument {}",
                            index.saturating_add(1)
                        ))
                        .tooltip("Remove SSH argument")
                        .on_click(move |_, _, app| {
                            remove_entity.update(app, |this, cx| {
                                this.emit(SettingsIntent::Invoke(remove_id.clone()), cx);
                            });
                        }),
                    )
                    .into_any_element(),
            );
        }
        let add_entity = cx.entity();
        let add_id = format!("remote:add-arg:{}", profile.id);
        fields.push(
            Button::new(SharedString::from(format!(
                "remote-add-argument-{}",
                profile.id
            )))
            .label("Add SSH argument")
            .icon(Icon::new(IconName::Plus).small())
            .outline()
            .on_click(move |_, _, app| {
                add_entity.update(app, |this, cx| {
                    this.emit(SettingsIntent::Invoke(add_id.clone()), cx);
                });
            })
            .into_any_element(),
        );
        fields
    }

    fn ansi_palette_preset(
        id: &str,
        index: usize,
        preset: &AnsiPalettePreset,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let entity = cx.entity();
        let id = id.to_owned();
        let colors = preset.colors.clone();
        let selector = format!("settings-ansi-palette-{id}-preset-{index}");
        let debug_selector = selector.clone();
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || debug_selector)
            .child(
                Button::new(SharedString::from(format!("{selector}-button")))
                    .label(preset.label.clone())
                    .outline()
                    .xsmall()
                    .on_click(move |_, _, app| {
                        entity.update(app, |this, cx| {
                            this.emit(
                                SettingsIntent::ReplaceAnsiPalette {
                                    id: id.clone(),
                                    colors: colors.clone(),
                                },
                                cx,
                            );
                        });
                    }),
            )
    }

    fn ansi_palette_toolbar(
        id: &str,
        colors: &[String],
        presets: &[AnsiPalettePreset],
        resettable: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let preset_buttons = presets
            .iter()
            .enumerate()
            .map(|(index, preset)| Self::ansi_palette_preset(id, index, preset, cx));
        let add_entity = cx.entity();
        let add_id = id.to_owned();
        let mut added = colors.to_vec();
        added.push("#000000".to_owned());
        let remove_entity = cx.entity();
        let remove_id = id.to_owned();
        let mut removed = colors.to_vec();
        removed.pop();
        let reset_entity = cx.entity();
        let reset_id = id.to_owned();

        gpui_kit::div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_1()
            .children(preset_buttons)
            .child(
                Button::new(SharedString::from(format!(
                    "settings-ansi-palette-{id}-add"
                )))
                .label("Add slot")
                .outline()
                .xsmall()
                .disabled(colors.len() >= 256)
                .on_click(move |_, _, app| {
                    add_entity.update(app, |this, cx| {
                        this.emit(
                            SettingsIntent::ReplaceAnsiPalette {
                                id: add_id.clone(),
                                colors: added.clone(),
                            },
                            cx,
                        );
                    });
                }),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "settings-ansi-palette-{id}-remove"
                )))
                .label("Remove last")
                .outline()
                .xsmall()
                .disabled(colors.is_empty())
                .on_click(move |_, _, app| {
                    remove_entity.update(app, |this, cx| {
                        this.emit(
                            SettingsIntent::ReplaceAnsiPalette {
                                id: remove_id.clone(),
                                colors: removed.clone(),
                            },
                            cx,
                        );
                    });
                }),
            )
            .when(resettable, |row| {
                row.child(debug_wrapper(
                    format!("settings-ansi-palette-{id}-reset"),
                    Button::new(SharedString::from(format!(
                        "settings-ansi-palette-{id}-reset-button"
                    )))
                    .label("Reset palette")
                    .outline()
                    .xsmall()
                    .disabled(colors.is_empty())
                    .on_click(move |_, _, app| {
                        reset_entity.update(app, |this, cx| {
                            this.emit(
                                SettingsIntent::ReplaceAnsiPalette {
                                    id: reset_id.clone(),
                                    colors: Vec::new(),
                                },
                                cx,
                            );
                        });
                    }),
                ))
            })
    }
}

/// Payload shared by every reorderable settings collection.
///
/// `scope` identifies the owning setting surface and fingerprints its current ordered snapshot;
/// `source` is that surface's domain row coordinate. A drop from an older snapshot therefore
/// cannot accidentally reorder a different collection or apply a stale index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OrderedRowDrag {
    pub(super) scope: String,
    pub(super) source: String,
    pub(super) label: String,
}

#[derive(Clone)]
struct OrderedRowDragPreview {
    label: String,
}

impl Render for OrderedRowDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_kit::div()
            .flex()
            .items_center()
            .h(rems(2.0))
            .min_w(rems(10.0))
            .max_w(rems(24.0))
            .px_2()
            .gap_1()
            .items_center()
            .overflow_hidden()
            .rounded_sm()
            .bg(cx.theme().button)
            .border_1()
            .border_color(cx.theme().ring)
            .text_sm()
            .truncate()
            .child(crate::gpui::sized_icon(
                "lucide:grip-vertical",
                crate::gpui::IconSize::Small,
                cx.theme().muted_foreground,
            ))
            .child(self.label.clone())
    }
}

fn ordered_row_drag_preview(
    dragged: &OrderedRowDrag,
    cx: &mut gpui_kit::App,
) -> Entity<OrderedRowDragPreview> {
    let label = dragged.label.clone();
    cx.new(|_| OrderedRowDragPreview { label })
}

/// A compact, visible drag handle for an ordered settings row.
/// Translate a drop before a row (or at the end) after removing the dragged row.
pub(super) fn ordered_row_move_offset(source: usize, before: usize) -> Option<isize> {
    let target = before.saturating_sub(usize::from(source < before));
    if target >= source {
        isize::try_from(target.checked_sub(source)?).ok()
    } else {
        isize::try_from(source.checked_sub(target)?)
            .ok()?
            .checked_neg()
    }
}

pub(super) struct OrderedRowMovement {
    pub selector: String,
    pub scope: String,
    pub index: usize,
    pub count: usize,
    pub enabled: bool,
}

impl OrderedRowMovement {
    pub fn apply(
        self,
        mut row: gpui_kit::Stateful<gpui_kit::Div>,
        intent: impl Fn(usize, isize) -> SettingsIntent + 'static,
        cx: &Context<GpuiSettings>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        if !self.enabled {
            return row;
        }
        let Self {
            selector,
            scope,
            index,
            count,
            ..
        } = self;
        let entity = cx.entity();
        let emit = Rc::new(move |index, offset, app: &mut gpui_kit::App| {
            entity.update(app, |this, cx| this.emit(intent(index, offset), cx));
        });
        let key_emit = Rc::clone(&emit);
        let drop_emit = Rc::clone(&emit);
        let drop_scope = scope.clone();
        let focus_border = cx.theme().ring;
        row = row
            .on_key_down(move |event: &KeyDownEvent, _, app| {
                let modifiers = event.keystroke.modifiers;
                if !modifiers.alt || modifiers.control || modifiers.platform || modifiers.function {
                    return;
                }
                let offset = match event.keystroke.key.as_str() {
                    "up" if index != 0 => -1,
                    "down" if index.saturating_add(1) != count => 1,
                    _ => return,
                };
                app.stop_propagation();
                key_emit(index, offset, app);
            })
            .drag_over::<OrderedRowDrag>(move |element, _, _, _| {
                element.border_t_1().border_color(focus_border)
            })
            .can_drop(move |value, _, _| {
                value
                    .downcast_ref::<OrderedRowDrag>()
                    .is_some_and(|dragged| {
                        dragged.scope == drop_scope
                            && dragged
                                .source
                                .parse::<usize>()
                                .is_ok_and(|source| source < count && source != index)
                    })
            })
            .on_drop(
                move |dragged: &OrderedRowDrag, _, app: &mut gpui_kit::App| {
                    let Ok(source) = dragged.source.parse::<usize>() else {
                        return;
                    };
                    if source >= count || source == index {
                        return;
                    }
                    let Some(offset) = ordered_row_move_offset(source, index) else {
                        return;
                    };
                    drop_emit(source, offset, app);
                },
            );
        if index.saturating_add(1) == count {
            row = row.child(ordered_row_end_drop_target(
                selector,
                scope,
                count.saturating_sub(1),
                move |source, app| {
                    let Some(offset) = ordered_row_move_offset(source, count) else {
                        return;
                    };
                    emit(source, offset, app);
                },
            ));
        }
        row
    }
}

pub(super) fn ordered_row_drag_handle(
    selector: String,
    scope: String,
    source: String,
    label: String,
    tint: gpui_kit::Hsla,
) -> AnyElement {
    let accessibility_label = format!("Reorder {label}");
    let drag = OrderedRowDrag {
        scope,
        source,
        label,
    };
    let handle_selector = selector + "-drag-handle";
    div()
        .id(SharedString::from(handle_selector.clone()))
        .debug_selector(move || handle_selector)
        .flex()
        .items_center()
        .justify_center()
        .w(rems(1.75))
        .h(rems(1.75))
        .cursor_move()
        .aria_label(accessibility_label)
        .aria_description("Drag to reorder")
        .on_drag(drag, |dragged, _, _, cx| {
            ordered_row_drag_preview(dragged, cx)
        })
        .child(crate::gpui::sized_icon(
            "lucide:grip-vertical",
            crate::gpui::IconSize::Small,
            tint,
        ))
        .into_any_element()
}

pub(super) fn ordered_row_remove_button(
    selector: String,
    mut label: String,
    enabled: bool,
    on_remove: impl Fn(&mut gpui_kit::App) + 'static,
) -> AnyElement {
    let action_selector = selector + "-remove";
    label.insert_str(0, "Remove ");
    debug_wrapper(
        action_selector.clone(),
        Button::new(SharedString::from(format!("kit-{action_selector}")))
            .ghost()
            .small()
            .icon(Icon::new(IconName::Delete))
            .accessibility_label(label)
            .tooltip("Remove")
            .disabled(!enabled)
            .on_click(move |_, _, app| {
                app.stop_propagation();
                on_remove(app);
            }),
    )
}

/// Add a small end target so the last row can be reached without relying on an exact row edge.
pub(super) fn ordered_row_end_drop_target(
    selector: String,
    scope: String,
    last_index: usize,
    on_drop: impl Fn(usize, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    let target_selector = selector + "-after";
    let drop_scope = scope;
    div()
        .id(SharedString::from(target_selector.clone()))
        .debug_selector(move || target_selector)
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(rems(0.75))
        .drag_over::<OrderedRowDrag>(|element, _, _, _| element.border_b_1())
        .can_drop(move |value, _, _| {
            value
                .downcast_ref::<OrderedRowDrag>()
                .is_some_and(|dragged| {
                    dragged.scope == drop_scope
                        && dragged
                            .source
                            .parse::<usize>()
                            .is_ok_and(|source| source < last_index)
                })
        })
        .on_drop(
            move |dragged: &OrderedRowDrag, _, app: &mut gpui_kit::App| {
                app.stop_propagation();
                let Ok(source) = dragged.source.parse::<usize>() else {
                    return;
                };
                if source < last_index {
                    on_drop(source, app);
                }
            },
        )
        .into_any_element()
}

pub(super) fn ordered_collection_scope(prefix: &str, revision: impl Hash) -> String {
    let mut hasher = DefaultHasher::new();
    revision.hash(&mut hasher);
    format!("{prefix}:{:016x}", hasher.finish())
}

pub(super) fn settings_row_identity(row: &SettingsRow) -> Option<&str> {
    match row {
        SettingsRow::Value { id, .. }
        | SettingsRow::AnsiPalette { id, .. }
        | SettingsRow::Action { id, .. }
        | SettingsRow::StringList { id, .. }
        | SettingsRow::ModifierRemaps { id, .. }
        | SettingsRow::Environment { id, .. }
        | SettingsRow::FontFeatures { id, .. }
        | SettingsRow::StatusSegments(StatusSegmentsSnapshot { id, .. })
        | SettingsRow::ModuleIntegrations(ModuleIntegrationsSnapshot { identity: id, .. })
        | SettingsRow::Remote(RemoteEditorSnapshot { id, .. }) => Some(id),
        SettingsRow::Section(_) | SettingsRow::Notice { .. } => None,
    }
}

fn render_setting_label(
    id: &str,
    label: &str,
    draft: Option<&crate::settings_session::SettingsSession>,
    cx: &Context<GpuiSettings>,
) -> AnyElement {
    let mut title = gpui_kit::div()
        .flex()
        .items_center()
        .gap_1()
        .child(Label::new(label.to_owned()));
    if let Some(draft) = draft.filter(|draft| draft.can_reset(id)) {
        let scheme = bootty_config::ApplicationIdentity::for_process().namespace();
        let link = format!("{scheme}://settings/{id}");
        let reset_id = id.to_owned();
        let owner = cx.entity();
        title = title
            .child(
                Button::new(SharedString::from(format!("settings-link-{id}")))
                    .ghost()
                    .xsmall()
                    .text_color(cx.theme().muted_foreground)
                    .icon(Icon::default().path("icons/link.svg"))
                    .tab_stop(false)
                    .accessibility_label(format!("Copy setting link for {label}"))
                    .tooltip("Copy setting link")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(link.clone()));
                    }),
            )
            .when(!draft.is_default(id), |title| {
                title.child(
                    Button::new(SharedString::from(format!("settings-reset-{id}")))
                        .ghost()
                        .xsmall()
                        .text_color(cx.theme().muted_foreground)
                        .icon(Icon::new(IconName::Undo))
                        .tab_stop(false)
                        .accessibility_label(format!("Reset {label} to default"))
                        .tooltip("Reset to default")
                        .on_click(move |_, _, cx| {
                            owner.update(cx, |settings, cx| {
                                settings.emit(SettingsIntent::RemoveValue(reset_id.clone()), cx);
                            });
                        }),
                )
            });
    }
    title.into_any_element()
}

/// Copied from Zed's shared settings item layout (`settings_ui.rs:1381-1452`).
fn render_settings_item_layout(
    id: &str,
    label: &str,
    help: &str,
    control: AnyElement,
    draft: Option<&crate::settings_session::SettingsSession>,
    cx: &Context<GpuiSettings>,
) -> AnyElement {
    gpui_kit::div()
        .flex()
        .items_center()
        .id(SharedString::from(format!("settings-item-{id}")))
        .min_w_0()
        .justify_between()
        .child(
            gpui_kit::div()
                .flex()
                .flex_col()
                .relative()
                .w_full()
                .max_w_2_3()
                .min_w_0()
                .child(render_setting_label(id, label, draft, cx))
                .child(
                    gpui_kit::component::text::TextView::markdown(
                        SharedString::from(format!("setting-help-{id}")),
                        help.to_owned(),
                    )
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
                ),
        )
        .child(control)
        .into_any_element()
}

/// Zed's dynamic child settings keep wide custom controls below their description, inset as one
/// full-width child surface instead of forcing them through the scalar two-column row.
fn render_structured_settings_item_layout(
    id: &str,
    label: &str,
    help: &str,
    control: AnyElement,
    draft: Option<&crate::settings_session::SettingsSession>,
    cx: &Context<GpuiSettings>,
) -> AnyElement {
    let selector = format!("settings-structured-{id}");
    let debug_selector = selector.clone();
    gpui_kit::div()
        .flex()
        .flex_col()
        .id(SharedString::from(selector))
        .debug_selector(move || debug_selector)
        .w_full()
        .min_w_0()
        .gap_3()
        .child(
            gpui_kit::div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .child(render_setting_label(id, label, draft, cx))
                .child(
                    gpui_kit::component::text::TextView::markdown(
                        SharedString::from(format!("setting-help-{id}")),
                        help.to_owned(),
                    )
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
                ),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .pl_4()
                .border_l_1()
                .border_dashed()
                .border_color(cx.theme().input)
                .child(control),
        )
        .into_any_element()
}

fn quantize_number(value: f32, precision: usize) -> f32 {
    let places = i32::try_from(precision).unwrap_or(8).min(8);
    let scale = 10_f32.powi(places);
    (value * scale).round() / scale
}

fn precision_step(precision: usize) -> f32 {
    10_f32.powi(
        i32::try_from(precision)
            .unwrap_or(8)
            .min(8)
            .saturating_neg(),
    )
}

fn valid_display_scale(display_scale: f32) -> f32 {
    if display_scale.is_finite() && display_scale > 0.0 {
        display_scale
    } else {
        1.0
    }
}

fn parse_number_input(text: &str, display_scale: f32) -> Option<f32> {
    let value = text.trim().parse::<f32>().ok()? / display_scale;
    value.is_finite().then_some(value)
}

fn normalize_number_input(
    value: f32,
    range: &std::ops::RangeInclusive<f32>,
    precision: usize,
    display_scale: f32,
) -> Option<f32> {
    let start = *range.start();
    let end = *range.end();
    if !value.is_finite() || !start.is_finite() || !end.is_finite() || start > end {
        return None;
    }
    let value = value.clamp(start, end);
    Some((quantize_number(value * display_scale, precision) / display_scale).clamp(start, end))
}

fn format_number(value: f32, precision: usize) -> String {
    format!("{value:.precision$}")
}

fn display_value(value: &ScalarValue) -> String {
    match value {
        ScalarValue::Bool(value) => if *value { "On" } else { "Off" }.to_owned(),
        ScalarValue::Text(value) | ScalarValue::Token(value) => value.clone(),
        ScalarValue::Number(value) => value.to_string(),
    }
}

fn remote_test_label(state: &RemoteTestState) -> String {
    match state {
        RemoteTestState::Idle => "Not tested".to_owned(),
        RemoteTestState::Testing => "Testing…".to_owned(),
        RemoteTestState::Passed => "Connection succeeded".to_owned(),
        RemoteTestState::Failed(error) => format!("Connection failed: {error}"),
    }
}

fn remote_test_color(state: &RemoteTestState, cx: &gpui_kit::App) -> gpui_kit::Hsla {
    match state {
        RemoteTestState::Failed(_) => cx.theme().danger,
        RemoteTestState::Passed => cx.theme().success,
        RemoteTestState::Idle | RemoteTestState::Testing => cx.theme().muted_foreground,
    }
}
