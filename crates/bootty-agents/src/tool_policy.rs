//! Host-issued authority for one launched provider. Client JSON never supplies identity or caller.

use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};

use crate::{AgentCommandExecutor, AgentKind, tool_spawn::ToolSpawnRequest};

static NEXT_ATTACHMENT: AtomicU64 = AtomicU64::new(1);

/// Separate terminal state from explicitly enabled browser/computer capture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent grants attenuate separately"
)]
pub struct ToolPolicy {
    pub own_terminal_read: bool,
    pub browser_capture: bool,
    pub computer_capture: bool,
    pub spawn_children: bool,
}

impl ToolPolicy {
    #[must_use]
    pub const fn own_terminal() -> Self {
        Self {
            own_terminal_read: true,
            browser_capture: false,
            computer_capture: false,
            spawn_children: false,
        }
    }

    /// Authority can only shrink, including authority passed to a future child launch.
    #[must_use]
    pub const fn attenuate(self, requested: Self) -> Self {
        Self {
            own_terminal_read: self.own_terminal_read && requested.own_terminal_read,
            browser_capture: self.browser_capture && requested.browser_capture,
            computer_capture: self.computer_capture && requested.computer_capture,
            spawn_children: self.spawn_children && requested.spawn_children,
        }
    }
}

/// Supplied by the host that resolved the original launch, never by the MCP client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolScope {
    pub provider: AgentKind,
    pub binding: CommandTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolCapture {
    Browser,
    Computer,
}

#[derive(Clone, Copy)]
pub enum NativeToolRead {
    Status,
    Models,
    Provider,
    Profiles,
    Agents,
    Activity { limit: usize },
}

impl NativeToolRead {
    const fn command(self) -> &'static str {
        match self {
            Self::Status => "agents.native.status",
            Self::Models => "agents.native.models",
            Self::Provider => "agents.native.provider",
            Self::Profiles => "agents.native.profiles",
            Self::Agents => "agents.native.activities",
            Self::Activity { .. } => "agents.native.activity",
        }
    }
}

#[derive(Clone, Copy)]
pub enum WorkspaceToolRead {
    Info,
    Terminals,
}

impl WorkspaceToolRead {
    const fn command(self) -> &'static str {
        match self {
            Self::Info => "spaces.inspect",
            Self::Terminals => "terminal.activities",
        }
    }
}

enum ReadTarget {
    Workspace(WorkspaceToolRead),
    Terminal(Vec<String>),
    Capture(ToolCapture),
    Native(NativeToolRead),
}

/// A capture destination/action captured by its feature owner; MCP receives no target/path field.
#[derive(Clone)]
pub struct ToolCapturedCommand {
    pub capture: ToolCapture,
    pub invocation: CommandInvocation,
}

/// One document explicitly attached by the user; it confers no navigation or input authority.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeBrowserAttachment {
    pub window: CommandTarget,
    pub page: u64,
    pub document: String,
}

/// Live browser authority projected to controls; unavailable grants cannot retain an attachment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NativeBrowserAccess {
    #[default]
    Unavailable,
    Ready,
    Attached(NativeBrowserAttachment),
}

impl NativeBrowserAccess {
    #[must_use]
    pub const fn supported(&self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    #[must_use]
    pub const fn current(&self) -> Option<&NativeBrowserAttachment> {
        match self {
            Self::Attached(attachment) => Some(attachment),
            _ => None,
        }
    }
}

impl NativeBrowserAttachment {
    /// # Errors
    /// Refuses an implicit window, page, or document before granting access.
    pub fn validate(&self) -> Result<(), String> {
        if self.window.kind != ResourceKind::ApplicationWindow
            || self.window.generation == 0
            || self.window.handle.is_empty()
            || self.window.handle.len() > 8192
            || self.page == 0
            || self.page > i64::MAX.unsigned_abs()
            || self.document.len() != 32
            || !self.document.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("Browser access requires an exact window, page and document".into());
        }
        Ok(())
    }

    fn invocation(&self, caller: Caller) -> CommandInvocation {
        CommandInvocation {
            target: Some(self.window.clone()),
            ..CommandInvocation::new(
                "browser.snapshot",
                vec![self.page.to_string(), self.document.clone()],
                caller,
            )
        }
    }

