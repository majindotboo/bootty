//! Short names in targets: a session or conversation written like `bootty-7c` becomes the exact
//! target it names, at its current generation, when the command runs.

use super::CommandOutcome;
use crate::{commands::ExactMuxTarget, state::AppState};
use bootty_control::{CommandInvocation, CommandTarget, short_name};
use std::collections::HashMap;

impl AppState {
    /// What to call each saved session in writing, by identity: the shortest name no other saved
    /// session in any Space shares.
    pub(crate) fn saved_session_names(&self) -> HashMap<String, String> {
        let full: Vec<(String, String)> = self
            .workspace
            .all_bindings()
            .flat_map(|binding| binding.sessions().sessions())
            .map(|saved| {
                let name = short_name::full_name(saved.cwd.as_ref(), &saved.identity);
                (saved.identity.clone(), name)
            })
            .collect();
        let names: Vec<String> = full.iter().map(|(_, name)| name.clone()).collect();
        full.iter()
            .map(|(identity, name)| (identity.clone(), short_name::shortest(name, &names)))
            .collect()
    }

    /// Replaces a target written as a short name with the exact target it names now. Native
    /// conversation commands name conversations, whose id argument may be the name too; other
    /// commands name live sessions.
    pub(super) fn named_target(
        &self,
        mut invocation: CommandInvocation,
    ) -> Result<CommandInvocation, CommandOutcome> {
        // Names stand for sessions and conversations, never Spaces.
        let Some(written) = invocation
            .target
            .as_ref()
            .filter(|target| target.kind != bootty_control::ResourceKind::Binding)
            .filter(|target| short_name::is_short_name(&target.handle))
            .map(|target| target.handle.clone())
        else {
            return Ok(invocation);
        };
        let target = if invocation.command.starts_with("agents.native.") {
            let service =
                self.native_agent_service()
                    .ok_or_else(|| CommandOutcome::Unavailable {
                        message: "Native conversations are unavailable".to_owned(),
                    })?;
            let target = service
                .named(&written)
                .map_err(|message| CommandOutcome::StaleTarget { message })?;
            if invocation.arguments.first() == Some(&written) {
                let generation = target.generation.to_string();
                invocation.arguments.splice(
                    ..2.min(invocation.arguments.len()),
                    [target.handle.clone(), generation],
                );
            }
            target
        } else {
            let kind = invocation
                .target
                .as_ref()
                .map_or(bootty_control::ResourceKind::Session, |target| target.kind);
            self.named_session(&written, kind)?
        };
        invocation.target = Some(target);
        Ok(invocation)
    }

    /// The live session a short name names, issued as a `kind` target.
    fn named_session(
        &self,
        written: &str,
        kind: bootty_control::ResourceKind,
    ) -> Result<CommandTarget, CommandOutcome> {
        let mut named = self.workspace.all_bindings().flat_map(|binding| {
            binding
                .sessions()
                .sessions()
                .iter()
                .filter(|saved| {
                    short_name::names(
                        written,
                        &short_name::full_name(saved.cwd.as_ref(), &saved.identity),
                    )
                })
                .map(move |saved| (binding, saved))
        });
        let (binding, saved) = match (named.next(), named.next()) {
            (Some(named), None) => named,
            (None, _) => {
                return Err(CommandOutcome::StaleTarget {
                    message: format!("No session is called “{written}”"),
                });
            }
            (Some(_), Some(_)) => {
                return Err(CommandOutcome::Denied {
                    message: format!(
                        "“{written}” names more than one session; write more of its name"
                    ),
                });
            }
        };
        let session = binding.session_attachment(&saved.identity).ok_or_else(|| {
            CommandOutcome::Unavailable {
                message: format!("“{written}” is closed; reopen it first"),
            }
        })?;
        let scope = binding.scope();
        let mux = binding.mux();
        let handle = self.binding_target_handle(scope, mux.binding_generation());
        ExactMuxTarget::Session(scope, session.id.clone())
            .command_target(kind, mux, &handle)
            .ok_or_else(|| CommandOutcome::Unsupported {
                message: format!("A session name can't stand for a {kind:?} target"),
            })
    }
}
