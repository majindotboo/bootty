//! User-owned pairing for the mobile app, using the shared command mailbox.
use std::time::{Duration, Instant};

use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, Styled,
    Window, div, prelude::*,
};

use crate::remote_connections::ConnectionStatus;

pub struct ConnectionsSetup {
    sender: BoundAppCommandSender,
    host: Entity<InputState>,
    status: Option<ConnectionStatus>,
    pending: Option<CommandCancellation>,
    error: Option<String>,
    reveal: bool,
    copied: bool,
    focus: FocusHandle,
}

impl ConnectionsSetup {
    pub(super) fn new(
        sender: BoundAppCommandSender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            sender,
            host: cx.new(|cx| {
                let mut input =
                    InputState::new(window, cx).placeholder("This computer's IP address");
                input.set_value("127.0.0.1", window, cx);
                input
            }),
            status: None,
            pending: None,
            error: None,
            reveal: false,
            copied: false,
            focus: cx.focus_handle(),
        };
        view.request("connections.status", Vec::new(), window, cx);
        view
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
                self.error = Some(format!("Connection command unavailable: {error:?}"));
                cx.notify();
                return;
            }
        };
        self.pending = Some(cancellation);
        self.error = None;
        self.copied = false;
        if command == "connections.revoke" {
            self.reveal = false;
        }
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
                        if value.get("copied").and_then(serde_json::Value::as_bool) == Some(true) {
                            this.copied = true;
                        } else {
                            match serde_json::from_value::<ConnectionStatus>(value) {
                                Ok(status) => {
                                    if this.host.read(cx).value().is_empty()
                                        && let Some(host) = status.suggested_host
                                    {
                                        this.host.update(cx, |input, cx| {
                                            input.set_value(host.to_string(), window, cx);
                                        });
                                    }
                                    if !status.enabled {
                                        this.reveal = false;
                                    }
                                    this.status = Some(status);
                                }
                                Err(error) => {
                                    this.error = Some(error.to_string());
                                }
                            }
                        }
                    }
                    Ok(outcome) => {
                        this.error = crate::commands::runtime::command_outcome_message(&outcome);
                    }
                    Err(_) => {
                        this.error = Some("Connection command was interrupted.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn enable(&mut self, window: &Window, cx: &mut Context<Self>) {
        let host = self.host.read(cx).value().trim().to_owned();
        self.request("connections.enable", vec![host], window, cx);
    }
}

impl Render for ConnectionsSetup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let listening = self.status.as_ref().is_some_and(|status| status.enabled);
        let code = self
            .status
            .as_ref()
            .and_then(|status| status.pairing_code.clone());
        let address = self
            .status
            .as_ref()
            .and_then(|status| status.address)
            .map(|address| address.to_string());
        let pending = self.pending.is_some();
        let lan_hint = self.status.as_ref().and_then(|status| status.suggested_host)
            .filter(|host| !host.is_loopback())
            .map(|host| format!("For a phone on your LAN, use {host}. Use 127.0.0.1 for a simulator on this Mac."));
        div().id("connections-setup").track_focus(&self.focus).flex().flex_col().gap_4().p_4().w_full()
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Connect Bootty on your phone to this desktop. Devices must be able to reach this computer on the same network."))
            .child(div().flex().flex_col().gap_2().child("Desktop IP address").child(Input::new(&self.host).disabled(listening || pending)))
            .when_some(lan_hint, |view, hint| view.child(div().text_sm().text_color(cx.theme().muted_foreground).child(hint)))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(if listening { "Connection enabled for this app run" } else { "Connection disabled" }))
            .when_some(address, |view, address| view.child(div().flex().flex_col().gap_1().child("Address for your phone").child(address)))
            .when_some(code, |view, code| view.child(div().flex().flex_col().gap_2()
                .child("Pairing code")
                .child(div().flex().items_center().gap_2()
                    .child(div().flex_1().min_w_0().truncate().text_color(cx.theme().muted_foreground).child(if self.reveal { code } else { "••••••••••••••••".into() }))
                    .child(Button::new("connection-reveal").small().ghost().label(if self.reveal { "Hide" } else { "Reveal" }).on_click(cx.listener(|this, _, _, cx| { this.reveal = !this.reveal; cx.notify(); })))
                    .child(Button::new("connection-copy").small().outline().label(if self.copied { "Copied" } else { "Copy" }).disabled(pending).on_click(cx.listener(|this, _, window, cx| { this.request("connections.copy", Vec::new(), window, cx); }))))))
            .when_some(self.error.clone(), |view, error| view.child(div().text_sm().text_color(cx.theme().danger).child(error)))
            .child(div().flex().items_center().gap_2()
                .child(Button::new("connection-enable").small().primary().label(if listening { "Revoke connection" } else { "Enable connection" }).disabled(pending)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if listening { this.request("connections.revoke", Vec::new(), window, cx); } else { this.enable(window, cx); }
                    })))
                .child(Button::new("connection-refresh").small().ghost().label("Refresh").disabled(pending).on_click(cx.listener(|this, _, window, cx| { this.request("connections.status", Vec::new(), window, cx); })))
                .when(pending, |view| view.child(div().text_sm().text_color(cx.theme().muted_foreground).child("Working…"))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Anyone with this code can control the desktop. Revoking disconnects paired devices and invalidates the code. Closing this dialog keeps the connection enabled; quitting Bootty revokes it."))
    }
}

impl Focusable for ConnectionsSetup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for ConnectionsSetup {
    fn drop(&mut self) {
        if let Some(cancellation) = self.pending.take() {
            let _ = cancellation.cancel();
        }
    }
}
