use std::{net::IpAddr, time::Instant};

use bootty_control::{
    ArgumentSchema, Caller, CommandCancellation, CommandDescriptor, CommandOutcome, CompactSchema,
    MutationClass, ValueType,
};

use crate::{
    commands::runtime::CommandDispatch,
    state::{AppEffect, AppState},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionCommand {
    Setup,
    Status,
    Enable,
    Revoke,
    Copy,
}

impl ConnectionCommand {
    pub(crate) const ALL: [Self; 5] = [
        Self::Setup,
        Self::Status,
        Self::Enable,
        Self::Revoke,
        Self::Copy,
    ];

    pub(crate) fn descriptor(self) -> CommandDescriptor {
        let (id, title) = match self {
            Self::Setup => ("connections.setup", "Connect a phone…"),
            Self::Status => ("connections.status", "Connection status"),
            Self::Enable => ("connections.enable", "Enable phone connection"),
            Self::Revoke => ("connections.revoke", "Revoke phone connection"),
            Self::Copy => ("connections.copy", "Copy pairing code"),
        };
        CommandDescriptor {
            id: id.into(), title: title.into(),
            description: "Pair the mobile app with this desktop. Enabling and revocation require an explicit user action.".into(),
            mutation: if self == Self::Status { MutationClass::Read } else { MutationClass::Write },
            arguments: CompactSchema { arguments: if self == Self::Enable { vec![ArgumentSchema {
                name: "host".into(), value_type: ValueType::String, required: true, choices: Vec::new(), minimum: None, maximum: None,
            }] } else { Vec::new() } },
            target: None, palette: self == Self::Setup,
        }
    }
}

impl AppState {
    pub(super) fn dispatch_connection_command(
        &self,
        command: ConnectionCommand,
        arguments: &[String],
        caller: Caller,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if command == ConnectionCommand::Setup {
            if let Err(error) = bootty_mux::executor::begin_synchronous_command(execution) {
                return CommandDispatch::Complete(super::command_outcome_for_mux_error(error));
            }
            effects.push(AppEffect::OpenConnections);
            return CommandDispatch::Complete(CommandOutcome::success());
        }
        let user_action = matches!(
            caller,
            Caller::CommandPalette | Caller::Keybinding | Caller::BuiltinKeybinding
        );
        if command != ConnectionCommand::Status && !user_action {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "Manage pairing from Bootty's Connections screen.".into(),
            });
        }
        let host = if command == ConnectionCommand::Enable {
            match arguments
                .first()
                .and_then(|text| text.parse::<IpAddr>().ok())
            {
                Some(host) => Some(host),
                None => {
                    return CommandDispatch::Complete(failure(
                        "Enter this computer's local IP address",
                    ));
                }
            }
        } else {
            None
        };
        let connections = self.remote_connections.clone();
        self.dispatch_committed_command(execution, move || {
            let run = || -> anyhow::Result<serde_json::Value> {
                let mut status = match command {
                    ConnectionCommand::Status => connections.status(user_action)?,
                    ConnectionCommand::Enable => {
                        connections.enable(host.context("IP address required")?)?
                    }
                    ConnectionCommand::Revoke => connections.revoke()?,
                    ConnectionCommand::Copy => {
                        connections.copy_pairing_code()?;
                        return Ok(serde_json::json!({"copied":true}));
                    }
                    ConnectionCommand::Setup => anyhow::bail!("Setup is a user interface command"),
                };
                if status.suggested_host.is_none() && user_action {
                    status.suggested_host = crate::remote_connections::suggested_host().ok();
                }
                Ok(serde_json::to_value(status)?)
            };
            match run() {
                Ok(value) => CommandOutcome::Success {
                    value,
                    warnings: Vec::new(),
                },
                Err(error) => failure(error.to_string()),
            }
        })
    }
}

use anyhow::Context as _;

fn failure(message: impl Into<String>) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "connection_failed".into(),
        message: message.into(),
    }
}
