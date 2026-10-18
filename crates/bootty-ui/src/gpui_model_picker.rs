//! Shared catalog picker; provider identity and accepted favorites remain agent-owned.
use crate::gpui::ElementTooltip as _;
use bootty_agents::{AgentKind, NativeModelOption};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    kbd::Kbd,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    popover::Popover,
    switch::Switch,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Task, Window, div, prelude::*, rems,
};

#[derive(Clone, PartialEq, Eq, gpui_kit::Action)]
#[action(namespace = bootty, no_json)]
struct SelectModel(usize);

pub fn init(cx: &mut App) {
    // Search owns typed spaces; the enclosing popover otherwise treats Space as confirm.
    cx.bind_keys([gpui_kit::KeyBinding::new(
        "space",
        gpui_kit::NoAction,
        Some("BoottyModelPicker > Input"),
    )]);
    cx.bind_keys((0usize..9).map(|jump| {
        gpui_kit::KeyBinding::new(
            &format!("secondary-{}", jump.saturating_add(1)),
            SelectModel(jump),
            Some("BoottyModelPicker"),
        )
    }));
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelPickerEvent {
    Select(String),
    SelectProvider(String),
    Favorite(String),
    /// Send the prompt to this model too, or stop sending it there.
    FanOut(String),
    Reasoning(bootty_agents::NativeModelSelection),
    FastMode(bool),
}

#[derive(Clone, PartialEq, Eq)]
enum Group {
    Favorites,
    Provider(String),
}

#[derive(Clone)]
enum Row {
    Model(NativeModelOption),
    Legacy(usize),
}

struct ModelList {
    provider: AgentKind,
    models: Vec<NativeModelOption>,
    current: Option<String>,
    group: Group,
    query: String,
    expanded_legacy: bool,
    rows: Vec<Row>,
    selected: Option<IndexPath>,
    /// The models a new session's prompt also goes to, when the picker offers that.
    fan_out: Option<Vec<String>>,
}

#[derive(Clone, Copy)]
pub struct ProviderModelCatalog<'a> {
    pub(crate) provider: AgentKind,
    pub(crate) models: &'a [NativeModelOption],
    pub(crate) current: Option<&'a str>,
    pub(crate) providers: &'a [String],
    pub(crate) enabled: bool,
}

impl ModelList {
    fn rebuild(&mut self) {
        let query = self.query.trim().to_lowercase();
        let visible = self.models.iter().filter(|model| {
            let group = model_group(self.provider, &model.id);
            (match &self.group {
                Group::Favorites => model.is_favorite,
                Group::Provider(selected) => *selected == group,
            }) && (query.is_empty()
                || model.display_name.to_lowercase().contains(&query)
                || model.id.to_lowercase().contains(&query)
                || group.to_lowercase().contains(&query))
        });
        let (legacy, mut current): (Vec<_>, Vec<_>) =
            visible.cloned().partition(|model| model.is_legacy);
        current.sort_by_key(|model| self.current.as_ref() != Some(&model.id));
        self.rows = current.into_iter().map(Row::Model).collect();
        if !legacy.is_empty() {
            if query.is_empty() && self.group != Group::Favorites {
                self.rows.push(Row::Legacy(legacy.len()));
            }
            if self.expanded_legacy || !query.is_empty() || self.group == Group::Favorites {
                self.rows.extend(legacy.into_iter().map(Row::Model));
            }
        }
    }

    fn confirm_row(&mut self, index: usize, cx: &mut Context<ListState<Self>>) {
        match self.rows.get(index) {
            Some(Row::Model(model)) => cx.emit(ModelPickerEvent::Select(model.id.clone())),
            Some(Row::Legacy(_)) => {
                self.expanded_legacy = !self.expanded_legacy;
                self.rebuild();
                cx.notify();
            }
            None => {}
        }
    }
}

impl EventEmitter<ModelPickerEvent> for ListState<ModelList> {}

