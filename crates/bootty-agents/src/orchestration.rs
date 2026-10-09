mod store;
mod values;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError, Weak},
};

use bootty_control::{CommandCancellation, CommandTarget, ResourceKind};

use crate::{NativeTurnOutcome, NativeTurnReceipt};

pub use values::{
    AgentPrompt, MAX_RUN_FILE_BYTES, MAX_RUN_NODES, MAX_SAVED_RUNS, OrchestrationContext,
    OrchestrationDispatch, OrchestrationLaunch, OrchestrationNode, OrchestrationNodeSpec,
    OrchestrationNodeState, OrchestrationOutcome, OrchestrationPlan, OrchestrationRun,
    OrchestrationSnapshot, OrchestrationToken,
};

/// Durable run owner. Host command workers own dispatch/process effects; snapshots own no process.
pub struct OrchestrationService {
    path: PathBuf,
    mutation: Mutex<()>,
    snapshot: Mutex<Arc<OrchestrationSnapshot>>,
    pending: Mutex<Vec<(OrchestrationToken, CommandCancellation)>>,
}

/// Keeps a pending dispatch registered until the host receives its accepted result.
pub struct OrchestrationDispatchGuard {
    owner: Weak<OrchestrationService>,
    token: OrchestrationToken,
    cancellation: CommandCancellation,
}

impl OrchestrationDispatchGuard {
    /// Forward unchanged to `AgentCommandExecutor::execute_pending` and the final owner gate.
    #[must_use]
    pub fn cancellation(&self) -> CommandCancellation {
        self.cancellation.clone()
    }
}

impl Drop for OrchestrationDispatchGuard {
    fn drop(&mut self) {
        _ = self.cancellation.cancel();
        if let Some(owner) = self.owner.upgrade() {
            owner
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|(token, _)| token != &self.token);
        }
    }
}

