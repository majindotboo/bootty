//! Compact setup progress and recovery, projected from the new-session owner.
use super::super::{IconSize, sized_icon};
use super::*;
use gpui_kit::base::StyledExt as _;

impl DialogView {
    pub(super) fn creation_status(
        spec: &DialogSpec,
        submit: Option<&DialogRow>,
        error: Option<&str>,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        v_flex()
            .id("dialog-creation-status")
            .debug_selector(|| "dialog-creation-status".into())
            .w_full()
            .min_w_0()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted.opacity(0.2))
            .child(Self::creation_header(spec, error.is_some(), cx))
            .child(
                v_flex().gap_1().children(
                    spec.progress
                        .iter()
                        .map(|step| Self::creation_step(step, cx)),
                ),
            )
            .when_some(error, |view, error| {
                view.child(Self::creation_diagnostic(error, cx))
            })
            .when_some(spec.hint.as_deref(), |view, hint| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(hint.to_owned()),
                )
            })
            .when_some(error, |view, error| {
                view.child(Self::creation_recovery(spec, submit, error, cx))
            })
            .into_any_element()
    }

    fn creation_header(spec: &DialogSpec, failed: bool, cx: &App) -> gpui_kit::AnyElement {
        let complete = spec
            .progress
            .iter()
            .filter(|step| step.state == DialogProgressState::Complete)
            .count();
        let title = if failed {
            match spec
                .progress
                .iter()
                .find(|step| step.state == DialogProgressState::Failed)
                .map(|step| step.id.as_str())
            {
                Some("worktree") => "Worktree setup failed",
                Some("names") => "Could not name the worktree",
                _ => "Session could not start",
            }
        } else if spec
            .progress
            .iter()
            .any(|step| step.id == "worktree" && step.state != DialogProgressState::Complete)
        {
            "Setting up worktree…"
        } else {
            "Starting session…"
        };

        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .items_center()
            .child(div().flex_1().text_sm().font_medium().child(title))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{complete}/{}", spec.progress.len())),
            )
            .child(h_flex().gap_1().children(spec.progress.iter().map(|step| {
                div().w_5().h_1().rounded_full().bg(match step.state {
                    DialogProgressState::Complete => cx.theme().primary,
                    DialogProgressState::Running => cx.theme().primary.opacity(0.5),
                    DialogProgressState::Failed => cx.theme().danger,
                    DialogProgressState::Pending => cx.theme().muted,
                })
            })))
            .into_any_element()
    }

    fn creation_recovery(
        spec: &DialogSpec,
        submit: Option<&DialogRow>,
        error: &str,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let copy = Self::creation_error_text(spec, error);

        h_flex()
            .w_full()
            .gap_2()
            .justify_end()
            .child(
                Button::new("dialog-copy-creation-error")
                    .debug_selector(|| "dialog-copy-creation-error".into())
                    .ghost()
                    .small()
                    .label("Copy details")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(copy.clone()));
                    }),
            )
            .when_some(submit, |row, submit| {
                row.child(
                    Self::new_session_start(spec, submit, "dialog-creation-retry", cx)
                        .small()
                        .label("Try again"),
                )
            })
            .into_any_element()
    }

    fn creation_step(step: &DialogProgressStep, cx: &App) -> gpui_kit::AnyElement {
        let color = match step.state {
            DialogProgressState::Failed => cx.theme().danger,
            _ => cx.theme().muted_foreground,
        };
        let marker = match step.state {
            DialogProgressState::Running => gpui_kit::component::spinner::Spinner::new()
                .small()
                .into_any_element(),
            DialogProgressState::Complete => {
                sized_icon("check", IconSize::Small, color).into_any_element()
            }
            DialogProgressState::Failed => {
                sized_icon("circle-alert", IconSize::Small, color).into_any_element()
            }
            DialogProgressState::Pending => {
                sized_icon("circle", IconSize::Small, color).into_any_element()
            }
        };
        let state = match step.state {
            DialogProgressState::Pending => "pending",
            DialogProgressState::Running => "in progress",
            DialogProgressState::Complete => "complete",
            DialogProgressState::Failed => "failed",
        };
        let selector = format!("dialog-creation-step-{}", step.id);
        h_flex()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            .role(gpui_kit::Role::Status)
            .aria_label(format!("{}: {state}", step.label))
            .w_full()
            .min_w_0()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(if step.state == DialogProgressState::Running {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .child(marker)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .child(step.label.clone()),
            )
            .when(step.state == DialogProgressState::Running, |view| {
                view.child(div().text_color(cx.theme().muted_foreground).child("Now"))
            })
            .into_any_element()
    }

    fn creation_diagnostic(error: &str, cx: &App) -> gpui_kit::AnyElement {
        public_selector(
            "dialog-prompt-validation",
            div()
                .id("creation-error")
                .role(gpui_kit::Role::Alert)
                .aria_label(error.to_owned())
                .w_full()
                .min_w_0()
                .max_h(rems(6.))
                .overflow_y_scroll()
                .p_2()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .bg(cx.theme().background)
                .text_xs()
                .text_color(cx.theme().foreground)
                .font_family(cx.theme().mono_font_family.clone())
                .child(error.to_owned()),
        )
    }

    fn creation_error_text(spec: &DialogSpec, error: &str) -> String {
        let host = spec
            .fields
            .iter()
            .find(|field| field.id == "host")
            .map_or("selected Space", |field| field.value.as_str());
        format!(
            "Space: {host}\nDirectory: {}\n\n{error}\n\n{}",
            spec.footer.as_deref().unwrap_or_default(),
            spec.hint.as_deref().unwrap_or_default()
        )
    }
}
