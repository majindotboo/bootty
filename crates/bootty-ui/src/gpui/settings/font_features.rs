//! Structured OpenType tag/value editor.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, button::Button, label::Label};

use gpui_kit::component::{IconName, tag::Tag};
use gpui_kit::component::{
    IndexPath, Size,
    combobox::{Combobox, ComboboxEvent, ComboboxState},
    input::{Input, InputEvent, InputState, MaskPattern, NumberInput},
    searchable_list::{
        SearchableListChange, SearchableListDelegate, SearchableListItem, SearchableVec,
    },
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement,
    Render, Role, SharedString, StatefulInteractiveElement as _, Styled, Subscription, Task,
    Window, div, prelude::*, rems,
};

use crate::settings_session::{FontFeatureDraft, dedupe_font_features};

use super::components::debug_wrapper;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFeaturePreset {
    pub label: String,
    pub feature: FontFeatureDraft,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFeatureEditorSnapshot {
    pub features: Vec<FontFeatureDraft>,
    pub presets: Vec<FontFeaturePreset>,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontFeatureEditorEvent {
    Replace(Vec<FontFeatureDraft>),
}

pub struct GpuiFontFeatureEditor {
    snapshot: FontFeatureEditorSnapshot,
    inputs: Option<FontFeatureInputs>,
    error: Option<String>,
}

struct FontFeatureInputs {
    tag: Entity<InputState>,
    value: Entity<InputState>,
    combobox: Entity<ComboboxState<FontFeatureDelegate>>,
    option_values: Vec<String>,
    selected_values: Vec<String>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
struct FontFeatureOption {
    tag: String,
    setting: String,
    description: SharedString,
}

#[derive(Clone)]
struct FontFeatureDelegate {
    items: SearchableVec<FontFeatureOption>,
    stable_values: Vec<String>,
}

impl FontFeatureDelegate {
    fn new(items: Vec<FontFeatureOption>) -> Self {
        Self {
            stable_values: items.iter().map(|item| item.setting.clone()).collect(),
            items: SearchableVec::new(items),
        }
    }

    fn stable_path(&self, row: IndexPath, value: &str) -> IndexPath {
        let stable_index = self
            .stable_values
            .iter()
            .position(|candidate| candidate == value)
            .unwrap_or_default();
        row.column(stable_index.saturating_add(1))
    }
}

impl SearchableListDelegate for FontFeatureDelegate {
    type Item = FontFeatureOption;

    fn items_count(&self, section: usize) -> usize {
        self.items.items_count(section)
    }

    fn item(&self, ix: IndexPath) -> Option<&Self::Item> {
        self.items.item(ix)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        let row = self.items.position(value)?;
        let item = self.items.item(row)?;
        Some(self.stable_path(row, &item.setting))
    }

    fn perform_search(&mut self, query: &str, window: &mut Window, cx: &mut App) -> Task<()> {
        self.items.perform_search(query, window, cx)
    }

    fn on_will_change(
        &mut self,
        selection: &mut Vec<(IndexPath, Self::Item)>,
        changes: &[SearchableListChange],
    ) {
        for change in changes {
            match change {
                SearchableListChange::Select { index } => {
                    let Some(item) = self.item(*index).cloned() else {
                        continue;
                    };
                    // OpenType values are mutually exclusive per tag. Keep the component's
                    // visible chips identical to the typed value that will be persisted.
                    selection.retain(|(_, selected)| selected.tag != item.tag);
                    selection.push((self.stable_path(*index, &item.setting), item));
                }
                SearchableListChange::Deselect { index } => {
                    if let Some(item) = self.item(*index) {
                        selection.retain(|(_, selected)| selected.setting != item.setting);
                    } else {
                        selection.retain(|(selected, _)| selected != index);
                    }
                }
            }
        }
    }
}

impl SearchableListItem for FontFeatureOption {
    type Value = String;

    fn title(&self) -> SharedString {
        self.setting.clone().into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let selector = format!("font-feature-option-{}", self.setting);
        let debug_selector = selector.clone();
        gpui_kit::div()
            .flex()
            .flex_col()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .role(Role::ListBoxOption)
            .aria_label(format!("{}: {}", self.setting, self.description))
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .child(Label::new(self.setting.clone()).text_sm())
            .child(
                Label::new(self.description.clone())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
    }

    fn value(&self) -> &Self::Value {
        &self.setting
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_ascii_lowercase();
        query.is_empty()
            || self.setting.to_ascii_lowercase().contains(&query)
            || self.description.to_ascii_lowercase().contains(&query)
    }
}

impl GpuiFontFeatureEditor {
    #[must_use]
    pub const fn new(snapshot: FontFeatureEditorSnapshot, _: &mut Context<Self>) -> Self {
        Self {
            snapshot,
            inputs: None,
            error: None,
        }
    }

    /// Construct the editor with its stateful fields and subscriptions before the first render.
    pub fn new_with_window(
        snapshot: FontFeatureEditorSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(snapshot, cx);
        this.ensure_inputs(window, cx);
        this
    }

    pub(super) fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs(window, cx);
    }

    fn inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> &FontFeatureInputs {
        self.inputs.get_or_insert_with(|| {
            let tag = cx.new(|cx| InputState::new(window, cx).placeholder("cv01"));
            let value = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("1")
                    .placeholder("1")
                    .mask_pattern(MaskPattern::Number {
                        separator: None,
                        fraction: Some(0),
                    })
                    .step(1.)
                    .min(0.)
                    .max(f64::from(u32::MAX))
            });
            let options = feature_options(&self.snapshot);
            let selected_values = feature_settings(&self.snapshot.features);
            let delegate = FontFeatureDelegate::new(options.clone());
            let selected_indices = selected_values
                .iter()
                .filter_map(|value| delegate.position(value))
                .collect();
            let combobox = cx.new(|cx| {
                ComboboxState::new(delegate, selected_indices, window, cx)
                    .multiple(true)
                    .searchable(true)
            });
            let mut subscriptions = vec![
                cx.subscribe_in(&tag, window, Self::on_input_event),
                cx.subscribe_in(&value, window, Self::on_input_event),
            ];
            subscriptions.push(cx.subscribe(&combobox, |this, _, event, cx| {
                this.on_combobox_event(event, cx);
            }));
            FontFeatureInputs {
                tag,
                value,
                combobox,
                option_values: options.into_iter().map(|option| option.setting).collect(),
                selected_values,
                _subscriptions: subscriptions,
            }
        })
    }

    fn sync_combobox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inputs) = &mut self.inputs else {
            return;
        };
        let options = feature_options(&self.snapshot);
        let option_values = options
            .iter()
            .map(|option| option.setting.clone())
            .collect::<Vec<_>>();
        let selected_values = feature_settings(&self.snapshot.features);
        if inputs.option_values == option_values && inputs.selected_values == selected_values {
            return;
        }
        inputs.combobox.update(cx, |combobox, cx| {
            if inputs.option_values != option_values {
                combobox.set_items(FontFeatureDelegate::new(options), window, cx);
            }
            if inputs.selected_values != selected_values {
                combobox.set_selected_values(&selected_values, window, cx);
            }
        });
        inputs.option_values = option_values;
        inputs.selected_values = selected_values;
    }

    fn on_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.error = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.add_draft(cx),
            _ => {}
        }
    }

    fn on_combobox_event(
        &mut self,
        event: &ComboboxEvent<FontFeatureDelegate>,
        cx: &mut Context<Self>,
    ) {
        let ComboboxEvent::Change(values) = event else {
            return;
        };
        let features = values
            .iter()
            .filter_map(|setting| FontFeatureDraft::parse(setting).ok())
            .collect();
        self.replace(features, cx);
    }

    pub fn set_snapshot(&mut self, snapshot: FontFeatureEditorSnapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        self.error = None;
        cx.notify();
    }

    #[must_use]
    pub fn features(&self) -> &[FontFeatureDraft] {
        &self.snapshot.features
    }

    fn add_draft(&mut self, cx: &mut Context<Self>) {
        let Some(inputs) = &self.inputs else {
            return;
        };
        let tag = inputs.tag.read(cx).value();
        let value = inputs.value.read(cx).value();
        let Ok(value) = value.parse::<u32>() else {
            self.error = Some("OpenType feature values must be whole numbers.".to_owned());
            cx.notify();
            return;
        };
        let feature = match FontFeatureDraft::new(tag.to_string(), value) {
            Ok(feature) => feature,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.replace(upsert(self.snapshot.features.clone(), feature), cx);
    }

    fn replace(&mut self, features: Vec<FontFeatureDraft>, cx: &mut Context<Self>) {
        let features = dedupe_font_features(features);
        self.snapshot.features.clone_from(&features);
        self.error = None;
        cx.emit(FontFeatureEditorEvent::Replace(features));
        cx.notify();
    }

    fn input(id: &'static str, input: impl IntoElement) -> impl IntoElement {
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .child(input)
    }
}