    fn from_invocation(invocation: &CommandInvocation) -> Result<Self, String> {
        if invocation.command != "browser.snapshot" || invocation.arguments.len() != 2 {
            return Err("Browser capture requires a document snapshot command".into());
        }
        let attachment = Self {
            window: invocation
                .target
                .clone()
                .ok_or("Browser capture requires a window")?,
            page: invocation
                .arguments
                .first()
                .and_then(|page| page.parse().ok())
                .ok_or("Browser capture requires a page")?,
            document: invocation
                .arguments
                .get(1)
                .cloned()
                .ok_or("Browser capture requires a document")?,
        };
        attachment.validate()?;
        Ok(attachment)
    }
}

/// Frozen selected profile; account paths and reusable launch configuration stay host-owned.
#[derive(Clone, Debug)]
pub struct ToolSpawnContext {
    pub profile: Option<String>,
}

/// Host-only authority for a child. Its fields cannot be supplied over JSON or widened by callers.
#[derive(Clone)]
pub struct ToolChildAuthority {
    parent: ToolLease,
}

impl ToolChildAuthority {
    #[must_use]
    pub const fn scope(&self) -> &ToolScope {
        &self.parent.scope
    }

    #[must_use]
    pub const fn caller(&self) -> Caller {
        self.parent.caller
    }

    /// # Errors
    /// Rejects a revoked or disabled ancestor before preparing child tools.
    pub(crate) fn into_lease(self) -> Result<ToolLease, String> {
        let state = self
            .parent
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.revoked || !state.policy.spawn_children || state.terminal.is_none() {
            return Err("Parent spawn authority is no longer live".to_owned());
        }
        let policy = state.policy.attenuate(ToolPolicy::own_terminal());
        let native_parent = state.native_session.clone();
        drop(state);
        let mut child = ToolLease::issue(
            self.parent.scope.clone(),
            self.parent.caller,
            policy,
            Vec::new(),
        )?;
        child.native_parent = native_parent;
        child.ancestor = Some(Box::new(self.parent));
        Ok(child)
    }
}

/// Keeps pending child-operation cancellation registered with its parent until completion.
/// The host forwards this token unchanged to the final shared command mutation gate.
pub struct ToolSpawnGuard {
    parent: ToolLease,
    request_id: u64,
    cancellation: CommandCancellation,
}

impl ToolSpawnGuard {
    #[must_use]
    pub fn cancellation(&self) -> CommandCancellation {
        self.cancellation.clone()
    }

    #[must_use]
    pub fn authority(&self) -> ToolChildAuthority {
        ToolChildAuthority {
            parent: self.parent.clone(),
        }
    }

    #[must_use]
    pub const fn caller(&self) -> Caller {
        self.parent.caller
    }
}

impl Drop for ToolSpawnGuard {
    fn drop(&mut self) {
        _ = self.cancellation.cancel();
        self.parent.finish_request(self.request_id);
    }
}

/// Tracks an unbound attachment's launch token without granting child or tool authority.
pub struct ToolLaunchGuard {
    lease: ToolLease,
    request_id: u64,
    cancellation: CommandCancellation,
}

impl ToolLaunchGuard {
    pub(crate) const fn application(
        lease: ToolLease,
        request_id: u64,
        cancellation: CommandCancellation,
    ) -> Self {
        Self {
            lease,
            request_id,
            cancellation,
        }
    }
}

impl Drop for ToolLaunchGuard {
    fn drop(&mut self) {
        _ = self.cancellation.cancel();
        self.lease.finish_request(self.request_id);
    }
}

pub struct GrantState {
    pub(crate) terminal: Option<CommandTarget>,
    spawned_terminals: Vec<CommandTarget>,
    native_session: Option<CommandTarget>,
    browser: Option<NativeBrowserAttachment>,
    browser_epoch: u64,
    browser_requests: Vec<u64>,
    pub(crate) revoked: bool,
    policy: ToolPolicy,
    pub(crate) pending: Vec<(u64, CommandCancellation)>,
    pub(crate) next_request: u64,
    pub(crate) application_epoch: u64,
    pub(crate) application_requests: Vec<u64>,
    pub(crate) applications: Vec<crate::NativeApplicationMention>,
    pub(crate) application_session: Option<CommandTarget>,
}

/// Clones share revocation. Dropping the bridge revokes the lease, including outstanding reads.
#[derive(Clone)]
pub struct ToolLease {
    attachment_id: u64,
    scope: ToolScope,
    caller: Caller,
    captures: Arc<[ToolCapturedCommand]>,
    spawn: Option<ToolSpawnContext>,
    pub(crate) ancestor: Option<Box<Self>>,
    native_parent: Option<CommandTarget>,
    pub(crate) state: Arc<Mutex<GrantState>>,
}