impl OrchestrationService {
    /// Recover in-flight work as interrupted, without replaying a prompt or trusting old terminals.
    /// # Errors
    /// Rejects invalid persisted state or a failed recovery commit before publication.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mut snapshot = store::load(&path)?;
        let mut recovered = false;
        for run in &mut snapshot.runs {
            let mut interrupted = false;
            for node in &mut run.nodes {
                if matches!(
                    node.state,
                    OrchestrationNodeState::Claimed | OrchestrationNodeState::Running { .. }
                ) {
                    node.state = OrchestrationNodeState::Interrupted {
                        target: node.state.target().cloned(),
                    };
                    interrupted = true;
                }
            }
            if interrupted {
                run.generation = next_generation(run.generation)?;
                recovered = true;
            }
        }
        if recovered {
            snapshot.revision = next_generation(snapshot.revision)?;
            snapshot.durability_warning = store::commit(&path, &snapshot)?;
        }
        Ok(Self {
            path,
            mutation: Mutex::new(()),
            snapshot: Mutex::new(Arc::new(snapshot)),
            pending: Mutex::new(Vec::new()),
        })
    }

    #[must_use]
    pub fn snapshot_arc(&self) -> Arc<OrchestrationSnapshot> {
        Arc::clone(&self.snapshot.lock().unwrap_or_else(PoisonError::into_inner))
    }

    #[must_use]
    pub fn snapshot(&self) -> OrchestrationSnapshot {
        let published = self.snapshot_arc();
        published.as_ref().clone()
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .revision
    }

    /// Register the exact pending owner token under the same mutation lock as cancellation.
    /// # Errors
    /// Rejects stale/non-claimed attempts or an already registered dispatch.
    pub fn begin_dispatch(
        self: &Arc<Self>,
        token: &OrchestrationToken,
    ) -> Result<OrchestrationDispatchGuard, String> {
        let mutation = self.mutation.lock().unwrap_or_else(PoisonError::into_inner);
        let mut snapshot = self.snapshot();
        let node = find_attempt(find_run(&mut snapshot.runs, token.run_id())?, token)?;
        if node.state != OrchestrationNodeState::Claimed {
            return Err("Only a current claimed attempt can start dispatch".to_owned());
        }
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.iter().any(|(registered, _)| registered == token) {
            return Err("Dispatch attempt already has a pending owner".to_owned());
        }
        let cancellation = CommandCancellation::new();
        pending.push((token.clone(), cancellation.clone()));
        drop(pending);
        drop(mutation);
        Ok(OrchestrationDispatchGuard {
            owner: Arc::downgrade(self),
            token: token.clone(),
            cancellation,
        })
    }

    /// Reconstruct only currently accepted attempts for trusted host observations.
    #[must_use]
    pub fn running_dispatches(&self) -> Vec<(OrchestrationDispatch, CommandTarget)> {
        self.snapshot()
            .runs
            .into_iter()
            .flat_map(|run| {
                run.nodes.into_iter().filter_map(move |node| {
                    let OrchestrationNodeState::Running { target } = node.state else {
                        return None;
                    };
                    Some((
                        OrchestrationDispatch {
                            title: node.spec.title,
                            token: OrchestrationToken {
                                run_id: run.id.clone(),
                                generation: run.generation,
                                node_id: node.spec.id,
                                attempt: node.attempt,
                            },
                            context: run.context.clone(),
                            launch: node.spec.launch,
                            prompt: node.spec.prompt,
                        },
                        target,
                    ))
                })
            })
            .collect()
    }

    /// The command owner supplies the run ID and captures authority before this call.
    /// # Errors
    /// Rejects duplicate IDs, a full store, or a failed durable commit.
    pub fn create(
        &self,
        id: String,
        context: OrchestrationContext,
        plan: &OrchestrationPlan,
    ) -> Result<OrchestrationRun, String> {
        values::validate_id(&id)?;
        self.update(|runs| {
            if runs.len() >= MAX_SAVED_RUNS || runs.iter().any(|run| run.id == id) {
                return Err("Run ID already exists or orchestration storage is full".to_owned());
            }
            let run = OrchestrationRun {
                id,
                generation: 1,
                context,
                cancelled: false,
                nodes: plan
                    .nodes()
                    .iter()
                    .map(|spec| OrchestrationNode {
                        spec: spec.clone(),
                        attempt: 0,
                        state: OrchestrationNodeState::Pending,
                        native_turn_id: None,
                    })
                    .collect(),
            };
            runs.push(run.clone());
            Ok(run)
        })
    }

    /// Claim only dependencies whose trusted owner has reported success. Tokens publish after commit.
    /// # Errors
    /// Rejects missing/cancelled runs, invalid limits, exhausted attempts or failed commits.
    pub fn claim_ready(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<OrchestrationDispatch>, String> {
        if limit == 0 || limit > MAX_RUN_NODES {
            return Err("Dispatch limit requires 1 to 32 nodes".to_owned());
        }
        self.update(|runs| {
            let run = find_run(runs, run_id)?;
            if run.cancelled {
                return Err("Cancelled runs require explicit retry or restart".to_owned());
            }
            let ready: Vec<String> = run
                .ready_nodes()
                .into_iter()
                .take(limit)
                .map(|node| node.spec.id.clone())
                .collect();
            let mut dispatches = Vec::new();
            for node in &mut run.nodes {
                if ready.contains(&node.spec.id) {
                    node.attempt = next_generation(node.attempt)?;
                    node.state = OrchestrationNodeState::Claimed;
                    dispatches.push(OrchestrationDispatch {
                        title: node.spec.title.clone(),
                        token: OrchestrationToken {
                            run_id: run.id.clone(),
                            generation: run.generation,
                            node_id: node.spec.id.clone(),
                            attempt: node.attempt,
                        },
                        context: run.context.clone(),
                        launch: node.spec.launch.clone(),
                        prompt: node.spec.prompt.clone(),
                    });
                }
            }
            Ok(dispatches)
        })
    }

    /// Accept the actual host-issued terminal after the shared launch command succeeds.
    /// # Errors
    /// Rejects stale tokens, wrong Bindings, duplicate launches, invalid targets or failed commits.
    pub fn accept_terminal(
        &self,
        token: &OrchestrationToken,
        binding: &CommandTarget,
        terminal: CommandTarget,
    ) -> Result<(), String> {
        self.accept_target(token, binding, terminal, ResourceKind::Terminal, None)
    }

    /// Accept the exact native conversation returned by the shared launch command.
    /// # Errors
    /// Rejects stale tokens, another Binding, duplicate targets or a failed commit.
    pub fn accept_native(
        &self,
        token: &OrchestrationToken,
        binding: &CommandTarget,
        target: CommandTarget,
        first_turn: &NativeTurnReceipt,
    ) -> Result<(), String> {
        values::bounded_text(&first_turn.id, 8192, "Accepted native turn")?;
        self.accept_target(
            token,
            binding,
            target,
            ResourceKind::Session,
            Some(first_turn.id.clone()),
        )
    }

    fn accept_target(
        &self,
        token: &OrchestrationToken,
        binding: &CommandTarget,
        terminal: CommandTarget,
        kind: ResourceKind,
        native_turn_id: Option<String>,
    ) -> Result<(), String> {
        values::validate_target(&terminal, kind)?;
        self.update(|runs| {
            if runs
                .iter()
                .flat_map(|run| &run.nodes)
                .any(|node| node.state.target() == Some(&terminal))
            {
                return Err("Target already belongs to another orchestration attempt".to_owned());
            }
            let run = find_run(runs, token.run_id())?;
            if run.context.binding() != binding {
                return Err("Dispatched terminal belongs to another Binding".to_owned());
            }
            let node = find_attempt(run, token)?;
            if node.spec.launch.target_kind() != kind {
                return Err("Target does not match the captured work destination".to_owned());
            }
            if node.state != OrchestrationNodeState::Claimed {
                return Err("Only a claimed attempt can accept a terminal".to_owned());
            }
            node.state = OrchestrationNodeState::Running { target: terminal };
            node.native_turn_id = native_turn_id;
            Ok(())
        })
    }

    /// Apply only a trusted completed turn/error/exit for the exact accepted terminal.
    /// # Errors
    /// Rejects stale/duplicate results, another terminal or a failed durable commit.
    pub fn finish(
        &self,
        token: &OrchestrationToken,
        terminal: &CommandTarget,
        outcome: OrchestrationOutcome,
    ) -> Result<(), String> {
        values::validate_target(terminal, ResourceKind::Terminal)?;
        if let OrchestrationOutcome::Failed { message } = &outcome {
            values::bounded_text(message, 4096, "Observed failure")?;
        }
        self.update(|runs| {
            let node = find_attempt(find_run(runs, token.run_id())?, token)?;
            if !matches!(&node.state, OrchestrationNodeState::Running { target } if target == terminal) {
                return Err("Result requires the exact running terminal and attempt".to_owned());
            }
            node.state = match outcome {
                OrchestrationOutcome::Succeeded => OrchestrationNodeState::Succeeded {
                    target: terminal.clone(),
                },
                OrchestrationOutcome::Failed { message } => OrchestrationNodeState::Failed {
                    target: Some(terminal.clone()),
                    message,
                },
            };
            Ok(())
        })
    }

    /// Observe only the accepted first turn of the exact native process generation.
    /// # Errors
    /// Rejects stale attempts, another document/turn/target, or failed durable publication.
    pub fn finish_native(
        &self,
        token: &OrchestrationToken,
        target: &CommandTarget,
        receipt: &NativeTurnReceipt,
    ) -> Result<(), String> {
        values::validate_target(target, ResourceKind::Session)?;
        values::bounded_text(&receipt.id, 8192, "Observed native turn")?;
        self.update(|runs| {
            let node = find_attempt(find_run(runs, token.run_id())?, token)?;
            if node.native_turn_id.as_deref() != Some(receipt.id.as_str())
                || !matches!(&node.state, OrchestrationNodeState::Running { target: accepted } if accepted == target)
            {
                return Err("Native result requires the exact accepted first turn and generation".to_owned());
            }
            node.state = match receipt.outcome {
                NativeTurnOutcome::Running => return Ok(()),
                NativeTurnOutcome::Succeeded => OrchestrationNodeState::Succeeded {
                    target: target.clone(),
                },
                NativeTurnOutcome::Failed | NativeTurnOutcome::Interrupted => {
                    OrchestrationNodeState::Failed {
                        target: Some(target.clone()),
                        message: match receipt.outcome {
                            NativeTurnOutcome::Interrupted => "The accepted provider turn was interrupted",
                            _ => "The accepted provider turn failed",
                        }.to_owned(),
                    }
                }
            };
            Ok(())
        })
    }

    /// Retain a tab that was actually created when its first prompt could not be tracked.
    /// # Errors
    /// Rejects another Binding, a stale claim, invalid target, or failed durable commit.
    pub fn reject_created_native(
        &self,
        token: &OrchestrationToken,
        binding: &CommandTarget,
        target: CommandTarget,
        message: String,
    ) -> Result<(), String> {
        values::validate_target(&target, ResourceKind::Session)?;
        values::bounded_text(&message, 4096, "Observed failure")?;
        self.update(|runs| {
            if runs
                .iter()
                .flat_map(|run| &run.nodes)
                .any(|node| node.state.target() == Some(&target))
            {
                return Err("Target already belongs to another orchestration attempt".to_owned());
            }
            let run = find_run(runs, token.run_id())?;
            if run.context.binding() != binding {
                return Err("Created native conversation belongs to another Binding".to_owned());
            }
            let node = find_attempt(run, token)?;
            if node.state != OrchestrationNodeState::Claimed
                || node.spec.launch.target_kind() != ResourceKind::Session
            {
                return Err(
                    "Only the exact claimed native attempt can retain a failed tab".to_owned(),
                );
            }
            node.state = OrchestrationNodeState::Failed {
                target: Some(target),
                message,
            };
            Ok(())
        })
    }

    /// Report a failed host launch without claiming that a terminal ever started.
    /// # Errors
    /// Rejects stale/non-claimed attempts, oversized failure summaries or failed commits.
    pub fn reject_dispatch(
        &self,
        token: &OrchestrationToken,
        message: String,
    ) -> Result<(), String> {
        values::bounded_text(&message, 4096, "Observed failure")?;
        self.update(|runs| {
            let node = find_attempt(find_run(runs, token.run_id())?, token)?;
            if node.state != OrchestrationNodeState::Claimed {
                return Err("Only a claimed attempt can report launch failure".to_owned());
            }
            node.state = OrchestrationNodeState::Failed {
                target: None,
                message,
            };
            Ok(())
        })
    }

    /// Persist cancellation before returning the exact live targets for the command owner to stop.
    /// # Errors
    /// Rejects unknown/already-cancelled runs or a failed durable commit.
    pub fn cancel(&self, run_id: &str) -> Result<Vec<CommandTarget>, String> {
        self.update(|runs| {
            let run = find_run(runs, run_id)?;
            if run.cancelled {
                return Err("Run was already cancelled".to_owned());
            }
            run.generation = next_generation(run.generation)?;
            run.cancelled = true;
            let mut targets = Vec::new();
            for node in &mut run.nodes {
                if matches!(
                    node.state,
                    OrchestrationNodeState::Pending
                        | OrchestrationNodeState::Claimed
                        | OrchestrationNodeState::Running { .. }
                ) {
                    let target = node.state.target().cloned();
                    targets.extend(target.iter().cloned());
                    node.state = OrchestrationNodeState::Cancelled { target };
                }
            }
            Ok(targets)
        })
    }

    /// Retry one failed, interrupted or cancelled node; claiming it issues a new attempt.
    /// # Errors
    /// Rejects unknown nodes, successful/live work or a failed durable commit.
    pub fn retry(&self, run_id: &str, node_id: &str) -> Result<(), String> {
        self.update(|runs| {
            let run = find_run(runs, run_id)?;
            let node = run
                .nodes
                .iter_mut()
                .find(|node| node.spec.id == node_id)
                .ok_or("Unknown orchestration node")?;
            if !matches!(
                node.state,
                OrchestrationNodeState::Failed { .. }
                    | OrchestrationNodeState::Interrupted { .. }
                    | OrchestrationNodeState::Cancelled { .. }
            ) {
                return Err("Only failed, interrupted or cancelled work can be retried".to_owned());
            }
            node.state = OrchestrationNodeState::Pending;
            node.native_turn_id = None;
            run.cancelled = false;
            Ok(())
        })
    }

    /// Explicitly restart unfinished work. Observed successes stay complete; live work must cancel first.
    /// # Errors
    /// Rejects unknown/live runs, exhausted generations or a failed durable commit.
    pub fn restart(&self, run_id: &str) -> Result<(), String> {
        self.update(|runs| {
            let run = find_run(runs, run_id)?;
            if run.nodes.iter().any(|node| {
                matches!(
                    node.state,
                    OrchestrationNodeState::Claimed | OrchestrationNodeState::Running { .. }
                )
            }) {
                return Err("Cancel in-flight work before restarting the run".to_owned());
            }
            run.generation = next_generation(run.generation)?;
            run.cancelled = false;
            for node in &mut run.nodes {
                if !matches!(node.state, OrchestrationNodeState::Succeeded { .. }) {
                    node.state = OrchestrationNodeState::Pending;
                    node.native_turn_id = None;
                }
            }
            Ok(())
        })
    }

    /// Explicitly reauthorize the same durable host destination after recovery.
    /// # Errors
    /// Rejects stale generations, changed caller/destination, live work or failed commits.
    pub fn restart_on_binding(
        &self,
        run_id: &str,
        expected_generation: u64,
        context: OrchestrationContext,
    ) -> Result<(), String> {
        self.update(|runs| {
            let run = find_run(runs, run_id)?;
            if run.generation != expected_generation
                || run.context.caller() != context.caller()
                || run.context.destination().is_none()
                || run.context.destination() != context.destination()
            {
                return Err(
                    "Restart requires the original caller and exact durable host destination"
                        .to_owned(),
                );
            }
            if run.nodes.iter().any(|node| {
                matches!(
                    node.state,
                    OrchestrationNodeState::Claimed | OrchestrationNodeState::Running { .. }
                )
            }) {
                return Err("Cancel in-flight work before restarting the run".to_owned());
            }
            run.generation = next_generation(run.generation)?;
            run.context = context;
            run.cancelled = false;
            for node in &mut run.nodes {
                if !matches!(node.state, OrchestrationNodeState::Succeeded { .. }) {
                    node.state = OrchestrationNodeState::Pending;
                    node.native_turn_id = None;
                }
            }
            Ok(())
        })
    }

    fn update<T>(
        &self,
        apply: impl FnOnce(&mut Vec<OrchestrationRun>) -> Result<T, String>,
    ) -> Result<T, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(PoisonError::into_inner);
        let published = Arc::clone(&self.snapshot.lock().unwrap_or_else(PoisonError::into_inner));
        let mut candidate = published.as_ref().clone();
        let result = apply(&mut candidate.runs)?;
        if candidate.runs == published.runs {
            return Ok(result);
        }
        candidate.revision = next_generation(candidate.revision)?;
        candidate.durability_warning = store::commit(&self.path, &candidate)?;
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        for (token, cancellation) in pending.iter() {
            let current = candidate.runs.iter().any(|run| {
                run.id == token.run_id()
                    && run.generation == token.generation()
                    && !run.cancelled
                    && run.nodes.iter().any(|node| {
                        node.spec.id == token.node_id()
                            && node.attempt == token.attempt()
                            && matches!(
                                node.state,
                                OrchestrationNodeState::Claimed
                                    | OrchestrationNodeState::Running { .. }
                            )
                    })
            });
            if !current {
                _ = cancellation.cancel();
            }
        }
        drop(pending);
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(candidate);
        Ok(result)
    }
}

impl Drop for OrchestrationService {
    fn drop(&mut self) {
        for (_, cancellation) in self
            .pending
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
        {
            _ = cancellation.cancel();
        }
    }
}

fn next_generation(current: u64) -> Result<u64, String> {
    current
        .checked_add(1)
        .ok_or_else(|| "Orchestration generation is exhausted".to_owned())
}

fn find_run<'a>(
    runs: &'a mut [OrchestrationRun],
    id: &str,
) -> Result<&'a mut OrchestrationRun, String> {
    runs.iter_mut()
        .find(|run| run.id == id)
        .ok_or_else(|| "Unknown orchestration run".to_owned())
}

fn find_attempt<'a>(
    run: &'a mut OrchestrationRun,
    token: &OrchestrationToken,
) -> Result<&'a mut OrchestrationNode, String> {
    if run.generation != token.generation() || run.cancelled {
        return Err("Stale orchestration run generation".to_owned());
    }
    run.nodes
        .iter_mut()
        .find(|node| node.spec.id == token.node_id() && node.attempt == token.attempt())
        .ok_or_else(|| "Stale orchestration node attempt".to_owned())
}