impl ListDelegate for ModelList {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.rows.len()
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = index;
    }

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        query.clone_into(&mut self.query);
        self.rebuild();
        Task::ready(())
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        if let Some(index) = self.selected {
            let disclosure = matches!(self.rows.get(index.row), Some(Row::Legacy(_)));
            self.confirm_row(index.row, cx);
            if disclosure {
                cx.focus_self(window);
            }
        }
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(if self.group == Group::Favorites && self.query.is_empty() {
                "No favorite models"
            } else {
                "No matching models"
            })
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        match self.rows.get(index.row)?.clone() {
            Row::Legacy(count) => Some(legacy_row(count, self.expanded_legacy, cx)),
            Row::Model(model) => {
                let group = model_group(self.provider, &model.id);
                let selected = self.current.as_ref() == Some(&model.id);
                let jump = self
                    .rows
                    .iter()
                    .take(index.row)
                    .filter(|row| matches!(row, Row::Model(_)))
                    .count();
                let hint = (jump < 9)
                    .then(|| {
                        Kbd::binding_for_action(
                            &SelectModel(jump),
                            Some("BoottyModelPicker"),
                            window,
                        )
                    })
                    .flatten();
                Some(
                    ListItem::new(SharedString::from(format!("model-option:{}", model.id)))
                        .accessibility_label(format!(
                            "{} · {group}{}",
                            model.display_name,
                            if selected { " · selected" } else { "" }
                        ))
                        .h_12()
                        .py_0()
                        .px_2()
                        .child(
                            div()
                                .debug_selector({
                                    let id = model.id.clone();
                                    move || format!("model-option:{id}")
                                })
                                .flex()
                                .items_center()
                                .h(rems(3.0))
                                .w_full()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .line_height(rems(1.125))
                                                .text_ellipsis()
                                                .child(model.display_name.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .text_xs()
                                                .line_height(rems(0.875))
                                                .text_color(crate::gpui::provider_color(&group, cx))
                                                .child(crate::gpui::sized_icon(
                                                    group_icon(&group, self.provider),
                                                    crate::gpui::IconSize::XSmall,
                                                    crate::gpui::provider_color(&group, cx),
                                                ))
                                                .child(group_label(&group)),
                                        ),
                                )
                                .child(div().w_4().flex_shrink_0().when(selected, |slot| {
                                    slot.child(Icon::new(IconName::Check).small())
                                }))
                                .child(div().w(rems(2.0)).flex_shrink_0().children(hint))
                                .children(self.fan_out.as_ref().filter(|_| !selected).map(
                                    |fan_out| {
                                        fan_out_button(&model, fan_out.contains(&model.id), cx)
                                    },
                                ))
                                .child(favorite_button(&model, cx)),
                        ),
                )
            }
        }
    }
}

// Popup focus, availability and the selected speed are independent facts.
#[allow(clippy::struct_excessive_bools)]
pub struct ModelPickerView {
    list: Entity<ListState<ModelList>>,
    trigger_focus: FocusHandle,
    reasoning_focus: FocusHandle,
    focus_reasoning: bool,
    open: bool,
    enabled: bool,
    providers: Vec<String>,
    groups: Vec<String>,
    reasoning_effort: Option<String>,
    fast_mode: bool,
    available_efforts: Vec<String>,
    _subscriptions: Vec<Subscription>,
}

impl ModelPickerView {
    /// Offers sending the prompt to more models, `fan_out` being those already chosen; `None`
    /// offers nothing.
    pub fn set_fan_out(&mut self, fan_out: Option<Vec<String>>, cx: &mut Context<Self>) {
        self.list.update(cx, |list, cx| {
            let delegate = list.delegate_mut();
            if delegate.fan_out != fan_out {
                delegate.fan_out = fan_out;
                cx.notify();
            }
        });
    }

    pub fn set_fast_mode(&mut self, fast: bool, cx: &mut Context<Self>) {
        if self.fast_mode != fast {
            self.fast_mode = fast;
            cx.notify();
        }
    }
    pub fn set_reasoning_effort(&mut self, value: Option<&str>, cx: &mut Context<Self>) {
        if self.reasoning_effort.as_deref() != value {
            self.reasoning_effort = value.map(str::to_owned);
            cx.notify();
        }
    }

    pub(crate) fn set_available_efforts(&mut self, efforts: &[String], cx: &mut Context<Self>) {
        if self.available_efforts != efforts {
            self.available_efforts = efforts.to_vec();
            cx.notify();
        }
    }