impl ToolLease {
    /// Catalog support is advertised before a prompt; only explicit attachment grants allow reads.
    #[must_use]
    pub fn browser_attachments_supported(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.ancestor.is_none() && !state.revoked && state.policy.browser_capture
    }

    /// The host calls this only for a user attachment action through the shared command path.
    /// # Errors
    /// Rejects stale, unbound, disabled or inherited conversation authority.
    pub fn attach_browser(
        &self,
        session: &CommandTarget,
        attachment: Option<NativeBrowserAttachment>,
    ) -> Result<(), String> {
        if let Some(attachment) = &attachment {
            attachment.validate()?;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.ancestor.is_some()
            || state.revoked
            || !state.policy.browser_capture
            || state.terminal.is_none()
            || state.native_session.as_ref() != Some(session)
        {
            return Err("Browser attachment requires this live root conversation's tools".into());
        }
        state.browser_epoch = state
            .browser_epoch
            .checked_add(1)
            .ok_or("Browser grant IDs are exhausted")?;
        for (id, pending) in &state.pending {
            if state.browser_requests.contains(id) {
                _ = pending.cancel();
            }
        }
        state.browser_requests.clear();
        state.browser = attachment;
        drop(state);
        Ok(())
    }

    #[must_use]
    pub fn browser_attachment(&self) -> Option<NativeBrowserAttachment> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (!state.revoked && state.policy.browser_capture)
            .then(|| state.browser.clone())
            .flatten()
    }

    #[must_use]
    pub fn browser_access(&self) -> NativeBrowserAccess {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.ancestor.is_some() || state.revoked || !state.policy.browser_capture {
            NativeBrowserAccess::Unavailable
        } else {
            state
                .browser
                .clone()
                .map_or(NativeBrowserAccess::Ready, NativeBrowserAccess::Attached)
        }
    }

    /// # Errors
    /// Rejects invalid host scope and unsupported captured commands before issuing authority.
    pub fn issue(
        scope: ToolScope,
        caller: Caller,
        policy: ToolPolicy,
        captures: Vec<ToolCapturedCommand>,
    ) -> Result<Self, String> {
        Self::issue_with_spawn(scope, caller, policy, captures, None)
    }

