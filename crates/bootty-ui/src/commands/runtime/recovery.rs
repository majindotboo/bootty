use super::CommandDispatch;
use crate::AppState;
use bootty_control::{CommandCancellation, CommandInvocation, CommandOutcome};
use bootty_mux::executor;
use std::{path::Path, time::Instant};
impl AppState {
    pub(super) fn dispatch_recovery(
        &self,
        invocation: &CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        let store = self.recovery_store();
        let args = invocation.arguments.clone();
        let action = invocation.command.clone();
        self.dispatch_committed_command(Some((deadline, cancellation)), move || {
            let result = (|| -> anyhow::Result<serde_json::Value> {
                let arg = |index: usize| {
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("Missing recovery argument {index}"))
                };
                Ok(match action.as_str() {
                    "recovery.list" => {
                        let listing = store.list()?;
                        serde_json::json!({ "entries": listing.entries.into_iter().map(|archive| serde_json::json!({"id":archive.id,"title":archive.title,"host":archive.host,"saved_at_ms":archive.saved_at_ms,"bytes":archive.text.len(),"omitted_lines":archive.omitted_lines})).collect::<Vec<_>>(), "warnings":listing.warnings })
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
}
