//! User-owned computer-use setup; every effect goes through the command mailbox.
use bootty_computer::{ComputerStatus, PermissionStatus};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    switch::Switch,
};
use gpui_kit::{
    App, Context, FocusHandle, Focusable, IntoElement, ParentElement, Render, Styled,
    StyledImage as _, Window, div, img, prelude::*,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct ComputerSetup {
    sender: BoundAppCommandSender,
    enabled: bool,
    status: Option<ComputerStatus>,
    pending: Option<CommandCancellation>,
    error: Option<String>,
    capture: Option<PathBuf>,
    focus: FocusHandle,
}

impl ComputerSetup {
    /// Supply the host's `CommandPalette` sender so only actual user actions can enable access.
    pub(super) fn new(
        sender: BoundAppCommandSender,
        enabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            sender,
            enabled,
            status: None,
            pending: None,
            error: None,
            capture: None,
            focus: cx.focus_handle(),
        };
        view.request("computer.status", Vec::new(), window, cx);
        view
    }

    fn refresh(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.request("computer.status", Vec::new(), window, cx);
    }

    fn request(
        &mut self,
        command: &'static str,
        arguments: Vec<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending.is_some() {
            return;
        }
        let mut invocation = CommandInvocation::from_action(command, Caller::CommandPalette);
        invocation.arguments = arguments;
        let cancellation = CommandCancellation::new();
        let receiver = match self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .unwrap_or_else(Instant::now),
            cancellation.clone(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.error = Some(format!("Computer command unavailable: {error:?}"));
                cx.notify();
                return;
            }
        };
        self.pending = Some(cancellation);
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |weak, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, window, cx| {
                this.pending = None;
                match outcome {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        if let Some(enabled) =
                            value.get("enabled").and_then(serde_json::Value::as_bool)
                        {
                            this.enabled = enabled;
                        }
                        if let Some(permissions) = value.get("permissions") {
                            match serde_json::from_value(permissions.clone()) {
                                Ok(status) => this.status = Some(status),
                                Err(error) => this.error = Some(error.to_string()),
                            }
                        }
                        if let Some(path) = value.get("path").and_then(serde_json::Value::as_str) {
                            this.clear_capture();
                            this.capture = Some(path.into());
                        }
                        if matches!(command, "computer.enable" | "computer.permission.request") {
                            this.refresh(window, cx);
                        }
                    }
                    Ok(outcome) => {
                        this.error = crate::commands::runtime::command_outcome_message(&outcome);
                    }
                    Err(_) => {
                        this.error = Some("Computer command response was interrupted.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn clear_capture(&mut self) {
        if let Some(path) = self.capture.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    fn permission_row(
        &self,
        title: &'static str,
        detail: &'static str,
        permission: &'static str,
        status: Option<PermissionStatus>,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        div()
            .flex()
            .items_center()
            .gap_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(title)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(permission_label(status)),
            )
            .child(
                Button::new(gpui_kit::SharedString::from(format!(
                    "computer-permission-{permission}"
                )))
                .small()
                .outline()
                .label("Grant…")
                .disabled(
                    self.pending.is_some()
                        || matches!(
                            status,
                            Some(PermissionStatus::Granted | PermissionStatus::Unsupported)
                        ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.request(
                        "computer.permission.request",
                        vec![permission.into()],
                        window,
                        cx,
                    );
                })),
            )
            .into_any_element()
    }
}

impl Render for ComputerSetup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let supported = cfg!(target_os = "macos");
        let ready = self.enabled
            && self.status.as_ref().is_some_and(|status| {
                status.accessibility == PermissionStatus::Granted
                    && status.screen_recording == PermissionStatus::Granted
                    && !status.secure_input
            });
        div().id("computer-setup").track_focus(&self.focus).flex().flex_col().gap_4().p_4().w_full()
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Let native agents see the desktop and use the mouse and keyboard. You control when access is enabled."))
            .child(Switch::new("computer-enabled").label("Enable computer use").checked(self.enabled)
                .disabled(self.pending.is_some() || !supported)
                .on_click(cx.listener(|this, enabled: &bool, window, cx| this.request("computer.enable", vec![enabled.to_string()], window, cx))))
            .child(self.permission_row("Screen Recording", "Screenshots include visible applications on this Mac.", "screen_recording", self.status.as_ref().map(|status| status.screen_recording), cx))
            .child(self.permission_row("Accessibility", "Allows mouse, keyboard, and application controls.", "accessibility", self.status.as_ref().map(|status| status.accessibility), cx))
            .when(!supported || self.status.as_ref().is_some_and(|status| status.screen_recording == PermissionStatus::Unsupported), |view| view.child(div().text_sm().text_color(cx.theme().muted_foreground).child("Computer use is available on macOS. Screenshots require macOS 14 or later.")))
            .when(self.status.as_ref().is_some_and(|status| status.secure_input), |view| view.child(div().text_sm().text_color(cx.theme().warning).child("Paused while secure input or a password field is active.")))
            .when_some(self.error.clone(), |view, error| view.child(div().text_sm().text_color(cx.theme().danger).child(error)))
            .child(div().flex().items_center().gap_2()
                .child(Button::new("computer-refresh").small().ghost().label("Check permissions").disabled(self.pending.is_some()).on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))))
                .child(Button::new("computer-capture-test").small().outline().label("Capture test screenshot").disabled(self.pending.is_some() || !ready).on_click(cx.listener(|this, _, window, cx| this.request("computer.snapshot", Vec::new(), window, cx))))
                .when(self.pending.is_some(), |view| view.child(div().text_sm().text_color(cx.theme().muted_foreground).child("Working…"))))
            .when_some(self.capture.clone(), |view, path| view.child(img(path).w_full().h_64().object_fit(gpui_kit::ObjectFit::Contain)))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Input pauses during secure input. Disable computer use here to stop new actions."))
    }
}

impl Focusable for ComputerSetup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for ComputerSetup {
    fn drop(&mut self) {
        if let Some(cancellation) = self.pending.take() {
            let _ = cancellation.cancel();
        }
        self.clear_capture();
    }
}

const fn permission_label(status: Option<PermissionStatus>) -> &'static str {
    match status {
        Some(PermissionStatus::Granted) => "Granted",
        Some(PermissionStatus::NotGranted) => "Not granted",
        Some(PermissionStatus::Unsupported) => "Unavailable",
        None => "Checking…",
    }
}
