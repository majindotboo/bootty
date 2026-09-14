use super::{CommandDispatch, PendingCommandResult};
use crate::{AppState, commands::JobAction};
use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_mux::{controller::SpaceId, executor};
use std::{sync::mpsc, time::Instant};
impl AppState {
    pub(super) fn dispatch_job_command(
        &self,
        action: JobAction,
        arguments: Vec<String>,
        scope: Option<SpaceId>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let remote = scope
            .and_then(|scope| self.workspace.binding(scope))
            .and_then(|binding| binding.multiplexer().remote.clone())
            .map(bootty_host::remote::RemoteHost::new);
        let jobs = self.commands.jobs.clone();
        let repaint = self.repaint.clone();
        let (deadline, cancellation) = executor::command_execution(execution);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = if let Err(error) =
                executor::begin_synchronous_command(Some((deadline, cancellation)))
            {
                super::command_outcome_for_mux_error(error)
            } else {
                let result = (|| -> anyhow::Result<serde_json::Value> {
                    let arg = |index: usize| {
                        arguments
                            .get(index)
                            .ok_or_else(|| anyhow::anyhow!("Missing job argument {index}"))
                    };
                    Ok(match action {
                        JobAction::Transfer => serde_json::to_value(jobs.start_transfer(
                            serde_json::from_str(arg(0)?)?,
                            remote,
                            repaint.clone(),
                        )?)?,
                        JobAction::RetryTransfer => {
                            serde_json::to_value(jobs.retry_transfer(arg(0)?, repaint.clone())?)?
                        }
                        JobAction::Start => serde_json::to_value(jobs.start(
                            serde_json::from_str(arg(0)?)?,
                            remote,
                            repaint.clone(),
                        )?)?,
                        JobAction::List => serde_json::to_value(jobs.list())?,
                        JobAction::Read => serde_json::to_value(jobs.read(
                            arg(0)?,
                            arg(1)?.parse()?,
                            arg(2)?.parse()?,
                        )?)?,
                        JobAction::Cancel => serde_json::to_value(jobs.cancel(arg(0)?)?)?,
                        JobAction::Forget => {
                            jobs.forget(arg(0)?)?;
                            serde_json::json!({"forgotten":arg(0)?})
                        }
                    })
                })();
                match result {
                    Ok(value) => CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    },
                    Err(error) => CommandOutcome::Failed {
                        code: "job_failed".to_owned(),
                        message: format!("{error:#}"),
                    },
                }
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }
}

pub(super) fn registry(
    events: Option<bootty_control::ControlEventSender>,
) -> std::sync::Arc<bootty_host::jobs::JobRegistry> {
    let Some(events) = events else {
        return std::sync::Arc::new(bootty_host::jobs::JobRegistry::default());
    };
    let (changed, updates) = mpsc::sync_channel(1);
    let jobs = std::sync::Arc::new(bootty_host::jobs::JobRegistry::new(Some(changed)));
    let owner = std::sync::Arc::downgrade(&jobs);
    std::thread::spawn(move || {
        while updates.recv().is_ok() {
            let Some(jobs) = owner.upgrade().filter(|jobs| jobs.is_active()) else {
                break;
            };
            let payload = match serde_json::to_value(jobs.list()) {
                Ok(payload) => payload,
                Err(error) => {
                    eprintln!("Unable to serialize job update: {error}");
                    continue;
                }
            };
            let Some(deadline) = Instant::now().checked_add(std::time::Duration::from_secs(5))
            else {
                continue;
            };
            let _ = events.publish(
                "bootty.jobs".to_owned(),
                jobs.generation(),
                "jobs.changed".to_owned(),
                payload,
                deadline,
                &CommandCancellation::new(),
            );
        }
    });
    jobs
}

impl AppState {
    pub fn job_overview(&self) -> Vec<bootty_host::jobs::JobSummary> {
        self.commands.jobs.list()
    }
}
