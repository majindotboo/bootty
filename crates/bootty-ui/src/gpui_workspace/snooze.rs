//! A local date and time becomes an absolute deadline on the shared session command path.
use super::GpuiWorkspace;
use bootty_control::{CommandInvocation, CommandOutcome};
use chrono::TimeZone as _;
use gpui_kit::component::{
    ActiveTheme as _, WindowExt as _,
    date_picker::{DatePicker, DatePickerState},
    dialog::DialogButtonProps,
    time_field::TimePrecision,
};
use gpui_kit::{
    Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div,
    prelude::*,
};

impl GpuiWorkspace {
    pub(super) fn open_custom_snooze(
        invocation: CommandInvocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = cx.weak_entity();
        let view = cx.new(|cx| SnoozeView {
            picker: cx.new(|cx| {
                let mut picker =
                    DatePickerState::new(window, cx).time_precision(TimePrecision::Minute);
                picker.set_date_time(
                    chrono::Local::now()
                        .checked_add_signed(chrono::Duration::hours(1))
                        .unwrap_or_else(chrono::Local::now)
                        .naive_local(),
                    window,
                    cx,
                );
                picker
            }),
            invocation,
            workspace,
            error: None,
        });
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let submit = view.clone();
            dialog
                .confirm()
                .title("Snooze session")
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Snooze")
                        .cancel_text("Cancel"),
                )
                .child(view.clone())
                .on_ok(move |_, _, cx| submit.update(cx, SnoozeView::submit))
        });
    }
}

struct SnoozeView {
    picker: Entity<DatePickerState>,
    invocation: CommandInvocation,
    workspace: WeakEntity<GpuiWorkspace>,
    error: Option<String>,
}

impl SnoozeView {
    fn submit(&mut self, cx: &mut Context<Self>) -> bool {
        let until = self
            .picker
            .read(cx)
            .date_time()
            .start()
            .and_then(|local| chrono::Local.from_local_datetime(&local).single())
            .filter(|until| *until > chrono::Local::now());
        let Some(until) = until else {
            self.error = Some(
                "Choose a future date and time. Ambiguous daylight-saving times cannot be used."
                    .into(),
            );
            cx.notify();
            return false;
        };
        let mut invocation = self.invocation.clone();
        invocation.arguments.push(until.timestamp().to_string());
        let outcome = self.workspace.update(cx, |workspace, cx| {
            let mut effects = Vec::new();
            let outcome = workspace.state.dispatch_command(
                invocation,
                crate::ViewportSnapshot::default(),
                &mut effects,
            );
            workspace.pending_effects.extend(effects);
            cx.notify();
            outcome
        });
        match outcome {
            Ok(CommandOutcome::Success { .. }) => true,
            other => {
                self.error = Some(other.map_or_else(
                    |_| "The workspace was closed".into(),
                    |outcome| {
                        crate::commands::command_outcome_message(&outcome)
                            .unwrap_or_else(|| "Could not snooze this session".into())
                    },
                ));
                cx.notify();
                false
            }
        }
    }
}

impl Render for SnoozeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(DatePicker::new(&self.picker))
            .when_some(self.error.clone(), |body, error| {
                body.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }
}
