use super::CommandDispatch;
use crate::{AppState, recovery::fingerprint};
use bootty_control::{CommandCancellation, CommandInvocation, CommandOutcome, ResourceKind};
use bootty_mux::executor;
use std::{path::Path, time::Instant};
impl AppState {
    pub(super) fn dispatch_recovery(
        &mut self,
        invocation: &CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        let action = invocation.command.as_str();
        let args = invocation.arguments.as_slice();
        if matches!(action, "recovery.resume" | "recovery.fork") {
            let Some(id) = args.first() else {
                return CommandDispatch::Complete(CommandOutcome::Failed {
                    code: "invalid_arguments".to_owned(),
                    message: "An archive ID is required".to_owned(),
                });
            };
            return self.relaunch_archive(
                id,
                action.ends_with("fork"),
                invocation.caller,
                deadline,
                cancellation,
            );
        }
        let store = self.recovery_store();
        let args = args.to_vec();
        let action = action.to_owned();
        self.dispatch_committed_command(Some((deadline, cancellation)), move || {
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
        })
    }
    fn relaunch_archive(
        &mut self,
        id: &str,
        fork: bool,
        caller: bootty_control::Caller,
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
        else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "The archive's original session is not open".into(),
            });
        };
        let Some(target) =
            self.mux_resource_target(binding.scope(), ResourceKind::Session, &session.id, None)
        else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "The recovery session is no longer addressable".into(),
            });
        };
        let exact = match self.resolve_command_target(
            "terminal.create_tab",
            Some(ResourceKind::Session),
            Some(&target),
        ) {
            Ok((_, Some(exact))) => exact,
            Ok(_) => {
                return CommandDispatch::Complete(CommandOutcome::Unavailable {
                    message: "The original recovery destination is unavailable".to_owned(),
                });
            }
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        let mut launch = agent.launch;
        if launch.account_directory.is_none() {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "This archive has no captured native account directory".to_owned(),
            });
        }
        match launch.session_arguments(agent.provider, &agent.session, fork) {
            Ok(arguments) => launch.arguments = arguments,
            Err(error) => {
                return CommandDispatch::Complete(CommandOutcome::Unavailable { message: error });
            }
        }
        let operation = if fork { "fork" } else { "resume" };
        let mut invocation = CommandInvocation::new(
            format!("agents.{}.{operation}", agent.provider),
            vec![agent.session],
            caller,
        );
        invocation.target = Some(target);
        self.dispatch_captured_terminal_agent(
            invocation,
            &exact,
            Some((deadline, cancellation)),
            launch,
        )
    }
}
