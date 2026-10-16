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
    App, Context, Entity, EventEmitter, FocusHandle, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Window, div, prelude::*,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[path = "gpui_agent_elicitation.rs"]
mod elicitation;

gpui_kit::actions!(
    bootty_request,
    [
        #[derive(Eq)]
        AllowRequest,
        #[derive(Eq)]
        DenyRequest
    ]
);

const REQUEST_KEYS: &str = "BoottyAgentRequest";

/// A focused request panel answers its front approval: Enter takes the primary choice and Escape
/// denies. Reusable scopes stay on their buttons, so no key grants more than once.
pub fn init_request_keys(cx: &mut App) {
    cx.bind_keys([
        gpui_kit::KeyBinding::new("enter", AllowRequest, Some(REQUEST_KEYS)),
        gpui_kit::KeyBinding::new("escape", DenyRequest, Some(REQUEST_KEYS)),
    ]);
}

#[derive(Clone, Copy)]
pub(super) enum NativeRequestOperation {
    Approve,
    Respond,
    /// Deny with a note for the agent.
    Deny,
}
impl NativeRequestOperation {
    pub(super) const fn id(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Respond => "respond",
            Self::Deny => "deny",
        }
    }
}

pub(super) struct NativeRequestResponse {
    pub target: CommandTarget,
    pub operation: NativeRequestOperation,
    pub arguments: Vec<String>,
    /// A plan to carry out in a new conversation instead of this one.
    pub implement_elsewhere: Option<String>,
}

/// A typed answer. A secret stays on one line, the only field that can mask it; other answers
/// can run to several lines with Shift+Enter, while Enter still moves on.
#[derive(Clone)]
enum AnswerField {
    Line(Entity<InputState>),
    Text(Entity<TextareaState>),
}

impl AnswerField {
    fn value(&self, cx: &App) -> String {
        match self {
            Self::Line(input) => input.read(cx).value().to_string(),
            Self::Text(input) => input.read(cx).value().to_string(),
        }
    }

    fn set_value(&self, value: String, window: &mut Window, cx: &mut App) {
        match self {
            Self::Line(input) => input.update(cx, |input, cx| input.set_value(value, window, cx)),
            Self::Text(input) => input.update(cx, |input, cx| input.set_value(value, window, cx)),
        }
    }

    fn render(&self, label: String, disabled: bool) -> gpui_kit::AnyElement {
        match self {
            Self::Line(input) => Input::new(input)
                .aria_label(label)
                .disabled(disabled)
                .into_any_element(),
            Self::Text(input) => Textarea::new(input)
                .aria_label(label)
                .disabled(disabled)
                .into_any_element(),
        }
    }
}

pub(super) struct NativeRequestView {
    target: CommandTarget,
    /// The provider runs on this computer, so it can read files given with answers.
    local: bool,
    requests: Vec<NativeAgentRequest>,
    provider_enabled: bool,
    pending: BTreeSet<String>,
    answers: BTreeMap<(String, String), AnswerField>,
    selected_answers: BTreeMap<(String, String), BTreeSet<String>>,
    answer_subscriptions: BTreeMap<(String, String), Subscription>,
    /// Files given with an answer; the provider reads them by their paths.
    answer_files: BTreeMap<(String, String), Vec<std::path::PathBuf>>,
    editors: BTreeMap<String, Entity<TextareaState>>,
    editor_subscriptions: BTreeMap<String, Subscription>,
    /// What each file-change approval would change: its `fileChange` input and diff rows.
    diffs: BTreeMap<String, (String, Vec<super::ToolDiff>)>,
    /// Answers being given to MCP forms, by request.
    forms: BTreeMap<String, elicitation::FormState>,
    focus: FocusHandle,
    /// Approvals and questions already shown, so only a new one takes focus.
    seen_keyed: BTreeSet<String>,
    /// The question each question request's digit keys answer, by request.
    question_cursor: BTreeMap<String, usize>,
    /// The panel has focus, so the front request shows its keys.
    keys_shown: bool,
    /// Notes being written to go with a denial, by request.
    notes: BTreeMap<String, (Entity<InputState>, Subscription)>,
}

impl NativeRequestView {
    pub(super) fn new(target: CommandTarget, local: bool, cx: &Context<Self>) -> Self {
        Self {
            target,
            local,
            requests: Vec::new(),
            provider_enabled: true,
            pending: BTreeSet::new(),
            answers: BTreeMap::new(),
            selected_answers: BTreeMap::new(),
            answer_subscriptions: BTreeMap::new(),
            answer_files: BTreeMap::new(),
            editors: BTreeMap::new(),
            editor_subscriptions: BTreeMap::new(),
            diffs: BTreeMap::new(),
            forms: BTreeMap::new(),
            notes: BTreeMap::new(),
            focus: cx.focus_handle(),
            seen_keyed: BTreeSet::new(),
            question_cursor: BTreeMap::new(),
            keys_shown: false,
        }
    }

