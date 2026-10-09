//! Branch publishing and PR creation stay scoped to the captured repository.
use super::*;
use bootty_git::github::{PullRequestCreationContext, PullRequestCreationRequest};

pub(super) enum CreationSubmission {
    Publish(PullRequestCreationContext),
    Create(PullRequestCreationRequest),
}

pub(super) struct CreationEditor {
    context: PullRequestCreationContext,
    base: Entity<InputState>,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    draft: bool,
    saving: bool,
    published: bool,
    _subscriptions: Vec<Subscription>,
}

impl CreationEditor {
    pub(super) fn new(
        context: PullRequestCreationContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let base = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Base branch");
            input.set_value(context.base.clone(), window, cx);
            input
        });
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Title"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("Description")
        });
        let subscriptions = vec![
            cx.observe(&base, |_, _, cx| cx.notify()),
            cx.observe(&title, |_, _, cx| cx.notify()),
            cx.observe(&body, |_, _, cx| cx.notify()),
        ];
        Self {
            context,
            base,
            title,
            body,
            draft: false,
            saving: false,
            published: false,
            _subscriptions: subscriptions,
        }
    }

    pub(super) const fn set_saving(&mut self, saving: bool) {
        self.saving = saving;
    }
    pub(super) fn published(&mut self, cx: &mut Context<Self>) {
        self.published = true;
        cx.notify();
    }

    fn submit(&self, cx: &mut Context<Self>) {
        let mut context = self.context.clone();
        self.base
            .read(cx)
            .value()
            .trim()
            .clone_into(&mut context.base);
        cx.emit(CreationSubmission::Create(PullRequestCreationRequest {
            context,
            title: self.title.read(cx).value().trim().to_owned(),
            body: self.body.read(cx).value().to_string(),
            draft: self.draft,
        }));
    }
}

impl EventEmitter<CreationSubmission> for CreationEditor {}
impl Render for CreationEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let head_repository = self
            .context
            .head_repository
            .as_ref()
            .unwrap_or(&self.context.repository);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(div().text_sm().child("New pull request"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{}/{}:{} → {}/{}",
                        head_repository.owner,
                        head_repository.name,
                        self.context.branch,
                        self.context.repository.owner,
                        self.context.repository.name
                    )),
            )
            .child(
                Input::new(&self.base)
                    .disabled(self.saving)
                    .aria_label("Base branch"),
            )
            .child(
                Input::new(&self.title)
                    .disabled(self.saving)
                    .aria_label("Pull request title"),
            )
            .child(Textarea::new(&self.body).disabled(self.saving))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .child(
                        Button::new("draft-pr")
                            .label("Draft")
                            .small()
                            .ghost()
                            .selected(self.draft)
                            .disabled(self.saving)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.draft = !this.draft;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("publish-pr-branch")
                            .label(if self.published {
                                "Published"
                            } else {
                                "Publish branch"
                            })
                            .small()
                            .disabled(self.saving)
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(CreationSubmission::Publish(this.context.clone()));
                            })),
                    )
                    .child(
                        Button::new("create-pr")
                            .label("Create pull request")
                            .small()
                            .primary()
                            .disabled(
                                self.saving
                                    || self.title.read(cx).value().trim().is_empty()
                                    || self.base.read(cx).value().trim().is_empty(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
    }
}
