//! MCP forms and sign-in pages in the request panel. The agent crate's schema decides what each
//! field accepts; this view collects answers and shows the first problem beside Submit.
use super::{NativeRequestOperation, NativeRequestView, field, request_details};
use bootty_agents::{Elicitation, ElicitationField, FieldKind, NativeAgentRequest, TextFormat};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, SharedString, Styled as _,
    Subscription, Window, div, prelude::*,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Answers being given to one MCP form: typed text by field, and chosen options by field. A
/// checked box is the option `true`.
#[derive(Default)]
pub(super) struct FormState {
    inputs: BTreeMap<String, Entity<InputState>>,
    chosen: BTreeMap<String, BTreeSet<String>>,
    subscriptions: Vec<Subscription>,
}

impl NativeRequestView {
    /// One state per pending MCP form, seeded with the server's defaults.
    pub(super) fn sync_forms(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let forms = self
            .requests
            .iter()
            .filter_map(|request| match request.elicitation() {
                Some(Elicitation::Form(fields)) => Some((request.id.clone(), fields)),
                _ => None,
            })
            .collect::<Vec<_>>();
        self.forms
            .retain(|id, _| forms.iter().any(|(form, _)| form == id));
        for (id, fields) in forms {
            if self.forms.contains_key(&id) {
                continue;
            }
            let mut state = FormState::default();
            for form_field in &fields {
                let key = form_field.key.clone();
                let (text, placeholder) = match &form_field.kind {
                    FieldKind::Text {
                        format, default, ..
                    } => (default.clone(), format.map(placeholder)),
                    FieldKind::Number { default, .. } => (default.map(number_text), None),
                    FieldKind::Boolean { default } => {
                        if *default {
                            state
                                .chosen
                                .insert(key, BTreeSet::from(["true".to_owned()]));
                        }
                        continue;
                    }
                    FieldKind::Choice { default, .. } => {
                        state.chosen.insert(key, default.iter().cloned().collect());
                        continue;
                    }
                    FieldKind::Choices { default, .. } => {
                        state.chosen.insert(key, default.iter().cloned().collect());
                        continue;
                    }
                };
                let input = cx.new(|cx| {
                    let input = InputState::new(window, cx).default_value(text.unwrap_or_default());
                    match placeholder {
                        Some(placeholder) => input.placeholder(placeholder),
                        None => input,
                    }
                });
                state.subscriptions.push(cx.subscribe_in(
                    &input,
                    window,
                    |_, _, _: &InputEvent, _, cx| cx.notify(),
                ));
                state.inputs.insert(key, input);
            }
            self.forms.insert(id, state);
        }
    }

    /// The non-empty answers by field key, typed as each field expects.
    fn form_answers(
        &self,
        id: &str,
        fields: &[ElicitationField],
        cx: &Context<Self>,
    ) -> Map<String, Value> {
        let Some(state) = self.forms.get(id) else {
            return Map::new();
        };
        let mut answers = Map::new();
        for form_field in fields {
            let chosen = state.chosen.get(&form_field.key);
            let text = state
                .inputs
                .get(&form_field.key)
                .map(|input| input.read(cx).value().to_string())
                .filter(|text| !text.trim().is_empty());
            let answer = match &form_field.kind {
                FieldKind::Text { .. } => text.map(Value::String),
                // Unparseable text stays text, so the field reports it.
                FieldKind::Number { .. } => text.map(|text| {
                    let text = text.trim();
                    text.parse::<i64>()
                        .map(Value::from)
                        .ok()
                        .or_else(|| {
                            text.parse::<f64>()
                                .ok()
                                .and_then(serde_json::Number::from_f64)
                                .map(Value::Number)
                        })
                        .unwrap_or_else(|| Value::String(text.to_owned()))
                }),
                FieldKind::Boolean { .. } => Some(Value::Bool(
                    chosen.is_some_and(|chosen| chosen.contains("true")),
                )),
                FieldKind::Choice { options, .. } => options
                    .iter()
                    .find(|option| chosen.is_some_and(|chosen| chosen.contains(&option.value)))
                    .map(|option| Value::String(option.value.clone())),
                FieldKind::Choices { options, .. } => {
                    let values = options
                        .iter()
                        .filter(|option| {
                            chosen.is_some_and(|chosen| chosen.contains(&option.value))
                        })
                        .map(|option| Value::String(option.value.clone()))
                        .collect::<Vec<_>>();
                    (!values.is_empty()).then_some(Value::Array(values))
                }
            };
            if let Some(answer) = answer {
                answers.insert(form_field.key.clone(), answer);
            }
        }
        answers
    }

    fn choose_form_option(
        &mut self,
        id: &str,
        key: &str,
        value: &str,
        multiple: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(chosen) = self
            .forms
            .get_mut(id)
            .map(|state| state.chosen.entry(key.to_owned()).or_default())
        else {
            return;
        };
        if chosen.remove(value) {
            cx.notify();
            return;
        }
        if !multiple {
            chosen.clear();
        }
        chosen.insert(value.to_owned());
        cx.notify();
    }

