use super::{AppEffect, AppState};
use bootty_mux::controller::SpaceId;
use bootty_terminal::{
    shell_lifecycle::ShellLifecycle,
    terminal_side_effect::{TerminalSideEffect, TerminalSideEffectEvent},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct TerminalNotifications {
    commands: HashMap<(SpaceId, u64, String), ShellLifecycle>,
    last_bell: Option<Instant>,
}

impl AppState {
    pub(super) fn apply_terminal_notification(
        &mut self,
        scope: SpaceId,
        generation: u64,
        event: &TerminalSideEffectEvent,
        now: Instant,
        window_focused: bool,
        effects: &mut Vec<AppEffect>,
    ) {
        let reported_scope = event
            .source_pane_id
            .as_deref()
            .and_then(bootty_mux::terminal::decode_scoped_pane_id)
            .map(|(scope, _)| scope);
        let actual_scope = reported_scope.unwrap_or(scope);
        let Some(binding) = self.workspace.binding(actual_scope) else {
            return;
        };
        if actual_scope == scope && binding.mux().binding_generation() != generation {
            return;
        }
        let scope = actual_scope;
        let generation = binding.mux().binding_generation();
        let pane = event
            .source_pane_id
            .as_deref()
            .map(|id| {
                bootty_mux::terminal::decode_scoped_pane_id(id)
                    .map_or_else(|| id.to_owned(), |(_, pane)| pane)
            })
            .or_else(|| {
                self.workspace
                    .binding(scope)
                    .and_then(bootty_mux::workspace::BindingRuntime::focused_pane)
            });
        let focused = window_focused
            && scope == self.workspace.active.binding.scope()
            && pane.as_deref() == self.focused_pane().as_deref();
        let observed_at = event.observed_at.unwrap_or(now);
        match &event.effect {
            TerminalSideEffect::Bell => {
                if self.config().session.bell != bootty_config::config::BellMode::Off
                    && self.notifications.last_bell.is_none_or(|last| {
                        now.saturating_duration_since(last) >= Duration::from_millis(100)
                    })
                {
                    self.notifications.last_bell = Some(now);
                    effects.push(AppEffect::Bell);
                }
            }
            TerminalSideEffect::ShellLifecycle(event) => {
                let Some(pane) = pane else {
                    return;
                };
                let key = (scope, generation, pane);
                let lifecycle = self.notifications.commands.entry(key.clone()).or_default();
                let completion = lifecycle.apply(*event, observed_at);
                if !lifecycle.is_running() {
                    self.notifications.commands.remove(&key);
                }
                if let Some(completion) = completion
                    && completion.elapsed
                        >= Duration::from_secs(u64::from(
                            self.config().session.command_notification_min_seconds,
                        ))
                    && self.config().session.command_notifications.allows(focused)
                {
                    let mut args = fluent_bundle::FluentArgs::new();
                    args.set("seconds", completion.elapsed.as_secs());
                    let title = if let Some(status) = completion.exit_code {
                        args.set("status", status);
                        self.localizer
                            .message("command-finished-status", Some(&args))
                    } else {
                        self.localizer.message("command-finished", None)
                    };
                    effects.push(AppEffect::DesktopNotification {
                        title,
                        body: self.localizer.message("command-duration", Some(&args)),
                    });
                }
            }
            _ => {}
        }
    }

    pub(super) fn retain_terminal_notifications(&mut self) {
        self.notifications
            .commands
            .retain(|(scope, generation, pane), _| {
                self.workspace.binding(*scope).is_some_and(|binding| {
                    binding.mux().binding_generation() == *generation
                        && binding
                            .mux()
                            .all_sessions()
                            .iter()
                            .flat_map(|session| &session.windows)
                            .flat_map(|window| &window.panes)
                            .any(|anchor| anchor.pane_id.as_deref() == Some(pane.as_str()))
                })
            });
    }
}
