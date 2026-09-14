//! Settings choices backed by `gpui-component`'s accessible select control.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::label::Label;

use std::rc::Rc;

use gpui_kit::component::{
    IndexPath, Sizable as _, Size,
    searchable_list::{SearchableListItem, SearchableVec},
    select::{Select, SelectEvent, SelectState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, rems,
};

use super::model::SettingsChoice;

type SelectionHandler = Rc<dyn Fn(String, &mut App)>;
type SelectChoiceDelegate = SearchableVec<SettingsChoice>;

/// Catalogs taller than Zed's 18rem menu include a search input.
pub(super) const SEARCHABLE_CHOICE_THRESHOLD: usize = 12;

pub(super) const fn uses_searchable_picker(choice_count: usize) -> bool {
    choice_count > SEARCHABLE_CHOICE_THRESHOLD
}

impl SearchableListItem for SettingsChoice {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone().into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let selector = format!("settings-picker-option-{}", self.token);
        let debug_selector = selector.clone();
        let description_selector = format!("settings-picker-option-description-{}", self.token);
        let accessible_label = self.description.as_ref().map_or_else(
            || self.label.clone(),
            |description| format!("{}: {description}", self.label),
        );
        gpui_kit::div()
            .flex()
            .flex_col()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .role(Role::ListBoxOption)
            .aria_label(accessible_label)
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .child(Label::new(self.label.clone()).text_sm())
            .when_some(self.description.clone(), |option, description| {
                option.child(
                    div()
                        .debug_selector(move || description_selector)
                        .min_w_0()
                        .overflow_hidden()
                        .child(
                            Label::new(description)
                                .text_xs()
                                .text_color(cx.theme().muted_foreground),
                        ),
                )
            })
    }

    fn value(&self) -> &Self::Value {
        &self.token
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_ascii_lowercase();
        query.is_empty()
            || self.label.to_ascii_lowercase().contains(&query)
            || self.token.to_ascii_lowercase().contains(&query)
            || self
                .description
                .as_ref()
                .is_some_and(|description| description.to_ascii_lowercase().contains(&query))
    }
}

struct SettingsChoiceState {
    select: Entity<SelectState<SelectChoiceDelegate>>,
    choices: Vec<SettingsChoice>,
    value: String,
    on_change: SelectionHandler,
    _subscriptions: [Subscription; 2],
}

impl SettingsChoiceState {
    fn new(
        choices: Vec<SettingsChoice>,
        value: String,
        on_change: SelectionHandler,
        searchable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (select, subscriptions) = Self::build_select(&choices, &value, searchable, window, cx);
        Self {
            select,
            choices,
            value,
            on_change,
            _subscriptions: subscriptions,
        }
    }

    fn sync_external(
        &mut self,
        choices: &[SettingsChoice],
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choices_changed = self.choices != choices;
        let value_changed = self.value != value;
        if !choices_changed && !value_changed {
            return;
        }

        self.select.update(cx, |select, cx| {
            if choices_changed {
                select.set_items(SearchableVec::new(choices.to_vec()), window, cx);
            }
            select.set_selected_value(&value.to_owned(), window, cx);
        });
        self.choices = choices.to_vec();
        value.clone_into(&mut self.value);
        cx.notify();
    }