    /// A form, a sign-in page, or a request Bootty can only decline.
    pub(super) fn render_elicitation(
        &self,
        row: gpui_kit::Stateful<gpui_kit::Div>,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let message = field(&request.parameters, "message")
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let row = row.child(div().text_sm().child(message));
        match request.elicitation() {
            Some(Elicitation::Form(fields)) => self.render_form(row, request, &fields, pending, cx),
            Some(Elicitation::Url { url, host }) => row
                .child(request_details(
                    SharedString::from(format!("elicitation-url:{}", request.id)),
                    url.clone(),
                ))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(self.open_page_button(request, url, &host, pending, cx))
                        .child(self.decline_button(request, pending, cx)),
                ),
            _ => row
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Bootty can't answer this kind of request."),
                )
                .child(self.decline_button(request, pending, cx)),
        }
    }

    fn render_form(
        &self,
        row: gpui_kit::Stateful<gpui_kit::Div>,
        request: &NativeAgentRequest,
        fields: &[ElicitationField],
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let id = request.id.clone();
        let answers = self.form_answers(&id, fields, cx);
        let response = bootty_agents::form_response(fields, &answers);
        let problem = response.as_ref().err().map(|(_, problem)| problem.clone());
        let target = self.target.clone();
        let submit = Button::new(SharedString::from(format!("elicitation-submit:{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("elicitation-submit:{id}")
            })
            .label("Submit")
            .small()
            .primary()
            .disabled(pending || response.is_err())
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Ok(response) = &response {
                    this.request_command(
                        &target,
                        NativeRequestOperation::Respond,
                        vec![id.clone(), response.to_string()],
                        window,
                        cx,
                    );
                }
            }));
        row.children(
            fields
                .iter()
                .map(|form_field| self.render_field(request, form_field, pending, cx)),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(submit)
                .child(self.decline_button(request, pending, cx))
                .children(problem.map(|problem| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(problem)
                })),
        )
    }

    fn render_field(
        &self,
        request: &NativeAgentRequest,
        form_field: &ElicitationField,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(state) = self.forms.get(&request.id) else {
            return div().into_any_element();
        };
        let id = request.id.clone();
        let key = form_field.key.clone();
        let description = form_field.description.clone().map(|description| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(description)
        });
        let control = match &form_field.kind {
            FieldKind::Boolean { .. } => {
                let checked = state
                    .chosen
                    .get(&key)
                    .is_some_and(|chosen| chosen.contains("true"));
                let owner = cx.weak_entity();
                return div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Checkbox::new(SharedString::from(format!("elicitation:{id}:{key}")))
                            .label(form_field.title.clone())
                            .checked(checked)
                            .disabled(pending)
                            .on_click(move |_, _, cx| {
                                _ = owner.update(cx, |this, cx| {
                                    this.choose_form_option(&id, &key, "true", true, cx);
                                });
                            }),
                    )
                    .children(description)
                    .into_any_element();
            }
            FieldKind::Text { .. } | FieldKind::Number { .. } => {
                let Some(input) = state.inputs.get(&key) else {
                    return div().into_any_element();
                };
                let text = input.read(cx).value().to_string();
                // Only an answer the person has started shows its problem here.
                let problem = (!text.trim().is_empty())
                    .then(|| {
                        let answers = self.form_answers(&id, std::slice::from_ref(form_field), cx);
                        form_field.check(answers.get(&key)).err()
                    })
                    .flatten();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .debug_selector(move || format!("elicitation-input:{id}:{key}"))
                            .child(
                                Input::new(input)
                                    .context_menu(crate::gpui::text_menu(input))
                                    .aria_label(form_field.title.clone())
                                    .disabled(pending),
                            ),
                    )
                    .children(problem.map(|problem| {
                        div().text_xs().text_color(cx.theme().danger).child(problem)
                    }))
                    .into_any_element()
            }
            FieldKind::Choice { options, .. } | FieldKind::Choices { options, .. } => {
                let multiple = matches!(form_field.kind, FieldKind::Choices { .. });
                let chosen = state.chosen.get(&key);
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(options.iter().map(|option| {
                        let (id, key, value) = (id.clone(), key.clone(), option.value.clone());
                        Button::new(SharedString::from(format!(
                            "elicitation:{id}:{key}:{value}"
                        )))
                        .label(option.label.clone())
                        .small()
                        .ghost()
                        .disabled(pending)
                        .selected(chosen.is_some_and(|chosen| chosen.contains(&option.value)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose_form_option(&id, &key, &value, multiple, cx);
                        }))
                    }))
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child(form_field.title.clone()))
            .children(description)
            .child(control)
            .into_any_element()
    }

    fn open_page_button(
        &self,
        request: &NativeAgentRequest,
        url: String,
        host: &str,
        pending: bool,
        cx: &Context<Self>,
    ) -> Button {
        let id = request.id.clone();
        let target = self.target.clone();
        Button::new(SharedString::from(format!("elicitation-open:{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("elicitation-open:{id}")
            })
            .label(format!("Open {host}"))
            .small()
            .primary()
            .disabled(pending)
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.open_url(&url);
                this.request_command(
                    &target,
                    NativeRequestOperation::Respond,
                    vec![
                        id.clone(),
                        json!({"action":"accept","content":null}).to_string(),
                    ],
                    window,
                    cx,
                );
            }))
    }

    fn decline_button(
        &self,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> Button {
        let id = request.id.clone();
        let target = self.target.clone();
        Button::new(SharedString::from(format!("decline:{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("elicitation-decline:{id}")
            })
            .label("Decline")
            .small()
            .disabled(pending)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.request_command(
                    &target,
                    NativeRequestOperation::Respond,
                    vec![
                        id.clone(),
                        json!({"action":"decline","content":null}).to_string(),
                    ],
                    window,
                    cx,
                );
            }))
    }
}

const fn placeholder(format: TextFormat) -> &'static str {
    match format {
        TextFormat::Email => "name@example.com",
        TextFormat::Uri => "https://",
        TextFormat::Date => "YYYY-MM-DD",
        TextFormat::DateTime => "2026-01-31T09:30:00Z",
    }
}

/// A number as someone would type it: whole numbers without a decimal point.
fn number_text(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{number:.0}")
    } else {
        number.to_string()
    }
}
