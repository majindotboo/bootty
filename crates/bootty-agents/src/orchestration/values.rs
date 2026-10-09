use std::collections::BTreeSet;

use bootty_control::{Caller, CommandTarget, ResourceKind};
use serde::{Deserialize, Serialize};

use crate::{AgentKind, AgentLaunch};

pub const MAX_RUN_NODES: usize = 32;
pub const MAX_SAVED_RUNS: usize = 64;
pub const MAX_RUN_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Host-captured authority. Wire data cannot deserialize this into a live launch context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrchestrationContext {
    binding: CommandTarget,
    caller: Caller,
    destination: Option<OrchestrationDestination>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct OrchestrationDestination {
    window: String,
    binding_id: String,
}

impl OrchestrationContext {
    /// # Errors
    /// Rejects a missing, invalid or non-Binding host target.
    pub fn capture(binding: CommandTarget, caller: Caller) -> Result<Self, String> {
        validate_target(&binding, ResourceKind::Binding)?;
        Ok(Self {
            binding,
            caller,
            destination: None,
        })
    }

    /// Capture the durable host destination separately from its opaque live target.
    /// # Errors
    /// Rejects invalid targets or unbounded host destination identifiers.
    pub fn capture_destination(
        binding: CommandTarget,
        caller: Caller,
        window: String,
        binding_id: String,
    ) -> Result<Self, String> {
        let mut context = Self::capture(binding, caller)?;
        bounded_text(&window, 256, "Host window")?;
        bounded_text(&binding_id, 256, "Host Binding identity")?;
        context.destination = Some(OrchestrationDestination { window, binding_id });
        Ok(context)
    }

    #[must_use]
    pub fn destination(&self) -> Option<(&str, &str)> {
        self.destination
            .as_ref()
            .map(|destination| (destination.window.as_str(), destination.binding_id.as_str()))
    }

    #[must_use]
    pub const fn binding(&self) -> &CommandTarget {
        &self.binding
    }

    #[must_use]
    pub const fn caller(&self) -> Caller {
        self.caller
    }
}

/// Frozen provider preferences supplied by the host, never by a node's wire request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrchestrationLaunch {
    provider: AgentKind,
    profile: Option<String>,
    launch: AgentLaunch,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_identity: Option<String>,
}

impl OrchestrationLaunch {
    /// # Errors
    /// Rejects invalid host launch preferences or an oversized profile name.
    pub fn capture(
        provider: AgentKind,
        profile: Option<String>,
        launch: AgentLaunch,
    ) -> Result<Self, String> {
        launch.validate()?;
        if let Some(profile) = &profile {
            bounded_text(profile, 256, "Profile")?;
        }
        Ok(Self {
            provider,
            profile,
            launch,
            task_identity: None,
        })
    }

    /// Capture native work inside an existing saved task; retries keep this destination.
    /// # Errors
    /// Rejects unsupported native providers, invalid configuration or task identity.
    pub fn capture_for_task(
        provider: AgentKind,
        profile: Option<String>,
        launch: AgentLaunch,
        task_identity: String,
    ) -> Result<Self, String> {
        bounded_text(&task_identity, 8192, "Saved task identity")?;
        if launch.account_directory.is_none() {
            return Err("Native work requires its captured account directory".to_owned());
        }
        crate::NativeSessionConfig::from_launch(provider, launch.clone())?;
        let mut captured = Self::capture(provider, profile, launch)?;
        captured.task_identity = Some(task_identity);
        Ok(captured)
    }

    #[must_use]
    pub fn task_identity(&self) -> Option<&str> {
        self.task_identity.as_deref()
    }

    #[must_use]
    pub const fn target_kind(&self) -> ResourceKind {
        if self.task_identity.is_some() {
            ResourceKind::Session
        } else {
            ResourceKind::Terminal
        }
    }

    #[must_use]
    pub const fn provider(&self) -> AgentKind {
        self.provider
    }

    #[must_use]
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    #[must_use]
    pub const fn launch(&self) -> &AgentLaunch {
        &self.launch
    }
}

/// A literal prompt, separate from provider argv and host authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct AgentPrompt(String);

