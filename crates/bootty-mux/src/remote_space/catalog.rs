use anyhow::{Result, bail};
use bootty_config::config::{
    BoottyConfig, MultiplexerBackendConfig, SshProfileConfig, SshRemoteConfig,
};
use bootty_git::{ProjectPickerEntry, WorktreePickerEntry};
use bootty_host::ssh::{SshRemote, remote_daemon_failure};
use bootty_host::{CommandRunner, SystemCommandRunner};

use crate::repository::{
    DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON, SpaceMuxOverride, SpaceRemoteOverride,
    WorkspaceBinding, WorkspaceRepository,
};
use crate::{
    RemoteSpaceSummary, command::MuxCommand, provider::MuxBackendRegistry, snapshot::MuxSnapshot,
};

/// Wire version used by the remote Space catalog and command endpoint.
pub const REMOTE_SPACE_CATALOG_VERSION: u32 = crate::remote_catalog::CATALOG_VERSION;

pub fn list(config: &BoottyConfig) -> Result<Vec<RemoteSpaceSummary>> {
    let (_, snapshot) = WorkspaceRepository::open(&config.config_path)?;
    Ok(snapshot
        .spaces()
        .iter()
        .filter_map(|space| {
            let binding = space.binding();
            if !binding_is_local(binding, config) {
                return None;
            }
            let backend = binding
                .backend_override()
                .unwrap_or(config.multiplexer.backend);
            supports_remote_space_catalog(backend).then(|| RemoteSpaceSummary {
                catalog_version: REMOTE_SPACE_CATALOG_VERSION,
                id: space.remote_id().to_owned(),
                name: space.name().to_owned(),
                backend,
            })
        })
        .collect())
}

pub fn create(
    config: &BoottyConfig,
    name: &str,
    backend: MultiplexerBackendConfig,
) -> Result<RemoteSpaceSummary> {
    if !supports_remote_space_catalog(backend) {
        bail!("remote Spaces need tmux or rmux")
    }
    let (mut repository, _) = WorkspaceRepository::open(&config.config_path)?;
    let space = repository
        .create_space(
            name,
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(backend),
                remote: SpaceRemoteOverride::Local,
            },
            config.multiplexer.hide_tmux_status,
        )?
        .ok_or_else(|| anyhow::anyhow!("remote Space name cannot be empty"))?;
    Ok(RemoteSpaceSummary {
        catalog_version: REMOTE_SPACE_CATALOG_VERSION,
        id: space.remote_id().to_owned(),
        name: space.name().to_owned(),
        backend,
    })
}

fn supports_remote_space_catalog(backend: MultiplexerBackendConfig) -> bool {
    matches!(
        backend,
        MultiplexerBackendConfig::Rmux | MultiplexerBackendConfig::Tmux
    )
}

/// The sessions this remote Space holds: the ones carrying its `@bootty_space` tag. The client
/// wrote that tag and can read it back, so there is no second copy to keep in step.
pub fn snapshot(
    config: &BoottyConfig,
    backends: &MuxBackendRegistry,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
) -> Result<MuxSnapshot> {
    let runtime = remote_space_runtime(config, backends, space_id, expected_backend)?;
    Ok(filter_snapshot_for_space(
        runtime.backend.snapshot()?,
        space_id,
    ))
}

fn filter_snapshot_for_space(mut snapshot: MuxSnapshot, space_id: &str) -> MuxSnapshot {
    snapshot
        .sessions
        .retain(|session| session.tag.space.as_deref() == Some(space_id));
    snapshot.active_session_id = snapshot
        .active_session_id
        .filter(|id| snapshot.sessions.iter().any(|session| &session.id == id));
    snapshot
}