    fn reasoning_options(&self, model: &NativeModelOption) -> Vec<String> {
        model
            .reasoning_efforts
            .iter()
            .filter(|effort| {
                self.available_efforts.is_empty() || self.available_efforts.contains(effort)
            })
            .cloned()
            .collect()
    }

    pub(crate) fn change_effort(&self, increase: bool, cx: &mut Context<Self>) -> bool {
        if !self.enabled {
            return false;
        }
        let delegate = self.list.read(cx).delegate();
        let Some(model) = delegate
            .models
            .iter()
            .find(|model| delegate.current.as_ref() == Some(&model.id))
        else {
            return false;
        };
        let current = self.reasoning_selection(model);
        let selection =
            step_reasoning_effort(model, current.as_deref(), &self.available_efforts, increase);
        if let Some(selection) = selection {
            cx.emit(ModelPickerEvent::Reasoning(selection));
        }
        true
    }

    fn reasoning_selection(&self, model: &NativeModelOption) -> Option<String> {
        self.reasoning_effort
            .as_ref()
            .and_then(|value| {
                model.reasoning_efforts.iter().find(|effort| {
                    *effort == value || crate::gpui_agent_session::reasoning_label(effort) == *value
                })
            })
            .cloned()
            .or_else(|| model.default_reasoning_effort.clone())
    }
    pub fn new(
        provider: AgentKind,
        models: Vec<NativeModelOption>,
        current: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_providers(provider, models, current, Vec::new(), window, cx)
    }

    pub fn new_with_providers(
        provider: AgentKind,
        models: Vec<NativeModelOption>,
        current: Option<&str>,
        providers: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = current.map(str::to_owned).or_else(|| {
            models
                .iter()
                .find(|model| model.is_default)
                .map(|model| model.id.clone())
        });
        let selected = models
            .iter()
            .find(|model| current.as_ref() == Some(&model.id));
        let group = selected.map_or_else(
            || provider.to_string(),
            |model| model_group(provider, &model.id),
        );
        let expanded_legacy = selected.is_some_and(|model| model.is_legacy);
        let mut delegate = ModelList {
            provider,
            models,
            current,
            group: Group::Provider(group),
            query: String::new(),
            expanded_legacy,
            rows: Vec::new(),
            selected: None,
            fan_out: None,
        };
        delegate.rebuild();
        let groups = model_groups(&delegate);
        let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
        let events = cx.subscribe_in(
            &list,
            window,
            |this, _, event: &ModelPickerEvent, window, cx| {
                if matches!(event, ModelPickerEvent::Select(_)) {
                    this.close(window, cx);
                }
                cx.emit(event.clone());
            },
        );
        let dismiss = cx.subscribe_in(&list, window, |this, _, event: &ListEvent, window, cx| {
            if matches!(event, ListEvent::Cancel) {
                this.close(window, cx);
            }
        });
        let layout = cx.observe(&list, |_, _, cx| cx.notify());
        Self {
            list,
            trigger_focus: cx.focus_handle().tab_stop(true),
            reasoning_focus: cx.focus_handle(),
            focus_reasoning: false,
            open: false,
            enabled: true,
            providers,
            groups,
            reasoning_effort: None,
            fast_mode: false,
            available_efforts: Vec::new(),
            _subscriptions: vec![events, dismiss, layout],
        }
    }

    pub fn set_models(
        &mut self,
        models: &[NativeModelOption],
        current: Option<&str>,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let provider = self.list.read(cx).delegate().provider;
        let providers = self.providers.clone();
        self.set_provider_models(
            ProviderModelCatalog {
                provider,
                models,
                current,
                providers: &providers,
                enabled,
            },
            window,
            cx,
        );
    }

