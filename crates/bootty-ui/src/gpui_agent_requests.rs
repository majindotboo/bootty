//! Captured provider requests own their answer controls and emit scoped response intents.
use super::tool_label;
use bootty_agents::NativeAgentRequest;
use bootty_control::CommandTarget;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    scroll::ScrollableElement as _,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div, prelude::*,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
pub(super) enum NativeRequestOperation {
    Approve,
    Respond,
}
impl NativeRequestOperation {
    pub(super) const fn id(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Respond => "respond",
        }
    }
}

pub(super) struct NativeRequestResponse {
    pub target: CommandTarget,
    pub operation: NativeRequestOperation,
    pub arguments: Vec<String>,
}

pub(super) struct NativeRequestView {
    target: CommandTarget,
    requests: Vec<NativeAgentRequest>,
    provider_enabled: bool,
    pending: BTreeSet<String>,
    answers: BTreeMap<(String, String), Entity<InputState>>,
    selected_answers: BTreeMap<(String, String), BTreeSet<String>>,
    answer_subscriptions: BTreeMap<(String, String), Subscription>,
    editors: BTreeMap<String, Entity<TextareaState>>,
    editor_subscriptions: BTreeMap<String, Subscription>,
}

impl NativeRequestView {
    pub(super) const fn new(target: CommandTarget) -> Self {
        Self {
            target,
            requests: Vec::new(),
            provider_enabled: true,
            pending: BTreeSet::new(),
            answers: BTreeMap::new(),
            selected_answers: BTreeMap::new(),
            answer_subscriptions: BTreeMap::new(),
            editors: BTreeMap::new(),
            editor_subscriptions: BTreeMap::new(),
        }
    }
    pub(super) fn update(
        &mut self,
        target: CommandTarget,
        requests: &[NativeAgentRequest],
        pending: &BTreeSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.target != target {
            self.answers.clear();
            self.selected_answers.clear();
            self.answer_subscriptions.clear();
            self.editors.clear();
            self.editor_subscriptions.clear();
        }
        self.target = target;
        self.requests = requests.to_vec();
        self.pending.clone_from(pending);
        self.sync_answers(window, cx);
        cx.notify();
    }
    pub(super) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.provider_enabled != enabled {
            self.provider_enabled = enabled;
            cx.notify();
        }
    }
    fn request_command(
        &self,
        target: &CommandTarget,
        operation: NativeRequestOperation,
        arguments: Vec<String>,
        _: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.target == *target && self.provider_enabled {
            cx.emit(NativeRequestResponse {
                target: target.clone(),
                operation,
                arguments,
            });
        }
    }
    fn sync_answers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.answers.retain(|(request_id, question_id), _| {
            self.requests.iter().any(|request| {
                &request.id == request_id
                    && ((request.method == "pi.input" && question_id == "value")
                        || questions(request).any(|question| {
                            question_key(request, question) == Some(question_id.as_str())
                        }))
            })
        });
        self.answer_subscriptions
            .retain(|key, _| self.answers.contains_key(key));
        self.selected_answers
            .retain(|key, _| self.answers.contains_key(key));
        self.sync_pi_answers(window, cx);
        for request in &self.requests {
            if request.method != "item/tool/requestUserInput" && !is_claude_question(request) {
                continue;
            }
            for question in questions(request) {
                let Some(id) = question_key(request, question) else {
                    continue;
                };
                let key = (request.id.clone(), id.to_owned());
                let std::collections::btree_map::Entry::Vacant(entry) =
                    self.answers.entry(key.clone())
                else {
                    continue;
                };
                let answer = cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(field(question, "isSecret") == true)
                        .placeholder(if is_claude_question(request) {
                            "Other answer (optional)…"
                        } else {
                            "Your answer…"
                        })
                });
                let clear_selection =
                    is_claude_question(request) && field(question, "multiSelect") != true;
                let selection_key = key.clone();
                let target = self.target.clone();
                let subscription = cx.subscribe_in(
                    &answer,
                    window,
                    move |this, answer, event: &InputEvent, _, cx| {
                        if this.target != target {
                            return;
                        }
                        if clear_selection
                            && matches!(event, InputEvent::Change)
                            && !answer.read(cx).value().trim().is_empty()
                        {
                            this.selected_answers.remove(&selection_key);
                        }
                        cx.notify();
                    },
                );
                entry.insert(answer);
                self.answer_subscriptions.insert(key, subscription);
            }
        }
    }

    fn sync_pi_answers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editors.retain(|id, _| {
            self.requests
                .iter()
                .any(|request| request.id == *id && request.method == "pi.editor")
        });
        self.editor_subscriptions
            .retain(|id, _| self.editors.contains_key(id));
        for request in &self.requests {
            if request.method == "pi.input" {
                let key = (request.id.clone(), "value".to_owned());
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    self.answers.entry(key.clone())
                {
                    let answer = cx.new(|cx| {
                        InputState::new(window, cx).placeholder(
                            field(&request.parameters, "placeholder")
                                .as_str()
                                .unwrap_or("Your answer…")
                                .to_owned(),
                        )
                    });
                    let id = request.id.clone();
                    let target = self.target.clone();
                    let subscription = cx.subscribe_in(
                        &answer,
                        window,
                        move |this, _, event: &InputEvent, window, cx| {
                            if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                                this.submit_pi_text_answer(&target, &id, window, cx);
                            }
                            cx.notify();
                        },
                    );
                    entry.insert(answer);
                    self.answer_subscriptions.insert(key, subscription);
                }
            } else if request.method == "pi.editor"
                && let std::collections::btree_map::Entry::Vacant(entry) =
                    self.editors.entry(request.id.clone())
            {
                let editor = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(3, 10)
                        .submit_on_enter(false)
                        .default_value(
                            field(&request.parameters, "prefill")
                                .as_str()
                                .unwrap_or("")
                                .to_owned(),
                        )
                });
                let subscription =
                    cx.subscribe_in(&editor, window, |_, _, _: &InputEvent, _, cx| cx.notify());
                entry.insert(editor);
                self.editor_subscriptions
                    .insert(request.id.clone(), subscription);
            }
        }
    }

    fn render_requests(&self, window: &Window, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let requests = div()
            .w_full()
            .max_w(gpui_kit::rems(48.0))
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .children(
                self.requests
                    .iter()
                    .map(|request| self.render_request(request, cx)),
            )
            .overflow_y_scrollbar();
        div()
            .h(gpui_kit::px(
                f32::from(window.viewport_size().height)
                    .mul_add(0.4, 0.0)
                    .min(256.0),
            ))
            .flex_shrink_0()
            .flex()
            .justify_center()
            .px_4()
            .child(requests)
            .into_any_element()
    }

    fn render_request(
        &self,
        request: &NativeAgentRequest,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let id = request.id.clone();
        let target = self.target.clone();
        let pending = !self.provider_enabled || self.pending.contains(&format!("request:{id}"));
        let mut row = div()
            .id(gpui_kit::SharedString::from(format!("request:{id}")))
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_2()
            .child(div().text_sm().child(request_title(request).to_owned()));
        if request.method == "claude.permission" {
            return row
                .child(self.render_claude_request(request, pending, cx))
                .into_any_element();
        }
        if matches!(
            request.method.as_str(),
            "pi.confirm" | "pi.select" | "pi.input" | "pi.editor"
        ) {
            return row
                .child(self.render_pi_request(request, pending, cx))
                .into_any_element();
        }
        if request.is_mcp_approval() || is_approval(&request.method) {
            row = self.render_approval(row, request, pending, cx);
        } else if request.method == "mcpServer/elicitation/request" {
            row = self.render_mcp_input(row, request, pending, cx);
        } else if request.method == "item/tool/requestUserInput" {
            for question in questions(request) {
                row = row.child(self.render_question(request, question, pending, cx));
            }
            let empty = questions(request).next().is_none()
                || questions(request).any(|question| {
                    field(question, "id").as_str().is_none_or(|question_id| {
                        self.answers
                            .get(&(id.clone(), question_id.to_owned()))
                            .is_none_or(|answer| answer.read(cx).value().trim().is_empty())
                    })
                });
            row = row.child(
                Button::new(gpui_kit::SharedString::from(format!("answer:{id}")))
                    .label("Send answer")
                    .small()
                    .disabled(pending || empty)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let answers = this
                            .answers
                            .iter()
                            .filter(|((request_id, _), _)| request_id == &id)
                            .map(|((_, question_id), answer)| {
                                (
                                    question_id.clone(),
                                    json!({"answers":[answer.read(cx).value().to_string()]}),
                                )
                            })
                            .collect::<serde_json::Map<_, _>>();
                        this.request_command(
                            &target,
                            NativeRequestOperation::Respond,
                            vec![id.clone(), json!({"answers":answers}).to_string()],
                            window,
                            cx,
                        );
                    })),
            );
        } else {
            row = row.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("This provider request is not supported here. Interrupt the turn to continue."));
        }
        row.into_any_element()
    }

    fn render_mcp_input(
        &self,
        row: gpui_kit::Stateful<gpui_kit::Div>,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let id = request.id.clone();
        let target = self.target.clone();
        row
                .child(request_details(
                    gpui_kit::SharedString::from(format!("elicitation-scope:{id}")),
                    approval_description(&request.parameters, crate::strings::home_dir().as_deref(), request.is_from_attached_tools()),
                ))
                .child(div().text_sm().text_color(cx.theme().muted_foreground)
                    .child("This MCP request needs structured input or authentication that Bootty does not yet support. Decline it to continue."))
                .child(Button::new(gpui_kit::SharedString::from(format!("decline:{id}")))
                    .label("Decline request")
                    .small()
                    .disabled(pending)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.request_command(&target, NativeRequestOperation::Respond, vec![id.clone(), json!({"action":"decline", "content":null}).to_string()], window, cx);
                    })))
    }

    fn render_approval(
        &self,
        row: gpui_kit::Stateful<gpui_kit::Div>,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let id = request.id.clone();
        let target = self.target.clone();
        // Permission scope comes from the provider request, never a generic allow label.
        let description = approval_description(
            &request.parameters,
            crate::strings::home_dir().as_deref(),
            request.is_from_attached_tools(),
        );
        row.child(request_details(
            gpui_kit::SharedString::from(format!("approval-scope:{id}")),
            description,
        ))
        .child(
            div().flex().flex_wrap().gap_2().children(
                bootty_agents::NativeApprovalDecision::ALL
                    .into_iter()
                    .filter(|decision| request.approval_response(*decision).is_some())
                    .map(|decision| {
                        let id = id.clone();
                        let target = target.clone();
                        Button::new(gpui_kit::SharedString::from(format!(
                            "{}:{id}",
                            decision.id()
                        )))
                        .debug_selector({
                            let id = id.clone();
                            move || format!("{}:{id}", decision.id())
                        })
                        .label(decision.label())
                        .small()
                        .when(
                            decision == bootty_agents::NativeApprovalDecision::AllowOnce,
                            gpui_kit::component::button::ButtonVariants::primary,
                        )
                        .disabled(pending)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.request_command(
                                    &target,
                                    NativeRequestOperation::Approve,
                                    vec![id.clone(), decision.id().to_owned()],
                                    window,
                                    cx,
                                );
                            },
                        ))
                    }),
            ),
        )
    }

    fn render_claude_request(
        &self,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let id = request.id.clone();
        let target = self.target.clone();
        let question = is_claude_question(request);
        let mut body = div().flex().flex_col().gap_2();
        if question {
            body = body.children(
                questions(request)
                    .map(|question| self.render_question(request, question, pending, cx)),
            );
        } else {
            body = body.child(request_details(
                gpui_kit::SharedString::from(format!("claude-approval-scope:{id}")),
                serde_json::to_string_pretty(field(&request.parameters, "input"))
                    .unwrap_or_default(),
            ));
        }
        let denied = id.clone();
        let denied_target = target.clone();
        let empty = question
            && (questions(request).next().is_none()
                || questions(request).any(|question| {
                    let Some(key) =
                        question_key(request, question).map(|key| (id.clone(), key.to_owned()))
                    else {
                        return true;
                    };
                    self.selected_answers
                        .get(&key)
                        .is_none_or(BTreeSet::is_empty)
                        && self
                            .answers
                            .get(&key)
                            .is_none_or(|answer| answer.read(cx).value().trim().is_empty())
                }));
        body.child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new(gpui_kit::SharedString::from(format!("claude-decline:{id}")))
                        .label(if question { "Cancel" } else { "Deny" })
                        .small()
                        .ghost()
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.request_command(
                                &denied_target,
                                NativeRequestOperation::Respond,
                                vec![denied.clone(), json!({"decision":"decline"}).to_string()],
                                window,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new(gpui_kit::SharedString::from(format!("claude-accept:{id}")))
                        .label(if question {
                            "Send answers"
                        } else {
                            "Allow once"
                        })
                        .small()
                        .primary()
                        .disabled(pending || empty)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if question {
                                this.submit_claude_answers(&target, &id, window, cx);
                            } else {
                                this.request_command(
                                    &target,
                                    NativeRequestOperation::Respond,
                                    vec![id.clone(), json!({"decision":"accept"}).to_string()],
                                    window,
                                    cx,
                                );
                            }
                        })),
                ),
        )
    }

    fn submit_claude_answers(
        &self,
        target: &CommandTarget,
        id: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.target != *target {
            return;
        }
        let Some(request) = self
            .requests
            .iter()
            .find(|request| request.id == id && is_claude_question(request))
        else {
            return;
        };
        let answers = questions(request)
            .filter_map(|question| {
                let key = question_key(request, question)?;
                let state_key = (id.to_owned(), key.to_owned());
                let mut selected = self
                    .selected_answers
                    .get(&state_key)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>();
                if let Some(answer) = self.answers.get(&state_key) {
                    let value = answer.read(cx).value().to_string();
                    if !value.trim().is_empty() {
                        selected.push(value);
                    }
                }
                Some((key.to_owned(), Value::String(selected.join(", "))))
            })
            .collect::<serde_json::Map<_, _>>();
        self.request_command(
            target,
            NativeRequestOperation::Respond,
            vec![id.to_owned(), json!({"answers":answers}).to_string()],
            window,
            cx,
        );
    }

    fn render_pi_request(
        &self,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let id = request.id.clone();
        let target = self.target.clone();
        let mut body = div().flex().flex_col().gap_2().when_some(
            field(&request.parameters, "message").as_str(),
            |body, message| {
                body.child(request_details(
                    gpui_kit::SharedString::from(format!("pi-request-message:{id}")),
                    message.to_owned(),
                ))
            },
        );
        if request.method == "pi.confirm" {
            body = body.child(
                div()
                    .flex()
                    .gap_2()
                    .children([(false, "No"), (true, "Yes")].into_iter().map(
                        |(confirmed, label)| {
                            let id = id.clone();
                            let target = target.clone();
                            Button::new(gpui_kit::SharedString::from(format!(
                                "pi-confirm:{id}:{confirmed}"
                            )))
                            .label(label)
                            .accessibility_label(format!("{label}: {}", request_title(request)))
                            .small()
                            .disabled(pending)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.request_command(
                                        &target,
                                        NativeRequestOperation::Respond,
                                        vec![
                                            id.clone(),
                                            json!({"confirmed":confirmed}).to_string(),
                                        ],
                                        window,
                                        cx,
                                    );
                                },
                            ))
                        },
                    )),
            );
        } else if request.method == "pi.select" {
            body = body.child(
                div().flex().flex_col().gap_1().children(
                    field(&request.parameters, "options")
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(|option| {
                            let value = option.to_owned();
                            let id = id.clone();
                            let target = target.clone();
                            Button::new(gpui_kit::SharedString::from(format!(
                                "pi-option:{id}:{value}"
                            )))
                            .label(value.clone())
                            .accessibility_label(format!("{}: {value}", request_title(request)))
                            .small()
                            .ghost()
                            .disabled(pending)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.request_command(
                                        &target,
                                        NativeRequestOperation::Respond,
                                        vec![id.clone(), json!({"value":value}).to_string()],
                                        window,
                                        cx,
                                    );
                                },
                            ))
                        }),
                ),
            );
        } else {
            body = body.child(self.render_pi_text_answer(request, pending, cx));
        }
        body.child(
            Button::new(gpui_kit::SharedString::from(format!("pi-cancel:{id}")))
                .label("Cancel")
                .accessibility_label(format!("Cancel: {}", request_title(request)))
                .small()
                .ghost()
                .disabled(pending)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.request_command(
                        &target,
                        NativeRequestOperation::Respond,
                        vec![id.clone(), json!({"cancelled":true}).to_string()],
                        window,
                        cx,
                    );
                })),
        )
    }

    fn render_pi_text_answer(
        &self,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let id = request.id.clone();
        let target = self.target.clone();
        let label = request_title(request).to_owned();
        let mut body = div().flex().flex_col().gap_2();
        if request.method == "pi.editor" {
            if let Some(editor) = self.editors.get(&id) {
                body = body.child(Textarea::new(editor).aria_label(label).disabled(pending));
            }
        } else if let Some(answer) = self.answers.get(&(id.clone(), "value".to_owned())) {
            body = body.child(Input::new(answer).aria_label(label).disabled(pending));
        }
        body.child(
            Button::new(gpui_kit::SharedString::from(format!("pi-answer:{id}")))
                .label(if request.method == "pi.editor" {
                    "Save answer"
                } else {
                    "Send answer"
                })
                .small()
                .primary()
                .disabled(pending)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.submit_pi_text_answer(&target, &id, window, cx);
                })),
        )
    }

    fn submit_pi_text_answer(
        &self,
        target: &CommandTarget,
        id: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.target != *target {
            return;
        }
        let Some(request) = self.requests.iter().find(|request| request.id == id) else {
            return;
        };
        let answer = if request.method == "pi.editor" {
            self.editors
                .get(id)
                .map(|editor| editor.read(cx).value().to_string())
        } else if request.method == "pi.input" {
            self.answers
                .get(&(id.to_owned(), "value".to_owned()))
                .map(|answer| answer.read(cx).value().to_string())
        } else {
            None
        };
        if let Some(answer) = answer {
            self.request_command(
                target,
                NativeRequestOperation::Respond,
                vec![id.to_owned(), json!({"value":answer}).to_string()],
                window,
                cx,
            );
        }
    }

    fn choose_question_option(
        &mut self,
        target: &CommandTarget,
        key: &(String, String),
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.target != *target {
            return;
        }
        let Some(request) = self.requests.iter().find(|request| request.id == key.0) else {
            return;
        };
        let Some(question) = questions(request)
            .find(|question| question_key(request, question) == Some(key.1.as_str()))
        else {
            return;
        };
        let Some(answer) = self.answers.get(key).cloned() else {
            return;
        };
        if is_claude_question(request) {
            let multiple = field(question, "multiSelect") == true;
            let selected = self.selected_answers.entry(key.clone()).or_default();
            if !multiple {
                selected.clear();
            }
            if multiple && selected.contains(label) {
                selected.remove(label);
            } else {
                selected.insert(label.to_owned());
            }
            if !multiple {
                answer.update(cx, |answer, cx| answer.set_value("", window, cx));
            }
            cx.notify();
        } else {
            answer.update(cx, |answer, cx| {
                answer.set_value(label.to_owned(), window, cx);
            });
        }
    }

    fn render_question(
        &self,
        request: &NativeAgentRequest,
        question: &Value,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(question_id) = question_key(request, question) else {
            return div().into_any_element();
        };
        let Some(answer) = self
            .answers
            .get(&(request.id.clone(), question_id.to_owned()))
        else {
            return div().into_any_element();
        };
        let label = field(question, "question")
            .as_str()
            .unwrap_or("Your answer");
        let options = field(question, "options")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|option| {
                let label = field(option, "label").as_str()?.to_owned();
                let id = gpui_kit::SharedString::from(format!(
                    "option:{}:{question_id}:{label}",
                    request.id
                ));
                let claude = is_claude_question(request);
                let key = (request.id.clone(), question_id.to_owned());
                let target = self.target.clone();
                let selected = if claude {
                    self.selected_answers
                        .get(&key)
                        .is_some_and(|selected| selected.contains(&label))
                } else {
                    answer.read(cx).value().as_ref() == label.as_str()
                };
                Some(
                    Button::new(id)
                        .label(label.clone())
                        .small()
                        .ghost()
                        .disabled(pending)
                        .selected(selected)
                        .tooltip(
                            field(option, "description")
                                .as_str()
                                .unwrap_or(&label)
                                .to_owned(),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.choose_question_option(&target, &key, &label, window, cx);
                        })),
                )
            });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().child(label.to_owned()))
            .when(
                is_claude_question(request) && field(question, "multiSelect") == true,
                |row| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Select one or more"),
                    )
                },
            )
            .child(div().flex().flex_wrap().gap_1().children(options))
            .child(
                Input::new(answer)
                    .aria_label(if is_claude_question(request) {
                        format!("Other answer: {label}")
                    } else {
                        label.to_owned()
                    })
                    .disabled(pending),
            )
            .into_any_element()
    }
}

