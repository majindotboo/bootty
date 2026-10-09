use super::{
    AppCommandAgentExecutor, AppState, CommandDispatch, LaunchContext, PendingCommandResult,
    failure, launch_tools,
};
use bootty_agents::{
    AgentCommandExecutor, AgentKind, PreparedTerminalRestore, TerminalAgentRecord,
    TerminalAgentService,
};
use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};
use bootty_mux::{executor, target::ExactMuxTarget};
use std::{
    sync::{Arc, mpsc},
    time::Instant,
};

impl AppState {
    pub(in crate::commands::runtime) fn hold_until_terminal_agent_association(
        &mut self,
        scope: bootty_mux::controller::SpaceId,
        identity: &str,
        session: &str,
        outcome: &CommandOutcome,
    ) -> Option<CommandDispatch> {
        let service = self.terminal_agent_service()?;
        let binding = self.workspace.binding(scope)?;
        let handle = self.binding_target_handle(scope, binding.mux().binding_generation());
        let requests: Vec<_> = service
            .records()
            .into_iter()
            .filter(|record| {
                record.location.is_none()
                    && record.binding_id == scope.persistence_value().to_string()
            })
            .filter_map(|record| {
                let original: Vec<String> = serde_json::from_str(&record.target.handle).ok()?;
                let location = binding.cold_agent_location(&original)?;
                if location.0 != identity {
                    return None;
                }
                let window = binding.restored_terminal_window(identity, &location.1)?;
                let pane = binding
                    .restored_terminal_pane(identity, &location.2)?
                    .pane_id
                    .as_ref()?;
                let exact = ExactMuxTarget::Pane(
                    scope,
                    session.to_owned(),
                    window.id.clone(),
                    pane.clone(),
                );
                let target = exact.command_target(
                    bootty_control::ResourceKind::Terminal,
                    binding.mux(),
                    &handle,
                )?;
                let mut invocation = CommandInvocation::new(
                    format!("agents.{}.associate", record.provider),
                    vec![record.target.handle],
                    Caller::Internal,
                );
                invocation.target = Some(target);
                Some(invocation)
            })
            .collect();
        if requests.is_empty() {
            return None;
        }
        self.workspace
            .binding_mut(scope)?
            .set_cold_agent_association_pending(identity, true);
        let commands = AppCommandAgentExecutor {
            creation_receipt: None,
            sender: self.commands.sender.clone(),
        };
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let (deadline, _) = executor::command_execution(None);
            let mut outcome = CommandOutcome::success();
            for invocation in requests {
                outcome = commands.execute(invocation, deadline, CommandCancellation::new());
                if !matches!(outcome, CommandOutcome::Success { .. }) {
                    break;
                }
            }
            let _ = sender.send(outcome);
            repaint();
        });
        Some(CommandDispatch::Pending(
            PendingCommandResult::SessionAgentAssociation {
                scope,
                session: session.to_owned(),
                identity: identity.to_owned(),
                result,
                outcome: Box::new(outcome.clone()),
            },
        ))
    }

    pub(in crate::commands::runtime) fn poll_terminal_agent_association(
        &mut self,
        pending: &mut super::super::PendingAppCommand,
    ) -> std::task::Poll<Option<CommandOutcome>> {
        use std::task::Poll;
        let PendingCommandResult::SessionAgentAssociation {
            scope,
            session,
            identity,
            result,
            outcome,
        } = &mut pending.result
        else {
            return Poll::Pending;
        };
        let associated = match super::super::poll_command_result(result) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(outcome)) => outcome,
            Poll::Ready(Err(_)) => {
                return Poll::Ready(Some(CommandOutcome::Failed {
                    code: "agent_association_failed".to_owned(),
                    message: "Saved agent association worker stopped".to_owned(),
                }));
            }
        };
        if !matches!(associated, CommandOutcome::Success { .. }) {
            return Poll::Ready(Some(associated));
        }
        if let Some(binding) = self.workspace.binding_mut(*scope) {
            binding.set_cold_agent_association_pending(identity, false);
        }
        match self.hold_until_session_checkpoint(
            *scope,
            session,
            *std::mem::replace(outcome, Box::new(CommandOutcome::success())),
        ) {
            CommandDispatch::Complete(outcome) => Poll::Ready(Some(outcome)),
            CommandDispatch::Pending(result) => {
                pending.result = result;
                Poll::Pending
            }
        }
    }

    pub(super) fn dispatch_terminal_agent_association(
        &self,
        invocation: CommandInvocation,
        exact: Option<&ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
        provider: AgentKind,
    ) -> CommandDispatch {
        if invocation.caller != Caller::Internal || invocation.target.is_none() {
            return CommandDispatch::Complete(failure(
                "Saved pane association requires an exact host target",
            ));
        }
        let Some(exact @ ExactMuxTarget::Pane(..)) = exact else {
            return CommandDispatch::Complete(failure(
                "Saved pane association requires an exact pane",
            ));
        };
        let Some(service) = self.terminal_agent_service() else {
            return CommandDispatch::Complete(failure("Terminal agent owner is unavailable"));
        };
        let Some(source) = invocation.arguments.first().and_then(|handle| {
            service
                .records()
                .into_iter()
                .find(|record| &record.target.handle == handle)
        }) else {
            return CommandDispatch::Complete(failure("Retained terminal agent is unavailable"));
        };
        let Some(binding) = self.workspace.binding(exact.scope()) else {
            return CommandDispatch::Complete(failure("Saved binding is unavailable"));
        };
        let Some(location) = binding.terminal_agent_location(exact) else {
            return CommandDispatch::Complete(failure(
                "The terminal has no saved logical location",
            ));
        };
        let original: Option<Vec<String>> = serde_json::from_str(&source.target.handle).ok();
        let legacy = original
            .as_ref()
            .and_then(|original| binding.cold_agent_location(original));
        if source.provider != provider
            || source.binding_id != exact.scope().persistence_value().to_string()
            || (invocation.target.as_ref() != Some(&source.target)
                && legacy.as_ref() != Some(&location))
        {
            return CommandDispatch::Complete(failure(
                "Saved pane association belongs to another retained terminal",
            ));
        }
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(super::super::command_outcome_for_mux_error(error));
        }
        let fence = binding.terminal_agent_association_fence();
        let location = bootty_agents::TerminalAgentLocation {
            task_identity: location.0,
            window_id: location.1,
            pane_id: location.2,
        };
        let (sender, result) = mpsc::channel();
        let (_, creation) = mpsc::channel();
        let observed = invocation.target;
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let outcome = fence()
                .map_err(|error| error.to_string())
                .and_then(|()| service.associate_location(&source, location))
                .map_or_else(
                    |error| failure(&error),
                    super::super::serialized_command_outcome,
                );
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::TerminalAgent {
            result,
            creation,
            observed,
        })
    }

    pub(super) fn dispatch_restored_terminal_agent(
        &mut self,
        invocation: CommandInvocation,
        exact: Option<&ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
        provider: AgentKind,
        launch: bool,
    ) -> CommandDispatch {
        if invocation.caller != Caller::Internal || invocation.target.is_none() {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "Terminal recovery requires the host's exact cold-restored destination"
                    .to_owned(),
            });
        }
        let Some(exact @ ExactMuxTarget::Pane(..)) = exact else {
            return CommandDispatch::Complete(failure("Terminal recovery requires an exact pane"));
        };
        let Some(service) = self.terminal_agent_service() else {
            return CommandDispatch::Complete(failure("Terminal agent owner is unavailable"));
        };
        let (deadline, cancellation) = executor::command_execution(execution);
        if launch {
            // The shared process owner accepts cancellation at the actual replacement boundary.
            return self.launch_restored_terminal_agent(
                &invocation,
                exact,
                provider,
                &service,
                (deadline, cancellation),
            );
        }
        if let Err(error) =
            executor::begin_synchronous_command(Some((deadline, cancellation.clone())))
        {
            return CommandDispatch::Complete(super::super::command_outcome_for_mux_error(error));
        }
        let Some(source) = invocation.arguments.first().and_then(|handle| {
            service
                .records()
                .into_iter()
                .find(|record| &record.target.handle == handle)
        }) else {
            return CommandDispatch::Complete(failure(
                "The retained terminal agent is unavailable",
            ));
        };
        let original = match self.validate_restored_terminal_agent(&source, exact, provider) {
            Ok(original) => original,
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        if !self
            .workspace
            .binding_mut(exact.scope())
            .is_some_and(|binding| binding.begin_restored_agent_terminal(&original, exact))
        {
            return CommandDispatch::Complete(failure(
                "Terminal agent recovery was already attempted",
            ));
        }
        let mut context = self.terminal_launch_context(&invocation, Some(exact));
        // Conversation restoration never recreates child-spawn or computer-capture grants.
        context.allow_spawn = false;
        context.computer_capture = None;
        let commands: Arc<dyn AgentCommandExecutor> = Arc::new(AppCommandAgentExecutor {
            creation_receipt: None,
            sender: self.commands.sender.clone(),
        });
        let (sender, receiver) = mpsc::channel();
        let (_, creation) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let outcome = prepare_restore(
                &service,
                &commands,
                invocation,
                source,
                &context,
                deadline,
                cancellation,
            );
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::TerminalAgent {
            result: receiver,
            creation,
            observed: None,
        })
    }

    pub(in crate::commands::runtime) fn poll_restored_terminal_agent(
        &mut self,
        pending: &mut super::super::PendingAppCommand,
    ) -> std::task::Poll<Option<CommandOutcome>> {
        use std::task::Poll;
        let PendingCommandResult::TerminalAgentRestore {
            restore,
            exact,
            start,
            acknowledged,
            history,
        } = &mut pending.result
        else {
            return Poll::Pending;
        };
        let Some(preparation) = restore.as_ref() else {
            return Poll::Ready(Some(failure("The restore preparation was consumed")));
        };
        if self
            .resolve_command_target(
                "terminal.capture",
                Some(bootty_control::ResourceKind::Terminal),
                Some(&preparation.target),
            )
            .is_err()
        {
            let prior = restore.take();
            std::thread::spawn(move || drop(prior));
            return Poll::Ready(Some(failure(
                "The respawned terminal closed before startup completed",
            )));
        }
        if !*acknowledged {
            match super::super::poll_command_result(start) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(CommandOutcome::Success { .. })) => *acknowledged = true,
                Poll::Ready(result) => {
                    let prior = restore.take();
                    std::thread::spawn(move || drop(prior));
                    return Poll::Ready(Some(
                        result.unwrap_or_else(|_| failure("The pane respawn worker stopped")),
                    ));
                }
            }
            if let Err(error) = self.workspace.restore_respawned_pane_history(
                exact.scope(),
                exact.ids().2.unwrap_or_default(),
                history,
            ) {
                let prior = restore.take();
                std::thread::spawn(move || drop(prior));
                return Poll::Ready(Some(failure(&error.to_string())));
            }
        }
        match self.restored_terminal_agent_started(exact) {
            Ok(false) => return Poll::Pending,
            Ok(true) => {}
            Err(error) => {
                let prior = restore.take();
                std::thread::spawn(move || drop(prior));
                return Poll::Ready(Some(failure(&error)));
            }
        }
        let Some(restore) = restore.take() else {
            return Poll::Ready(Some(failure("The restore preparation was consumed")));
        };
        let Some(service) = self.terminal_agent_service() else {
            std::thread::spawn(move || drop(restore));
            return Poll::Ready(Some(failure("Terminal agent owner is unavailable")));
        };
        let (sender, result) = mpsc::channel();
        let (_, creation) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let outcome = service.register_restored(restore.prepared, restore.target, &restore.source)
                .map_or_else(|error| failure(&error), |record| CommandOutcome::Success {
                    value: serde_json::json!({"terminal": record.target, "agent": record, "restored": true}), warnings: Vec::new(),
                });
            let _ = sender.send(outcome);
            repaint();
        });
        pending.result = PendingCommandResult::TerminalAgent {
            result,
            creation,
            observed: None,
        };
        Poll::Pending
    }

    fn restored_terminal_agent_started(&mut self, exact: &ExactMuxTarget) -> Result<bool, String> {
        let native = self
            .workspace
            .binding(exact.scope())
            .is_some_and(|binding| {
                binding.backend_policy().panes.topology
                    == bootty_mux::provider::PaneTopology::ProcessLocal
            });
        if !native {
            return Ok(true);
        }
        let runtime = exact
            .ids()
            .2
            .and_then(|pane| self.workspace.space_terminal_runtime(exact.scope(), pane))
            .ok_or_else(|| "Respawned native terminal is unavailable".to_owned())?;
        runtime.started().map_err(|error| error.to_string())
    }

    fn validate_restored_terminal_agent(
        &self,
        source: &TerminalAgentRecord,
        exact: &ExactMuxTarget,
        provider: AgentKind,
    ) -> Result<Vec<String>, CommandOutcome> {
        let binding = self
            .workspace
            .binding(exact.scope())
            .ok_or_else(|| failure("The restored binding is unavailable"))?;
        if !cfg!(unix) || binding.multiplexer().remote.is_some() {
            return Err(CommandOutcome::Unsupported {
                message: "Terminal agent recovery requires its local POSIX host".to_owned(),
            });
        }
        if source.provider != provider
            || source.binding_id != exact.scope().persistence_value().to_string()
        {
            return Err(failure(
                "Terminal agent recovery belongs to another provider or binding",
            ));
        }
        if !self
            .config()
            .agents
            .provider(&provider.to_string())
            .is_some_and(|preferences| preferences.enabled)
        {
            return Err(CommandOutcome::Denied {
                message: "This provider is disabled in Settings".to_owned(),
            });
        }
        source.recovery_launch().map_err(|error| failure(&error))?;
        let location = source
            .location
            .as_ref()
            .ok_or_else(|| failure("Retained terminal topology association is unavailable"))?;
        let original = binding
            .cold_agent_source_path(
                &location.task_identity,
                &location.window_id,
                &location.pane_id,
            )
            .ok_or_else(|| failure("The retained terminal location is not cold-restored"))?;
        if binding
            .cold_restored_agent_terminal(&original, true)
            .as_ref()
            != Some(exact)
        {
            return Err(failure(
                "The terminal is not this agent's untouched cold-restored pane",
            ));
        }
        Ok(original)
    }

    fn launch_restored_terminal_agent(
        &mut self,
        invocation: &CommandInvocation,
        exact: &ExactMuxTarget,
        provider: AgentKind,
        service: &Arc<TerminalAgentService>,
        execution: (Instant, CommandCancellation),
    ) -> CommandDispatch {
        let Some(restore) = invocation
            .arguments
            .first()
            .and_then(|token| service.take_restore(token))
        else {
            return CommandDispatch::Complete(failure(
                "The private recovery preparation is no longer available",
            ));
        };
        let valid = service
            .record(&restore.source.target)
            .filter(|current| {
                current.provider == restore.source.provider
                    && current.binding_id == restore.source.binding_id
                    && current.launch == restore.source.launch
                    && current.location == restore.source.location
                    && current.observation.session_id == restore.source.observation.session_id
                    && current.observation.session_file == restore.source.observation.session_file
                    && current.recovery_launch().is_ok()
                    && restore.prepared.tools_enabled()
            })
            .ok_or_else(|| failure("Retained agent provenance changed during recovery"))
            .and_then(|_| self.validate_restored_terminal_agent(&restore.source, exact, provider))
            .and_then(|original| {
                if invocation.target.as_ref() != Some(&restore.target) {
                    return Err(failure("The prepared recovery destination changed"));
                }
                let history = self
                    .workspace
                    .binding(exact.scope())
                    .and_then(|binding| binding.cold_restored_agent_history(&original))
                    .ok_or_else(|| failure("The original restored pane history is unavailable"))?;
                if !self
                    .workspace
                    .binding_mut(exact.scope())
                    .is_some_and(|binding| {
                        binding.admit_restored_agent_terminal(&original, exact, true)
                    })
                {
                    return Err(failure("The cold-restored pane was already consumed"));
                }
                Ok(history)
            });
        let history = match valid {
            Ok(history) => history,
            Err(outcome) => {
                std::thread::spawn(move || drop(restore));
                return CommandDispatch::Complete(outcome);
            }
        };
        let (sender, start) = mpsc::channel();
        let repaint = self.repaint.clone();
        self.workspace.respawn_pane_command(
            exact,
            restore.prepared.argv(),
            restore.prepared.launch.cwd.clone(),
            Arc::clone(&history),
            Some(execution),
            move |result| {
                let _ = sender.send(
                    result.map_or_else(super::super::command_outcome_for_mux_error, |()| {
                        CommandOutcome::success()
                    }),
                );
                repaint();
            },
        );
        CommandDispatch::Pending(PendingCommandResult::TerminalAgentRestore {
            restore: Some(Box::new(restore)),
            exact: exact.clone(),
            start,
            acknowledged: false,
            history,
        })
    }
}

