use super::{CommandDispatch, PendingCommandResult};
use crate::AppState;
use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_mux::{executor, target::ExactMuxTarget};
use std::{sync::mpsc, time::Instant};

impl AppState {
    fn dispatch_history_search(
        &self,
        arguments: &[String],
        remote: Option<bootty_host::remote::RemoteHost>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(spec) = arguments.first() else {
            return shell_failure("A history search specification is required".to_owned());
        };
        let request = match serde_json::from_str::<bootty_host::shell_history::HistoryRequest>(spec)
        {
            Ok(request) => request,
            Err(error) => return shell_failure(error.to_string()),
        };
        let mode = arguments
            .get(1)
            .cloned()
            .unwrap_or_else(|| "local".to_owned());
        let (deadline, cancellation) = executor::command_execution(execution);
        let (sender, receiver) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let result = executor::begin_synchronous_command(Some((deadline, cancellation)))
                .map_err(|error| anyhow::anyhow!("History read stopped: {error:?}"))
                .and_then(|()| search_history(request, remote, &mode, deadline));
            let outcome = match result {
                Ok(value) => CommandOutcome::Success {
                    value,
                    warnings: Vec::new(),
                },
                Err(error) => CommandOutcome::Failed {
                    code: "history_failed".to_owned(),
                    message: format!("{error:#}"),
                },
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }

    pub(super) fn dispatch_shell_prompt(
        &mut self,
        exact: Option<&ExactMuxTarget>,
        action: &'static str,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(exact) = exact else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "No terminal prompt is available".to_owned(),
            });
        };
        self.dispatch_attached_shell_prompt(exact, action, arguments, execution)
    }

    fn dispatch_attached_shell_prompt(
        &mut self,
        exact: &ExactMuxTarget,
        action: &'static str,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let text = match prompt_edit(action, arguments) {
            Ok(text) => text,
            Err(message) => return shell_failure(message.to_owned()),
        };
        let Some(binding) = self.workspace.binding_mut(exact.scope()) else {
            return shell_failure("Shell host closed".to_owned());
        };
        if action == "history.search" {
            let remote = binding
                .multiplexer()
                .remote
                .clone()
                .map(bootty_host::remote::RemoteHost::new);
            return self.dispatch_history_search(arguments, remote, execution);
        }
        // Client attachments and shared mux panes cannot promise exclusive input ownership.
        if binding.multiplexer().backend != bootty_config::config::MultiplexerBackendConfig::Native
        {
            return CommandDispatch::Complete(CommandOutcome::Unsupported { message: "Prompt editing requires a native pane with Bootty shell hooks; shared mux input cannot be leased exclusively".to_owned() });
        }
        let remote = binding
            .multiplexer()
            .remote
            .clone()
            .map(bootty_host::remote::RemoteHost::new);
        let Some(terminal) = exact
            .ids()
            .2
            .and_then(|pane| binding.terminal_mut().focused_terminal_runtime(pane))
        else {
            return shell_failure("Shell pane is no longer attached".to_owned());
        };
        let (deadline, cancellation) = executor::command_execution(execution);
        if let Err(error) = executor::begin_synchronous_command(Some((deadline, cancellation))) {
            return CommandDispatch::Complete(super::command_outcome_for_mux_error(error));
        }
        let pending = match terminal.prompt(text) {
            Ok(pending) => pending,
            Err(error) => return shell_failure(error.to_string()),
        };
        let query = arguments.first().cloned().unwrap_or_default();
        let mode = arguments
            .get(1)
            .cloned()
            .unwrap_or_else(|| "local".to_owned());
        let (sender, receiver) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let result = pending
                .receive("reading shell prompt")
                .and_then(|prompt| prompt.map_err(anyhow::Error::msg))
                .and_then(|mut prompt| {
                    prompt.cwd =
                        bootty_mux::workspace::terminal_cwd_for_mux_command(Some(prompt.cwd), None)
                            .unwrap_or_default();
                    for record in &mut prompt.recent {
                        record.cwd = bootty_mux::workspace::terminal_cwd_for_mux_command(
                            Some(record.cwd.clone()),
                            None,
                        )
                        .unwrap_or_default();
                    }
                    if action != "shell.history" {
                        return Ok(serde_json::to_value(prompt)?);
                    }
                    let request = bootty_host::shell_history::HistoryRequest {
                        shell: prompt.shell.clone(),
                        path: prompt.history_file.clone(),
                        query,
                        cwd: prompt.cwd.clone(),
                        recent: prompt
                            .recent
                            .iter()
                            .map(|record| bootty_host::shell_history::HistoryEntry {
                                command: record.command.clone(),
                                cwd: Some(record.cwd.clone()),
                                timestamp: Some(record.timestamp),
                                exit_code: record.exit_code,
                                frequency: 1,
                            })
                            .collect(),
                    };
                    let history = search_history(request, remote, &mode, deadline)?;
                    Ok(serde_json::json!({"prompt":prompt,"history":history}))
                });
            let outcome = match result {
                Ok(value) => CommandOutcome::Success {
                    value,
                    warnings: Vec::new(),
                },
                Err(error) => CommandOutcome::Failed {
                    code: "shell_prompt_failed".to_owned(),
                    message: format!("{error:#}"),
                },
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }
}

fn search_history(
    mut request: bootty_host::shell_history::HistoryRequest,
    remote: Option<bootty_host::remote::RemoteHost>,
    mode: &str,
    deadline: Instant,
) -> anyhow::Result<serde_json::Value> {
    use anyhow::Context as _;
    use bootty_host::semantic_history;

    let semantic = match mode {
        "local" => None,
        "semantic" => {
            if request.query.trim().is_empty() || request.query.len() > 4096 {
                anyhow::bail!("Semantic history requires a nonempty query of at most 4096 bytes");
            }
            let key = std::env::var("TYPESAFE_API_KEY").context(
                "Semantic history requires TYPESAFE_API_KEY in the desktop process environment",
            )?;
            // Retrieve a broad candidate set on its owning host; credentials stay on the desktop.
            Some((std::mem::take(&mut request.query), key))
        }
        _ => anyhow::bail!("History mode must be local or semantic"),
    };
    let history = remote.map_or_else(
        || request.execute(),
        |remote| request.execute_remote(&remote, bootty_host::SystemCommandRunner),
    )?;
    if let Some((query, key)) = semantic {
        let history = semantic_history::rerank(history, &query, |body| {
            semantic_history::evaluate(
                body,
                &key,
                deadline.saturating_duration_since(Instant::now()),
            )
        })?;
        Ok(serde_json::to_value(history)?)
    } else {
        Ok(serde_json::to_value(history)?)
    }
}

fn prompt_edit(
    action: &str,
    arguments: &[String],
) -> Result<Option<(u64, String, bool)>, &'static str> {
    if action != "shell.apply" {
        return Ok(None);
    }
    let [revision, text, submit] = arguments else {
        return Err("Expected revision, text, and submit arguments");
    };
    Ok(Some((
        revision.parse().map_err(|_| "Expected a prompt revision")?,
        text.clone(),
        submit.parse().map_err(|_| "Submit must be true or false")?,
    )))
}

fn shell_failure(message: String) -> CommandDispatch {
    CommandDispatch::Complete(CommandOutcome::Failed {
        code: "shell_prompt_failed".to_owned(),
        message,
    })
}
