//! Shared catalog picker; provider identity and accepted favorites remain agent-owned.
use bootty_agents::{AgentKind, NativeModelOption};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    kbd::Kbd,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    popover::Popover,
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
    Favorite(String),
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
                                .child(favorite_button(&model, cx)),
                        ),
                )
            }
        }
    }
}

pub struct ModelPickerView {
    list: Entity<ListState<ModelList>>,
    trigger_focus: FocusHandle,
    open: bool,
    enabled: bool,
    groups: Vec<String>,
    _subscriptions: Vec<Subscription>,
}

impl ModelPickerView {
    pub fn new(
        provider: AgentKind,
        models: Vec<NativeModelOption>,
        current: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected = models
            .iter()
            .find(|model| current == Some(model.id.as_str()))
            .or_else(|| models.iter().find(|model| model.is_default));
        let group = selected.map_or_else(
            || provider.to_string(),
            |model| model_group(provider, &model.id),
        );
        let expanded_legacy = selected.is_some_and(|model| model.is_legacy);
        let current = selected.map(|model| model.id.clone());
        let mut delegate = ModelList {
            provider,
            models,
            current,
            group: Group::Provider(group),
            query: String::new(),
            expanded_legacy,
            rows: Vec::new(),
            selected: None,
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
            open: false,
            enabled: true,
            groups,
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
        let changed_enabled = self.enabled != enabled;
        self.enabled = enabled;
        let changed_models = self.list.update(cx, |list, cx| {
            let delegate = list.delegate_mut();
            if delegate.models != models || delegate.current.as_deref() != current {
                delegate.models = models.to_vec();
                delegate.current = models
                    .iter()
                    .find(|model| Some(model.id.as_str()) == current)
                    .or_else(|| models.iter().find(|model| model.is_default))
                    .map(|model| model.id.clone());
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
        if changed_models || changed_enabled {
            cx.notify();
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
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

    fn render_content(&self, _: &Window, cx: &Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let delegate = self.list.read(cx).delegate();
        let group = delegate.group.clone();
        let provider = delegate.provider;
        let rows = u8::try_from(delegate.rows.len().clamp(2, 7)).unwrap_or(7);
        div()
            .debug_selector(|| "model-picker-content".into())
            .key_context("BoottyModelPicker")
            .on_action(cx.listener(Self::select_jump_action))
            .flex()
            .items_stretch()
            .w(rems(22.5))
            .h(rems(3.0f32.mul_add(f32::from(rows), 3.0)))
            .child(
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
                            .tooltip("Favorite models")
                            .on_click(move |_, window, cx| {
                                _ = owner.update(cx, |this, cx| {
                                    this.select_group(Group::Favorites, window, cx);
                                });
                            }),
                    )
                    .child(div().h_px().bg(cx.theme().border.opacity(0.5)).my_1())
                    .children(self.groups.iter().map(|id| {
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
                            .tooltip(format!("{label} models"))
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
                    })),
            )
            .child(
                List::new(&self.list)
                    .search_placeholder("Search models…")
                    .small()
                    .w_full()
                    .h_full()
                    .min_w_0(),
            )
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    // A nested picker consumes Escape before the enclosing composer dialog.
                    if event.keystroke.key == "escape" {
                        this.close(window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
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
            || delegate.provider.to_string(),
            |model| model.display_name.clone(),
        );
        let icon = current.map_or_else(
            || delegate.provider.icon(),
            |model| {
                group_icon(
                    &model_group(delegate.provider, &model.id),
                    delegate.provider,
                )
            },
        );
        let group = current.map_or_else(
            || delegate.provider.to_string(),
            |model| model_group(delegate.provider, &model.id),
        );
        let owner = cx.entity().downgrade();
        let content = cx.entity().downgrade();
        let focus = self.list.read(cx).focus_handle(cx);
        let picker = Popover::new("model-picker")
            .anchor(gpui_kit::Anchor::BottomLeft)
            .p_0()
            .open(self.open)
            .track_focus(&focus)
            .trigger(
                Button::new("model-picker-trigger")
                    .ghost()
                    .small()
                    .min_w_0()
                    .max_w(rems(20.))
                    .dropdown_caret(true)
                    .disabled(!self.enabled)
                    .accessibility_label(format!("Model: {label}"))
                    .child(crate::gpui::sized_icon(
                        icon,
                        crate::gpui::IconSize::Small,
                        crate::gpui::provider_color(&group, cx),
                    ))
                    .child(
                        div()
                            .debug_selector(|| "model-picker-trigger".into())
                            .min_w_0()
                            .text_ellipsis()
                            .child(label),
                    ),
            )
            .on_open_change(move |open, window, cx| {
                _ = owner.update(cx, |this, cx| {
                    this.open = *open;
                    if *open {
                        this.list.update(cx, |list, cx| {
                            list.set_selected_index(Some(IndexPath::default()), window, cx);
                            list.focus(window, cx);
                        });
                    } else {
                        this.trigger_focus.focus(window, cx);
                    }
                    cx.notify();
                });
            })
            .content(move |_, window, cx| {
                content
                    .update(cx, |this, cx| {
                        this.render_content(window, cx).into_any_element()
                    })
                    .unwrap_or_else(|_| gpui_kit::Empty.into_any_element())
            });
        // Kit buttons own their focus handle; retain ours on their enclosing element.
        div()
            .min_w_0()
            .track_focus(&self.trigger_focus)
            .child(picker)
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
        .tooltip(label)
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            _ = owner.update(cx, |list, cx| {
                list.focus(window, cx);
                cx.emit(ModelPickerEvent::Favorite(id.clone()));
            });
        })
}