    pub fn set_provider_models(
        &mut self,
        catalog: ProviderModelCatalog<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ProviderModelCatalog {
            provider,
            models,
            current,
            providers,
            enabled,
        } = catalog;
        let changed_enabled = self.enabled != enabled;
        self.enabled = enabled;
        let changed_providers = self.providers != providers;
        self.providers = providers.to_vec();
        let changed_models = self.list.update(cx, |list, cx| {
            let delegate = list.delegate_mut();
            let changed_provider = delegate.provider != provider;
            let selected = current.map(str::to_owned).or_else(|| models.iter().find(|model| model.is_default).map(|model| model.id.clone()));
            if changed_provider || delegate.models != models || delegate.current != selected {
                delegate.provider = provider;
                delegate.models = models.to_vec();
                delegate.current = selected;
                let available_groups = model_groups(delegate);
                let preferred_group = delegate.current.as_deref().map_or_else(
                    || provider.to_string(),
                    |id| model_group(provider, id),
                );
                if changed_provider
                    || matches!(&delegate.group, Group::Provider(group) if !available_groups.contains(group))
                {
                    delegate.group = Group::Provider(
                        available_groups
                            .iter()
                            .find(|group| group.as_str() == preferred_group.as_str())
                            .cloned()
                            .or_else(|| available_groups.first().cloned())
                            .unwrap_or_else(|| provider.to_string()),
                    );
                }
                if changed_provider {
                    delegate.expanded_legacy = delegate
                        .current
                        .as_ref()
                        .and_then(|current| models.iter().find(|model| &model.id == current))
                        .is_some_and(|model| model.is_legacy);
                }
                delegate.rebuild();
                cx.notify();
                return true;
            }
            false
        });
        if changed_models {
            self.groups = model_groups(self.list.read(cx).delegate());
        }
        if !enabled && self.open {
            self.close(window, cx);
        }
        if changed_models || changed_enabled || changed_providers {
            cx.notify();
        }
    }