    pub(super) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The request Enter, Escape and the digit keys answer: the first approval or question shown.
    fn front_keyed(&self) -> Option<&NativeAgentRequest> {
        self.requests
            .iter()
            .find(|request| is_decision(request) || is_question(request))
            .filter(|request| {
                self.provider_enabled && !self.pending.contains(&format!("request:{}", request.id))
            })
    }

    pub(super) fn has_keyed(&self) -> bool {
        self.requests
            .iter()
            .any(|request| is_decision(request) || is_question(request))
    }

    fn answer_front(
        &self,
        choose: fn(&NativeAgentRequest) -> Option<bootty_agents::NativeApprovalDecision>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(request) = self.front_keyed().filter(|request| is_decision(request)) else {
            return;
        };
        let Some(decision) = choose(request) else {
            return;
        };
        let (target, id) = (self.target.clone(), request.id.clone());
        self.request_command(
            &target,
            NativeRequestOperation::Approve,
            vec![id, decision.id().to_owned()],
            window,
            cx,
        );
    }
    /// `changes` holds each file-change approval's changes, by request, in the transcript's
    /// `fileChange` shape; paths show relative to `cwd`.
    pub(super) fn update(
        &mut self,
        target: CommandTarget,
        requests: &[NativeAgentRequest],
        pending: &BTreeSet<String>,
        (changes, cwd): (&BTreeMap<String, String>, &std::path::Path),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.target != target {
            self.answers.clear();
            self.selected_answers.clear();
            self.answer_subscriptions.clear();
            self.editors.clear();
            self.editor_subscriptions.clear();
            self.forms.clear();
        }
        self.target = target;
        self.requests = requests.to_vec();
        self.pending.clone_from(pending);
        crate::gpui_application_icons::load(
            requests
                .iter()
                .filter_map(NativeAgentRequest::application_access)
                .map(|application| application.bundle_id),
            cx,
        );
        self.notes
            .retain(|id, _| requests.iter().any(|request| request.id == *id));
        self.sync_answers(window, cx);
        self.sync_forms(window, cx);
        self.diffs.retain(|id, _| changes.contains_key(id));
        for (id, input) in changes {
            if self.diffs.get(id).is_some_and(|(seen, _)| seen == input) {
                continue;
            }
            let files = super::parse_tool_diffs(input, cwd)
                .unwrap_or_default()
                .into_iter()
                .map(|(path, file)| super::tool_diff(&path, file, cwd, window, cx))
                .collect();
            self.diffs.insert(id.clone(), (input.clone(), files));
        }
        let keyed = self
            .requests
            .iter()
            .filter(|request| is_decision(request) || is_question(request))
            .map(|request| request.id.clone())
            .collect::<BTreeSet<_>>();
        let arrived = keyed.iter().any(|id| !self.seen_keyed.contains(id));
        self.question_cursor.retain(|id, _| keyed.contains(id));
        self.seen_keyed = keyed;
        cx.notify();
        arrived
    }