impl EventEmitter<FontFeatureEditorEvent> for GpuiFontFeatureEditor {}

impl Render for GpuiFontFeatureEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        self.sync_combobox(window, cx);
        let count = self.snapshot.features.len();
        let inputs = self.inputs(window, cx);
        let combobox = inputs.combobox.clone();
        let tag_input = inputs.tag.clone();
        let value_input = inputs.value.clone();
        let add_entity = cx.entity();
        let clear_entity = add_entity.clone();
        let footer_entity = add_entity;
        let enabled = self.snapshot.enabled;
        let component = Combobox::new(&combobox)
            .with_size(Size::Medium)
            .menu_width(rems(20.))
            .menu_max_h(rems(18.))
            .placeholder("Select OpenType features…")
            .search_placeholder("Search font features…")
            .cleanable(true)
            .disabled(!enabled)
            .render_trigger(|trigger, _, _cx| {
                if trigger.selection().is_empty() {
                    return div()
                        .child(
                            trigger
                                .placeholder()
                                .cloned()
                                .unwrap_or_else(|| "Select OpenType features…".into()),
                        )
                        .into_any_element();
                }

                // Keep the trigger at Combobox's fixed input height. The selected values remain
                // individually discoverable through their tooltips; values past the available
                // width are clipped by the trigger rather than growing it vertically.
                gpui_kit::div()
                    .flex()
                    .items_center()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .items_center()
                    .gap_1()
                    .children(
                        trigger
                            .selection()
                            .iter()
                            .map(|(_, option)| Self::selected_feature(option)),
                    )
                    .into_any_element()
            })
            .footer(move |_, cx| {
                Self::feature_footer(&tag_input, &value_input, &footer_entity, enabled, cx)
            });
        gpui_kit::div()
            .flex()
            .flex_col()
            .w(rems(27.5))
            .max_w_full()
            .gap_2()
            .child(debug_wrapper(
                "font-feature-combobox".to_owned(),
                div()
                    .id("font-feature-combobox-control")
                    .debug_selector(|| "font-feature-combobox-control".to_owned())
                    .aria_label("OpenType font features")
                    .aria_description("Select enabled or disabled OpenType features")
                    .child(component),
            ))
            .when(count == 0, |this| {
                this.child(
                    Label::new("Uses the font's default OpenType features")
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    gpui_kit::component::alert::Alert::error("font-feature-error", error).banner(),
                )
            })
            .child(
                div().flex().child(debug_wrapper(
                    "font-feature-clear".to_owned(),
                    Button::new("font-feature-clear")
                        .label("Clear features")
                        .outline()
                        .xsmall()
                        .disabled(!enabled || count == 0)
                        .on_click(move |_, _, app| {
                            clear_entity.update(app, |this, cx| this.replace(Vec::new(), cx));
                        }),
                )),
            )
    }
}