impl AgentPrompt {
    /// # Errors
    /// Rejects empty prompts, NUL, or prompts larger than the provider launch bound.
    pub fn new(text: String) -> Result<Self, String> {
        // 8 KiB matches AgentLaunch's argument bound; raise both only with provider support.
        if text.trim().is_empty() || text.len() > 8192 || text.contains('\0') {
            return Err("Agent prompt must be nonempty, at most 8 KiB, and without NUL".to_owned());
        }
        Ok(Self(text))
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrchestrationNodeSpec {
    pub id: String,
    pub title: String,
    pub dependencies: Vec<String>,
    pub prompt: AgentPrompt,
    pub launch: OrchestrationLaunch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrchestrationPlan {
    nodes: Vec<OrchestrationNodeSpec>,
}

impl OrchestrationPlan {
    /// # Errors
    /// Rejects oversized plans, duplicate/missing dependencies and dependency cycles.
    pub fn new(nodes: Vec<OrchestrationNodeSpec>) -> Result<Self, String> {
        if nodes.is_empty() || nodes.len() > MAX_RUN_NODES {
            return Err("An orchestration run requires 1 to 32 nodes".to_owned());
        }
        let mut ids = BTreeSet::new();
        for node in &nodes {
            validate_id(&node.id)?;
            bounded_text(&node.title, 256, "Node title")?;
            if !ids.insert(node.id.as_str()) {
                return Err("Orchestration node IDs must be unique".to_owned());
            }
        }
        for node in &nodes {
            let mut dependencies = BTreeSet::new();
            if node.dependencies.len() > MAX_RUN_NODES {
                return Err("A node accepts at most 32 dependencies".to_owned());
            }
            for dependency in &node.dependencies {
                if dependency == &node.id
                    || !ids.contains(dependency.as_str())
                    || !dependencies.insert(dependency)
                {
                    return Err(
                        "Dependencies must be unique existing nodes other than self".to_owned()
                    );
                }
            }
        }
        let mut completed = BTreeSet::new();
        while completed.len() < nodes.len() {
            let before = completed.len();
            for node in &nodes {
                if node.dependencies.iter().all(|id| completed.contains(id)) {
                    completed.insert(node.id.clone());
                }
            }
            if completed.len() == before {
                return Err("Orchestration dependencies contain a cycle".to_owned());
            }
        }
        Ok(Self { nodes })
    }

    #[must_use]
    pub fn nodes(&self) -> &[OrchestrationNodeSpec] {
        &self.nodes
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrchestrationNodeState {
    Pending,
    Claimed,
    Running {
        target: CommandTarget,
    },
    Succeeded {
        target: CommandTarget,
    },
    Failed {
        target: Option<CommandTarget>,
        message: String,
    },
    Interrupted {
        target: Option<CommandTarget>,
    },
    Cancelled {
        target: Option<CommandTarget>,
    },
}

impl OrchestrationNodeState {
    #[must_use]
    pub const fn target(&self) -> Option<&CommandTarget> {
        match self {
            Self::Running { target } | Self::Succeeded { target } => Some(target),
            Self::Failed { target, .. }
            | Self::Interrupted { target }
            | Self::Cancelled { target } => target.as_ref(),
            Self::Pending | Self::Claimed => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrchestrationNode {
    pub spec: OrchestrationNodeSpec,
    pub attempt: u64,
    pub state: OrchestrationNodeState,
    /// Exact accepted first provider turn; absent for terminal work and unreconciled legacy work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_turn_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrchestrationRun {
    pub id: String,
    pub generation: u64,
    pub context: OrchestrationContext,
    pub cancelled: bool,
    pub nodes: Vec<OrchestrationNode>,
}

impl OrchestrationRun {
    #[must_use]
    pub fn ready_nodes(&self) -> Vec<&OrchestrationNode> {
        if self.cancelled {
            return Vec::new();
        }
        self.nodes
            .iter()
            .filter(|node| {
                node.state == OrchestrationNodeState::Pending
                    && node.spec.dependencies.iter().all(|id| {
                        self.nodes.iter().any(|dependency| {
                            &dependency.spec.id == id
                                && matches!(
                                    dependency.state,
                                    OrchestrationNodeState::Succeeded { .. }
                                )
                        })
                    })
            })
            .collect()
    }
}

/// Opaque dispatch identity; only the durable run owner issues it after commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrchestrationToken {
    pub(super) run_id: String,
    pub(super) generation: u64,
    pub(super) node_id: String,
    pub(super) attempt: u64,
}

impl OrchestrationToken {
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    #[must_use]
    pub const fn attempt(&self) -> u64 {
        self.attempt
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrchestrationDispatch {
    pub token: OrchestrationToken,
    pub title: String,
    pub context: OrchestrationContext,
    pub launch: OrchestrationLaunch,
    pub prompt: AgentPrompt,
}

/// Supplied only by the trusted terminal/provider owner after a completed turn, error or exit.
/// Terminal text, idle state and unknown observations cannot establish success.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum OrchestrationOutcome {
    Succeeded,
    Failed { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrchestrationSnapshot {
    pub revision: u64,
    pub runs: Vec<OrchestrationRun>,
    pub durability_warning: Option<String>,
}

/// # Errors
/// Rejects empty, oversized or non-identifier run/node IDs.
pub fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "Run and node IDs require 1 to 128 ASCII letters, digits, '-' or '_'".to_owned(),
        );
    }
    Ok(())
}

/// # Errors
/// Rejects empty, oversized or control-bearing metadata.
pub fn bounded_text(text: &str, maximum: usize, name: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > maximum || text.chars().any(char::is_control) {
        return Err(format!(
            "{name} must be nonempty, bounded and without control characters"
        ));
    }
    Ok(())
}

/// # Errors
/// Rejects a wrong resource kind, zero generation or invalid opaque host handle.
pub fn validate_target(target: &CommandTarget, kind: ResourceKind) -> Result<(), String> {
    if target.kind != kind || target.generation == 0 {
        return Err("Orchestration requires an exact generation-scoped host target".to_owned());
    }
    bounded_text(&target.handle, 8192, "Host target")
}
