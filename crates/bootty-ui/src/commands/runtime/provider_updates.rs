use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use bootty_agents::{AgentKind, ProviderUpdate};
use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};

use super::CommandDispatch;
use crate::{
    commands::ExactMuxTarget,
    state::{AppEffect, AppState},
};

const RETRY: Duration = Duration::from_mins(1);
const DAILY: Duration = Duration::from_hours(24);
const TIMEOUT: Duration = Duration::from_mins(10);

#[derive(Default)]
pub(super) struct ProviderUpdates {
    next: HashMap<AgentKind, Instant>,
    pending: Option<(
        AgentKind,
        mpsc::Receiver<CommandOutcome>,
        CommandCancellation,
    )>,
    changed: Arc<AtomicBool>,
}

impl Drop for ProviderUpdates {
    fn drop(&mut self) {
        if let Some((_, _, cancellation)) = &self.pending {
            let _ = cancellation.cancel();
        }
    }
}

impl AppState {
    pub(crate) fn poll_provider_updates(&mut self, now: Instant, effects: &mut Vec<AppEffect>) {
        if self
            .commands
            .provider_updates
            .changed
            .swap(false, Ordering::AcqRel)
        {
            self.refresh_updated_provider_catalogs();
        }
        let mut update_error = None;
        let state = &mut self.commands.provider_updates;
        if let Some((provider, reply, _)) = &state.pending {
            match reply.try_recv() {
                Ok(outcome) => {
                    let delay = if matches!(&outcome, CommandOutcome::Failed { code, .. } if code == "provider_busy")
                    {
                        RETRY
                    } else {
                        DAILY
                    };
                    state
                        .next
                        .insert(*provider, now.checked_add(delay).unwrap_or(now));
                    state.pending = None;
                    if let CommandOutcome::Failed { code, message } = outcome
                        && code != "provider_busy"
                    {
                        update_error = Some(message);
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    state.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(error) = update_error {
            self.record_error("Could not update provider", &error);
        }
        if !self.config().agents.auto_update || self.commands.terminal_agents.is_none() {
            return;
        }
        if self.commands.provider_updates.pending.is_some() {
            return;
        }
        let enabled = AgentKind::ALL
            .into_iter()
            .filter(|provider| {
                self.config()
                    .agents
                    .provider(&provider.to_string())
                    .is_some_and(|config| config.enabled)
            })
            .collect::<Vec<_>>();
        for provider in enabled.iter().copied() {
            let next = self
                .commands
                .provider_updates
                .next
                .entry(provider)
                .or_insert_with(|| now.checked_add(RETRY).unwrap_or(now));
            if now < *next {
                continue;
            }
            let mut invocation =
                CommandInvocation::from_action("agents.providers.update", Caller::Internal);
            invocation.arguments = vec![provider.to_string()];
            let cancellation = CommandCancellation::default();
            match self.app_command_sender(Caller::Internal).submit(
                invocation,
                now.checked_add(TIMEOUT).unwrap_or(now),
                cancellation.clone(),
            ) {
                Ok(reply) => {
                    self.commands.provider_updates.pending = Some((provider, reply, cancellation));
                }
                Err(_) => {
                    self.commands
                        .provider_updates
                        .next
                        .insert(provider, now.checked_add(RETRY).unwrap_or(now));
                }
            }
            break;
        }
        // The worker repaints when it completes. Otherwise wake at the next eligible provider,
        // including the shorter busy retry, rather than polling every minute after success.
        if self.commands.provider_updates.pending.is_none()
            && let Some(after) = self
                .commands
                .provider_updates
                .next
                .iter()
                .filter(|(provider, _)| enabled.contains(provider))
                .map(|(_, next)| next.saturating_duration_since(now))
                .min()
        {
            effects.push(AppEffect::RepaintAfter(after));
        }
    }

    pub(super) fn dispatch_provider_update(
        &self,
        provider: AgentKind,
        invocation: &CommandInvocation,
        exact: Option<&ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if invocation.command != "agents.providers.update"
            && self.terminal_launch_context(invocation, exact).remote
        {
            return CommandDispatch::Complete(CommandOutcome::Unsupported {
                message: "Update remote providers on their execution host.".into(),
            });
        }
        let preferences = self
            .config()
            .agents
            .provider(&provider.to_string())
            .cloned()
            .unwrap_or_default();
        let program = if invocation.command != "agents.providers.update"
            && let Some(program) = invocation
                .arguments
                .get(1)
                .filter(|program| !program.is_empty())
        {
            program.clone()
        } else if preferences.program.is_empty() {
            provider.default_program().to_owned()
        } else {
            preferences.program.clone()
        };
        let terminals = self.terminal_agent_service();
        let native = self.native_agent_service();
        let changed = self.commands.provider_updates.changed.clone();
        let (deadline, cancellation) = execution.unwrap_or_else(|| {
            let now = Instant::now();
            (
                now.checked_add(TIMEOUT).unwrap_or(now),
                CommandCancellation::default(),
            )
        });
        self.dispatch_committed_command(Some((deadline, cancellation.clone())), move || {
            let update = match ProviderUpdate::prepare(provider, &program) {
                Ok(Some(update)) => update,
                Ok(None) => {
                    return CommandOutcome::Unsupported {
                        message: "This installation must be updated manually.".into(),
                    };
                }
                Err(code) => return CommandOutcome::Failed {
                    code,
                    message:
                        "Close this provider's active conversations and terminals before updating."
                            .into(),
                },
            };
            if let Err(message) = update.run(deadline, || cancellation.is_cancelled()) {
                return CommandOutcome::Failed {
                    code: "provider_update_failed".into(),
                    message,
                };
            }
            if let Some(native) = native
                && let Err(message) = native.invalidate_provider_catalogs()
            {
                return CommandOutcome::Failed {
                    code: "provider_catalog_invalidation_failed".into(),
                    message,
                };
            }
            drop(update);
            if let Some(terminals) = terminals {
                terminals.inspect_provider(
                    provider,
                    &program,
                    preferences
                        .selected_profile()
                        .and_then(|profile| profile.directory.as_deref()),
                    None,
                );
            }
            changed.store(true, Ordering::Release);
            CommandOutcome::Success {
                value: serde_json::json!({"provider":provider,"checked":true}),
                warnings: Vec::new(),
            }
        })
    }
}
