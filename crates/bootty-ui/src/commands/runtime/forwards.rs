use super::{CommandDispatch, PendingCommandResult};
use crate::AppState;
use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_host::{
    CancellableCommandRunner,
    remote::RemoteHost,
    ssh_forward::{ForwardInfo, ForwardLease},
};
use bootty_mux::{controller::SpaceId, executor};
use std::{
    sync::{Arc, mpsc},
    time::Instant,
};
pub enum ForwardResult {
    Opened {
        replaces: Option<String>,
        scope: SpaceId,
        generation: u64,
        lease: Arc<ForwardLease>,
    },
    Closed(String),
    Checked(ForwardInfo),
}
enum ForwardRequest {
    Open {
        scope: SpaceId,
        generation: u64,
        remote: bootty_host::ssh::SshRemote,
        url: url::Url,
    },
    Retry {
        scope: SpaceId,
        generation: u64,
        lease: Arc<ForwardLease>,
    },
    Close(Arc<ForwardLease>),
    Check(Arc<ForwardLease>),
}

impl ForwardRequest {
    fn execute(self, runner: &CancellableCommandRunner) -> anyhow::Result<ForwardResult> {
        match self {
            Self::Open {
                scope,
                generation,
                remote,
                url,
            } => Ok(ForwardResult::Opened {
                replaces: None,
                scope,
                generation,
                lease: ForwardLease::start(remote, &url, runner)?,
            }),
            Self::Retry {
                scope,
                generation,
                lease,
            } => Ok(ForwardResult::Opened {
                replaces: Some(lease.id().to_owned()),
                scope,
                generation,
                lease: lease.restart(runner)?,
            }),
            Self::Close(lease) => {
                lease.close(runner)?;
                Ok(ForwardResult::Closed(lease.id().to_owned()))
            }
            Self::Check(lease) => {
                lease.check(runner)?;
                Ok(ForwardResult::Checked(lease.info()))
            }
        }
    }
}

impl AppState {
    pub fn forward_overview(&self) -> Vec<ForwardInfo> {
        self.commands
            .forwards
            .iter()
            .map(|(_, _, lease)| lease.info())
            .collect()
    }
    pub(super) fn dispatch_forward(
        &self,
        action: &str,
        args: &[String],
        scope: Option<SpaceId>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let fail = |message: &str| {
            CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: message.to_owned(),
            })
        };
        if action == "forwards.list" {
            return CommandDispatch::Complete(super::serialized_command_outcome(
                self.forward_overview(),
            ));
        }
        let Some(argument) = args.first() else {
            return fail("A forward URL or lease ID is required");
        };
        let request = if action == "forwards.open" {
            let Some(scope) = scope else {
                return fail("No host binding");
            };
            let Some(binding) = self.workspace.binding(scope) else {
                return fail("Host binding was closed");
            };
            let Some(RemoteHost::Ssh(remote)) =
                binding.multiplexer().remote.clone().map(RemoteHost::new)
            else {
                return fail(
                    "SSH forwards require an SSH binding; WSL uses Windows localhost forwarding",
                );
            };
            if argument.len() > 8192 {
                return fail("Forward URL exceeds its bound");
            }
            let Ok(url) = url::Url::parse(argument) else {
                return fail("Expected a loopback HTTP or HTTPS URL");
            };
            if self.commands.forwards.len() >= 64 {
                return fail("Close a forwarding lease before opening another");
            }
            ForwardRequest::Open {
                scope,
                generation: binding.mux().binding_generation(),
                remote,
                url,
            }
        } else {
            let Some((scope, generation, lease)) = self
                .commands
                .forwards
                .iter()
                .find(|(_, _, lease)| lease.id() == argument)
            else {
                return fail("Forward lease no longer exists");
            };
            let lease = Arc::clone(lease);
            match action {
                "forwards.retry" => ForwardRequest::Retry {
                    scope: *scope,
                    generation: *generation,
                    lease,
                },
                "forwards.close" => ForwardRequest::Close(lease),
                "forwards.check" => ForwardRequest::Check(lease),
                _ => return fail("Unknown forward operation"),
            }
        };
        let (deadline, cancellation) = executor::command_execution(execution);
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<ForwardResult> {
                executor::begin_synchronous_command(Some((deadline, cancellation.clone())))
                    .map_err(|error| anyhow::anyhow!("Forward request stopped: {error:?}"))?;
                let runner = CancellableCommandRunner::with_deadline_and_cancellation_check(
                    bootty_host::CommandCancellation::default(),
                    deadline,
                    move || cancellation.is_cancelled(),
                );
                request.execute(&runner)
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(result);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Forward { result })
    }
    pub(super) fn finish_forward(&mut self, result: ForwardResult) -> CommandOutcome {
        match result {
            ForwardResult::Opened {
                replaces,
                scope,
                generation,
                lease,
            } => {
                if self
                    .workspace
                    .binding(scope)
                    .is_none_or(|binding| binding.mux().binding_generation() != generation)
                {
                    return CommandOutcome::StaleTarget {
                        message: "Forward host changed before establishment".to_owned(),
                    };
                }
                if let Some(id) = replaces {
                    if !self
                        .commands
                        .forwards
                        .iter()
                        .any(|(_, _, lease)| lease.id() == id)
                    {
                        return CommandOutcome::StaleTarget {
                            message: "Forward closed while retrying it".to_owned(),
                        };
                    }
                    self.commands
                        .forwards
                        .retain(|(_, _, lease)| lease.id() != id);
                }
                if self.commands.forwards.len() >= 64 {
                    return CommandOutcome::Unavailable {
                        message: "Forward lease limit reached".to_owned(),
                    };
                }
                let info = lease.info();
                self.commands.forwards.push((scope, generation, lease));
                super::serialized_command_outcome(info)
            }
            ForwardResult::Closed(id) => {
                self.commands
                    .forwards
                    .retain(|(_, _, lease)| lease.id() != id);
                CommandOutcome::Success {
                    value: serde_json::json!({"closed":id}),
                    warnings: Vec::new(),
                }
            }
            ForwardResult::Checked(info) => {
                if !self
                    .commands
                    .forwards
                    .iter()
                    .any(|(_, _, lease)| lease.id() == info.id)
                {
                    return CommandOutcome::StaleTarget {
                        message: "Forward closed while checking it".to_owned(),
                    };
                }
                super::serialized_command_outcome(info)
            }
        }
    }
}