    pub(crate) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }
        self.open = true;
        self.focus_reasoning = false;
        self.list.update(cx, |list, cx| {
            list.set_selected_index(Some(IndexPath::default()), window, cx);
            list.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn open_reasoning(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open(window, cx);
        self.focus_reasoning = true;
        self.reasoning_focus.focus(window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.focus_reasoning = false;
        self.trigger_focus.focus(window, cx);
        cx.notify();
    }

    fn select_group(&self, group: Group, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |list, cx| {
            list.delegate_mut().group = group;
            list.delegate_mut().rebuild();
            list.set_selected_index(Some(IndexPath::default()), window, cx);
            list.scroll_to_item(
                IndexPath::default(),
                gpui_kit::ScrollStrategy::Top,
                window,
                cx,
            );
            list.focus(window, cx);
            cx.notify();
        });
        cx.notify();
    }

    fn select_jump(&mut self, jump: usize, window: &mut Window, cx: &mut Context<Self>) {
        let model = self
            .list
            .read(cx)
            .delegate()
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Model(model) => Some(model.id.clone()),
                Row::Legacy(_) => None,
            })
            .nth(jump);
        if self.open
            && let Some(model) = model
        {
            self.close(window, cx);
            cx.emit(ModelPickerEvent::Select(model));
        }
    }

    fn render_provider_rail(&self, cx: &Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let delegate = self.list.read(cx).delegate();
        let group = delegate.group.clone();
        let provider = delegate.provider;
        let active_provider = provider.display_name();
        let providers = self.providers.clone();
        let groups = self.groups.clone();
        div()
            .id("model-provider-rail")
            .w(rems(2.75))
            .flex_shrink_0()
            .p_1()
            .flex()
            .flex_col()
            .gap_1()
            .overflow_y_scroll()
            .bg(cx.theme().muted.opacity(0.35))
            .child(
                Button::new("model-favorites")
                    .debug_selector(|| "model-favorites".into())
                    .ghost()
                    .small()
                    .w_8()
                    .h_8()
                    .flex_shrink_0()
                    .icon(IconName::Star)
                    .selected(group == Group::Favorites)
                    .accessibility_label("Favorite models")
                    .element_tooltip("Favorite models")
                    .on_click(move |_, window, cx| {
                        _ = owner.update(cx, |this, cx| {
                            this.select_group(Group::Favorites, window, cx);
                        });
                    }),
            )
            .child(div().h_px().bg(cx.theme().border.opacity(0.5)).my_1())
            .children(providers.iter().map(|provider| {
                let owner = cx.entity().downgrade();
                let provider = provider.clone();
                let selected = provider.eq_ignore_ascii_case(active_provider);
                let label = provider.clone();
                Button::new(SharedString::from(format!(
                    "model-agent-provider:{provider}"
                )))
                .debug_selector({
                    let provider = provider.clone();
                    move || format!("model-agent-provider:{provider}")
                })
                .ghost()
                .small()
                .w_8()
                .h_8()
                .flex_shrink_0()
                .selected(selected)
                .accessibility_label(format!("{label} provider"))
                .element_tooltip(format!("{label} provider"))
                .child(crate::gpui::sized_icon(
                    provider_icon(&provider),
                    crate::gpui::IconSize::Medium,
                    crate::gpui::provider_color(&provider, cx),
                ))
                .on_click(move |_, _, cx| {
                    _ = owner.update(cx, |_, cx| {
                        cx.emit(ModelPickerEvent::SelectProvider(provider.clone()));
                    });
                })
            }))
            .when(provider == AgentKind::Pi && !groups.is_empty(), |rail| {
                rail.child(div().h_px().bg(cx.theme().border.opacity(0.5)).my_1())
                    .children(groups.iter().map(|id| {
                        let owner = cx.entity().downgrade();
                        let id = id.clone();
                        let label = group_label(&id);
                        Button::new(SharedString::from(format!("model-provider:{id}")))
                            .debug_selector({
                                let id = id.clone();
                                move || format!("model-provider:{id}")
                            })
                            .ghost()
                            .small()
                            .w_8()
                            .h_8()
                            .flex_shrink_0()
                            .selected(group == Group::Provider(id.clone()))
                            .accessibility_label(format!("{label} models"))
                            .element_tooltip(format!("{label} models"))
                            .child(crate::gpui::sized_icon(
                                group_icon(&id, provider),
                                crate::gpui::IconSize::Medium,
                                crate::gpui::provider_color(&id, cx),
                            ))
                            .on_click(move |_, window, cx| {
                                _ = owner.update(cx, |this, cx| {
                                    this.select_group(Group::Provider(id.clone()), window, cx);
                                });
                            })
                    }))
            })
    }

    fn render_content(&self, _: &Window, cx: &Context<Self>) -> impl IntoElement {
        let delegate = self.list.read(cx).delegate();
        let rows = u8::try_from(delegate.rows.len().clamp(2, 7)).unwrap_or(7);
        let current = delegate
            .models
            .iter()
            .find(|model| delegate.current.as_ref() == Some(&model.id));
        let body = div()
            .debug_selector(|| "model-picker-content".into())
            .key_context("BoottyModelPicker")
            .on_action(cx.listener(Self::select_jump_action))
            .flex()
            .items_stretch()
            .w(rems(22.5))
            .h(rems(3.0f32.mul_add(f32::from(rows), 3.0)))
            .child(self.render_provider_rail(cx))
            .child(
                List::new(&self.list)
                    .search_placeholder("Search models…")
                    .small()
                    .w_full()
                    .h_full()
                    .min_w_0(),
            );
        div()
            .key_context("BoottyModelPicker")
            .on_action(
                cx.listener(|this, _: &gpui_kit::base::actions::Confirm, window, cx| {
                    if this.reasoning_focus.is_focused(window)
                        && this.select_reasoning_key("enter", cx)
                    {
                        cx.stop_propagation();
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .flex()
            .flex_col()
            .child(body)
            .when_some(
                current.filter(|model| !model.reasoning_efforts.is_empty()),
                |popup, model| popup.child(self.render_reasoning_options(model, cx)),
            )
            .when(
                delegate.provider != AgentKind::Pi
                    && (self.fast_mode || current.is_some_and(|model| model.supports_fast_mode)),
                |popup| {
                    popup.child(
                        div()
                            .p_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .child(
                                div().debug_selector(|| "model-fast-mode".into()).child(
                                    Switch::new("model-fast-mode")
                                        .small()
                                        .label("Fast mode")
                                        .checked(self.fast_mode)
                                        .disabled(!self.enabled)
                                        .on_click(cx.listener(|_, fast, _, cx| {
                                            cx.emit(ModelPickerEvent::FastMode(*fast));
                                        })),
                                ),
                            )
                            // What the provider says fast mode trades, to choose by.
                            .children(
                                current
                                    .and_then(|model| model.fast_mode_detail.clone())
                                    .map(|detail| {
                                        div()
                                            .debug_selector(|| "model-fast-mode-detail".into())
                                            .pt_1()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(detail)
                                    }),
                            ),
                    )
                },
            )
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    // A nested picker consumes Escape before the enclosing composer dialog.
                    if event.keystroke.key == "escape" {
                        this.close(window, cx);
                        cx.stop_propagation();
                    } else if this.reasoning_focus.is_focused(window)
                        && this.select_reasoning_key(&event.keystroke.key, cx)
                    {
                        cx.stop_propagation();
                    }
                }),
            )
    }

    fn render_reasoning_options(
        &self,
        model: &NativeModelOption,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let selected = self.reasoning_selection(model);
        let options = self.reasoning_options(model);
        div()
            .p_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_wrap()
            .gap_1()
            .children(options.iter().map(|effort| {
                let selection = bootty_agents::NativeModelSelection {
                    plan_mode: None,
                    fast_mode: None,
                    model: model.id.clone(),
                    reasoning_effort: Some(effort.clone()),
                };
                let button = Button::new(SharedString::from(format!("model-effort:{effort}")))
                    .small()
                    .ghost()
                    .selected(selected.as_ref() == Some(effort))
                    .disabled(!self.enabled)
                    .label(crate::gpui_agent_session::reasoning_label(effort))
                    .debug_selector({
                        let effort = effort.clone();
                        move || format!("model-effort:{effort}")
                    })
                    .accessibility_label(format!(
                        "Reasoning effort: {}",
                        crate::gpui_agent_session::reasoning_label(effort)
                    ))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ModelPickerEvent::Reasoning(selection.clone()));
                    }));
                div()
                    .id(SharedString::from(format!("model-effort-focus:{effort}")))
                    .rounded_md()
                    .focus_visible(|style| style.border_1().border_color(cx.theme().primary))
                    .when(
                        selected.as_ref().or_else(|| options.first()) == Some(effort),
                        |row| row.track_focus(&self.reasoning_focus),
                    )
                    .child(button)
            }))
    }

    fn select_reasoning_key(&self, key: &str, cx: &mut Context<Self>) -> bool {
        let delegate = self.list.read(cx).delegate();
        let Some(model) = delegate
            .models
            .iter()
            .find(|model| delegate.current.as_ref() == Some(&model.id))
        else {
            return false;
        };
        let selected = self.reasoning_selection(model);
        let options = self.reasoning_options(model);
        let index = options
            .iter()
            .position(|effort| selected.as_ref() == Some(effort))
            .unwrap_or(0);
        let next = match key {
            "enter" | "space" => index,
            "right" | "down" => index
                .saturating_add(1)
                .checked_rem(options.len())
                .unwrap_or(0),
            "left" | "up" => index
                .checked_sub(1)
                .unwrap_or_else(|| options.len().saturating_sub(1)),
            _ => return false,
        };
        let Some(effort) = options.get(next) else {
            return false;
        };
        let selection = bootty_agents::NativeModelSelection {
            plan_mode: None,
            fast_mode: None,
            model: model.id.clone(),
            reasoning_effort: Some(effort.clone()),
        };
        cx.emit(ModelPickerEvent::Reasoning(selection));
        true
    }

    fn open_changed(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open = open;
        if open && self.focus_reasoning {
            self.reasoning_focus.focus(window, cx);
        } else if open {
            self.list.update(cx, |list, cx| {
                list.set_selected_index(Some(IndexPath::default()), window, cx);
                list.focus(window, cx);
            });
        } else {
            self.focus_reasoning = false;
            self.trigger_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn select_jump_action(
        &mut self,
        action: &SelectModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_jump(action.0, window, cx);
    }
}

fn provider_icon(provider: &str) -> &'static str {
    match provider.to_ascii_lowercase().as_str() {
        "codex" => AgentKind::Codex.icon(),
        "claude" => AgentKind::Claude.icon(),
        "pi" => AgentKind::Pi.icon(),
        _ => "bot",
    }
}