    /// The diff rows for an approval, when it changes files.
    fn render_diffs(&self, id: &str, cx: &Context<Self>) -> Option<gpui_kit::Div> {
        let (_, files) = self.diffs.get(id).filter(|(_, files)| !files.is_empty())?;
        Some(
            div()
                .debug_selector({
                    let id = id.to_owned();
                    move || format!("approval-diff:{id}")
                })
                .flex()
                .flex_col()
                .gap_2()
                .children(super::tool_diff_files(files, cx)),
        )
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
                implement_elsewhere: None,
            });
        }
    }

    /// The button for one decision, showing its key when the request is in front.
    fn decision_button(
        &self,
        request: &NativeAgentRequest,
        decision: bootty_agents::NativeApprovalDecision,
        (primary, keys): (Option<bootty_agents::NativeApprovalDecision>, bool),
        pending: bool,
        cx: &Context<Self>,
    ) -> Button {
        use bootty_agents::NativeApprovalDecision as Decision;
        let id = request.id.clone();
        let target = self.target.clone();
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
            Some(decision) == primary,
            gpui_kit::component::button::ButtonVariants::primary,
        )
        .when_some(
            keys.then(|| match decision {
                _ if Some(decision) == primary => Some("enter"),
                Decision::Deny => Some("escape"),
                _ => None,
            })
            .flatten()
            .and_then(|key| gpui_kit::Keystroke::parse(key).ok()),
            |button, key| button.child(gpui_kit::component::kbd::Kbd::new(key)),
        )
        .disabled(pending)
        .on_click(cx.listener(move |this, _, window, cx| {
            this.request_command(
                &target,
                NativeRequestOperation::Approve,
                vec![id.clone(), decision.id().to_owned()],
                window,
                cx,
            );
        }))
    }

    /// For Claude's plan, carrying it out in a new conversation instead.
    fn implement_elsewhere_button(
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> Option<Button> {
        (request.method == "claude.permission"
            && field(&request.parameters, "tool_name") == "ExitPlanMode")
            .then(|| {
                let id = request.id.clone();
                let request = request.clone();
                Button::new(gpui_kit::SharedString::from(format!("implement-new:{id}")))
                    .debug_selector(move || format!("implement-new:{id}"))
                    .label("In a new conversation")
                    .small()
                    .ghost()
                    .disabled(pending)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.implement_elsewhere(&request, cx);
                    }))
            })
    }

    /// Declines Claude's plan here, saying why, and carries it out in a new conversation.
    fn implement_elsewhere(&self, request: &NativeAgentRequest, cx: &mut Context<Self>) {
        let Some(plan) = field(field(&request.parameters, "input"), "plan").as_str() else {
            return;
        };
        if self.provider_enabled {
            cx.emit(NativeRequestResponse {
                target: self.target.clone(),
                operation: NativeRequestOperation::Deny,
                arguments: vec![
                    request.id.clone(),
                    "The plan is being carried out in a new conversation.".to_owned(),
                ],
                implement_elsewhere: Some(plan.to_owned()),
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
        self.answer_files
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
                let placeholder = if is_claude_question(request) {
                    "Other answer (optional)…"
                } else {
                    "Your answer…"
                };
                let answer = if field(question, "isSecret") == true {
                    AnswerField::Line(cx.new(|cx| {
                        InputState::new(window, cx)
                            .masked(true)
                            .placeholder(placeholder)
                    }))
                } else {
                    AnswerField::Text(cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .auto_grow(1, 6)
                            .submit_on_enter(true)
                            .placeholder(placeholder)
                    }))
                };
                let clear_selection =
                    is_claude_question(request) && field(question, "multiSelect") != true;
                let selection_key = key.clone();
                let target = self.target.clone();
                // Typing an answer of its own replaces a single choice.
                let changed = move |this: &mut Self, event: &InputEvent, cx: &mut Context<Self>| {
                    if this.target != target {
                        return;
                    }
                    if clear_selection
                        && matches!(event, InputEvent::Change)
                        && this
                            .answers
                            .get(&selection_key)
                            .is_some_and(|answer| !answer.value(cx).trim().is_empty())
                    {
                        this.selected_answers.remove(&selection_key);
                    }
                    cx.notify();
                };
                let subscription = match &answer {
                    AnswerField::Line(input) => {
                        cx.subscribe_in(input, window, move |this, _, event: &InputEvent, _, cx| {
                            changed(this, event, cx);
                        })
                    }
                    AnswerField::Text(input) => {
                        cx.subscribe_in(input, window, move |this, _, event: &InputEvent, _, cx| {
                            changed(this, event, cx);
                        })
                    }
                };
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
                    let input = cx.new(|cx| {
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
                        &input,
                        window,
                        move |this, _, event: &InputEvent, window, cx| {
                            if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                                this.submit_pi_text_answer(&target, &id, window, cx);
                            }
                            cx.notify();
                        },
                    );
                    entry.insert(AnswerField::Line(input));
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

    /// The panel grows with its requests, up to 24rem or 40% of the window, then scrolls.
    fn render_requests(&self, window: &Window, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let limit = (f32::from(window.viewport_size().height) * 0.4)
            .min(f32::from(window.rem_size()) * 24.0);
        let requests = div()
            .w_full()
            .max_w(gpui_kit::rems(48.0))
            .max_h(gpui_kit::px(limit))
            .flex()
            .flex_col()
            .children(
                self.requests
                    .iter()
                    .map(|request| self.render_request(request, cx)),
            )
            .overflow_y_scrollbar();
        div()
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
            .child(request_heading(request, cx));
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
        if let Some(application) = request.application_access() {
            row = row
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Let the agent see and control")
                        .child(crate::gpui_prompt_attachments::application_chip(
                            &application.bundle_id,
                            &application.name,
                            false,
                            cx,
                        ))
                        .child("for the rest of this conversation."),
                )
                .child(self.approval_buttons(request, pending, cx));
        } else if request.is_mcp_approval() || is_approval(&request.method) {
            row = self.render_approval(row, request, pending, cx);
        } else if request.method == "mcpServer/elicitation/request" {
            row = self.render_elicitation(row, request, pending, cx);
        } else if request.method == "item/tool/requestUserInput" {
            row = row.child(self.render_questions(request, pending, cx));
            row = row.child(
                Button::new(gpui_kit::SharedString::from(format!("answer:{id}")))
                    .debug_selector({
                        let id = id.clone();
                        move || format!("answer:{id}")
                    })
                    .label("Send answer")
                    .small()
                    .disabled(pending || !self.all_answered(request, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.submit_codex_answers(&target, &id, window, cx);
                    })),
            );
        } else {
            row = row.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("This provider request is not supported here. Interrupt the turn to continue."));
        }
        row.into_any_element()
    }

    fn render_approval(
        &self,
        row: gpui_kit::Stateful<gpui_kit::Div>,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let id = request.id.clone();
        if let Some(diffs) = self.render_diffs(&id, cx) {
            return row
                .child(diffs)
                .child(self.approval_buttons(request, pending, cx));
        }
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
        .child(self.approval_buttons(request, pending, cx))
    }

    /// One button per decision this exact request supports.
    fn approval_buttons(
        &self,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        use bootty_agents::NativeApprovalDecision as Decision;
        let id = request.id.clone();
        let primary = primary_decision(request);
        let keys = self.keys_shown
            && self
                .front_keyed()
                .is_some_and(|front| front.id == request.id);
        let note = self.notes.get(&request.id).map(|(note, _)| note.clone());
        let noted = request.approval_response(Decision::Deny).is_some()
            && request.method != bootty_agents::APPLICATION_ACCESS_METHOD;
        let buttons = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(
                Decision::ALL
                    .into_iter()
                    .filter(|decision| request.approval_response(*decision).is_some())
                    .map(|decision| {
                        self.decision_button(request, decision, (primary, keys), pending, cx)
                    }),
            )
            .children(Self::implement_elsewhere_button(request, pending, cx));
        let opener = id.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(buttons.when(noted && note.is_none(), |row| {
                row.child(
                    Button::new(gpui_kit::SharedString::from(format!("deny-note:{id}")))
                        .debug_selector({
                            let id = id.clone();
                            move || format!("deny-note:{id}")
                        })
                        .label("Deny with note…")
                        .small()
                        .ghost()
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_note(&opener, window, cx);
                        })),
                )
            }))
            .when_some(note, |body, note| {
                let closing = id.clone();
                body.child(
                    div()
                        .debug_selector({
                            let id = id.clone();
                            move || format!("deny-note-input:{id}")
                        })
                        // Enter sends the note, never the card's Enter-to-allow, and Escape
                        // closes the note rather than denying without it.
                        .on_action(|_: &gpui_kit::component::input::Enter, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_action(cx.listener(
                            move |this, _: &gpui_kit::component::input::Escape, _, cx| {
                                this.notes.remove(&closing);
                                cx.stop_propagation();
                                cx.notify();
                            },
                        ))
                        .child(
                            Input::new(&note)
                                .aria_label("Why the agent shouldn't do this")
                                .disabled(pending),
                        ),
                )
            })
    }

    /// Opens a note to go with denying `id`. Enter denies with it; Escape closes it.
    fn open_note(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let note = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Tell the agent why, then press Enter…")
        });
        let (request, target) = (id.to_owned(), self.target.clone());
        let subscription = cx.subscribe_in(
            &note,
            window,
            move |this, note, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    let text = note.read(cx).value().trim().to_owned();
                    if !text.is_empty() {
                        this.request_command(
                            &target,
                            NativeRequestOperation::Deny,
                            vec![request.clone(), text],
                            window,
                            cx,
                        );
                    }
                }
            },
        );
        // Focus once the field is drawn; the button's own click takes focus first.
        let focused = note.clone();
        cx.defer_in(window, move |_, window, cx| {
            focused.update(cx, |note, cx| note.focus(window, cx));
        });
        self.notes.insert(id.to_owned(), (note, subscription));
        cx.notify();
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
        if !question {
            return div()
                .flex()
                .flex_col()
                .gap_2()
                .child(request_details(
                    gpui_kit::SharedString::from(format!("claude-approval-scope:{id}")),
                    claude_approval_description(
                        &request.parameters,
                        crate::strings::home_dir().as_deref(),
                    ),
                ))
                .children(self.render_diffs(&id, cx))
                .child(self.approval_buttons(request, pending, cx));
        }
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.render_questions(request, pending, cx));
        let denied = id.clone();
        let denied_target = target.clone();
        let empty = !self.all_answered(request, cx);
        body.child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new(gpui_kit::SharedString::from(format!("claude-decline:{id}")))
                        .label("Cancel")
                        .small()
                        .ghost()
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.decline_claude_question(&denied_target, &denied, window, cx);
                        })),
                )
                .child(
                    Button::new(gpui_kit::SharedString::from(format!("claude-accept:{id}")))
                        .label("Send answers")
                        .small()
                        .primary()
                        .disabled(pending || empty)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.submit_claude_answers(&target, &id, window, cx);
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
                    let value = self.answer_with_files(&state_key, answer.value(cx));
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

    fn submit_codex_answers(
        &self,
        target: &CommandTarget,
        id: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let answers = self
            .answers
            .iter()
            .filter(|((request_id, _), _)| request_id == id)
            .map(|(key, answer)| {
                let value = self.answer_with_files(key, answer.value(cx));
                (key.1.clone(), json!({"answers":[value]}))
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

    fn decline_claude_question(
        &self,
        target: &CommandTarget,
        id: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.request_command(
            target,
            NativeRequestOperation::Respond,
            vec![id.to_owned(), json!({"decision":"decline"}).to_string()],
            window,
            cx,
        );
    }

    /// Whether `question` has an answer: a chosen option or typed text.
    fn answered(&self, request: &NativeAgentRequest, question: &Value, cx: &App) -> bool {
        let Some(key) =
            question_key(request, question).map(|key| (request.id.clone(), key.to_owned()))
        else {
            return false;
        };
        self.selected_answers
            .get(&key)
            .is_some_and(|selected| !selected.is_empty())
            || self
                .answer_files
                .get(&key)
                .is_some_and(|files| !files.is_empty())
            || self
                .answers
                .get(&key)
                .is_some_and(|answer| !answer.value(cx).trim().is_empty())
    }

    /// The typed answer to a question with the files given with it, one line each by path, the
    /// way providers' question answers take files.
    fn answer_with_files(&self, key: &(String, String), typed: String) -> String {
        let files = self
            .answer_files
            .get(key)
            .into_iter()
            .flatten()
            .map(|path| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                format!("Attached file \"{name}\": {:?}", path.display().to_string())
            });
        std::iter::once(typed)
            .filter(|typed| !typed.trim().is_empty())
            .chain(files)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Gives files with an answer, at most eight, each once.
    fn attach_answer_files(
        &mut self,
        key: &(String, String),
        paths: impl IntoIterator<Item = std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if !self.answers.contains_key(key) {
            return;
        }
        let files = self.answer_files.entry(key.clone()).or_default();
        for path in paths {
            if path.is_absolute() && !files.contains(&path) && files.len() < 8 {
                files.push(path);
            }
        }
        cx.notify();
    }

    /// Lets the person choose files to give with an answer.
    fn choose_answer_files(key: (String, String), window: &Window, cx: &Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await {
                _ = this.update(cx, |this, cx| this.attach_answer_files(&key, paths, cx));
            }
        })
        .detach();
    }

    fn all_answered(&self, request: &NativeAgentRequest, cx: &App) -> bool {
        questions(request).next().is_some()
            && questions(request).all(|question| self.answered(request, question, cx))
    }

    /// The question the digit keys answer in `request`.
    fn current_question(&self, request: &NativeAgentRequest) -> usize {
        self.question_cursor.get(&request.id).copied().unwrap_or(0)
    }

    /// A digit picks that option on the front question, then moves to the next question unless
    /// it takes several options.
    fn choose_by_digit(
        &mut self,
        digit: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(request) = self.front_keyed().filter(|request| is_question(request)) else {
            return false;
        };
        let index = self.current_question(request);
        let Some(question) = questions(request).nth(index) else {
            return false;
        };
        let (Some(key), Some(label)) = (
            question_key(request, question),
            field(question, "options")
                .as_array()
                .and_then(|options| options.get(digit.checked_sub(1)?))
                .and_then(|option| field(option, "label").as_str()),
        ) else {
            return false;
        };
        let several = is_claude_question(request) && field(question, "multiSelect") == true;
        let next = (!several && index.saturating_add(1) < questions(request).count())
            .then(|| index.saturating_add(1));
        let (id, key, label) = (request.id.clone(), key.to_owned(), label.to_owned());
        let target = self.target.clone();
        self.choose_question_option(&target, &(id.clone(), key), &label, window, cx);
        if let Some(next) = next {
            self.question_cursor.insert(id, next);
        }
        cx.notify();
        true
    }

    /// Enter moves to the next question, and sends the answers from the last one.
    fn advance_question(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(request) = self.front_keyed().filter(|request| is_question(request)) else {
            return;
        };
        let next = self.current_question(request).saturating_add(1);
        if next < questions(request).count() {
            self.question_cursor.insert(request.id.clone(), next);
            cx.notify();
        } else if self.all_answered(request, cx) {
            let (target, id) = (self.target.clone(), request.id.clone());
            if is_claude_question(request) {
                self.submit_claude_answers(&target, &id, window, cx);
            } else {
                self.submit_codex_answers(&target, &id, window, cx);
            }
        }
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
            body = body.child(answer.render(label, pending));
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
                .map(|answer| answer.value(cx))
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
                answer.set_value(String::new(), window, cx);
            }
            cx.notify();
        } else {
            answer.set_value(label.to_owned(), window, cx);
        }
    }

    /// One option of a question: its label, what choosing it does, and its digit key if shown.
    fn render_option(
        &self,
        request: &NativeAgentRequest,
        (question_id, answer): (&str, &AnswerField),
        option: &Value,
        digit: Option<usize>,
        pending: bool,
        cx: &Context<Self>,
    ) -> Option<Button> {
        let label = field(option, "label").as_str()?.to_owned();
        let id =
            gpui_kit::SharedString::from(format!("option:{}:{question_id}:{label}", request.id));
        let key = (request.id.clone(), question_id.to_owned());
        let target = self.target.clone();
        let selected = if is_claude_question(request) {
            self.selected_answers
                .get(&key)
                .is_some_and(|selected| selected.contains(&label))
        } else {
            answer.value(cx) == label
        };
        let description = field(option, "description")
            .as_str()
            .map(str::trim)
            .filter(|description| !description.is_empty() && *description != label)
            .map(str::to_owned);
        let digit = digit.and_then(|digit| gpui_kit::Keystroke::parse(&digit.to_string()).ok());
        // An option's description says what choosing it does, so it shows with the label.
        Some(
            Button::new(id.clone())
                .debug_selector(move || id.to_string())
                .ghost()
                .small()
                .w_full()
                .h_auto()
                .py_1()
                .justify_start()
                .disabled(pending)
                .selected(selected)
                .when_some(digit, |button, digit| {
                    button.child(gpui_kit::component::kbd::Kbd::new(digit))
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .text_left()
                        .child(label.clone())
                        .children(description.map(|description| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .whitespace_normal()
                                .child(description)
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.choose_question_option(&target, &key, &label, window, cx);
                })),
        )
    }

    /// A request's questions one at a time, with where this one stands among them. Answers
    /// already given stay when moving back and forth.
    fn render_questions(
        &self,
        request: &NativeAgentRequest,
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let count = questions(request).count();
        let current = self.current_question(request).min(count.saturating_sub(1));
        let question = questions(request)
            .enumerate()
            .nth(current)
            .map(|question| self.render_question(request, question, pending, cx));
        let id = request.id.clone();
        let step = |label: &'static str, to: usize, enabled: bool| {
            let id = id.clone();
            Button::new(gpui_kit::SharedString::from(format!(
                "question-{label}:{id}"
            )))
            .debug_selector({
                let id = id.clone();
                move || format!("question-{label}:{id}")
            })
            .label(if label == "back" { "Back" } else { "Next" })
            .xsmall()
            .ghost()
            .disabled(!enabled)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.question_cursor.insert(id.clone(), to);
                cx.notify();
            }))
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .when(count > 1, |body| {
                body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .debug_selector({
                                    let id = id.clone();
                                    let shown = current.saturating_add(1);
                                    move || format!("question-progress:{id}:{shown}/{count}")
                                })
                                .flex_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "Question {} of {count}",
                                    current.saturating_add(1)
                                )),
                        )
                        .child(step("back", current.saturating_sub(1), current > 0))
                        .child(step(
                            "next",
                            current.saturating_add(1),
                            current.saturating_add(1) < count,
                        )),
                )
            })
            .children(question)
    }

    fn render_question(
        &self,
        request: &NativeAgentRequest,
        (index, question): (usize, &Value),
        pending: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        // The question the digit keys answer shows them on its options.
        let digits = self.keys_shown
            && self.current_question(request) == index
            && self
                .front_keyed()
                .is_some_and(|front| front.id == request.id);
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
            .enumerate()
            .filter_map(|(position, option)| {
                // The digit keys reach the first nine options.
                let digit = (digits && position < 9).then(|| position.saturating_add(1));
                self.render_option(request, (question_id, answer), option, digit, pending, cx)
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
            .child(div().flex().flex_col().gap_0p5().children(options))
            .child(
                div()
                    .debug_selector({
                        let selector = format!("question-answer:{}:{question_id}", request.id);
                        move || selector
                    })
                    .child(answer.render(
                        if is_claude_question(request) {
                            format!("Other answer: {label}")
                        } else {
                            label.to_owned()
                        },
                        pending,
                    )),
            )
            .when(self.local, |row| {
                row.child(self.render_answer_files(
                    &(request.id.clone(), question_id.to_owned()),
                    pending,
                    cx,
                ))
            })
            .into_any_element()
    }

    /// The files given with an answer, each removable, and a way to give more. Files dropped
    /// on the question join them.
    fn render_answer_files(
        &self,
        key: &(String, String),
        pending: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let files = self.answer_files.get(key).cloned().unwrap_or_default();
        let selector = format!("question-files:{}:{}", key.0, key.1);
        let (drop_key, choose_key) = (key.clone(), key.clone());
        div()
            .id(gpui_kit::SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .on_drop(
                cx.listener(move |this, paths: &gpui_kit::ExternalPaths, _, cx| {
                    this.attach_answer_files(&drop_key, paths.paths().iter().cloned(), cx);
                }),
            )
            .children(files.into_iter().map(|path| {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let remove_key = key.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .child(name.clone())
                    .child(
                        Button::new(gpui_kit::SharedString::from(format!(
                            "remove-answer-file:{}",
                            path.display()
                        )))
                        .icon(gpui_kit::component::IconName::Close)
                        .xsmall()
                        .ghost()
                        .accessibility_label(format!("Remove {name}"))
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(files) = this.answer_files.get_mut(&remove_key) {
                                files.retain(|file| *file != path);
                            }
                            cx.notify();
                        })),
                    )
            }))
            .child(
                Button::new(gpui_kit::SharedString::from(format!(
                    "attach-answer-files:{}:{}",
                    key.0, key.1
                )))
                .label("Attach files…")
                .xsmall()
                .ghost()
                .disabled(pending)
                .on_click(cx.listener(move |this, _, window, cx| {
                    _ = this;
                    Self::choose_answer_files(choose_key.clone(), window, cx);
                })),
            )
    }
}

