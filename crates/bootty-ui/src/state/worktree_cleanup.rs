//! Bootty's worktrees on each host. An hourly pass, a pass soon after a session is deleted,
//! archived or settled, and the `worktrees.clean` command all run the same cleanup.
use super::AppState;
use bootty_config::config::RemoteConfig;
use bootty_control::{CommandOutcome, CommandWarning};
use bootty_git::{CleanupReport, CleanupRequest, Retention, SessionCheckout};
use bootty_mux::session_membership::SessionLifecycle;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, PoisonError, mpsc},
    time::{Duration, Instant},
};

const PASS_INTERVAL: Duration = Duration::from_hours(1);
/// How often a host that wasn't ready is looked at again.
const CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// One host's checkouts and every session that may use them.
struct HostCleanup {
    host: String,
    remote: Option<RemoteConfig>,
    request: CleanupRequest,
}

pub type HostResults = Vec<(String, Result<CleanupReport, String>)>;

#[derive(Default)]
pub(super) struct WorktreeCleanup {
    /// When each host, by label, last had an automatic pass. A request clears it.
    passes: BTreeMap<String, Instant>,
    /// Look at the hosts on the next frame instead of the next check.
    requested: bool,
    last_check: Option<Instant>,
    /// The limit and directory the last check used; a change asks for a pass.
    settings: Option<(u16, Option<String>)>,
    running: Option<mpsc::Receiver<HostResults>>,
    /// Passes never overlap, so two can't race for the same checkout.
    exclusive: Arc<Mutex<()>>,
    /// Kept checkouts and failures already shown, so an unchanged one stays quiet.
    reported: BTreeSet<String>,
}

impl AppState {
    /// Clean every host soon, after a change that may have freed a checkout.
    pub(crate) fn request_worktree_cleanup(&mut self) {
        self.worktree_cleanup.passes.clear();
        self.worktree_cleanup.requested = true;
        (self.repaint)();
    }