impl GpuiFontFeatureEditor {
    fn selected_feature(option: &FontFeatureOption) -> impl IntoElement {
        let selector = format!("font-feature-selected-{}", option.setting);
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            .flex_none()
            .tooltip({
                let description = option.description.clone();
                move |_, cx| {
                    cx.new(|_| gpui_kit::component::tooltip::Tooltip::new(description.clone()))
                        .into()
                }
            })
            .child(Tag::secondary().child(option.setting.clone()))
    }

    fn feature_footer(
        tag_input: &Entity<InputState>,
        value_input: &Entity<InputState>,
        owner: &Entity<Self>,
        enabled: bool,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        let footer_add_entity = owner.clone();
        gpui_kit::div()
            .flex()
            .items_center()
            .id("font-feature-footer")
            .debug_selector(|| "font-feature-footer".to_owned())
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .gap_1()
            .child(Self::input(
                "font-feature-tag",
                crate::gpui::focus_input(
                    tag_input,
                    Input::new(tag_input)
                        .aria_label("OpenType feature tag")
                        .with_size(Size::Small)
                        .disabled(!enabled)
                        .w_20(),
                ),
            ))
            .child(Label::new("=").text_color(cx.theme().muted_foreground))
            .child(Self::input(
                "font-feature-value",
                crate::gpui::focus_input(
                    value_input,
                    NumberInput::new(value_input)
                        .placeholder("Feature value")
                        .with_size(Size::Small)
                        .disabled(!enabled)
                        .w_24(),
                ),
            ))
            .child(debug_wrapper(
                "font-feature-add".to_owned(),
                Button::new("font-feature-add")
                    .label("Add or update")
                    .icon(Icon::new(IconName::Plus).small())
                    .outline()
                    .xsmall()
                    .truncate()
                    .w(rems(7.))
                    .disabled(!enabled)
                    .on_click(move |_, _, app| {
                        footer_add_entity.update(app, Self::add_draft);
                    }),
            ))
            .into_any_element()
    }
}