impl EventEmitter<ModelPickerEvent> for ModelPickerView {}

impl Focusable for ModelPickerView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.trigger_focus.clone()
    }
}

impl Render for ModelPickerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let delegate = self.list.read(cx).delegate();
        let current = delegate
            .models
            .iter()
            .find(|model| delegate.current.as_ref() == Some(&model.id));
        let label = current.map_or_else(
            || {
                delegate
                    .current
                    .clone()
                    .unwrap_or_else(|| delegate.provider.display_name().to_owned())
            },
            |model| model.display_name.clone(),
        );
        let integrated = !self.providers.is_empty();
        let group = if integrated {
            delegate.provider.to_string()
        } else {
            current.map_or_else(
                || delegate.provider.to_string(),
                |model| model_group(delegate.provider, &model.id),
            )
        };
        let icon = group_icon(&group, delegate.provider);
        let owner = cx.entity().downgrade();
        let content = cx.entity().downgrade();
        let focus = if self.focus_reasoning {
            self.reasoning_focus.clone()
        } else {
            self.list.read(cx).focus_handle(cx)
        };
        let picker = Popover::new("model-picker")
            .anchor(gpui_kit::Anchor::BottomLeft)
            .p_0()
            .open(self.open)
            .track_focus(&focus)
            .trigger(
                Button::new("model-picker-trigger")
                    .debug_selector(|| "model-picker-trigger".into())
                    .ghost()
                    .small()
                    .min_w_0()
                    .max_w(rems(20.))
                    .dropdown_caret(true)
                    .disabled(!self.enabled)
                    .accessibility_label(if integrated {
                        format!("{} model: {label}", delegate.provider.display_name())
                    } else {
                        format!("Model: {label}")
                    })
                    .child(crate::gpui::sized_icon(
                        icon,
                        crate::gpui::IconSize::Small,
                        crate::gpui::provider_color(&group, cx),
                    ))
                    .children(
                        (self.fast_mode && delegate.provider != AgentKind::Pi).then(|| {
                            crate::gpui::sized_icon(
                                "zap",
                                crate::gpui::IconSize::Small,
                                cx.theme().warning,
                            )
                        }),
                    )
                    .child(
                        div()
                            .debug_selector({
                                let label = label.clone();
                                move || format!("model-label:{label}")
                            })
                            .min_w_0()
                            .text_ellipsis()
                            .child(label),
                    )
                    .when_some(
                        current.and_then(|model| self.reasoning_selection(model)),
                        |button, effort| {
                            button.child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::gpui_agent_session::reasoning_label(&effort)),
                            )
                        },
                    ),
            )
            .on_open_change(move |open, window, cx| {
                _ = owner.update(cx, |this, cx| {
                    this.open_changed(*open, window, cx);
                });
            })
            .content(move |_, window, cx| {
                content
                    .update(cx, |this, cx| {
                        this.render_content(window, cx).into_any_element()
                    })
                    .unwrap_or_else(|_| gpui_kit::Empty.into_any_element())
            });
        self.render_trigger(picker)
    }
}