impl EventEmitter<NativeRequestResponse> for NativeRequestView {}
impl Render for NativeRequestView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_requests(window, cx)
    }
}

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}
pub(super) fn is_claude_question(request: &NativeAgentRequest) -> bool {
    request.method == "claude.permission"
        && field(&request.parameters, "tool_name") == "AskUserQuestion"
}
fn question_key<'a>(request: &NativeAgentRequest, question: &'a Value) -> Option<&'a str> {
    field(
        question,
        if is_claude_question(request) {
            "question"
        } else {
            "id"
        },
    )
    .as_str()
}
fn questions(request: &NativeAgentRequest) -> impl Iterator<Item = &Value> {
    let parameters = if is_claude_question(request) {
        field(&request.parameters, "input")
    } else {
        &request.parameters
    };
    field(parameters, "questions")
        .as_array()
        .into_iter()
        .flatten()
}
pub(super) fn is_approval(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "pi.confirm"
            | "claude.permission"
    )
}
fn request_title(request: &NativeAgentRequest) -> &str {
    field(&request.parameters, "title")
        .as_str()
        .or_else(|| {
            (request.method == "claude.permission" && !is_claude_question(request))
                .then(|| field(&request.parameters, "tool_name").as_str())
                .flatten()
        })
        .unwrap_or_else(|| match request.method.as_str() {
            "item/commandExecution/requestApproval" => "Allow this command?",
            "item/fileChange/requestApproval" => "Allow these file changes?",
            "item/tool/requestUserInput"
            | "pi.select"
            | "pi.input"
            | "pi.editor"
            | "claude.permission" => "Your answer",
            "pi.confirm" => "Confirm",
            "mcpServer/elicitation/request" if request.is_mcp_approval() => "Allow this tool?",
            "mcpServer/elicitation/request" => "MCP request",
            _ => "Provider request",
        })
}
#[derive(IntoElement)]
struct RequestDetails {
    id: gpui_kit::SharedString,
    content: gpui_kit::AnyElement,
}