impl EventEmitter<NativeRequestResponse> for NativeRequestView {}
impl Render for NativeRequestView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.keys_shown = self.focus.contains_focused(window, cx);
        div()
            .track_focus(&self.focus)
            .key_context(REQUEST_KEYS)
            .on_action(cx.listener(|this, _: &AllowRequest, window, cx| {
                if this.front_keyed().is_some_and(is_question) {
                    this.advance_question(window, cx);
                } else {
                    this.answer_front(primary_decision, window, cx);
                }
            }))
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    // Typing into an answer field keeps its digits.
                    if this.focus.is_focused(window)
                        && !event.keystroke.modifiers.modified()
                        && let Ok(digit @ 1..=9) = event.keystroke.key.parse::<usize>()
                        && this.choose_by_digit(digit, window, cx)
                    {
                        cx.stop_propagation();
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &DenyRequest, window, cx| {
                if let Some(request) = this
                    .front_keyed()
                    .filter(|request| is_claude_question(request))
                {
                    let (target, id) = (this.target.clone(), request.id.clone());
                    this.decline_claude_question(&target, &id, window, cx);
                    return;
                }
                this.answer_front(
                    |request| {
                        let deny = bootty_agents::NativeApprovalDecision::Deny;
                        request.approval_response(deny).map(|_| deny)
                    },
                    window,
                    cx,
                );
            }))
            .child(self.render_requests(window, cx))
    }
}

