//! The current plan sits in the composer drawer: its steps while the agent works through them,
//! and Implement once a proposed plan is ready.
use super::NativeAgentSessionView;
use bootty_agents::{NativeSessionStatus, NativeTodo, NativeTodoStatus};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::{
    AnyElement, Context, ParentElement as _, SharedString, Styled as _, div, prelude::*,
};

impl NativeAgentSessionView {
    /// The plan section of the composer drawer, when there is anything to show.
    pub(super) fn render_plan(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let todos = &self.record.snapshot.todos;
        let unfinished = todos
            .iter()
            .any(|todo| todo.status != NativeTodoStatus::Completed);
        let ready = self.plan_ready();
        (unfinished || ready).then(|| {
            div()
                .id("native-plan")
                .debug_selector(|| "native-plan".into())
                .flex()
                .flex_col()
                .min_w_0()
                .px_1()
                .pb_1()
                .when(unfinished, |section| {
                    section.child(Self::render_todos(todos, cx))
                })
                .when(ready, |section| section.child(self.render_implement(cx)))
                .into_any_element()
        })
    }

    /// The latest proposed plan can be carried out while nothing has followed it.
    fn plan_ready(&self) -> bool {
        self.record.snapshot.status == NativeSessionStatus::Idle
            && self.provider_enabled()
            && self.can_prompt()
            && self
                .record
                .snapshot
                .transcript
                .iter()
                .rev()
                .find(|item| matches!(item.role.as_str(), "user" | "plan"))
                .is_some_and(|item| item.role == "plan" && item.complete)
    }

    fn render_todos(todos: &[NativeTodo], cx: &Context<Self>) -> impl IntoElement {
        let done = todos
            .iter()
            .filter(|todo| todo.status == NativeTodoStatus::Completed)
            .count();
        let summary = format!("{done} of {} done", todos.len());
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .child(
                div()
                    .h_6()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .w_6()
                            .flex()
                            .justify_center()
                            .child(crate::gpui::sized_icon(
                                "list-checks",
                                crate::gpui::IconSize::Small,
                                cx.theme().muted_foreground,
                            )),
                    )
                    .child(div().flex_1().child("Plan"))
                    .child(
                        div()
                            .debug_selector({
                                let summary = summary.clone();
                                move || format!("native-todos:{summary}")
                            })
                            .child(summary),
                    ),
            )
            .child(
                div()
                    .id("native-todo-list")
                    .max_h_32()
                    .overflow_y_scroll()
                    .children(todos.iter().enumerate().map(|(ix, todo)| {
                        let (icon, color) = match todo.status {
                            NativeTodoStatus::Pending => ("circle", cx.theme().muted_foreground),
                            NativeTodoStatus::InProgress => ("circle-dot", cx.theme().foreground),
                            NativeTodoStatus::Completed => {
                                ("circle-check", cx.theme().muted_foreground)
                            }
                        };
                        div()
                            .id(SharedString::from(format!("native-todo:{ix}")))
                            .flex()
                            .items_start()
                            .gap_1()
                            .py_0p5()
                            .text_sm()
                            .text_color(color)
                            .when(todo.status == NativeTodoStatus::Completed, |row| {
                                row.line_through()
                            })
                            .child(
                                div()
                                    .w_6()
                                    .h_5()
                                    .flex()
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .child(crate::gpui::sized_icon(
                                        icon,
                                        crate::gpui::IconSize::Small,
                                        color,
                                    )),
                            )
                            .child(div().flex_1().min_w_0().child(todo.text.clone()))
                    })),
            )
    }

    fn render_implement(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .h_8()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .w_6()
                    .flex()
                    .justify_center()
                    .child(crate::gpui::sized_icon(
                        "list-checks",
                        crate::gpui::IconSize::Small,
                        cx.theme().muted_foreground,
                    )),
            )
            .child(div().flex_1().child("Plan ready"))
            .child(
                Button::new("native-implement-plan")
                    .debug_selector(|| "native-implement-plan".into())
                    .primary()
                    .xsmall()
                    .label("Implement")
                    .loading(self.pending.contains("implement"))
                    .tooltip("Implement plan")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.command("implement", Vec::new(), window, cx);
                    })),
            )
    }
}
