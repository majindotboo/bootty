use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};

use bootty_config::config::RemoteConfig;
use bootty_git::{self as project, ProjectPickerEntry, WorktreePickerEntry};
use bootty_host::{CancellableCommandRunner, CommandCancellation};
use bootty_mux::controller::RepaintHandle;

use crate::error_catalog::ErrorNotice;

pub enum NewSessionEffect {
    ListProjects,
    ListWorktrees(String, Vec<String>),
    ToggleFavorite(String),
    CreateWorktree(String, bootty_git::WorktreeRequest),
}

pub enum NewSessionOutcome {
    Projects(Vec<ProjectPickerEntry>),
    Worktrees(Vec<WorktreePickerEntry>),
    Favorite { path: String, favorite: bool },
    CreatedWorktree(String),
}

#[derive(Clone)]
enum NewSessionTarget {
    Local { home: Option<PathBuf> },
    Remote(RemoteConfig),
}

pub struct NewSessionWorker {
    target: NewSessionTarget,
    repaint: RepaintHandle,
    task: Option<NewSessionTask>,
}

impl NewSessionWorker {
    pub(crate) fn local(repaint: RepaintHandle) -> Self {
        let mut owner = Self {
            target: NewSessionTarget::Local {
                home: project::home_dir(),
            },
            repaint,
            task: None,
        };
        owner.start(NewSessionEffect::ListProjects);
        owner
    }

    pub(crate) fn remote(remote: RemoteConfig, repaint: RepaintHandle) -> Self {
        let mut owner = Self {
            target: NewSessionTarget::Remote(remote),
            repaint,
            task: None,
        };
        owner.start(NewSessionEffect::ListProjects);
        owner
    }

    pub(crate) const fn is_busy(&self) -> bool {
        self.task.is_some()
    }

    pub(crate) const fn is_remote(&self) -> bool {
        matches!(&self.target, NewSessionTarget::Remote(_))
    }

    pub(crate) fn start(&mut self, effect: NewSessionEffect) {
        let (sender, receiver) = mpsc::channel();
        let cancellation = CommandCancellation::default();
        let runner = CancellableCommandRunner::new(cancellation.clone());
        let repaint = self.repaint.clone();
        let target = self.target.clone();
        self.task = Some(NewSessionTask {
            receiver,
            cancellation,
        });
        let Some(permit) = NewSessionWorkerPermit::acquire() else {
            let error = if self.is_remote() {
                ErrorNotice::RemoteProjectOperationStopping.to_string()
            } else {
                "the previous local project operation is still stopping".to_owned()
            };
            let _ = sender.send(Err(error));
            repaint();
            return;
        };
        std::thread::spawn(move || {
            let _permit = permit;
            let result = run_effect(&target, effect, &runner).map_err(|error| error.to_string());
            let _ = sender.send(result);
            repaint();
        });
    }

    pub(crate) fn poll(&mut self) -> Option<Result<NewSessionOutcome, String>> {
        let result = match self.task.as_ref()?.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                let error = if self.is_remote() {
                    ErrorNotice::RemoteProjectTaskStopped.to_string()
                } else {
                    "local project task stopped".to_owned()
                };
                Err(error)
            }
        };
        self.task = None;
        Some(result)
    }
}

fn run_effect(
    target: &NewSessionTarget,
    effect: NewSessionEffect,
    runner: &CancellableCommandRunner,
) -> Result<NewSessionOutcome, anyhow::Error> {
    Ok(match (target, effect) {
        (NewSessionTarget::Local { home }, NewSessionEffect::ListProjects) => {
            NewSessionOutcome::Projects(project::discover_project_picker_entries(home.as_deref()))
        }
        (NewSessionTarget::Local { .. }, NewSessionEffect::ListWorktrees(project, open_cwds)) => {
            let mut worktrees = project::discover_worktree_picker_entries(&project);
            project::mark_occupied_worktrees(&mut worktrees, &open_cwds);
            NewSessionOutcome::Worktrees(worktrees)
        }
        (NewSessionTarget::Local { home }, NewSessionEffect::ToggleFavorite(path)) => {
            let favorite = project::toggle_favorite_project_path(home.as_deref(), &path)?;
            NewSessionOutcome::Favorite { path, favorite }
        }
        (NewSessionTarget::Local { .. }, NewSessionEffect::CreateWorktree(project, request)) => {
            NewSessionOutcome::CreatedWorktree(
                project::Git::new()
                    .create_worktree(&project, &request)
                    .map_err(anyhow::Error::msg)?,
            )
        }
        (NewSessionTarget::Remote(remote), NewSessionEffect::ListProjects) => {
            NewSessionOutcome::Projects(bootty_mux::remote_space::list_remote_projects_with_runner(
                remote, runner,
            )?)
        }
        (NewSessionTarget::Remote(remote), NewSessionEffect::ListWorktrees(project, open_cwds)) => {
            NewSessionOutcome::Worktrees(
                bootty_mux::remote_space::list_remote_worktrees_with_runner(
                    remote, &project, &open_cwds, runner,
                )?,
            )
        }
        (NewSessionTarget::Remote(remote), NewSessionEffect::ToggleFavorite(path)) => {
            let favorite = bootty_mux::remote_space::toggle_remote_project_favorite_with_runner(
                remote, &path, runner,
            )?;
            NewSessionOutcome::Favorite { path, favorite }
        }
        (NewSessionTarget::Remote(remote), NewSessionEffect::CreateWorktree(project, request)) => {
            NewSessionOutcome::CreatedWorktree(
                bootty_mux::remote_space::create_remote_worktree_request_with_runner(
                    // Once started, observe completion rather than killing a Git mutation.
                    remote,
                    &project,
                    &request,
                    &bootty_host::SystemCommandRunner,
                )?,
            )
        }
    })
}

struct NewSessionTask {
    receiver: Receiver<Result<NewSessionOutcome, String>>,
    cancellation: CommandCancellation,
}

impl Drop for NewSessionTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

static NEW_SESSION_WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

struct NewSessionWorkerPermit;

impl NewSessionWorkerPermit {
    fn acquire() -> Option<Self> {
        (!NEW_SESSION_WORKER_ACTIVE.swap(true, Ordering::AcqRel)).then_some(Self)
    }
}

impl Drop for NewSessionWorkerPermit {
    fn drop(&mut self) {
        NEW_SESSION_WORKER_ACTIVE.store(false, Ordering::Release);
    }
}
