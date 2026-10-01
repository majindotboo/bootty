use std::time::{Duration, Instant};

use bootty_agents::{AgentKind, NativeSessionRecord};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, ResourceKind,
};
use gpui_kit::component::{Sizable as _, WindowExt as _, button::Button};
use gpui_kit::{Context, ParentElement as _, Styled as _, Window, div};

use super::GpuiWorkspace;

impl GpuiWorkspace {
    pub(super) fn conversation_returns_to_terminal(&self, invocation: &CommandInvocation) -> bool {
        crate::gpui_actions::invocation_returns_to_terminal(
            invocation,
            &self.state.command_catalog(),
        )
    }

    pub(super) fn open_agent_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.close_overlay_dialogs();
        self.dialogs.clear_presentation(window, cx);
        self.native_sessions.opened = true;
        let owner = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let mut providers = div().flex().flex_col().gap_2();
            for provider in AgentKind::ALL {
                let owner = owner.clone();
                providers = providers.child(
                    Button::new(gpui_kit::SharedString::from(format!(
                        "conversation-provider-{provider}"
                    )))
                    .label(format!("Open {provider} conversation"))
                    .small()
                    .outline()
                    .on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        _ = owner.update(cx, |this, cx| {
                            this.start_agent_conversation(provider, window, cx);
                        });
                    }),
                );
            }
            dialog.title("Open agent conversation…").child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child("Conversations use the provider protocol and keep their own history.")
                    .child(providers),
            )
        });
        cx.notify();
    }

    fn start_agent_conversation(
        &mut self,
        provider: AgentKind,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let mut invocation = CommandInvocation::from_action(
            &format!("harness.{provider}.start"),
            Caller::CommandPalette,
        );
        invocation.target = self
            .state
            .current_command_target_for("session.create", ResourceKind::Binding);
        let now = Instant::now();
        let receiver = match self
            .state
            .app_command_sender(Caller::CommandPalette)
            .submit(
                invocation,
                now.checked_add(Duration::from_secs(60)).unwrap_or(now),
                CommandCancellation::new(),
            ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.state
                    .record_render_error(format!("Conversation launch failed: {error:?}"));
                cx.notify();
                return;
            }
        };
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                match result {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        match serde_json::from_value::<NativeSessionRecord>(value) {
                            Ok(record) => this.select_native_session(&record, window, cx),
                            Err(error) => this.state.record_render_error(error),
                        }
                    }
                    Ok(outcome) => this.state.record_render_error(
                        crate::commands::command_outcome_message(&outcome)
                            .unwrap_or_else(|| "Conversation launch failed".to_owned()),
                    ),
                    Err(error) => this.state.record_render_error(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
