use super::GpuiWorkspace;
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget,
};
use gpui_kit::{Context, Window};
use std::time::{Duration, Instant};

impl GpuiWorkspace {
    pub(super) fn open_terminal_agent_command(
        &self,
        invocation: CommandInvocation,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let sender = self.state.app_command_sender(invocation.caller);
        let Some(deadline) = Instant::now().checked_add(Duration::from_secs(30)) else {
            return;
        };
        let response = sender.submit(invocation, deadline, CommandCancellation::new());
        cx.spawn_in(window, async move |owner, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    response
                        .map_err(|error| format!("{error:?}"))
                        .and_then(|response| {
                            response
                                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                                .map_err(|error| error.to_string())
                        })
                })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                match outcome {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        if let Some(target) = value
                            .get("terminal_target")
                            .cloned()
                            .and_then(|value| serde_json::from_value::<CommandTarget>(value).ok())
                        {
                            let mut focus =
                                CommandInvocation::from_action("agents.focus", Caller::Internal);
                            focus.target = Some(target);
                            this.invoke_gpui_command(focus, window, cx);
                        }
                    }
                    Ok(outcome) => this
                        .state
                        .record_error(format!("Start agent terminal: {outcome:?}")),
                    Err(error) => this
                        .state
                        .record_error(format!("Start agent terminal: {error}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
