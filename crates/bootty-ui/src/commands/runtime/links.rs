use super::{CommandDispatch, PendingCommandResult};
use crate::{
    AppState,
    state::{AppEffect, ViewportSnapshot},
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use bootty_host::{
    CancellableCommandRunner,
    files::{FileRequest, FileResponse},
    remote::RemoteHost,
    ssh_forward::{ForwardLease, loopback_destination},
};
use bootty_mux::{controller::SpaceId, executor, target::ExactMuxTarget};
use bootty_terminal::terminal_links::{LinkTarget, parse_location};
use std::{
    sync::{Arc, mpsc},
    time::Instant,
};

pub enum ResolvedLink {
    Url {
        url: String,
        forward: Option<Arc<ForwardLease>>,
    },
    File {
        path: String,
        directory: bool,
        line: u32,
        column: u32,
    },
}

pub(super) fn failure(message: String) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "link_open_failed".to_owned(),
        message,
    }
}

impl AppState {
    pub(crate) fn take_link_forwards(&mut self) -> Vec<Arc<ForwardLease>> {
        self.commands
            .forwards
            .drain(..)
            .map(|(_, _, forward)| forward)
            .collect()
    }

    pub(super) fn dispatch_link_open(
        &mut self,
        target: CommandTarget,
        exact: &ExactMuxTarget,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let scope = exact.scope();
        let Some(binding) = self.workspace.binding_mut(scope) else {
            return CommandDispatch::Complete(failure("Link host was closed".to_owned()));
        };
        let remote = binding.multiplexer().remote.clone().map(RemoteHost::new);
        let (session, window, pane) = exact.ids();
        let pane = pane.map(str::to_owned);
        let anchor_cwd = binding
            .mux()
            .all_sessions()
            .iter()
            .find(|candidate| Some(candidate.id.as_str()) == session)
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .find(|candidate| Some(candidate.id.as_str()) == window)
            })
            .and_then(|window| {
                window
                    .panes
                    .iter()
                    .find(|candidate| candidate.pane_id.as_deref() == pane.as_deref())
            })
            .and_then(|anchor| anchor.cwd.clone());
        let reported_cwd = pane
            .as_deref()
            .and_then(|pane| binding.terminal_mut().focused_terminal_runtime(pane))
            .and_then(|terminal| terminal.current_working_directory().ok().flatten());
        let base = bootty_mux::workspace::terminal_cwd_for_mux_command(
            arguments.get(1).cloned().or(reported_cwd),
            anchor_cwd,
        );
        let Some(location) = arguments.first().cloned() else {
            return CommandDispatch::Complete(failure("A link location is required".to_owned()));
        };
        let existing = remote
            .as_ref()
            .and_then(RemoteHost::as_ssh)
            .and_then(|remote| {
                url::Url::parse(&location).ok().and_then(|url| {
                    self.commands
                        .forwards
                        .iter()
                        .find(|(owner, _, forward)| {
                            *owner == scope && forward.matches(remote, &url)
                        })
                        .map(|(_, _, forward)| Arc::clone(forward))
                })
            });
        let (deadline, cancellation) = executor::command_execution(execution);
        let runner = CancellableCommandRunner::with_deadline_and_cancellation_check(
            bootty_host::CommandCancellation::default(),
            deadline,
            move || cancellation.is_cancelled(),
        );
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let outcome = resolve_link(&location, base, remote, existing, &runner)
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Link {
            scope,
            target,
            result,
        })
    }

    pub(super) fn finish_link_open(
        &mut self,
        scope: SpaceId,
        target: &CommandTarget,
        link: ResolvedLink,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        if let Err(outcome) =
            self.resolve_command_target("link.open", Some(ResourceKind::Terminal), Some(target))
        {
            return outcome;
        }
        let Some(binding) = self.workspace.binding(scope) else {
            return CommandOutcome::StaleTarget {
                message: "Link host was closed".to_owned(),
            };
        };
        let generation = binding.mux().binding_generation();
        match link {
            ResolvedLink::Url { url, forward } => {
                if let Some(forward) = forward
                    && !self
                        .commands
                        .forwards
                        .iter()
                        .any(|(_, _, existing)| Arc::ptr_eq(existing, &forward))
                {
                    // Keep browser connections valid for this binding's lifetime; reject overflow
                    // instead of silently disconnecting an older page.
                    if self.commands.forwards.len() >= 64 {
                        return failure(
                            "This window already has 64 active URL forwards".to_owned(),
                        );
                    }
                    self.commands.forwards.push((scope, generation, forward));
                }
                effects.push(AppEffect::OpenUrl(url.clone()));
                CommandOutcome::Success {
                    value: serde_json::json!({"url":url}),
                    warnings: Vec::new(),
                }
            }
            ResolvedLink::File {
                path,
                directory,
                line,
                column,
            } => {
                let target = CommandTarget {
                    kind: ResourceKind::Binding,
                    handle: self.binding_target_handle(scope, generation),
                    generation,
                };
                let mut invocation = CommandInvocation::new(
                    if directory {
                        "files.browse"
                    } else {
                        "files.open"
                    },
                    if directory {
                        vec![path]
                    } else {
                        vec![path, line.to_string(), column.to_string()]
                    },
                    Caller::Internal,
                );
                invocation.target = Some(target);
                self.dispatch_command(invocation, ViewportSnapshot::default(), effects)
            }
        }
    }
}