    fn build_select(
        choices: &[SettingsChoice],
        value: &str,
        searchable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<SelectState<SelectChoiceDelegate>>, [Subscription; 2]) {
        let selected_index = choices
            .iter()
            .position(|choice| choice.token == value)
            .map(IndexPath::new);
        let select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(choices.to_vec()),
                selected_index,
                window,
                cx,
            )
            .searchable(searchable)
        });
        let subscription = cx.subscribe(&select, move |this, _, event, cx| {
            if let SelectEvent::Confirm(Some(value)) = event {
                this.value.clone_from(value);
                (this.on_change)(value.clone(), cx);
                cx.notify();
            }
        });
        let trigger_focus = select.focus_handle(cx);
        let mut was_open = false;
        let close_subscription = cx.observe_in(&select, window, move |this, select, window, cx| {
            // Kit emits no DismissEvent and restores a filtered index on close. Remove this
            // observer when Kit clears search and restores the committed value on dismissal.
            // Focusable returns the popup handle while open and the trigger handle while closed.
            let is_open = select.focus_handle(cx) != trigger_focus;
            if std::mem::replace(&mut was_open, is_open) && !is_open {
                select.update(cx, |select, cx| {
                    let value = select.selected_value().unwrap_or(&this.value).clone();
                    select.set_selected_value(&value, window, cx);
                    cx.notify();
                });
            }
        });
        (select, [subscription, close_subscription])
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_dropdown(
    selector: String,
    label: &str,
    description: &str,
    current: &str,
    choices: &[SettingsChoice],
    enabled: bool,
    on_change: SelectionHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    render_select(
        selector,
        label,
        description,
        current,
        choices,
        enabled,
        on_change,
        gpui_kit::rems(210.0 / 16.0),
        None,
        window,
        cx,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_compact_dropdown(
    selector: String,
    label: &str,
    description: &str,
    current: &str,
    choices: &[SettingsChoice],
    enabled: bool,
    on_change: SelectionHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    render_select(
        selector,
        label,
        description,
        current,
        choices,
        enabled,
        on_change,
        gpui_kit::rems(118.0 / 16.0),
        None,
        window,
        cx,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_searchable_picker(
    selector: String,
    label: &str,
    description: &str,
    catalog_name: &str,
    current: &str,
    choices: &[SettingsChoice],
    enabled: bool,
    on_change: SelectionHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    render_select(
        selector,
        label,
        description,
        current,
        choices,
        enabled,
        on_change,
        gpui_kit::rems(210.0 / 16.0),
        Some(catalog_name),
        window,
        cx,
    )
}

fn searchable_picker_placeholder(catalog_name: &str) -> &'static str {
    match catalog_name {
        "fonts" => "Choose a font…",
        "themes" => "Choose a theme…",
        _ => "Choose an option…",
    }
}

#[allow(clippy::too_many_arguments)]
fn render_select(
    selector: String,
    label: &str,
    description: &str,
    current: &str,
    choices: &[SettingsChoice],
    enabled: bool,
    on_change: SelectionHandler,
    width: gpui_kit::Rems,
    search_catalog: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let searchable = search_catalog.is_some();
    let state_key = SharedString::from(format!("{selector}-select-state-{searchable}"));
    let state = window.use_keyed_state(state_key, cx, |window, cx| {
        SettingsChoiceState::new(
            choices.to_vec(),
            current.to_owned(),
            on_change.clone(),
            searchable,
            window,
            cx,
        )
    });
    state.update(cx, |state, cx| {
        state.on_change = on_change;
        state.sync_external(choices, current, window, cx);
    });
    let select = state.read(cx).select.clone();

    let debug_selector = selector.clone();
    let component_selector = format!("settings-select-{selector}");
    let component_debug_selector = component_selector.clone();
    let menu_width = if choices.iter().any(|choice| choice.description.is_some()) {
        gpui_kit::rems(320.0 / 16.0)
    } else {
        gpui_kit::rems(210.0 / 16.0)
    };
    let component = Select::new(&select)
        .with_size(Size::Medium)
        .menu_width(menu_width)
        .menu_max_h(rems(18.))
        .accessibility_label(label.to_owned())
        .when_some(search_catalog, |select, catalog| {
            select
                .placeholder(searchable_picker_placeholder(catalog))
                .search_placeholder(format!("Search {catalog}…"))
        })
        .disabled(!enabled);

    div()
        .id(SharedString::from(selector))
        .debug_selector(move || debug_selector)
        .aria_description(description.to_owned())
        .flex_none()
        .w(width)
        .child(
            div()
                .id(SharedString::from(component_selector))
                .debug_selector(move || component_debug_selector)
                .size_full()
                .child(component),
        )
        .into_any_element()
}
