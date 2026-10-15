//! Conversation grants for local apps. A grant follows the app's bundle rather than one window and
//! lasts until the conversation's tools are revoked. Prompt mentions and user approvals add grants.
use crate::{
    AgentCommandExecutor,
    tool_policy::{ToolLaunchGuard, ToolLease},
};
use bootty_computer::{
    ComputerAction, ComputerElement, ComputerResultSnapshot, ComputerTarget, MAX_ELEMENTS,
};
use bootty_control::{
    CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use serde::{Deserialize, Serialize};
use std::{
    ops::Range,
    sync::{Arc, PoisonError, mpsc},
    time::Instant,
};

/// The approval request Bootty raises when an agent asks for an app it may not use yet.
pub const APPLICATION_ACCESS_METHOD: &str = "bootty/computer/requestAccess";

// Bounds keep one conversation from growing host state without limit; raise them if real
// conversations need more apps or concurrent requests.
const MAX_GRANTED_APPLICATIONS: usize = 64;
const MAX_WAITING_REQUESTS: usize = 8;

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

/// An app a conversation may use, identified by its bundle.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeApplication {
    pub bundle_id: String,
    pub name: String,
}
impl NativeApplication {
    /// # Errors
    /// Rejects empty, oversized or control-character identities.
    pub fn validate(&self) -> Result<(), String> {
        let valid = |value: &str| value.len() <= 256 && !value.chars().any(char::is_control);
        if self.bundle_id.is_empty() || !valid(&self.bundle_id) || !valid(&self.name) {
            return Err("Invalid application identity".into());
        }
        Ok(())
    }
}
impl From<&ComputerTarget> for NativeApplication {
    fn from(target: &ComputerTarget) -> Self {
        Self {
            bundle_id: target.bundle_id.clone(),
            name: target
                .application_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| {
                    target
                        .bundle_id
                        .rsplit('.')
                        .next()
                        .unwrap_or(&target.bundle_id)
                        .to_owned()
                }),
        }
    }
}

/// What an agent asks of an app. A press names an element by its number in the app's latest
/// snapshot, so the host presses exactly the element that snapshot listed.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum NativeApplicationAction {
    Press(NativeElementPress),
    Computer(ComputerAction),
}
impl NativeApplicationAction {
    #[must_use]
    pub const fn is_capture(&self) -> bool {
        matches!(self, Self::Computer(action) if action.is_capture())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeElementPress {
    action: PressTag,
    /// Numbered from 1, as the snapshot lists them.
    pub element: std::num::NonZeroU16,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PressTag {
    Press,
}

/// What `computer_snapshot` returns for an app: its image and the elements an agent can press.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeApplicationSnapshot {
    pub image: ComputerResultSnapshot,
    pub elements: Vec<ComputerElement>,
    /// More elements were on screen than the listing returns.
    pub omitted: bool,
    /// Why no elements were listed, such as a missing Accessibility grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// An app's latest capture: its window and the elements listed with it.
struct Observation {
    target: ComputerTarget,
    elements: Vec<ComputerElement>,
}

/// Granted apps, each one's latest capture, and tool calls waiting on the user.
#[derive(Default)]
pub struct ApplicationGrants {
    granted: Vec<NativeApplication>,
    observed: Vec<Observation>,
    waiters: Vec<(String, mpsc::Sender<bool>)>,
}
impl ApplicationGrants {
    /// Dropping waiting senders wakes their tool calls without a grant.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
    fn granted(&self, bundle_id: &str) -> bool {
        self.granted
            .iter()
            .any(|granted| granted.bundle_id == bundle_id)
    }
    fn grant(&mut self, application: NativeApplication) -> Result<(), String> {
        application.validate()?;
        if self.granted(&application.bundle_id) {
            return Ok(());
        }
        if self.granted.len() >= MAX_GRANTED_APPLICATIONS {
            return Err("This conversation already uses too many apps".into());
        }
        self.granted.push(application);
        Ok(())
    }
    fn answer(&mut self, bundle_id: &str, allowed: bool) {
        self.waiters.retain(|(waiting, sender)| {
            if waiting == bundle_id {
                _ = sender.send(allowed);
                false
            } else {
                true
            }
        });
    }
}

/// Authority to use one granted app in one live conversation. Only the lease constructs it.
#[derive(Clone)]
pub struct NativeApplicationAccess {
    lease: Arc<ToolLease>,
    application: NativeApplication,
}
impl std::fmt::Debug for NativeApplicationAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeApplicationAccess")
            .field("application", &self.application)
            .finish_non_exhaustive()
    }
}
impl PartialEq for NativeApplicationAccess {
    fn eq(&self, other: &Self) -> bool {
        self.lease.attachment_id() == other.lease.attachment_id()
            && self.application == other.application
    }
}
impl NativeApplicationAccess {
    #[must_use]
    pub const fn application(&self) -> &NativeApplication {
        &self.application
    }
    #[must_use]
    pub fn current(&self) -> bool {
        let state = self
            .lease
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        !state.revoked && state.applications.granted(&self.application.bundle_id)
    }
    /// The window from this app's latest capture. Input coordinates refer to it.
    #[must_use]
    pub fn observed(&self) -> Option<ComputerTarget> {
        self.observation(|observation| observation.target.clone())
    }
    /// The element numbered `number` in this app's latest capture.
    #[must_use]
    pub fn element(&self, number: std::num::NonZeroU16) -> Option<ComputerElement> {
        self.observation(|observation| {
            observation
                .elements
                .get(usize::from(number.get()).checked_sub(1)?)
                .cloned()
        })
        .flatten()
    }
    fn observation<T>(&self, read: impl FnOnce(&Observation) -> T) -> Option<T> {
        let state = self
            .lease
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state
            .applications
            .observed
            .iter()
            .find(|observation| observation.target.bundle_id == self.application.bundle_id)
            .map(read)
    }
    /// Remember the window a capture saw and the elements listed with it, replacing the app's
    /// previous capture. Only this app's windows are accepted.
    pub fn observe(&self, target: ComputerTarget, mut elements: Vec<ComputerElement>) {
        if target.bundle_id != self.application.bundle_id {
            return;
        }
        elements.truncate(MAX_ELEMENTS);
        let mut state = self
            .lease
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let observed = &mut state.applications.observed;
        observed.retain(|known| known.target.bundle_id != target.bundle_id);
        observed.push(Observation { target, elements });
        drop(state);
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
            || !state.applications.granted(&self.application.bundle_id)
            || state.pending.len() >= 8
        {
            return Err("Application access changed or is busy".into());
        }
        let request_id = state.next_request;
        state.next_request = request_id
            .checked_add(1)
            .ok_or("Tool request IDs are exhausted")?;
        state.pending.push((request_id, cancellation.clone()));
        drop(state);
        Ok(ToolLaunchGuard::application(
            self.lease.as_ref().clone(),
            request_id,
            cancellation,
        ))
    }
}

impl ToolLease {
    /// Provider children cannot widen their inherited tool authority through app grants.
    #[must_use]
    pub const fn application_mentions_supported(&self) -> bool {
        self.ancestor.is_none()
    }