pub fn execute(
    config: &BoottyConfig,
    backends: &MuxBackendRegistry,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
    payload: &str,
) -> Result<()> {
    let command = crate::remote_space::decode_command(payload)?;
    let mut runtime = remote_space_runtime(config, backends, space_id, expected_backend)?;
    // A command may only touch a session this Space holds. Asking the session itself is the whole
    // check: no ownership table to consult, and no way for the answer to drift from the truth.
    if let Some(session_id) = command_session_id(&command) {
        let snapshot = runtime.backend.snapshot()?;
        let session = snapshot
            .sessions
            .iter()
            .find(|session| crate::snapshot::session_matches(session, session_id))
            .ok_or_else(|| anyhow::anyhow!("this session is no longer available"))?;
        if session.tag.space.as_deref() != Some(space_id) {
            bail!("session does not belong to remote Space {space_id}")
        }
    }
    runtime.backend.execute(command)
}

struct RemoteSpaceRuntime {
    backend: Box<dyn crate::backend::MuxBackend>,
}

fn remote_space_runtime(
    config: &BoottyConfig,
    backends: &MuxBackendRegistry,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
) -> Result<RemoteSpaceRuntime> {
    let (_repository, snapshot) = WorkspaceRepository::open(&config.config_path)?;
    let space = snapshot
        .spaces()
        .iter()
        .find(|space| space.remote_id() == space_id)
        .ok_or_else(|| anyhow::anyhow!("remote Space {space_id} is unavailable"))?;
    let binding = space.binding();
    if !binding_is_local(binding, config) {
        bail!("remote Space {space_id} points to another SSH host")
    }
    let mut multiplexer = config.multiplexer.clone();
    multiplexer.backend = binding
        .backend_override()
        .unwrap_or(config.multiplexer.backend);
    if multiplexer.backend != expected_backend {
        bail!(
            "Remote Space now uses {} instead of {}. Edit this Space and select it again.",
            backend_name(multiplexer.backend),
            backend_name(expected_backend),
        )
    }
    multiplexer.remote = None;
    multiplexer.remote_space_id = None;
    Ok(RemoteSpaceRuntime {
        backend: backends.build_backend(&multiplexer, Some(&config.config_path)),
    })
}

fn command_session_id(command: &MuxCommand) -> Option<&str> {
    match command {
        MuxCommand::CreateProjectSession { .. } | MuxCommand::CreateWorktreeSession { .. } => None,
        MuxCommand::ActivateWindow { session_id, .. }
        | MuxCommand::NewWindow { session_id, .. }
        | MuxCommand::RenameWindow { session_id, .. }
        | MuxCommand::ActivateNextWindow { session_id }
        | MuxCommand::ActivatePreviousWindow { session_id }
        | MuxCommand::ActivateLastWindow { session_id }
        | MuxCommand::ActivateWindowIndex { session_id, .. }
        | MuxCommand::MoveWindow { session_id, .. }
        | MuxCommand::MoveWindowPreservingSelection { session_id, .. }
        | MuxCommand::SplitPane { session_id, .. }
        | MuxCommand::SelectPane { session_id, .. }
        | MuxCommand::SelectNextPane { session_id, .. }
        | MuxCommand::SelectPreviousPane { session_id, .. }
        | MuxCommand::KillPane { session_id, .. }
        | MuxCommand::ClosePane { session_id, .. }
        | MuxCommand::TogglePaneZoom { session_id, .. }
        | MuxCommand::RenameSession { session_id, .. }
        | MuxCommand::DitchSession { session_id }
        | MuxCommand::StampSession { session_id, .. } => Some(session_id),
    }
}

fn backend_name(backend: MultiplexerBackendConfig) -> &'static str {
    match backend {
        MultiplexerBackendConfig::Herdr => "herdr",
        MultiplexerBackendConfig::Native => "native",
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
    }
}

fn binding_is_local(binding: &WorkspaceBinding, config: &BoottyConfig) -> bool {
    match binding.remote_override() {
        SpaceRemoteOverride::Local => true,
        SpaceRemoteOverride::Inherit => config.multiplexer.remote.is_none(),
        SpaceRemoteOverride::Profile(_) | SpaceRemoteOverride::Inline(_) => false,
    }
}

pub fn list_remote(profile: &SshProfileConfig) -> Result<Vec<RemoteSpaceSummary>> {
    list_remote_with_runner(profile, &SystemCommandRunner)
}