fn resolve_link(
    location: &str,
    base: Option<String>,
    remote: Option<RemoteHost>,
    existing: Option<Arc<ForwardLease>>,
    runner: &CancellableCommandRunner,
) -> anyhow::Result<ResolvedLink> {
    use anyhow::Context as _;
    let target = parse_location(location)
        .or_else(|| {
            url::Url::parse(location)
                .ok()
                .map(|_| LinkTarget::Url(location.to_owned()))
        })
        .context("not a URL or file location")?;
    let (path, line, column) = match target {
        LinkTarget::Url(value) => {
            let mut url = url::Url::parse(&value).context("invalid URL")?;
            if url.scheme() == "file" {
                if let Some(remote) = &remote
                    && url.host_str() == Some(remote.host())
                {
                    url.set_host(None)?;
                }
                (url.to_string(), None, None)
            } else {
                anyhow::ensure!(
                    !matches!(url.scheme(), "javascript" | "data"),
                    "this URL scheme cannot be opened from a terminal"
                );
                if let Some(remote) = remote
                    && loopback_destination(&url).is_some()
                {
                    let Some(ssh) = remote.as_ssh() else {
                        anyhow::ensure!(cfg!(windows), "WSL localhost links require Windows");
                        // Windows owns WSL localhost forwarding; never invent an SSH tunnel.
                        return Ok(ResolvedLink::Url {
                            url: url.into(),
                            forward: None,
                        });
                    };
                    let forward = if let Some(existing) =
                        existing.filter(|existing| existing.check(runner).is_ok())
                    {
                        existing
                    } else {
                        ForwardLease::start(ssh.clone(), &url, runner)?
                    };
                    return Ok(ResolvedLink::Url {
                        url: forward.url(url)?,
                        forward: Some(forward),
                    });
                }
                return Ok(ResolvedLink::Url {
                    url: url.into(),
                    forward: None,
                });
            }
        }
        LinkTarget::File { path, line, column } => (path, line, column),
    };
    let request = FileRequest::Resolve { path, base };
    let response = if let Some(remote) = remote {
        request.execute_remote(&remote, runner.clone())?
    } else {
        request.execute()?
    };
    let FileResponse::Location { path, is_directory } = response else {
        anyhow::bail!("host returned an unexpected file location response");
    };
    anyhow::ensure!(
        !is_directory || line.is_none(),
        "a directory cannot have a line number"
    );
    Ok(ResolvedLink::File {
        path,
        directory: is_directory,
        line: line.unwrap_or(1),
        column: column.unwrap_or(1),
    })
}