    /// # Errors
    /// Rejects invalid exact targets, captured commands or frozen profile metadata.
    pub fn issue_with_spawn(
        scope: ToolScope,
        caller: Caller,
        policy: ToolPolicy,
        captures: Vec<ToolCapturedCommand>,
        spawn: Option<ToolSpawnContext>,
    ) -> Result<Self, String> {
        if spawn
            .as_ref()
            .and_then(|spawn| spawn.profile.as_ref())
            .is_some_and(|profile| {
                profile.is_empty() || profile.len() > 256 || profile.chars().any(char::is_control)
            })
        {
            return Err("Spawn context requires a bounded captured profile ID".to_owned());
        }
        if scope.binding.kind != ResourceKind::Binding
            || scope.binding.handle.is_empty()
            || scope.binding.handle.len() > 8192
            || scope.binding.generation == 0
        {
            return Err("Tool authority requires an exact live Binding target".to_owned());
        }
        if captures.len() > 2
            || captures.iter().enumerate().any(|(index, capture)| {
                captures
                    .iter()
                    .skip(index.saturating_add(1))
                    .any(|other| other.capture == capture.capture)
                    || capture.invocation.arguments.len() > 8
                    || capture
                        .invocation
                        .arguments
                        .iter()
                        .map(String::len)
                        .fold(0_usize, usize::saturating_add)
                        > 32 * 1024
            })
            || captures.iter().any(|capture| {
                capture.invocation.target.as_ref().is_none_or(|target| {
                    target.kind != ResourceKind::ApplicationWindow
                        || target.handle.is_empty()
                        || target.handle.len() > 8192
                        || target.generation == 0
                }) || match capture.capture {
                    ToolCapture::Computer => !capture.invocation.arguments.is_empty(),
                    ToolCapture::Browser => {
                        NativeBrowserAttachment::from_invocation(&capture.invocation).is_err()
                    }
                }
            })
            || captures.iter().any(|capture| {
                capture.invocation.command
                    != match capture.capture {
                        ToolCapture::Browser => "browser.snapshot",
                        ToolCapture::Computer => "computer.capture",
                    }
            })
        {
            return Err("Tool captures require bounded host-captured snapshot commands".to_owned());
        }
        let browser = captures
            .iter()
            .find(|capture| capture.capture == ToolCapture::Browser)
            .map(|capture| NativeBrowserAttachment::from_invocation(&capture.invocation))
            .transpose()?;
        let captures = captures
            .into_iter()
            .filter(|capture| capture.capture != ToolCapture::Browser)
            .collect::<Vec<_>>();
        let attachment_id = NEXT_ATTACHMENT
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| "Tool attachment identities are exhausted")?;
        Ok(Self {
            attachment_id,
            scope,
            caller,
            captures: Arc::from(captures),
            spawn,
            ancestor: None,
            native_parent: None,
            state: Arc::new(Mutex::new(GrantState {
                spawned_terminals: Vec::new(),
                terminal: None,
                native_session: None,
                browser,
                browser_epoch: 0,
                browser_requests: Vec::new(),
                revoked: false,
                policy,
                pending: Vec::new(),
                next_request: 0,
                application_epoch: 0,
                application_requests: Vec::new(),
                applications: Vec::new(),
                application_session: None,
            })),
        })
    }

    /// Process-local selector only; the host still validates the complete retained authority.
    #[must_use]
    pub const fn attachment_id(&self) -> u64 {
        self.attachment_id
    }

    #[must_use]
    pub const fn scope(&self) -> &ToolScope {
        &self.scope
    }

    #[must_use]
    pub const fn caller(&self) -> Caller {
        self.caller
    }

    /// Keep the host's restore cancellation tied to this fresh attachment until launch finishes.
    /// # Errors
    /// Rejects cancelled, bound, revoked, child or busy attachments before shared mutation.
    pub fn begin_launch(
        &self,
        cancellation: &CommandCancellation,
    ) -> Result<ToolLaunchGuard, String> {
        if cancellation.is_cancelled() {
            return Err("Agent launch was cancelled before acceptance".to_owned());
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked || state.terminal.is_some() || self.ancestor.is_some() {
            return Err("Tool authority is no longer awaiting its launch".to_owned());
        }
        if state.pending.len() >= 8 {
            return Err("Tool authority is busy".to_owned());
        }
        let request_id = state.next_request;
        state.next_request = request_id
            .checked_add(1)
            .ok_or("Tool request IDs are exhausted")?;
        state.pending.push((request_id, cancellation.clone()));
        drop(state);
        Ok(ToolLaunchGuard {
            lease: self.clone(),
            request_id,
            cancellation: cancellation.clone(),
        })
    }

    #[must_use]
    pub const fn spawn_context(&self) -> Option<&ToolSpawnContext> {
        self.spawn.as_ref()
    }

    /// Bind exactly once after the host registered the successfully launched terminal in this Binding.
    /// # Errors
    /// Rejects another Binding/generation, invalid targets, revoked or already bound leases.
    pub fn bind(&self, binding: &CommandTarget, terminal: CommandTarget) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if binding != &self.scope.binding
            || terminal.kind != ResourceKind::Terminal
            || terminal.handle.is_empty()
            || terminal.handle.len() > 8192
            || terminal.generation == 0
        {
            return Err(
                "Tool authority must bind the exact launch Binding and Terminal".to_owned(),
            );
        }
        if state.revoked
            || state.terminal.is_some()
            || self
                .ancestor
                .as_ref()
                .is_some_and(|ancestor| ancestor.revoked())
        {
            return Err("Tool authority is no longer awaiting its launch".to_owned());
        }
        state.terminal = Some(terminal);
        drop(state);
        Ok(())
    }

    pub(crate) fn terminal_target(&self) -> Option<CommandTarget> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked {
            None
        } else {
            state.terminal.clone()
        }
    }

    /// Bind the persisted conversation before its provider discovers the automatic catalog.
    /// # Errors
    /// Rejects invalid, revoked or reused conversation authority.
    pub fn bind_native_session(&self, target: CommandTarget) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if target.kind != ResourceKind::Session
            || target.handle.is_empty()
            || target.handle.len() > 8192
            || target.generation == 0
            || state.revoked
            || state.terminal.is_none()
            || state.native_session.is_some()
        {
            return Err("Conversation tools require one exact persisted Session".into());
        }
        state.native_session = Some(target);
        drop(state);
        Ok(())
    }

    #[must_use]
    pub fn native_session_target(&self) -> Option<CommandTarget> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (!state.revoked)
            .then(|| state.native_session.clone())
            .flatten()
    }

    #[must_use]
    pub fn native_parent_target(&self) -> Option<CommandTarget> {
        // Provenance survives revocation of an already accepted child; it grants no access.
        self.native_parent.clone()
    }

    pub(crate) fn native_reads_enabled(&self) -> bool {
        self.enabled(None)
            && self
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .native_session
                .is_some()
    }

    pub(crate) fn native_agents_enabled(&self) -> bool {
        self.ancestor.is_none() && self.native_reads_enabled()
    }

    pub(crate) fn workspace_read_enabled(&self) -> bool {
        self.ancestor.is_none() && self.enabled(None)
    }

    pub fn revoke(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.revoked = true;
        state.terminal = None;
        state.native_session = None;
        state.browser = None;
        state.browser_requests.clear();
        state.spawned_terminals.clear();
        state.applications.clear();
        state.application_session = None;
        for (_, pending) in state.pending.drain(..) {
            _ = pending.cancel();
        }
    }

    /// Feature disabling takes effect immediately; enabling requires a new host-issued launch.
    pub fn restrict(&self, requested: ToolPolicy) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.policy = state.policy.attenuate(requested);
        if !state.policy.browser_capture {
            state.browser = None;
            state.browser_requests.clear();
        }
        if !state.policy.spawn_children {
            state.spawned_terminals.clear();
        }
        for (_, pending) in state.pending.drain(..) {
            _ = pending.cancel();
        }
    }

    #[must_use]
    pub fn enabled(&self, capture: Option<ToolCapture>) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        !state.revoked
            && self
                .ancestor
                .as_ref()
                .is_none_or(|ancestor| ancestor.enabled(capture))
            && match capture {
                None => state.policy.own_terminal_read,
                Some(ToolCapture::Browser) => {
                    state.policy.browser_capture && state.browser.is_some()
                }
                Some(ToolCapture::Computer) => {
                    state.policy.computer_capture && self.captured(ToolCapture::Computer).is_some()
                }
            }
    }

    /// Execute reads with captured identity and discard results after authority is withdrawn.
    pub(crate) fn invoke(
        &self,
        arguments: Vec<String>,
        capture: Option<ToolCapture>,
        commands: &dyn AgentCommandExecutor,
        deadline: Instant,
    ) -> CommandOutcome {
        self.invoke_read(
            capture.map_or(ReadTarget::Terminal(arguments), ReadTarget::Capture),
            commands,
            deadline,
        )
    }

    pub(crate) fn invoke_native(
        &self,
        read: NativeToolRead,
        commands: &dyn AgentCommandExecutor,
        deadline: Instant,
    ) -> CommandOutcome {
        if matches!(read, NativeToolRead::Agents | NativeToolRead::Profiles)
            && self.ancestor.is_some()
        {
            return unavailable("Child tools cannot read sibling or configured profile metadata");
        }
        self.invoke_read(ReadTarget::Native(read), commands, deadline)
    }

    pub(crate) fn invoke_workspace(
        &self,
        read: WorkspaceToolRead,
        commands: &dyn AgentCommandExecutor,
        deadline: Instant,
    ) -> CommandOutcome {
        if self.ancestor.is_some() {
            return unavailable("Child tools cannot read their parent's Space metadata");
        }
        self.invoke_read(ReadTarget::Workspace(read), commands, deadline)
    }

    fn invoke_read(
        &self,
        destination: ReadTarget,
        commands: &dyn AgentCommandExecutor,
        deadline: Instant,
    ) -> CommandOutcome {
        let capture = match destination {
            ReadTarget::Capture(capture) => Some(capture),
            _ => None,
        };
        if self
            .ancestor
            .as_ref()
            .is_some_and(|ancestor| !ancestor.enabled(capture))
        {
            return unavailable("Ancestor tool authority is no longer live");
        }
        let cancellation = CommandCancellation::new();
        let (invocation, request_id, browser_epoch) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.revoked || state.pending.len() >= 8 {
                return unavailable("Tool authority is revoked or busy");
            }
            let mut invocation = match self.read_invocation(&state, destination) {
                Ok(invocation) => invocation,
                Err(message) => return unavailable(&message),
            };
            invocation.caller = self.caller;
            let request_id = state.next_request;
            let Some(next) = request_id.checked_add(1) else {
                return unavailable("Tool request IDs are exhausted");
            };
            state.next_request = next;
            state.pending.push((request_id, cancellation.clone()));
            if capture == Some(ToolCapture::Browser) {
                state.browser_requests.push(request_id);
            }
            let browser_epoch = state.browser_epoch;
            drop(state);
            (invocation, request_id, browser_epoch)
        };
        let ancestor_request = match self
            .ancestor
            .as_ref()
            .map(|ancestor| ancestor.begin_descendant_read(cancellation.clone()))
        {
            Some(Ok(id)) => Some(id),
            Some(Err(message)) => {
                self.finish_request(request_id);
                return unavailable(&message);
            }
            None => None,
        };
        let outcome = commands.execute(invocation, deadline, cancellation.clone());
        if let Some((ancestor, id)) = self.ancestor.as_ref().zip(ancestor_request) {
            ancestor.finish_request(id);
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.pending.retain(|(id, _)| *id != request_id);
        state.browser_requests.retain(|id| *id != request_id);
        let enabled = match capture {
            None => state.policy.own_terminal_read,
            Some(ToolCapture::Browser) => state.policy.browser_capture,
            Some(ToolCapture::Computer) => state.policy.computer_capture,
        };
        let revoked = state.revoked
            || !enabled
            || cancellation.is_cancelled()
            || capture == Some(ToolCapture::Browser) && state.browser_epoch != browser_epoch;
        drop(state);
        if revoked
            || self
                .ancestor
                .as_ref()
                .is_some_and(|ancestor| !ancestor.enabled(capture))
        {
            unavailable("Tool authority was revoked during the read")
        } else {
            outcome
        }
    }

    fn read_invocation(
        &self,
        state: &GrantState,
        destination: ReadTarget,
    ) -> Result<CommandInvocation, String> {
        let terminal = state
            .terminal
            .clone()
            .ok_or("The launched terminal is not registered yet")?;
        if !matches!(destination, ReadTarget::Capture(_)) && !state.policy.own_terminal_read {
            return Err("Reads are disabled for this launch".into());
        }
        let (command, arguments, target) = match destination {
            ReadTarget::Workspace(read) => (read.command(), Vec::new(), self.scope.binding.clone()),
            ReadTarget::Capture(capture) => {
                let enabled = match capture {
                    ToolCapture::Browser => state.policy.browser_capture,
                    ToolCapture::Computer => state.policy.computer_capture,
                };
                if capture == ToolCapture::Browser {
                    return state
                        .browser
                        .as_ref()
                        .filter(|_| enabled)
                        .map(|browser| browser.invocation(self.caller))
                        .ok_or_else(|| "Attach a browser document before reading it".into());
                }
                return self
                    .captured(capture)
                    .filter(|_| enabled)
                    .map(|captured| captured.invocation.clone())
                    .ok_or_else(|| "Capture is not enabled for this launch".into());
            }
            ReadTarget::Terminal(arguments) => ("terminal.capture", arguments, terminal),
            ReadTarget::Native(read) => {
                let target = state
                    .native_session
                    .clone()
                    .ok_or("No native conversation is attached to this launch")?;
                if matches!(read, NativeToolRead::Agents) {
                    return Ok(CommandInvocation {
                        target: Some(self.scope.binding.clone()),
                        ..CommandInvocation::new(read.command(), Vec::new(), self.caller)
                    });
                }
                let mut arguments = vec![target.handle.clone(), target.generation.to_string()];
                if let NativeToolRead::Activity { limit } = read {
                    arguments.push(limit.to_string());
                }
                (read.command(), arguments, target)
            }
        };
        let mut invocation = CommandInvocation::new(command, arguments, self.caller);
        invocation.target = Some(target);
        Ok(invocation)
    }

    #[must_use]
    pub fn spawn_enabled(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        !state.revoked
            && state.policy.spawn_children
            && self.spawn.is_some()
            && self.ancestor.is_none()
    }

    /// # Errors
    /// Requires a live registered parent, explicit spawn grant and its captured provider/profile.
    pub fn authorize_spawn(
        &self,
        request: &ToolSpawnRequest,
    ) -> Result<ToolChildAuthority, String> {
        let state = self.spawn_state(request)?;
        drop(state);
        Ok(ToolChildAuthority {
            parent: self.clone(),
        })
    }

    /// # Errors
    /// Rejects invalid/stale/revoked parents or a full pending request catalog before queueing.
    pub fn begin_spawn(&self, request: &ToolSpawnRequest) -> Result<ToolSpawnGuard, String> {
        self.begin_spawn_with_cancellation(request, CommandCancellation::new())
    }

    /// Track the incoming pending command token through final child creation acceptance.
    /// # Errors
    /// Rejects cancelled commands, invalid/stale/revoked parents or a full pending catalog.
    pub fn begin_spawn_with_cancellation(
        &self,
        request: &ToolSpawnRequest,
        cancellation: CommandCancellation,
    ) -> Result<ToolSpawnGuard, String> {
        drop(self.spawn_state(request)?);
        self.begin_child_operation(cancellation)
    }

    /// Track supervision under the same grant that created the child.
    /// # Errors
    /// Rejects non-native, revoked, attenuated, unregistered or busy parents.
    pub fn begin_child_control(
        &self,
        cancellation: CommandCancellation,
    ) -> Result<ToolSpawnGuard, String> {
        if self.native_session_target().is_none() {
            return Err("Child supervision requires a live native parent".into());
        }
        self.begin_child_operation(cancellation)
    }

    fn begin_child_operation(
        &self,
        cancellation: CommandCancellation,
    ) -> Result<ToolSpawnGuard, String> {
        if cancellation.is_cancelled() {
            return Err("Child operation was cancelled before acceptance".to_owned());
        }
        let mut state = self.parent_state()?;
        if state.pending.len() >= 8 {
            return Err("Parent tool authority is busy".to_owned());
        }
        let request_id = state.next_request;
        state.next_request = request_id
            .checked_add(1)
            .ok_or("Tool request IDs are exhausted")?;
        state.pending.push((request_id, cancellation.clone()));
        drop(state);
        Ok(ToolSpawnGuard {
            parent: self.clone(),
            request_id,
            cancellation,
        })
    }

    fn spawn_state(
        &self,
        request: &ToolSpawnRequest,
    ) -> Result<MutexGuard<'_, GrantState>, String> {
        request.validate()?;
        let spawn = self
            .spawn
            .as_ref()
            .ok_or("Spawning is unavailable for this launch")?;
        request.validate_parent(self.scope.provider, spawn.profile.as_deref())?;
        let state = self.parent_state()?;
        // At most 128 shells per live grant, including queued creations. Resuming never
        // recovers write authority from history; it must come from a new accepted creation.
        if matches!(request, ToolSpawnRequest::Shell { .. })
            && state
                .spawned_terminals
                .len()
                .saturating_add(state.pending.len())
                >= 128
        {
            return Err("This parent grant has reached its child terminal limit".into());
        }
        Ok(state)
    }

    fn parent_state(&self) -> Result<MutexGuard<'_, GrantState>, String> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked
            || !state.policy.spawn_children
            || state.terminal.is_none()
            || self.ancestor.is_some()
            || self.spawn.is_none()
        {
            return Err("Parent spawn authority is no longer live".to_owned());
        }
        Ok(state)
    }

    pub(crate) fn invoke_child_control(
        &self,
        request: &crate::ToolChildControlRequest,
        commands: &dyn AgentCommandExecutor,
        deadline: Instant,
    ) -> CommandOutcome {
        if let Err(message) = request.validate() {
            return unavailable(&message);
        }
        let guard = match self.begin_child_control(CommandCancellation::new()) {
            Ok(guard) => guard,
            Err(message) => return unavailable(&message),
        };
        let Some(parent) = self.native_session_target() else {
            return unavailable("Native parent is no longer live");
        };
        let Ok(encoded) = serde_json::to_string(request) else {
            return unavailable("Child control request cannot be encoded");
        };
        let mut invocation = CommandInvocation::new(
            "agents.native.control-child",
            vec![
                parent.handle.clone(),
                parent.generation.to_string(),
                encoded,
                self.attachment_id.to_string(),
            ],
            self.caller,
        );
        invocation.target = Some(parent);
        commands.execute_pending(invocation, deadline, guard.cancellation())
    }

    pub(crate) fn invoke_spawn(
        &self,
        request: &ToolSpawnRequest,
        commands: &dyn AgentCommandExecutor,
        deadline: Instant,
    ) -> CommandOutcome {
        let guard = match self.begin_spawn(request) {
            Ok(guard) => guard,
            Err(message) => return unavailable(&message),
        };
        let Ok(encoded) = serde_json::to_string(request) else {
            return unavailable("Spawn request cannot be encoded");
        };
        let mut invocation = CommandInvocation::new(
            "agents.spawn",
            vec![encoded, self.attachment_id.to_string()],
            self.caller,
        );
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked || !state.policy.spawn_children {
            return unavailable("Parent spawn authority is no longer live");
        }
        let Some(terminal) = state.terminal.clone() else {
            return unavailable("The parent terminal is not registered");
        };
        drop(state);
        if let Some(native) = self.native_session_target() {
            "agents.native.spawn".clone_into(&mut invocation.command);
            invocation
                .arguments
                .insert(0, native.generation.to_string());
            invocation.arguments.insert(0, native.handle.clone());
            invocation.target = Some(native);
        } else {
            invocation.target = Some(terminal);
        }
        // The host repeats authorization immediately before mutation. Once accepted, preserve the
        // issued child IDs even when later revocation disables its tools; never hide a created child.
        let outcome = commands.execute_pending(invocation, deadline, guard.cancellation());
        if matches!(request, ToolSpawnRequest::Shell { .. })
            && let CommandOutcome::Success { value, .. } = &outcome
            && value
                .get("created")
                .and_then(|created| serde_json::from_value::<CommandTarget>(created.clone()).ok())
                .is_some_and(|created| {
                    created.kind == ResourceKind::Session && created.generation > 0
                })
            && let Some(terminal) = value
                .get("terminal")
                .and_then(|value| serde_json::from_value::<CommandTarget>(value.clone()).ok())
            && terminal.kind == ResourceKind::Terminal
            && terminal.generation > 0
            && !terminal.handle.is_empty()
            && terminal.handle.len() <= 8192
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if !state.revoked
                && state.policy.spawn_children
                && state.terminal.as_ref() != Some(&terminal)
                && !state.spawned_terminals.contains(&terminal)
                && state.spawned_terminals.len() < 128
            {
                state.spawned_terminals.push(terminal);
            }
        }
        drop(guard);
        outcome
    }

    pub(crate) fn invoke_spawned_terminal(
        &self,
        request: &crate::ToolTerminalRequest,
        commands: &dyn AgentCommandExecutor,
        deadline: Instant,
    ) -> CommandOutcome {
        use crate::ToolTerminalOperation;
        if let Err(message) = request.validate() {
            return unavailable(&message);
        }
        let guard = match self.begin_child_operation(CommandCancellation::new()) {
            Ok(guard) => guard,
            Err(message) => return unavailable(&message),
        };
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked
            || !state.policy.spawn_children
            || !state.spawned_terminals.contains(&request.terminal)
        {
            return unavailable("This terminal was not created under the current parent grant");
        }
        drop(state);
        let (command, arguments) = match request.operation {
            ToolTerminalOperation::Read => (
                "terminal.capture",
                vec!["plain".into(), "screen".into(), "128".into()],
            ),
            ToolTerminalOperation::Paste => (
                "terminal.paste",
                vec![request.text.clone().unwrap_or_default()],
            ),
            ToolTerminalOperation::Submit => ("terminal.submit", Vec::new()),
            ToolTerminalOperation::Interrupt => ("terminal.write", vec!["\u{3}".into()]),
            ToolTerminalOperation::Close => ("pane.close", Vec::new()),
        };
        let mut invocation = CommandInvocation::new(command, arguments, self.caller);
        invocation.target = Some(request.terminal.clone());
        if matches!(request.operation, ToolTerminalOperation::Close) {
            // The exact child and operation were authorized by the live parent grant.
            invocation.confirmation = Some(invocation.confirmation());
        }
        let outcome = commands.execute_pending(invocation, deadline, guard.cancellation());
        if matches!(request.operation, ToolTerminalOperation::Read) && !self.spawn_enabled() {
            return unavailable("Terminal read authority was withdrawn");
        }
        if matches!(request.operation, ToolTerminalOperation::Close)
            && matches!(outcome, CommandOutcome::Success { .. })
        {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .spawned_terminals
                .retain(|terminal| terminal != &request.terminal);
        }
        outcome
    }

    fn begin_descendant_read(&self, cancellation: CommandCancellation) -> Result<u64, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.revoked || !state.policy.own_terminal_read || state.pending.len() >= 8 {
            return Err("Ancestor tool authority is revoked or busy".to_owned());
        }
        let id = state.next_request;
        state.next_request = id.checked_add(1).ok_or("Tool request IDs are exhausted")?;
        state.pending.push((id, cancellation));
        drop(state);
        Ok(id)
    }

    fn finish_request(&self, request_id: u64) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.pending.retain(|(id, _)| *id != request_id);
        state.browser_requests.retain(|id| *id != request_id);
        state.application_requests.retain(|id| *id != request_id);
    }

    pub(crate) fn revoked(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .revoked
            || self
                .ancestor
                .as_ref()
                .is_some_and(|ancestor| ancestor.revoked())
    }

    fn captured(&self, capture: ToolCapture) -> Option<&ToolCapturedCommand> {
        self.captures
            .iter()
            .find(|candidate| candidate.capture == capture)
    }
}

fn unavailable(message: &str) -> CommandOutcome {
    CommandOutcome::Unavailable {
        message: message.to_owned(),
    }
}
