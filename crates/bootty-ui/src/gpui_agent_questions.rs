//! Async questions are transcript content; their answers use the ordinary prompt path.
use bootty_agents::NativeAsyncQuestion;
use bootty_control::CommandTarget;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::Button,
    input::{InputEvent, Textarea, TextareaState},
};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Window, div,
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
    enabled: bool,
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
                        .submit_on_enter(false)
                });
                cx.subscribe(&answer, |_, _, _: &InputEvent, cx| cx.notify())
                    .detach();
                answer
            })
            .collect();
        Self {
            target,
            item,
            questions,
            answers,
            enabled: true,
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

    fn render_question(
        &self,
        ix: usize,
        question: &NativeAsyncQuestion,
        answer: &Entity<TextareaState>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().child(question.title.clone()))
            .child(div().flex().flex_wrap().gap_1().children(
                question.options.iter().flatten().map(|option| {
                    let answer = answer.clone();
                    let value = option.clone();
                    Button::new(SharedString::from(format!(
                        "async-option:{}:{ix}:{option}",
                        self.item
                    )))
                    .debug_selector({
                        let id = self.item.clone();
                        let value = value.clone();
                        move || format!("async-option:{id}:{ix}:{value}")
                    })
                    .small()
                    .outline()
                    .label(option.clone())
                    .selected(answer.read(cx).value().as_ref() == option.as_str())
                    .disabled(!self.enabled)
                    .on_click(move |_, window, cx| {
                        answer.update(cx, |answer, cx| answer.set_value(value.clone(), window, cx));
                    })
                }),
            ))
            .child(
                div()
                    .debug_selector({
                        let id = self.item.clone();
                        move || format!("async-input:{id}:{ix}")
                    })
                    .child(
                        Textarea::new(answer)
                            .context_menu(crate::gpui::text_menu(answer))
                            .aria_label(question.title.clone())
                            .disabled(!self.enabled),
                    ),
            )
    }

    fn submit(&self, cx: &mut Context<Self>) {
        if !self.enabled
            || self
                .answers
                .iter()
                .any(|answer| answer.read(cx).value().trim().is_empty())
        {
            return;
        }
        let text = self
            .questions
            .iter()
            .zip(&self.answers)
            .map(|(question, answer)| {
                format!("{}\n{}", question.title, answer.read(cx).value().trim())
            })
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
        let empty = self
            .answers
            .iter()
            .any(|answer| answer.read(cx).value().trim().is_empty());
        div()
            .debug_selector({
                let id = self.item.clone();
                move || format!("async-questions:{id}")
            })
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .children(
                self.questions
                    .iter()
                    .zip(&self.answers)
                    .enumerate()
                    .map(|(ix, (question, answer))| self.render_question(ix, question, answer, cx)),
            )
            .child(
                Button::new("send-answer")
                    .debug_selector({
                        let id = self.item.clone();
                        move || format!("async-answer:{id}")
                    })
                    .small()
                    .label("Send answer")
                    .disabled(!self.enabled || empty)
                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
            )
    }
}