fn upsert(mut features: Vec<FontFeatureDraft>, feature: FontFeatureDraft) -> Vec<FontFeatureDraft> {
    if let Some(existing) = features.iter_mut().find(|entry| entry.tag == feature.tag) {
        existing.value = feature.value;
    } else {
        features.push(feature);
    }
    features
}

fn feature_settings(features: &[FontFeatureDraft]) -> Vec<String> {
    dedupe_font_features(features.to_vec())
        .iter()
        .filter_map(|feature| feature.setting().ok())
        .collect()
}

fn feature_options(snapshot: &FontFeatureEditorSnapshot) -> Vec<FontFeatureOption> {
    let mut options = Vec::new();
    for feature in snapshot
        .features
        .iter()
        .chain(snapshot.presets.iter().map(|preset| &preset.feature))
    {
        let Ok(setting) = feature.setting() else {
            continue;
        };
        if options
            .iter()
            .any(|option: &FontFeatureOption| option.setting == setting)
        {
            continue;
        }
        options.push(FontFeatureOption {
            tag: feature.tag.clone(),
            setting,
            description: feature_description(&feature.tag).into(),
        });
    }
    options
}

fn feature_description(tag: &str) -> String {
    match tag {
        "liga" => "Standard ligatures".to_owned(),
        "calt" => "Contextual alternates".to_owned(),
        "dlig" => "Discretionary ligatures".to_owned(),
        "kern" => "Kerning".to_owned(),
        "zero" => "Slashed zero".to_owned(),
        "tnum" => "Tabular figures".to_owned(),
        "onum" => "Oldstyle figures".to_owned(),
        "ss01" => "Stylistic set 1".to_owned(),
        "ss02" => "Stylistic set 2".to_owned(),
        tag => format!("Custom OpenType feature {tag}"),
    }
}
