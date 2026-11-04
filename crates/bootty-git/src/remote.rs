//! Project and worktree operations on a remote host, run by the host's installed daemon.

use bootty_config::config::RemoteConfig;
use bootty_host::remote::{RemoteHost, remote_daemon_failure};

use crate::runner::CommandRunner;
use crate::{
    CleanupReport, CleanupRequest, ProjectPickerEntry, WorktreePickerEntry, WorktreeRequest,
};

/// # Errors
/// Returns remote command failures or invalid project responses.
pub fn list_projects(
    remote: &RemoteConfig,
    runner: &impl CommandRunner,
) -> Result<Vec<ProjectPickerEntry>, String> {
    parse(&run(remote, &["remote-project", "list"], runner)?)
}

/// # Errors
/// Returns remote command failures or invalid favorite responses.
pub fn toggle_project_favorite(
    remote: &RemoteConfig,
    path: &str,
    runner: &impl CommandRunner,
) -> Result<bool, String> {
    parse(&run(
        remote,
        &["remote-project", "favorite", "--path", path],
        runner,
    )?)
}

/// # Errors
/// Returns remote command failures or invalid favorite responses.
pub fn add_project_favorite(
    remote: &RemoteConfig,
    path: &str,
    runner: &impl CommandRunner,
) -> Result<bool, String> {
    parse(&run(
        remote,
        &["remote-project", "add", "--path", path],
        runner,
    )?)
}

/// # Errors
/// Returns remote command failures or invalid worktree responses.
pub fn list_worktrees(
    remote: &RemoteConfig,
    project: &str,
    open_cwds: &[String],
    runner: &impl CommandRunner,
) -> Result<Vec<WorktreePickerEntry>, String> {
    let mut args = vec!["remote-worktree", "list", "--project", project];
    for cwd in open_cwds {
        args.extend(["--open-cwd", cwd]);
    }
    parse(&run(remote, &args, runner)?)
}

/// # Errors
/// Returns remote command, worktree creation, or invalid response errors.
pub fn create_worktree(
    remote: &RemoteConfig,
    project: &str,
    request: &WorktreeRequest,
    runner: &impl CommandRunner,
) -> Result<String, String> {
    request.validate()?;
    let payload = serde_json::to_string(request).map_err(|error| error.to_string())?;
    parse(&run(
        remote,
        &[
            "remote-worktree",
            "create",
            "--project",
            project,
            "--request",
            &payload,
        ],
        runner,
    )?)
}

/// Run Bootty's worktree cleanup on the remote host that holds the checkouts.
/// # Errors
/// Returns remote command failures or an invalid cleanup report.
pub fn clean_worktrees(
    remote: &RemoteConfig,
    request: &CleanupRequest,
    runner: &impl CommandRunner,
) -> Result<CleanupReport, String> {
    let payload = serde_json::to_string(request).map_err(|error| error.to_string())?;
    parse(&run(
        remote,
        &["remote-worktree", "clean", "--request", &payload],
        runner,
    )?)
}

fn run(
    remote: &RemoteConfig,
    args: &[&str],
    runner: &impl CommandRunner,
) -> Result<String, String> {
    let remote = RemoteHost::new(remote.clone());
    remote
        .ensure_daemon_with(runner)
        .map_err(|error| error.to_string())?;
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let (program, args) = remote
        .proxy_command(bootty_host::REMOTE_DAEMON_PROGRAM, &args)
        .map_err(|error| error.to_string())?;
    let output = runner
        .run(&program, &args)
        .map_err(|error| error.to_string())?;
    if output.success {
        Ok(output.stdout)
    } else {
        Err(remote_daemon_failure(remote.host(), &output.stderr))
    }
}

fn parse<T: serde::de::DeserializeOwned>(output: &str) -> Result<T, String> {
    serde_json::from_str(output).map_err(|error| error.to_string())
}