    pub(super) fn poll_worktree_cleanup(&mut self, now: Instant) {
        if let Some(running) = &self.worktree_cleanup.running {
            match running.try_recv() {
                Ok(results) => {
                    self.worktree_cleanup.running = None;
                    self.report_worktree_cleanup(results);
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => self.worktree_cleanup.running = None,
            }
        }
        let session = &self.config().session;
        let settings = (session.worktree_limit, session.worktree_directory.clone());
        if self.worktree_cleanup.settings.as_ref() != Some(&settings) {
            if self.worktree_cleanup.settings.is_some() {
                self.request_worktree_cleanup();
            }
            self.worktree_cleanup.settings = Some(settings);
        }
        if !self.worktree_cleanup.requested
            && self
                .worktree_cleanup
                .last_check
                .is_some_and(|check| now.saturating_duration_since(check) < CHECK_INTERVAL)
        {
            return;
        }
        self.worktree_cleanup.requested = false;
        self.worktree_cleanup.last_check = Some(now);
        let Ok(hosts) = self.worktree_cleanup_hosts() else {
            return;
        };
        let hosts = hosts
            .into_iter()
            .filter_map(Result::ok)
            .filter(|host| {
                self.worktree_cleanup
                    .passes
                    .get(&host.host)
                    .is_none_or(|last| now.saturating_duration_since(*last) >= PASS_INTERVAL)
            })
            .collect::<Vec<_>>();
        if hosts.is_empty() {
            return;
        }
        for host in &hosts {
            self.worktree_cleanup.passes.insert(host.host.clone(), now);
        }
        self.worktree_cleanup.running = Some(self.spawn_worktree_cleanup(hosts));
    }

    fn spawn_worktree_cleanup(&self, hosts: Vec<HostCleanup>) -> mpsc::Receiver<HostResults> {
        let (sender, receiver) = mpsc::channel();
        let exclusive = Arc::clone(&self.worktree_cleanup.exclusive);
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            _ = sender.send(clean_hosts(hosts, &exclusive));
            repaint();
        });
        receiver
    }

    /// Start `worktrees.clean` on every ready host. Hosts that aren't ready come back as
    /// warnings.
    /// # Errors
    /// Returns why this app has no worktree directory of its own to clean.
    pub(crate) fn start_worktree_cleanup(
        &self,
    ) -> Result<(mpsc::Receiver<HostResults>, Vec<CommandWarning>), CommandOutcome> {
        let mut hosts = Vec::new();
        let mut warnings = Vec::new();
        let candidates =
            self.worktree_cleanup_hosts()
                .map_err(|message| CommandOutcome::Failed {
                    code: "worktree_directory_unset".to_owned(),
                    message: message.to_owned(),
                })?;
        for host in candidates {
            match host {
                Ok(host) => hosts.push(host),
                Err(host) => warnings.push(CommandWarning {
                    code: "host_not_ready".to_owned(),
                    message: format!("Skipped {host}: its backend hasn't listed its sessions yet"),
                }),
            }
        }
        Ok((self.spawn_worktree_cleanup(hosts), warnings))
    }

    /// Show what `worktrees.clean` did and report it to the caller.
    pub(crate) fn finish_worktree_cleanup(
        &mut self,
        results: HostResults,
        warnings: Vec<CommandWarning>,
    ) -> CommandOutcome {
        let mut removed = 0_usize;
        let mut details = Vec::new();
        for (host, result) in &results {
            match result {
                Ok(report) => {
                    removed = removed.saturating_add(report.removed.len());
                    for kept in &report.kept {
                        self.worktree_cleanup
                            .reported
                            .insert(format!("{}\n{}", kept.path, kept.reason));
                        details.push(format!("Kept {}. {}.", kept.path, kept.reason));
                    }
                }
                Err(error) => {
                    details.push(format!("Couldn't clean up worktrees on {host}. {error}"));
                }
            }
        }
        let summary = match removed {
            0 => "No worktrees to remove.".to_owned(),
            1 => "Removed 1 worktree.".to_owned(),
            count => format!("Removed {count} worktrees."),
        };
        self.record_message(
            std::iter::once(summary)
                .chain(details)
                .collect::<Vec<_>>()
                .join(" "),
        );
        cleanup_outcome(results, warnings)
    }

    /// One cleanup per host, from every saved session in every Space. A host whose backend
    /// hasn't listed its live sessions yet comes back as its label: its open terminals would
    /// look closed.
    ///
    /// The configured directory is always this app's. The host default is only when this is the
    /// identity's own configuration: another configuration keeps other sessions, so checkouts
    /// they use would look unused.
    fn worktree_cleanup_hosts(&self) -> Result<Vec<Result<HostCleanup, String>>, &'static str> {
        let config = &self.config().session;
        let directory = config
            .worktree_directory
            .clone()
            .filter(|directory| !directory.trim().is_empty());
        if directory.is_none()
            && self.config().config_path
                != bootty_config::ApplicationIdentity::for_process().default_config_path()
        {
            return Err(
                "This configuration keeps its own sessions. Set session.worktree-directory to clean up its worktrees.",
            );
        }
        let now = chrono::Utc::now().timestamp();
        let mut hosts = Vec::<(HostCleanup, bool)>::new();
        for binding in self.workspace.all_bindings() {
            let remote = binding.multiplexer().remote.clone();
            let ready =
                !binding.tracks_session_membership() || binding.mux().has_session_snapshot();
            let checkouts = binding_checkouts(binding, now);
            if let Some((host, host_ready)) =
                hosts.iter_mut().find(|(host, _)| host.remote == remote)
            {
                *host_ready &= ready;
                host.request.sessions.extend(checkouts);
                continue;
            }
            hosts.push((
                HostCleanup {
                    host: remote
                        .as_ref()
                        .map_or_else(|| "Local".to_owned(), RemoteConfig::label),
                    remote,
                    request: CleanupRequest {
                        directory: directory.clone(),
                        limit: config.worktree_limit,
                        sessions: checkouts.collect(),
                        now,
                    },
                },
                ready,
            ));
        }
        Ok(hosts
            .into_iter()
            .map(|(host, ready)| if ready { Ok(host) } else { Err(host.host) })
            .collect())
    }

    fn report_worktree_cleanup(&mut self, results: HostResults) {
        let mut notices = Vec::new();
        for (host, result) in results {
            match result {
                Ok(report) => {
                    for kept in report.kept {
                        if self
                            .worktree_cleanup
                            .reported
                            .insert(format!("{}\n{}", kept.path, kept.reason))
                        {
                            notices.push(format!("Kept {}. {}.", kept.path, kept.reason));
                        }
                    }
                }
                Err(error) => {
                    if self
                        .worktree_cleanup
                        .reported
                        .insert(format!("{host}\n{error}"))
                    {
                        notices.push(format!("Couldn't clean up worktrees on {host}. {error}"));
                    }
                }
            }
        }
        if !notices.is_empty() {
            self.record_message(notices.join(" "));
        }
    }
}