fn prepare_restore(
    service: &Arc<TerminalAgentService>,
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: CommandInvocation,
    source: TerminalAgentRecord,
    context: &LaunchContext,
    deadline: Instant,
    cancellation: CommandCancellation,
) -> CommandOutcome {
    let mut launch = match source.recovery_launch() {
        Ok(launch) => launch,
        Err(error) => return failure(&error),
    };
    let provider = source.provider;
    let (tools, warning) = launch_tools(
        commands,
        &invocation,
        provider,
        "resume",
        context,
        &mut launch,
    );
    let unobserved = (provider == AgentKind::Codex && !context.process_local).then(|| {
        "Codex runs directly in this persistent terminal; app-owned observation is unavailable"
            .to_owned()
    });
    let prepared = if let Some(tools) = tools {
        service.prepare_with_tools(provider, launch, tools, unobserved)
    } else if let Some(detail) = unobserved {
        TerminalAgentService::prepare_unobserved(provider, launch, detail)
    } else {
        service.prepare(provider, launch)
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return failure(&error),
    };
    let Some(target) = invocation.target else {
        return failure("Recovery destination is unavailable");
    };
    let token = match service.stage_restore(PreparedTerminalRestore {
        prepared,
        source,
        target: target.clone(),
    }) {
        Ok(token) => token,
        Err(error) => return failure(&error),
    };
    let mut launch = CommandInvocation::new(
        format!("agents.{provider}.restore.launch"),
        vec![token.clone()],
        Caller::Internal,
    );
    launch.target = Some(target);
    let mut outcome = commands.execute(launch, deadline, cancellation);
    // Preparation that never reached its host gate is dropped on this worker.
    drop(service.take_restore(&token));
    if let CommandOutcome::Success { warnings, .. } = &mut outcome {
        warnings.extend(warning);
    }
    outcome
}