/// A blocking question the panel asks: Codex's user input request or Claude's `AskUserQuestion`.
fn is_question(request: &NativeAgentRequest) -> bool {
    request.method == "item/tool/requestUserInput" || is_claude_question(request)
}

/// A request answered with one of its approval buttons rather than typed input.
fn is_decision(request: &NativeAgentRequest) -> bool {
    (is_approval(&request.method) && !is_claude_question(request))
        || request.is_mcp_approval()
        || request.application_access().is_some()
}

/// The choice Enter takes: allow once, or for the session where once isn't offered.
fn primary_decision(request: &NativeAgentRequest) -> Option<bootty_agents::NativeApprovalDecision> {
    use bootty_agents::NativeApprovalDecision as Decision;
    [Decision::AllowOnce, Decision::AllowSession]
        .into_iter()
        .find(|decision| request.approval_response(*decision).is_some())
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
/// The request's title, and which subagent asked when the conversation didn't.
fn request_heading(request: &NativeAgentRequest, cx: &App) -> gpui_kit::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .child(request_title(request).to_owned()),
        )
        .when_some(request.subagent.clone(), |heading, agent| {
            heading.child(
                div()
                    .debug_selector({
                        let selector = format!("request-subagent:{}:{agent}", request.id);
                        move || selector
                    })
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .max_w(gpui_kit::rems(12.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Bot).xsmall())
                    .child(div().truncate().child(agent)),
            )
        })
}