/// Every claim one binding's sessions make. A live terminal protects its directory whether or
/// not this app saved its session, so another client's work on a shared backend stays.
///
/// Deliberate limit: a closed session another app saved on a shared host makes no claim. Add a
/// host-side record of Bootty's sessions if two apps with one identity share hosts.
fn binding_checkouts(
    binding: &bootty_mux::workspace::BindingRuntime,
    now: i64,
) -> impl Iterator<Item = SessionCheckout> {
    let saved = binding
        .sessions()
        .sessions()
        .iter()
        .filter(|saved| !saved.state.deleted)
        .map(|saved| SessionCheckout {
            cwd: saved.cwd.clone(),
            retention: if saved.state.archived {
                Retention::Archived
            } else if saved.state.lifecycle == SessionLifecycle::Settled {
                Retention::Settled
            } else {
                Retention::Active
            },
            last_activity: saved.state.last_activity_at.unwrap_or_default(),
            open: binding.session_attachment(&saved.identity).is_some(),
        })
        .collect::<Vec<_>>();
    let live = binding
        .mux()
        .all_sessions()
        .iter()
        .flat_map(|session| {
            std::iter::once(&session.anchor)
                .chain(session.windows.iter().flat_map(|window| &window.panes))
        })
        .filter_map(|pane| pane.cwd.clone())
        .map(move |cwd| SessionCheckout {
            cwd,
            retention: Retention::Active,
            last_activity: now,
            open: true,
        })
        .collect::<Vec<_>>();
    saved.into_iter().chain(live)
}

fn clean_hosts(hosts: Vec<HostCleanup>, exclusive: &Mutex<()>) -> HostResults {
    let _pass = exclusive.lock().unwrap_or_else(PoisonError::into_inner);
    hosts
        .into_iter()
        .map(|host| {
            let result = match &host.remote {
                None => bootty_git::Git::new().clean_worktrees(&host.request),
                Some(remote) => bootty_mux::remote_space::clean_remote_worktrees_with_runner(
                    remote,
                    &host.request,
                    &bootty_host::SystemCommandRunner,
                )
                .map_err(|error| error.to_string()),
            };
            (host.host, result)
        })
        .collect()
}

/// Every host's report. A host that failed is a warning unless every host failed.
fn cleanup_outcome(results: HostResults, mut warnings: Vec<CommandWarning>) -> CommandOutcome {
    let mut hosts = Vec::new();
    let mut failures = Vec::new();
    for (host, result) in results {
        match result {
            Ok(report) => hosts.push(serde_json::json!({
                "host": host,
                "removed": report.removed,
                "kept": report.kept,
                "remaining": report.remaining,
            })),
            Err(error) => failures.push(format!("Couldn't clean up worktrees on {host}. {error}")),
        }
    }
    if hosts.is_empty() && !failures.is_empty() {
        return CommandOutcome::Failed {
            code: "worktree_cleanup_failed".to_owned(),
            message: failures.join(" "),
        };
    }
    warnings.extend(failures.into_iter().map(|message| CommandWarning {
        code: "worktree_cleanup_failed".to_owned(),
        message,
    }));
    CommandOutcome::Success {
        value: serde_json::json!({ "hosts": hosts }),
        warnings,
    }
}