impl ModelPickerView {
    fn render_trigger(&self, picker: impl IntoElement) -> gpui_kit::AnyElement {
        // Kit buttons own their focus handle; retain ours on their enclosing element.
        crate::gpui::with_composer_tooltip(
            "model-picker-tooltip",
            div()
                .min_w_0()
                .track_focus(&self.trigger_focus)
                .child(picker),
            "Model and reasoning effort",
            vec![crate::gpui::composer_focus_action(
                crate::gpui::ComposerControl::Model,
                self.trigger_focus.clone(),
            )],
        )
    }
}

fn model_group(provider: AgentKind, id: &str) -> String {
    if provider == AgentKind::Pi {
        id.split_once('/')
            .map_or("pi", |(backend, _)| backend)
            .to_owned()
    } else {
        provider.to_string()
    }
}

fn model_groups(delegate: &ModelList) -> Vec<String> {
    let mut groups = Vec::new();
    for model in &delegate.models {
        let group = model_group(delegate.provider, &model.id);
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    groups
}

fn group_label(group: &str) -> String {
    match group {
        "codex" => "Codex",
        "claude" => "Claude",
        "pi" => "Pi",
        "openai" => "OpenAI",
        "openai-codex" => "OpenAI Codex",
        "anthropic" => "Anthropic",
        "google" => "Google",
        "google-gemini-cli" => "Gemini CLI",
        "openrouter" => "OpenRouter",
        "github-copilot" => "GitHub Copilot",
        other => other,
    }
    .to_owned()
}

fn group_icon(group: &str, provider: AgentKind) -> &'static str {
    match group {
        "openai" | "openai-codex" | "codex" => AgentKind::Codex.icon(),
        "anthropic" | "claude" => AgentKind::Claude.icon(),
        "google" | "google-gemini-cli" => "bootstrap:google",
        "github-copilot" => "bootstrap:github",
        "fireworks" => "sparkles",
        "openrouter" => "route",
        "radius" => "circle-dot",
        "ollama" => "hard-drive",
        "pi" => provider.icon(),
        _ => "cloud",
    }
}