fn request_details(id: gpui_kit::SharedString, content: impl IntoElement) -> RequestDetails {
    RequestDetails {
        id,
        content: content.into_any_element(),
    }
}

impl gpui_kit::RenderOnce for RequestDetails {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll = window
            .use_keyed_state(
                (gpui_kit::ElementId::from(self.id.clone()), "scroll"),
                cx,
                |_, _| gpui_kit::ScrollHandle::new(),
            )
            .read(cx)
            .clone();
        div()
            .id(self.id.clone())
            .debug_selector(move || self.id.to_string())
            .text_sm()
            .max_h(gpui_kit::px(
                f32::from(window.viewport_size().height)
                    .mul_add(0.16, 0.0)
                    .min(128.0),
            ))
            .flex_shrink_0()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .child(
                div()
                    .debug_selector(|| "request-details-text".into())
                    .child(self.content),
            )
            .vertical_scrollbar(&scroll)
    }
}

fn approval_description(
    parameters: &Value,
    home: Option<&std::path::Path>,
    attached_bootty: bool,
) -> String {
    if let (Some(server), Some(message)) = (
        field(parameters, "serverName").as_str(),
        field(parameters, "message").as_str(),
    ) {
        if attached_bootty {
            let prefix = format!("Allow the {server} MCP server to run tool \"");
            if let Some(tool) = message
                .strip_prefix(&prefix)
                .and_then(|tool| tool.strip_suffix("\"?"))
            {
                return format!("Allow Bootty to {}?", tool_label(tool).to_lowercase());
            }
            return format!("Bootty: {}", message.replace(server, "Bootty"));
        }
        return format!("{server}: {message}");
    }
    if let Some(command) = field(parameters, "command").as_str() {
        let mut description = command.to_owned();
        if let Some(cwd) = field(parameters, "cwd").as_str() {
            description.push_str("\nIn ");
            description.push_str(&bootty_git::project::display_path(cwd, home));
        }
        if let Some(reason) = field(parameters, "reason").as_str() {
            description.push('\n');
            description.push_str(reason);
        }
        if let Some(amendment) = field(parameters, "proposedExecpolicyAmendment").as_array() {
            let prefix = amendment
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            description.push_str("\nAlways allow applies to commands starting with: ");
            description.push_str(&prefix);
        }
        return description;
    }
    let mut scope = parameters.clone();
    if let Some(scope) = scope.as_object_mut() {
        // Once-only decisions never apply the provider's reusable policy amendments.
        for key in [
            "kind",
            "threadId",
            "turnId",
            "itemId",
            "startedAtMs",
            "availableDecisions",
            "proposedExecpolicyAmendment",
        ] {
            scope.remove(key);
        }
        if let Some(cwd) = scope.get("cwd").and_then(Value::as_str) {
            let cwd = bootty_git::project::display_path(cwd, home);
            scope.insert("cwd".to_owned(), cwd.into());
        }
    }
    serde_json::to_string_pretty(&scope).unwrap_or_else(|_| scope.to_string())
}