fn request_title(request: &NativeAgentRequest) -> &str {
    field(&request.parameters, "title")
        .as_str()
        .or_else(|| {
            (request.method == "claude.permission" && !is_claude_question(request))
                .then(|| claude_title(&request.parameters))
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
            "mcpServer/elicitation/request" => field(&request.parameters, "serverName")
                .as_str()
                .unwrap_or("MCP request"),
            method if method == bootty_agents::APPLICATION_ACCESS_METHOD => "Allow app access?",
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

/// The files an approval would change, in the transcript's `fileChange` shape. Codex names the
/// file-change item it asks about; a Claude edit carries its old and new text.
pub(super) fn approval_change(
    request: &NativeAgentRequest,
    transcript: &[bootty_agents::NativeTranscriptItem],
) -> Option<String> {
    match request.method.as_str() {
        "item/fileChange/requestApproval" => {
            let item = field(&request.parameters, "itemId").as_str()?;
            transcript
                .iter()
                .find(|candidate| candidate.id == item)?
                .tool
                .as_ref()
                .filter(|tool| tool.name == "fileChange")
                .map(|tool| tool.input.clone())
        }
        "claude.permission" => {
            let input = field(&request.parameters, "input");
            let path = field(input, "file_path").as_str()?;
            let text = |value: &Value, key| field(value, key).as_str().map(str::to_owned);
            let diff = match field(&request.parameters, "tool_name").as_str()? {
                "Edit" => snippet_diff(&text(input, "old_string")?, &text(input, "new_string")?),
                "MultiEdit" => field(input, "edits")
                    .as_array()?
                    .iter()
                    .map(|edit| {
                        Some(snippet_diff(
                            &text(edit, "old_string")?,
                            &text(edit, "new_string")?,
                        ))
                    })
                    .collect::<Option<String>>()?,
                "Write" => snippet_diff("", &text(input, "content")?),
                _ => return None,
            };
            Some(serde_json::json!([{"path": path, "diff": diff}]).to_string())
        }
        _ => None,
    }
}

/// A one-hunk patch from `old` to `new`: shared leading and trailing lines as context, the rest
/// removed then added. An edit's input has no file positions, so lines count within the snippet.
fn snippet_diff(old: &str, new: &str) -> String {
    let old = old.lines().collect::<Vec<_>>();
    let new = new.lines().collect::<Vec<_>>();
    let prefix = old
        .iter()
        .zip(&new)
        .take_while(|(before, after)| before == after)
        .count();
    let suffix = old
        .iter()
        .skip(prefix)
        .rev()
        .zip(new.iter().skip(prefix).rev())
        .take_while(|(before, after)| before == after)
        .count();
    let removed = old.len().saturating_sub(prefix).saturating_sub(suffix);
    let added = new.len().saturating_sub(prefix).saturating_sub(suffix);
    let start = |count: usize| usize::from(count > 0);
    let mut diff = format!(
        "@@ -{},{} +{},{} @@\n",
        start(old.len()),
        old.len(),
        start(new.len()),
        new.len()
    );
    let lines = old
        .iter()
        .take(prefix)
        .map(|line| (' ', *line))
        .chain(
            old.iter()
                .skip(prefix)
                .take(removed)
                .map(|line| ('-', *line)),
        )
        .chain(new.iter().skip(prefix).take(added).map(|line| ('+', *line)))
        .chain(
            old.iter()
                .skip(prefix.saturating_add(removed))
                .map(|line| (' ', *line)),
        );
    for (mark, line) in lines {
        diff.push(mark);
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn claude_title(parameters: &Value) -> &'static str {
    match field(parameters, "tool_name").as_str() {
        Some("Bash") => "Allow this command?",
        Some("Write" | "Edit" | "MultiEdit" | "NotebookEdit") => "Allow these file changes?",
        Some("ExitPlanMode") => "Implement this plan?",
        _ => "Allow this tool?",
    }
}

/// What a Claude approval would do: the command or file, Claude's description of it, and what a
/// reusable choice would allow.
fn claude_approval_description(parameters: &Value, home: Option<&std::path::Path>) -> String {
    let input = field(parameters, "input");
    let subject = field(input, "command")
        .as_str()
        .or_else(|| field(input, "plan").as_str())
        .map(str::to_owned)
        .or_else(|| {
            field(input, "file_path")
                .as_str()
                .or_else(|| field(input, "notebook_path").as_str())
                .map(|path| bootty_git::project::display_path(path, home))
        })
        .unwrap_or_else(|| {
            let tool = field(parameters, "tool_name").as_str().unwrap_or_default();
            format!(
                "{}\n{}",
                tool_label(tool),
                serde_json::to_string_pretty(input).unwrap_or_default()
            )
        });
    let mut lines = Vec::new();
    if let Some(description) = field(parameters, "description")
        .as_str()
        .filter(|description| !subject.ends_with(description))
    {
        lines.push(description.to_owned());
    }
    lines.insert(0, subject);
    for suggestion in field(parameters, "permission_suggestions")
        .as_array()
        .into_iter()
        .flatten()
    {
        match field(suggestion, "type").as_str() {
            Some("addRules") => {
                let rules = field(suggestion, "rules")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|rule| {
                        let tool = field(rule, "toolName").as_str()?;
                        Some(field(rule, "ruleContent").as_str().map_or_else(
                            || tool.to_owned(),
                            |content| format!("{tool}({content})"),
                        ))
                    })
                    .collect::<Vec<_>>();
                if !rules.is_empty() {
                    lines.push(format!("Reusable choices allow {}", rules.join(", ")));
                }
            }
            Some("setMode") if field(suggestion, "mode") == "acceptEdits" => {
                lines.push("Allow for this session accepts every edit until it ends".to_owned());
            }
            _ => {}
        }
    }
    lines.join("\n")
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
