use std::time::Instant;

use bootty_control::{CommandCancellation, CommandOutcome, ResourceKind};
use bootty_mux::{controller::SpaceId, executor, session_lifecycle::TaskLifecycle};

use super::{CommandDispatch, command_outcome_for_mux_error};
use crate::{
    commands::{ExactMuxTarget, TaskAction},
    state::{AppEffect, AppState},
};

impl AppState {
    pub(super) fn dispatch_task(
        &mut self,
        action: TaskAction,
        arguments: &[String],
        target: Option<&ExactMuxTarget>,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(ExactMuxTarget::Binding(scope)) = target else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "The task command needs a current Space binding target".to_owned(),
            });
        };
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return self.reject_command(command_outcome_for_mux_error(error));
        }
        let Some(binding) = self.workspace.binding(*scope) else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "The target Space binding is no longer live".to_owned(),
            });
        };
        let outcome = match action {
            TaskAction::Show => return self.show_saved_tasks_command(*scope, effects),
            TaskAction::List => CommandOutcome::Success {
                value: binding
                    .sessions()
                    .sessions()
                    .iter()
                    .map(|task| {
                        serde_json::json!({
                            "identity": task.identity,
                            "title": task.label(),
                            "cwd": task.cwd,
                            "lifecycle": binding.sessions().task_lifecycle(&task.identity).map(lifecycle_name),
                            "attached_observed": binding.task_attachment_observed(&task.identity),
                        })
                    })
                    .collect::<Vec<_>>()
                    .into(),
                warnings: Vec::new(),
            },
            TaskAction::Set => {
                let [identity, state] = arguments else {
                    return self.reject_command(invalid_arguments());
                };
                let lifecycle = match state.as_str() {
                    "active" => TaskLifecycle::Active,
                    "settled" => TaskLifecycle::Settled,
                    "archived" => TaskLifecycle::Archived,
                    _ => return self.reject_command(invalid_arguments()),
                };
                if !binding.sessions().contains(identity) {
                    return self.reject_command(CommandOutcome::StaleTarget {
                        message: "The target Space does not hold this session identity".to_owned(),
                    });
                }
                match self
                    .workspace
                    .set_session_lifecycle(*scope, identity, lifecycle)
                {
                    Ok(changed) => CommandOutcome::Success {
                        value: serde_json::json!({"identity": identity, "lifecycle": state, "changed": changed}),
                        warnings: Vec::new(),
                    },
                    Err(error) => CommandOutcome::Failed {
                        code: "persistence_failed".to_owned(),
                        message: error.to_string(),
                    },
                }
            }
        };
        CommandDispatch::Complete(outcome)
    }

    fn show_saved_tasks_command(
        &mut self,
        scope: SpaceId,
        effects: &mut Vec<AppEffect>,
    ) -> CommandDispatch {
        let Some(space) = self
            .workspace
            .spaces()
            .find(|space| space.binding.scope() == scope)
        else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "The target Space binding is no longer live".to_owned(),
            });
        };
        let mux = space.binding.mux();
        let handle = self.binding_target_handle(scope, mux.binding_generation());
        let Some(target) =
            ExactMuxTarget::Binding(scope).command_target(ResourceKind::Binding, mux, &handle)
        else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "The target Space binding is no longer live".to_owned(),
            });
        };
        effects.push(AppEffect::OpenSavedTasks {
            title: space.name.clone(),
            target,
        });
        CommandDispatch::Complete(CommandOutcome::Success {
            value: serde_json::json!({"opened": true}),
            warnings: Vec::new(),
        })
    }
}

const fn lifecycle_name(lifecycle: TaskLifecycle) -> &'static str {
    match lifecycle {
        TaskLifecycle::Active => "active",
        TaskLifecycle::Settled => "settled",
        TaskLifecycle::Archived => "archived",
    }
}

fn invalid_arguments() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: "session.task.set expects an identity and active, settled or archived".to_owned(),
    }
}
