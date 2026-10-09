//! Explicit prompt mentions grant access to exact application windows for one conversation.
use crate::{
    AgentCommandExecutor,
    tool_policy::{ToolLaunchGuard, ToolLease},
};
use bootty_computer::{ComputerAction, ComputerTarget};
use bootty_control::{
    CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use serde::{Deserialize, Serialize};
use std::{
    ops::Range,
    sync::{Arc, PoisonError},
    time::Instant,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NativeApplicationMention {
    pub id: String,
    pub target: ComputerTarget,
    pub prompt_range: Range<usize>,
}
impl NativeApplicationMention {
    /// # Errors
    /// Rejects oversized or malformed host window mentions.
    pub fn validate(&self) -> Result<(), String> {
        self.target.validate().map_err(|e| e.to_string())?;
        if self.id.is_empty() || self.id.len() > 256 || self.id.chars().any(char::is_control) {
            return Err("Invalid application reference".into());
        }
        Ok(())
    }
}

/// Only a live conversation lease can construct this non-serializable authority.
#[derive(Clone)]
pub struct NativeApplicationAccess {
    lease: Arc<ToolLease>,
    epoch: u64,
    mention: Arc<NativeApplicationMention>,
}
impl std::fmt::Debug for NativeApplicationAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeApplicationAccess")
            .field("epoch", &self.epoch)
            .field("mention", &self.mention)
            .finish_non_exhaustive()
    }
}
impl PartialEq for NativeApplicationAccess {
    fn eq(&self, other: &Self) -> bool {
        self.lease.attachment_id() == other.lease.attachment_id()
            && self.epoch == other.epoch
            && self.mention == other.mention
    }
}
impl NativeApplicationAccess {
    #[must_use]
    pub fn target(&self) -> &ComputerTarget {
        &self.mention.target
    }
    #[must_use]
    pub fn current(&self) -> bool {
        let state = self
            .lease
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        !state.revoked
            && state.application_epoch == self.epoch
            && state
                .applications
                .iter()
                .any(|mention| mention == self.mention.as_ref())
    }
    /// Keep revocation connected to the final shared command mutation gate.
    /// # Errors
    /// Rejects revoked scopes, a full request queue or a cancelled invocation.
    pub fn begin(&self, cancellation: CommandCancellation) -> Result<ToolLaunchGuard, String> {
        let mut state = self
            .lease
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if cancellation.is_cancelled()
            || state.revoked
            || state.application_epoch != self.epoch
            || state.pending.len() >= 8
        {
            return Err("Application access changed or is busy".into());
        }
        let request_id = state.next_request;
        state.next_request = request_id
            .checked_add(1)
            .ok_or("Tool request IDs are exhausted")?;
        state.pending.push((request_id, cancellation.clone()));
        state.application_requests.push(request_id);
        drop(state);
        Ok(ToolLaunchGuard::application(
            self.lease.as_ref().clone(),
            request_id,
            cancellation,
        ))
    }
}
impl ToolLease {
    /// Provider children cannot widen their inherited tool authority through app mentions.
    #[must_use]
    pub const fn application_mentions_supported(&self) -> bool {
        self.ancestor.is_none()
    }

    /// The host calls this only with mentions from an explicit user submission.
    /// # Errors
    /// Rejects invalid, duplicate, unbound or revoked conversation grants.
    pub fn grant_applications(
        &self,
        session: &CommandTarget,
        mentions: &[NativeApplicationMention],
    ) -> Result<(), String> {
        if session.kind != ResourceKind::Session
            || session.generation == 0
            || session.handle.is_empty()
            || mentions.len() > 8
        {
            return Err(
                "Application mentions require an exact conversation and at most eight windows"
                    .into(),
            );
        }
        for (index, mention) in mentions.iter().enumerate() {
            mention.validate()?;
            if mentions
                .iter()
                .take(index)
                .any(|prior| prior.id == mention.id)
            {
                return Err("Duplicate application mention".into());
            }
        }
        if self.ancestor.is_some() {
            return if mentions.is_empty() {
                Ok(())
            } else {
                Err("Inherited child tools do not grant application input".into())
            };
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked
            || state.terminal.is_none()
            || self.ancestor.is_some()
            || state
                .application_session
                .as_ref()
                .is_some_and(|prior| prior != session)
        {
            return Err("Conversation tools are no longer attached".into());
        }
        let epoch = state
            .application_epoch
            .checked_add(1)
            .ok_or("Application grant IDs are exhausted")?;
        for (id, pending) in &state.pending {
            if state.application_requests.contains(id) {
                _ = pending.cancel();
            }
        }
        state.application_requests.clear();
        state.application_epoch = epoch;
        state.applications = mentions.to_vec();
        state.application_session = Some(session.clone());
        drop(state);
        Ok(())
    }
    /// Resolve an opaque prompt reference; agent inputs cannot provide another target.
    /// # Errors
    /// Rejects an unmentioned, stale or revoked application window.
    pub fn application_access(
        &self,
        session: &CommandTarget,
        id: &str,
    ) -> Result<NativeApplicationAccess, String> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked || state.application_session.as_ref() != Some(session) {
            return Err("Application access is no longer live".into());
        }
        let mention = state
            .applications
            .iter()
            .find(|mention| mention.id == id)
            .ok_or("Mention the application in your prompt before using it")?
            .clone();
        Ok(NativeApplicationAccess {
            lease: Arc::new(self.clone()),
            epoch: state.application_epoch,
            mention: Arc::new(mention),
        })
    }
    pub(crate) fn invoke_application(
        &self,
        id: &str,
        action: &ComputerAction,
        commands: &dyn AgentCommandExecutor,
        deadline: Instant,
    ) -> CommandOutcome {
        let session = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.application_session.clone()
        };
        let Some(session) = session else {
            return CommandOutcome::Denied {
                message: "Mention an application in your prompt first".into(),
            };
        };
        let Ok(access) = self.application_access(&session, id) else {
            return CommandOutcome::StaleTarget {
                message: "Application access changed".into(),
            };
        };
        if let Err(error) = access.target().validate_action(action) {
            return CommandOutcome::Denied {
                message: error.to_string(),
            };
        }
        let action = match serde_json::to_string(action) {
            Ok(action) => action,
            Err(error) => {
                return CommandOutcome::Failed {
                    code: "invalid_action".into(),
                    message: error.to_string(),
                };
            }
        };
        let mut invocation = CommandInvocation::new(
            "agents.native.computer",
            vec![
                session.handle.clone(),
                session.generation.to_string(),
                id.into(),
                action,
            ],
            self.caller(),
        );
        invocation.target = Some(session);
        let outcome = commands.execute(invocation, deadline, CommandCancellation::new());
        if access.current() {
            outcome
        } else {
            CommandOutcome::StaleTarget {
                message: "Application access changed during the command".into(),
            }
        }
    }
}