    /// The host calls this only with mentions from an explicit user submission. Mentions add
    /// grants; they never remove one.
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
            || state
                .application_session
                .as_ref()
                .is_some_and(|prior| prior != session)
        {
            return Err("Conversation tools are no longer attached".into());
        }
        state.application_session = Some(session.clone());
        for mention in mentions {
            state
                .applications
                .grant(NativeApplication::from(&mention.target))?;
        }
        drop(state);
        Ok(())
    }

    /// `None` means the user has not allowed this app in this conversation.
    /// # Errors
    /// Rejects a stale or revoked conversation.
    pub fn application_access(
        &self,
        session: &CommandTarget,
        application: &NativeApplication,
    ) -> Result<Option<NativeApplicationAccess>, String> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked || state.application_session.as_ref() != Some(session) {
            return Err("Application access is no longer live".into());
        }
        Ok(state
            .applications
            .granted(&application.bundle_id)
            .then(|| NativeApplicationAccess {
                lease: Arc::new(self.clone()),
                application: application.clone(),
            }))
    }

    /// Wait for the user's answer about an app. An existing grant answers immediately.
    /// # Errors
    /// Rejects a stale conversation, an invalid identity or too many waiting requests.
    pub fn await_application(
        &self,
        session: &CommandTarget,
        application: &NativeApplication,
    ) -> Result<mpsc::Receiver<bool>, String> {
        application.validate()?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked || state.application_session.as_ref() != Some(session) {
            return Err("Application access is no longer live".into());
        }
        let (sender, receiver) = mpsc::channel();
        if state.applications.granted(&application.bundle_id) {
            _ = sender.send(true);
        } else if state.applications.waiters.len() >= MAX_WAITING_REQUESTS {
            return Err("Too many app requests are waiting for the user".into());
        } else {
            state
                .applications
                .waiters
                .push((application.bundle_id.clone(), sender));
        }
        drop(state);
        Ok(receiver)
    }

    /// Record the user's answer and wake every tool call waiting on it.
    /// # Errors
    /// Rejects revoked tools, an unbound conversation or an invalid identity.
    pub fn decide_application(
        &self,
        application: &NativeApplication,
        allowed: bool,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked || state.application_session.is_none() {
            return Err("Conversation tools are no longer attached".into());
        }
        if allowed {
            state.applications.grant(application.clone())?;
        }
        state.applications.answer(&application.bundle_id, allowed);
        drop(state);
        Ok(())
    }

    /// The host resolves the app, checks or requests the grant and runs the action.
    pub(crate) fn invoke_application(
        &self,
        application: &str,
        action: &NativeApplicationAction,
        commands: &dyn AgentCommandExecutor,
        deadline: Instant,
    ) -> CommandOutcome {
        let session = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.application_session.clone().filter(|_| !state.revoked)
        };
        let Some(session) = session else {
            return CommandOutcome::Denied {
                message: "App access starts after a prompt in this conversation".into(),
            };
        };
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
                application.into(),
                action,
            ],
            self.caller(),
        );
        invocation.target = Some(session);
        commands.execute(invocation, deadline, CommandCancellation::new())
    }
}
