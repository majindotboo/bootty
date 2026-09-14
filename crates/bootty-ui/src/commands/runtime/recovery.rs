use super::{CommandDispatch, PendingCommandResult, command_outcome_for_mux_error};
use crate::{AppState, recovery::fingerprint};
use bootty_agents::LaunchShell;
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, ResourceKind,
};
use bootty_mux::executor;
use std::{path::Path, sync::mpsc, time::Instant};
impl AppState {
    pub(super) fn dispatch_recovery(
        &self,
        action: &str,
        args: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        if matches!(action, "recovery.resume" | "recovery.fork") {
            let Some(id) = args.first() else {
                return CommandDispatch::Complete(CommandOutcome::Failed {
                    code: "invalid_arguments".to_owned(),
                    message: "An archive ID is required".to_owned(),
                });
            };
            return self.relaunch_archive(id, action.ends_with("fork"), deadline, cancellation);
        }
        let store = self.recovery_store();
        let args = args.to_vec();
        let action = action.to_owned();
        let repaint = self.repaint.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = if let Err(e) =
                executor::begin_synchronous_command(Some((deadline, cancellation)))
            {
                command_outcome_for_mux_error(e)
            } else {
                let result = (|| -> anyhow::Result<serde_json::Value> {
                    let arg = |index: usize| {
                        args.get(index)
                            .ok_or_else(|| anyhow::anyhow!("Missing recovery argument {index}"))
                    };
                    Ok(match action.as_str() {
                        "recovery.list" => {
                            let listing = store.list()?;
                            serde_json::json!({ "entries": listing.entries.into_iter().map(|archive| serde_json::json!({"id":archive.id,"title":archive.title,"host":archive.host,"saved_at_ms":archive.saved_at_ms,"bytes":archive.text.len(),"omitted_lines":archive.omitted_lines,"resumable":archive.agent.is_some()})).collect::<Vec<_>>(), "warnings":listing.warnings })
                        }
                        "recovery.get" => serde_json::to_value(store.get(arg(0)?)?)?,
                        "recovery.export" => {
                            store.export(arg(0)?, Path::new(arg(1)?))?;
                            serde_json::json!({"exported":arg(1)?})
                        }
                        "recovery.delete" => {
                            store.delete(arg(0)?)?;
                            serde_json::json!({"deleted":arg(0)?})
                        }
                        _ => anyhow::bail!("unknown recovery action"),
                    })
                })();
                match result {
                    Ok(value) => CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    },
                    Err(e) => CommandOutcome::Failed {
                        code: "recovery_failed".into(),
                        message: format!("{e:#}"),
                    },
                }
            };
            let _ = tx.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(rx))
    }
    fn relaunch_archive(
        &self,
        id: &str,
        fork: bool,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> CommandDispatch {
        let Some(archive) = self.recovery_archive(id) else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "Previous-session archive is no longer available".into(),
            });
        };
        let Some(agent) = archive.agent else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "This archive has no reusable agent session".into(),
            });
        };
        let binding = self.workspace.all_bindings().find(|b| {
            b.scope().persistence_value().to_string() == archive.scope && {
                let actual = b
                    .multiplexer()
                    .remote
                    .as_ref()
                    .and_then(|r| serde_json::to_vec(r).ok())
                    .map_or_else(|| "local".into(), |v| fingerprint(&v));
                actual == archive.host_fingerprint
            }
        });
        let Some(binding) = binding else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "The archive's original host is not open".into(),
            });
        };
        let Some(session) = binding
            .mux()
            .all_sessions()
            .iter()
            .find(|s| s.id == archive.session)
            .or_else(|| binding.mux().all_sessions().first())
        else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "The original host has no session for a recovered tab".into(),
            });
        };
        let Some(target) =
            self.mux_resource_target(binding.scope(), ResourceKind::Session, &session.id, None)
        else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "The recovery session is no longer addressable".into(),
            });
        };
        let shell = if cfg!(windows) && binding.multiplexer().remote.is_none() {
            LaunchShell::Windows
        } else {
            LaunchShell::Posix
        };
        let mut launch = agent.launch;
        match launch.session_arguments(agent.provider, &agent.session, fork) {
            Ok(arguments) => launch.arguments = arguments,
            Err(e) => return CommandDispatch::Complete(CommandOutcome::Unavailable { message: e }),
        }
        let command = match launch.shell_command(agent.provider, shell) {
            Ok(command) => command,
            Err(e) => {
                return CommandDispatch::Complete(CommandOutcome::Failed {
                    code: "invalid_recovery".into(),
                    message: e,
                });
            }
        };
        self.start_recovered_tab(target, command, id.to_owned(), deadline, cancellation)
    }

    fn start_recovered_tab(
        &self,
        target: bootty_control::CommandTarget,
        command: String,
        archive_id: String,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> CommandDispatch {
        let sender = self.app_command_sender(Caller::Internal);
        let repaint = self.repaint.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let run = || -> Result<serde_json::Value, CommandOutcome> {
                let submit =
                    |invocation: CommandInvocation| -> Result<CommandOutcome, CommandOutcome> {
                        let response = sender
                            .submit(invocation, deadline, cancellation.clone())
                            .map_err(|e| CommandOutcome::Failed {
                                code: "recovery_mailbox".into(),
                                message: format!("recovery mailbox: {e:?}"),
                            })?;
                        response
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                            .map_err(|_| CommandOutcome::deadline_exceeded())
                    };
                let mut tab = CommandInvocation::from_action("new_tab", Caller::Internal);
                tab.target = Some(target);
                let outcome = submit(tab)?;
                let CommandOutcome::Success { value, .. } = outcome else {
                    return Err(outcome);
                };
                let created: bootty_control::CommandTarget =
                    serde_json::from_value(value.get("created").cloned().unwrap_or_default())
                        .map_err(|e| CommandOutcome::Failed {
                            code: "recovery_target".into(),
                            message: e.to_string(),
                        })?;
                let mut paste = CommandInvocation::from_action("terminal.paste", Caller::Internal);
                paste.target = Some(created.clone());
                paste.arguments = vec![command];
                let outcome = submit(paste)?;
                if !matches!(outcome, CommandOutcome::Success { .. }) {
                    return Err(outcome);
                }
                let mut enter = CommandInvocation::from_action("terminal.submit", Caller::Internal);
                enter.target = Some(created.clone());
                let outcome = submit(enter)?;
                if !matches!(outcome, CommandOutcome::Success { .. }) {
                    return Err(outcome);
                }
                Ok(serde_json::json!({"started":true,"target":created,"archive":archive_id}))
            };
            let outcome = match run() {
                Ok(value) => CommandOutcome::Success {
                    value,
                    warnings: Vec::new(),
                },
                Err(outcome) => outcome,
            };
            let _ = tx.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(rx))
    }
}