pub fn list_remote_projects_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    runner: &R,
) -> Result<Vec<ProjectPickerEntry>> {
    let output = run_remote_config(remote, &["remote-project", "list"], runner)?;
    Ok(serde_json::from_str(&output)?)
}

pub fn toggle_remote_project_favorite_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    path: &str,
    runner: &R,
) -> Result<bool> {
    let output = run_remote_config(
        remote,
        &["remote-project", "favorite", "--path", path],
        runner,
    )?;
    Ok(serde_json::from_str(&output)?)
}

pub fn list_remote_worktrees_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    project: &str,
    open_cwds: &[String],
    runner: &R,
) -> Result<Vec<WorktreePickerEntry>> {
    let mut args = vec![
        "remote-worktree".to_owned(),
        "list".to_owned(),
        "--project".to_owned(),
        project.to_owned(),
    ];
    for cwd in open_cwds {
        args.extend(["--open-cwd".to_owned(), cwd.clone()]);
    }
    let output = run_remote_config_owned(remote, &args, runner)?;
    Ok(serde_json::from_str(&output)?)
}

pub fn create_remote_worktree_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    project: &str,
    branch: &str,
    runner: &R,
) -> Result<String> {
    let output = run_remote_config(
        remote,
        &[
            "remote-worktree",
            "create",
            "--project",
            project,
            "--branch",
            branch,
        ],
        runner,
    )?;
    Ok(serde_json::from_str(&output)?)
}

pub fn list_remote_with_runner<R: CommandRunner>(
    profile: &SshProfileConfig,
    runner: &R,
) -> Result<Vec<RemoteSpaceSummary>> {
    let output = run_remote(profile, &["remote-space", "list"], runner)?;
    let spaces = serde_json::from_str::<Vec<RemoteSpaceSummary>>(&output)?;
    validate_versions(&spaces)?;
    Ok(spaces)
}

pub fn create_remote_with_runner<R: CommandRunner>(
    profile: &SshProfileConfig,
    name: &str,
    backend: MultiplexerBackendConfig,
    runner: &R,
) -> Result<RemoteSpaceSummary> {
    let backend = match backend {
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
        MultiplexerBackendConfig::Herdr | MultiplexerBackendConfig::Native => {
            bail!("remote Spaces need tmux or rmux")
        }
    };
    let output = run_remote(
        profile,
        &[
            "remote-space",
            "create",
            "--name",
            name,
            "--backend",
            backend,
        ],
        runner,
    )?;
    let space = serde_json::from_str::<RemoteSpaceSummary>(&output)?;
    validate_versions(std::slice::from_ref(&space))?;
    Ok(space)
}

fn run_remote<R: CommandRunner>(
    profile: &SshProfileConfig,
    args: &[&str],
    runner: &R,
) -> Result<String> {
    run_remote_config(&profile.to_remote(), args, runner)
}

fn run_remote_config<R: CommandRunner>(
    remote: &SshRemoteConfig,
    args: &[&str],
    runner: &R,
) -> Result<String> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    run_remote_config_owned(remote, &args, runner)
}

fn run_remote_config_owned<R: CommandRunner>(
    remote: &SshRemoteConfig,
    args: &[String],
    runner: &R,
) -> Result<String> {
    let remote = SshRemote::new(remote.clone());
    remote.ensure_daemon_with(runner)?;
    let host = remote.host().to_owned();
    let (program, args) = remote.proxy_command(bootty_host::REMOTE_DAEMON_PROGRAM, args)?;
    let output = runner.run(&program, &args)?;
    if output.success {
        return Ok(output.stdout);
    }
    bail!("{}", remote_daemon_failure(&host, &output.stderr))
}

fn validate_versions(spaces: &[RemoteSpaceSummary]) -> Result<()> {
    if let Some(space) = spaces
        .iter()
        .find(|space| space.catalog_version != REMOTE_SPACE_CATALOG_VERSION)
    {
        bail!(
            "remote Space catalog version {} is not supported",
            space.catalog_version
        )
    }
    Ok(())
}