fn legacy_row(count: usize, expanded: bool, cx: &App) -> ListItem {
    ListItem::new("model-legacy-disclosure")
        .accessibility_label(format!(
            "Legacy models: {count}, {}",
            if expanded { "expanded" } else { "collapsed" }
        ))
        .h_12()
        .py_0()
        .px_2()
        .border_t_1()
        .border_color(cx.theme().border.opacity(0.5))
        .child(
            div()
                .debug_selector(|| "model-legacy-disclosure".into())
                .flex()
                .items_center()
                .h(rems(3.0))
                .w_full()
                .gap_2()
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .small(),
                )
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Legacy models"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(count.to_string()),
                ),
        )
}

fn fan_out_button(
    model: &NativeModelOption,
    chosen: bool,
    cx: &Context<ListState<ModelList>>,
) -> Button {
    let owner = cx.entity().downgrade();
    let id = model.id.clone();
    let label = if chosen {
        format!("Don't send to {} too", model.display_name)
    } else {
        format!("Send to {} too", model.display_name)
    };
    Button::new(SharedString::from(format!("fan-out-model:{}", model.id)))
        .debug_selector({
            let id = model.id.clone();
            move || format!("model-fan-out:{id}")
        })
        .ghost()
        .xsmall()
        .w_6()
        .h_6()
        .flex_shrink_0()
        .icon(Icon::default().path("icons/copy-plus.svg"))
        .selected(chosen)
        .text_color(if chosen {
            cx.theme().primary
        } else {
            cx.theme().muted_foreground
        })
        .accessibility_label(label.clone())
        .element_tooltip(label)
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            _ = owner.update(cx, |list, cx| {
                list.focus(window, cx);
                cx.emit(ModelPickerEvent::FanOut(id.clone()));
            });
        })
}

fn favorite_button(model: &NativeModelOption, cx: &Context<ListState<ModelList>>) -> Button {
    let owner = cx.entity().downgrade();
    let id = model.id.clone();
    let label = format!(
        "{} {}",
        if model.is_favorite {
            "Unfavorite"
        } else {
            "Favorite"
        },
        model.display_name
    );
    Button::new(SharedString::from(format!("favorite-model:{}", model.id)))
        .debug_selector({
            let id = model.id.clone();
            move || format!("model-favorite:{id}")
        })
        .ghost()
        .xsmall()
        .w_6()
        .h_6()
        .flex_shrink_0()
        .icon(IconName::Star)
        .selected(model.is_favorite)
        .text_color(if model.is_favorite {
            cx.theme().warning
        } else {
            cx.theme().muted_foreground
        })
        .accessibility_label(label.clone())
        .element_tooltip(label)
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            _ = owner.update(cx, |list, cx| {
                list.focus(window, cx);
                cx.emit(ModelPickerEvent::Favorite(id.clone()));
            });
        })
}

/// Step in provider-advertised order, skipping hidden levels and stopping at either end.
#[must_use]
fn step_reasoning_effort(
    model: &NativeModelOption,
    current: Option<&str>,
    available: &[String],
    increase: bool,
) -> Option<bootty_agents::NativeModelSelection> {
    let index = model
        .reasoning_efforts
        .iter()
        .position(|effort| Some(effort.as_str()) == current);
    let mut choices = model
        .reasoning_efforts
        .iter()
        .enumerate()
        .filter(|(_, effort)| available.is_empty() || available.contains(effort));
    let next = if increase {
        choices.find(|(candidate, _)| index.is_none_or(|index| *candidate > index))
    } else {
        choices.rfind(|(candidate, _)| index.is_none_or(|index| *candidate < index))
    }?;
    Some(bootty_agents::NativeModelSelection {
        plan_mode: None,
        fast_mode: None,
        model: model.id.clone(),
        reasoning_effort: Some(next.1.clone()),
    })
}
