use std::{fs, io::Read as _, path::Path};

use bootty_control::{Caller, CommandTarget};
use bootty_write::{CommitOutcome, NewFileMode, WriteTarget};
use serde::{Deserialize, Serialize};

use super::values::{
    AgentPrompt, MAX_RUN_FILE_BYTES, MAX_RUN_NODES, MAX_SAVED_RUNS, OrchestrationContext,
    OrchestrationLaunch, OrchestrationNode, OrchestrationNodeSpec, OrchestrationNodeState,
    OrchestrationPlan, OrchestrationRun, OrchestrationSnapshot, bounded_text, validate_id,
    validate_target,
};
use crate::{AgentKind, AgentLaunch};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedContext {
    binding: CommandTarget,
    caller: Caller,
    #[serde(default)]
    destination: Option<SavedDestination>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedDestination {
    window: String,
    binding_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedLaunch {
    provider: AgentKind,
    profile: Option<String>,
    launch: AgentLaunch,
    #[serde(default)]
    task_identity: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedNodeSpec {
    id: String,
    title: String,
    dependencies: Vec<String>,
    prompt: String,
    launch: SavedLaunch,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedNode {
    spec: SavedNodeSpec,
    attempt: u64,
    state: OrchestrationNodeState,
    #[serde(default)]
    native_turn_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedRun {
    id: String,
    generation: u64,
    context: SavedContext,
    cancelled: bool,
    nodes: Vec<SavedNode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedState {
    version: u64,
    revision: u64,
    runs: Vec<SavedRun>,
}

#[derive(Serialize)]
struct SavedStateRef<'a> {
    version: u64,
    revision: u64,
    runs: &'a [OrchestrationRun],
}

fn restore_node(node: SavedNode) -> Result<OrchestrationNode, String> {
    if node.attempt == 0
        && (!matches!(
            node.state,
            OrchestrationNodeState::Pending | OrchestrationNodeState::Cancelled { target: None }
        ) || node.state.target().is_some())
    {
        return Err("A saved dispatched node requires a nonzero attempt".to_owned());
    }
    let launch = match node.spec.launch.task_identity {
        Some(task) => OrchestrationLaunch::capture_for_task(
            node.spec.launch.provider,
            node.spec.launch.profile,
            node.spec.launch.launch,
            task,
        )?,
        None => OrchestrationLaunch::capture(
            node.spec.launch.provider,
            node.spec.launch.profile,
            node.spec.launch.launch,
        )?,
    };
    if let Some(target) = node.state.target() {
        validate_target(target, launch.target_kind())?;
    }
    if let Some(id) = &node.native_turn_id {
        bounded_text(id, 8192, "Accepted native turn")?;
        if launch.target_kind() != bootty_control::ResourceKind::Session
            || node.state.target().is_none()
        {
            return Err("Saved native turn requires its exact native destination".to_owned());
        }
    } else if launch.target_kind() == bootty_control::ResourceKind::Session
        && matches!(node.state, OrchestrationNodeState::Succeeded { .. })
    {
        return Err("Native success requires an accepted first-turn receipt".to_owned());
    }
    if let OrchestrationNodeState::Failed { message, .. } = &node.state {
        bounded_text(message, 4096, "Observed failure")?;
    }
    Ok(OrchestrationNode {
        spec: OrchestrationNodeSpec {
            id: node.spec.id,
            title: node.spec.title,
            dependencies: node.spec.dependencies,
            prompt: AgentPrompt::new(node.spec.prompt)?,
            launch,
        },
        attempt: node.attempt,
        state: node.state,
        native_turn_id: node.native_turn_id,
    })
}

fn restore_run(saved: SavedRun) -> Result<OrchestrationRun, String> {
    validate_id(&saved.id)?;
    if saved.generation == 0 {
        return Err("Saved orchestration generation must be nonzero".to_owned());
    }
    if saved.nodes.is_empty() || saved.nodes.len() > MAX_RUN_NODES {
        return Err("Saved orchestration run requires 1 to 32 nodes".to_owned());
    }
    let context = match saved.context.destination {
        Some(destination) => OrchestrationContext::capture_destination(
            saved.context.binding,
            saved.context.caller,
            destination.window,
            destination.binding_id,
        )?,
        None => OrchestrationContext::capture(saved.context.binding, saved.context.caller)?,
    };
    let nodes = saved
        .nodes
        .into_iter()
        .map(restore_node)
        .collect::<Result<Vec<_>, String>>()?;
    OrchestrationPlan::new(nodes.iter().map(|node| node.spec.clone()).collect())?;
    for node in &nodes {
        let dispatched = matches!(
            node.state,
            OrchestrationNodeState::Claimed
                | OrchestrationNodeState::Running { .. }
                | OrchestrationNodeState::Succeeded { .. }
                | OrchestrationNodeState::Failed { .. }
                | OrchestrationNodeState::Interrupted { .. }
        );
        if (saved.cancelled
            && matches!(
                node.state,
                OrchestrationNodeState::Pending
                    | OrchestrationNodeState::Claimed
                    | OrchestrationNodeState::Running { .. }
            ))
            || (dispatched
                && node.spec.dependencies.iter().any(|id| {
                    !nodes.iter().any(|dependency| {
                        &dependency.spec.id == id
                            && matches!(dependency.state, OrchestrationNodeState::Succeeded { .. })
                    })
                }))
        {
            return Err(
                "Saved orchestration node lifecycle contradicts its dependencies".to_owned(),
            );
        }
    }
    Ok(OrchestrationRun {
        id: saved.id,
        generation: saved.generation,
        context,
        cancelled: saved.cancelled,
        nodes,
    })
}

/// # Errors
/// Rejects malformed, oversized or invalid persisted run authority and lifecycles.
pub fn load(path: &Path) -> Result<OrchestrationSnapshot, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OrchestrationSnapshot {
                revision: 1,
                runs: Vec::new(),
                durability_warning: None,
            });
        }
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_RUN_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).map_err(|error| error.to_string())? > MAX_RUN_FILE_BYTES {
        return Err("Orchestration state exceeds 4 MiB".to_owned());
    }
    let saved: SavedState = serde_json::from_slice(&bytes)
        .map_err(|_| "Orchestration state is malformed".to_owned())?;
    if saved.version != 1 || saved.revision == 0 || saved.runs.len() > MAX_SAVED_RUNS {
        return Err("Unsupported or oversized orchestration state".to_owned());
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut runs = Vec::new();
    for run in saved.runs {
        if !ids.insert(run.id.clone()) {
            return Err("Saved orchestration run IDs must be unique".to_owned());
        }
        runs.push(restore_run(run)?);
    }
    Ok(OrchestrationSnapshot {
        revision: saved.revision,
        runs,
        durability_warning: None,
    })
}

/// # Errors
/// Returns failed atomic commit phases; committed durability warnings remain explicit.
pub fn commit(path: &Path, snapshot: &OrchestrationSnapshot) -> Result<Option<String>, String> {
    let bytes = serde_json::to_vec(&SavedStateRef {
        version: 1,
        revision: snapshot.revision,
        runs: &snapshot.runs,
    })
    .map_err(|error| error.to_string())?;
    // 64 runs/4 MiB stay deliberately finite; raise them only with a bounded archival owner.
    if snapshot.runs.len() > MAX_SAVED_RUNS
        || u64::try_from(bytes.len()).map_err(|error| error.to_string())? > MAX_RUN_FILE_BYTES
    {
        return Err("Orchestration storage exceeds 64 runs or 4 MiB".to_owned());
    }
    #[cfg(unix)]
    if let Ok(metadata) = fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
    }
    let outcome = WriteTarget::resolve(path)
        .map_err(|error| error.into_io().to_string())?
        .lock()
        .map_err(|error| error.to_string())?
        .replace(&bytes, NewFileMode::Private)
        .map_err(|error| error.into_io().to_string())?;
    match outcome {
        CommitOutcome::Confirmed => Ok(None),
        CommitOutcome::CommittedWithDurabilityWarning(error) => Ok(Some(format!(
            "Orchestration committed with a durability warning: {error}"
        ))),
    }
}
