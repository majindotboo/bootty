//! Async questions use the ordinary prompt path and preserve the conversation's draft.
use bootty_agents::NativeAsyncQuestion;
use bootty_control::CommandTarget;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{InputEvent, Textarea, TextareaState},
    radio::{Radio, RadioGroup},
};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollHandle, SharedString, Styled as _, Window, div,
    prelude::*,
};

pub(super) struct AsyncQuestionAnswer {
    pub target: CommandTarget,
    pub item: String,
    pub text: String,
}

pub(super) struct AsyncQuestionsView {
    target: CommandTarget,
    item: String,
    questions: Vec<NativeAsyncQuestion>,
    answers: Vec<Entity<TextareaState>>,
    selections: Vec<Option<usize>>,
    enabled: bool,
    scroll: ScrollHandle,
}

impl AsyncQuestionsView {
    pub fn new(
        target: CommandTarget,
        item: String,
        questions: Vec<NativeAsyncQuestion>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let answers = questions
            .iter()
            .map(|_| {
                let answer = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(1, 6)
                        .submit_on_enter(true)
                });
                cx.subscribe(&answer, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                        this.submit(cx);
                    }
                    cx.notify();
                })
                .detach();
                answer
            })
            .collect();
        let scroll = ScrollHandle::new();
        let selections = questions
            .iter()
            .map(|question| {
                question
                    .options
                    .as_ref()
                    .filter(|options| !options.is_empty())
                    .map(|_| 0)
            })
            .collect();
        Self {
            target,
            item,
            questions,
            answers,
            selections,
            enabled: true,
            scroll,
        }
    }

    pub fn questions(&self) -> &[NativeAsyncQuestion] {
        &self.questions
    }

    pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled != enabled {
            self.enabled = enabled;
            cx.notify();
        }
    }

    fn answer(&self, ix: usize, cx: &Context<Self>) -> String {
        self.selections
            .get(ix)
            .copied()
            .flatten()
            .and_then(|selected| self.questions.get(ix)?.options.as_ref()?.get(selected))
            .map_or_else(
                || {
                    self.answers.get(ix).map_or_else(String::new, |answer| {
                        answer.read(cx).value().trim().to_owned()
                    })
                },
                Clone::clone,
            )
    }

    fn render_question(
        &self,
        ix: usize,
        question: &NativeAsyncQuestion,
        answer: &Entity<TextareaState>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let options = question.options.as_deref().unwrap_or_default();
        let selected = self.selections.get(ix).copied().flatten();
        let count = options.len();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().child(question.title.clone()))
            .when(!options.is_empty(), |body| {
                body.child(
                    RadioGroup::vertical(SharedString::from(format!(
                        "async-choices:{}:{ix}",
                        self.item
                    )))
                    .w_full()
                    .disabled(!self.enabled)
                    .selected_index(Some(selected.unwrap_or(count)))
                    .children(options.iter().map(|option| {
                        let id = format!("async-option:{}:{ix}:{option}", self.item);
                        Radio::new(SharedString::from(id.clone()))
                            .small()
                            .py_2()
                            .px_2()
                            .w_full()
                            .accessibility_label(option.clone())
                            .child(
                                div()
                                    .debug_selector(move || id)
                                    .flex_1()
                                    .text_sm()
                                    .child(option.clone()),
                            )
                    }))
                    .child(
                        Radio::new(SharedString::from(format!(
                            "async-other:{}:{ix}",
                            self.item
                        )))
                        .small()
                        .py_2()
                        .px_2()
                        .accessibility_label("Write an answer…")
                        .child(
                            div().text_sm().child("Write an answer…").debug_selector({
                                let id = self.item.clone();
                                move || format!("async-other:{id}:{ix}")
                            }),
                        ),
                    )
                    .on_change(cx.listener(
                        move |this, index: &usize, window, cx| {
                            if let Some(selection) = this.selections.get_mut(ix) {
                                *selection = (*index < count).then_some(*index);
                            }
                            if *index == count
                                && let Some(answer) = this.answers.get(ix)
                            {
                                answer.read(cx).focus_handle(cx).focus(window, cx);
                            }
                            cx.notify();
                        },
                    )),
                )
            })
            .when(selected.is_none(), |body| {
                body.child(
                    div()
                        .id(SharedString::from(format!(
                            "async-input:{}:{ix}",
                            self.item
                        )))
                        .on_prepaint({
                            let answer = answer.clone();
                            let scroll = self.scroll.clone();
                            move |bounds, window, cx| {
                                if answer.read(cx).focus_handle(cx).is_focused(window) {
                                    reveal_answer(&scroll, bounds, window);
                                }
                            }
                        })
                        .debug_selector({
                            let id = self.item.clone();
                            move || format!("async-input:{id}:{ix}")
                        })
                        .child(
                            Textarea::new(answer)
                                .context_menu(crate::gpui::text_menu(answer))
                                .aria_label("Your answer")
                                .disabled(!self.enabled),
                        ),
                )
            })
    }

    fn submit(&self, cx: &mut Context<Self>) {
        let answers: Vec<_> = (0..self.questions.len())
            .map(|ix| self.answer(ix, cx))
            .collect();
        if !self.enabled || answers.iter().any(String::is_empty) {
            return;
        }
        let text = self
            .questions
            .iter()
            .zip(answers)
            .map(|(question, answer)| format!("{}\n{answer}", question.title))
            .collect::<Vec<_>>()
            .join("\n\n");
        cx.emit(AsyncQuestionAnswer {
            target: self.target.clone(),
            item: self.item.clone(),
            text,
        });
    }
}

impl EventEmitter<AsyncQuestionAnswer> for AsyncQuestionsView {}
impl Render for AsyncQuestionsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = (0..self.questions.len()).any(|ix| self.answer(ix, cx).is_empty());
        div()
            .id(SharedString::from(format!("async-questions:{}", self.item)))
            .debug_selector({
                let id = self.item.clone();
                move || format!("async-questions:{id}")
            })
            .w_full()
            .min_h_0()
            .max_h_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(cx.theme().primary.opacity(0.5))
            .rounded_lg()
            .bg(cx.theme().popover)
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().primary)
                    .child("Your input"),
            )
            .child(
                div()
                    .id("question-content")
                    .track_scroll(&self.scroll)
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .children(self.questions.iter().zip(&self.answers).enumerate().map(
                        |(ix, (question, answer))| self.render_question(ix, question, answer, cx),
                    )),
            )
            .child(
                div().flex_none().flex().justify_end().child(
                    Button::new("send-answer")
                        .debug_selector({
                            let id = self.item.clone();
                            move || format!("async-answer:{id}")
                        })
                        .primary()
                        .label("Send answer")
                        .disabled(!self.enabled || empty)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
            )
    }
}

/// Keep a focused answer in its own scroll region after its bounds are known.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Finite measured scroll geometry is adjusted to a visible edge"
)]
fn reveal_answer(
    scroll: &ScrollHandle,
    bounds: gpui_kit::Bounds<gpui_kit::Pixels>,
    window: &mut Window,
) {
    let viewport = scroll.bounds();
    if viewport.size.height <= gpui_kit::px(0.) {
        return;
    }
    let delta = if bounds.size.height >= viewport.size.height || bounds.bottom() > viewport.bottom()
    {
        viewport.bottom() - bounds.bottom()
    } else if bounds.top() < viewport.top() {
        viewport.top() - bounds.top()
    } else {
        return;
    };
    if delta != gpui_kit::px(0.) {
        let mut offset = scroll.offset();
        offset.y += delta;
        scroll.set_offset(offset);
        window.refresh();
    }
}
