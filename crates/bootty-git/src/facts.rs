use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{
    runner::{CommandRunner, SystemCommandRunner},
    worktree::Git,
};

pub const MAX_WATCHED_WORKTREES: usize = 64;
pub const LIVE_FACT_INTERVAL: Duration = Duration::from_millis(250);
pub const FOCUSED_FACT_INTERVAL: Duration = Duration::from_secs(5);
pub const BACKGROUND_FACT_INTERVAL: Duration = Duration::from_secs(30);
pub const FORCED_DIFF_INTERVAL: Duration = Duration::from_mins(1);
/// Minimum gap between deep refreshes that already have an answer. This keeps a batch of
/// sessions from launching one `git diff` per row in the same frame.
pub const REFRESH_SPACING: Duration = Duration::from_millis(500);
/// The cold refresh budget per refresh frame in the original provider.
pub const COLD_REFRESH_BURST: usize = 1;
/// Minimum gap between cold refreshes. A newly visible sidebar gets one deep refresh at a time.
pub const COLD_REFRESH_SPACING: Duration = Duration::from_millis(50);
pub const FACT_CACHE_TTL: Duration = Duration::from_mins(5);

/// Git facts shared by sidebar, chrome and future panels. `cache_key` is
/// supplied by the binding owner and must include host, repository and
/// worktree identity; a cwd alone is not a safe remote identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitFacts {
    pub branch: Option<String>,
    pub branch_status: BranchStatus,
    pub diff_added: Option<u64>,
    pub diff_removed: Option<u64>,
    pub worktree_revision: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BranchStatus {
    #[default]
    Unknown,
    Current,
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSessionFactsInput {
    /// The host and target scope that owns this session. This is part of the
    /// cache identity; a remote path alone is never a valid key.
    pub scope_key: String,
    /// The mux session identity within `scope_key`.
    pub session_id: String,
    pub cwd: Option<String>,
    pub pane_pid: Option<u32>,
    pub process: Option<String>,
    pub selected: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitSessionFacts {
    pub pane_pid: Option<u32>,
    pub branch: Option<String>,
    pub branch_status: BranchStatus,
    pub diff_added: Option<u64>,
    pub diff_removed: Option<u64>,
    pub worktree_revision: u64,
    pub display_process: Option<String>,
}

#[derive(Clone, Debug)]
struct CachedFacts {
    identity: String,
    facts: GitFacts,
    seen_at: Instant,
    live_at: Option<Instant>,
    /// The last deep pass, including a pass that decided the revision was already settled.
    refreshed_at: Option<Instant>,
    diff_at: Option<Instant>,
    diff_revision: u64,
    diff_counted: bool,
    live_running: bool,
    diff_running: bool,
}

impl CachedFacts {
    fn new(identity: String, now: Instant, revision: u64) -> Self {
        Self {
            identity,
            facts: GitFacts {
                worktree_revision: revision,
                ..GitFacts::default()
            },
            seen_at: now,
            live_at: None,
            refreshed_at: None,
            diff_at: None,
            diff_revision: 0,
            diff_counted: false,
            live_running: false,
            diff_running: false,
        }
    }
}

/// A bounded background Git fact cache. Calls to [`Self::refresh`] only read
/// cached values and schedule work; they never wait for Git on the caller's
/// thread.
#[derive(Clone)]
pub struct GitFactsCache<R = SystemCommandRunner> {
    git: Git<R>,
    revisions: WorktreeRevisionCache,
    entries: Arc<Mutex<HashMap<String, CachedFacts>>>,
    schedule: Arc<Mutex<RefreshSchedule>>,
    retired: Arc<AtomicBool>,
}

#[derive(Default)]
struct RefreshSchedule {
    last_fact_refresh: Option<Instant>,
    last_cold_refresh: Option<Instant>,
    cold_frame: Option<Instant>,
    cold_refreshes: usize,
}

impl GitFactsCache<SystemCommandRunner> {
    pub fn new() -> Self {
        Self::with_runner(SystemCommandRunner)
    }
}

impl Default for GitFactsCache<SystemCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> GitFactsCache<R>
where
    R: CommandRunner + Clone + Send + Sync + 'static,
{
    pub fn with_runner(runner: R) -> Self {
        Self::with_runner_and_revisions(runner, WorktreeRevisionCache::default())
    }

    /// Construct a cache for a runner whose paths belong to another host.
    /// Remote paths must never be watched or canonicalized by the desktop.
    pub fn with_remote_runner(runner: R) -> Self {
        Self::with_runner_and_revisions(runner, WorktreeRevisionCache::disabled())
    }

    fn with_runner_and_revisions(runner: R, revisions: WorktreeRevisionCache) -> Self {
        Self {
            git: Git::with_runner(runner),
            revisions,
            entries: Arc::default(),
            schedule: Arc::default(),
            retired: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Return the latest cached facts and schedule bounded live/deep refreshes.
    /// `cache_key` is an owner-provided host/repository/worktree identity.
    pub fn refresh(&self, cache_key: &str, cwd: &str, selected: bool, now: Instant) -> GitFacts {
        let identity = format!("{cache_key}\0{cwd}");
        self.refresh_with_identity(cache_key, cwd, selected, now, identity)
    }

    fn refresh_with_identity(
        &self,
        cache_key: &str,
        cwd: &str,
        selected: bool,
        now: Instant,
        identity: String,
    ) -> GitFacts {
        if self.retired.load(Ordering::Acquire) {
            return GitFacts::default();
        }
        let revision = if cwd.is_empty() {
            0
        } else {
            let revision = self.revisions.cached_revision(cwd);
            self.revisions.ensure_watched(cwd.to_owned());
            revision
        };
        let mut start_live = false;
        let mut start_diff = false;
        let mut snapshot = GitFacts::default();
        let mut schedule = self.schedule.lock().ok();
        if let Ok(mut entries) = self.entries.lock() {
            let entry = entries
                .entry(cache_key.to_owned())
                .or_insert_with(|| CachedFacts::new(identity.clone(), now, revision));
            if entry.identity != identity {
                *entry = CachedFacts::new(identity.clone(), now, revision);
            }
            entry.seen_at = now;
            entry.facts.worktree_revision = revision;

            if !entry.live_running
                && entry
                    .live_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= LIVE_FACT_INTERVAL)
            {
                if cwd.is_empty() {
                    entry.live_at = Some(now);
                } else {
                    entry.live_running = true;
                    start_live = true;
                }
            }

            let interval = if selected {
                FOCUSED_FACT_INTERVAL
            } else {
                BACKGROUND_FACT_INTERVAL
            };
            let due = entry
                .refreshed_at
                .is_none_or(|at| now.saturating_duration_since(at) > interval);
            if !entry.diff_running && due {
                let cold = entry.refreshed_at.is_none();
                let allowed = schedule.as_ref().is_some_and(|schedule| {
                    let burst_available = !cold
                        || schedule.cold_frame != Some(now)
                        || schedule.cold_refreshes < COLD_REFRESH_BURST;
                    let last = if cold {
                        // A cold pass is also a deep pass. Use both timestamps so a caller that
                        // presents known and new sessions in one frame still starts at most one.
                        schedule.last_cold_refresh.max(schedule.last_fact_refresh)
                    } else {
                        schedule.last_fact_refresh
                    };
                    burst_available
                        && last.is_none_or(|at| {
                            now.saturating_duration_since(at)
                                >= if cold {
                                    COLD_REFRESH_SPACING
                                } else {
                                    REFRESH_SPACING
                                }
                        })
                });
                if allowed {
                    // Mark every selected pass, even one that finds an unchanged watched tree,
                    // so a due batch advances at the same bounded cadence as the Lua provider.
                    entry.refreshed_at = Some(now);
                    if let Some(schedule) = schedule.as_mut() {
                        schedule.last_fact_refresh = Some(now);
                        if cold {
                            schedule.last_cold_refresh = Some(now);
                            if schedule.cold_frame != Some(now) {
                                schedule.cold_frame = Some(now);
                                schedule.cold_refreshes = 0;
                            }
                            schedule.cold_refreshes += 1;
                        }
                    }

                    let changed =
                        !entry.diff_counted || revision == 0 || revision != entry.diff_revision;
                    let forced = entry.diff_at.is_some_and(|at| {
                        now.saturating_duration_since(at) >= FORCED_DIFF_INTERVAL
                    });
                    if !cwd.is_empty() && (changed || forced) {
                        entry.diff_running = true;
                        entry.diff_at = Some(now);
                        entry.diff_revision = revision;
                        start_diff = true;
                    }
                }
            }
            snapshot = entry.facts.clone();
        }

        if start_live {
            self.spawn_live(cache_key.to_owned(), identity.clone(), cwd.to_owned());
        }
        if start_diff {
            self.spawn_diff(cache_key.to_owned(), identity, cwd.to_owned());
        }
        snapshot
    }

    /// Refresh facts for one mux session. The caller owns target identity and
    /// supplies a host-scoped key; process text comes from the mux snapshot,
    /// so Git never shells out to rediscover it.
    pub fn refresh_session(&self, input: &GitSessionFactsInput, now: Instant) -> GitSessionFacts {
        let cache_key = format!("{}\0{}", input.scope_key, input.session_id);
        let cwd = input.cwd.as_deref().unwrap_or_default();
        // The mux snapshot has no pane id in this adapter, so the pane process identity is the
        // closest available equivalent. It prevents a completed job for a replaced pane from
        // publishing into the new pane's row while retaining one cache entry per session.
        let identity = format!("{cache_key}\0{cwd}\0{}", input.pane_pid.unwrap_or_default());
        let mut facts = self.refresh_with_identity(&cache_key, cwd, input.selected, now, identity);
        GitSessionFacts {
            pane_pid: input.pane_pid,
            branch: facts.branch.take(),
            branch_status: facts.branch_status,
            diff_added: facts.diff_added,
            diff_removed: facts.diff_removed,
            worktree_revision: facts.worktree_revision,
            display_process: input
                .process
                .as_deref()
                .filter(|process| input.session_id.starts_with('$') && !process.is_empty())
                .map(str::to_owned),
        }
    }

    pub fn get(&self, cache_key: &str, now: Instant) -> Option<GitFacts> {
        let Ok(mut entries) = self.entries.lock() else {
            return None;
        };
        let facts = entries.get_mut(cache_key)?;
        facts.seen_at = now;
        Some(facts.facts.clone())
    }

    /// Return the watcher revision already known for `cwd` and request
    /// registration if this is the first sighting. This is memory-only on the
    /// caller's thread and is useful to compatibility bindings that expose
    /// the old revision-only API.
    pub fn worktree_revision(&self, cwd: &str) -> u64 {
        let revision = self.revisions.cached_revision(cwd);
        self.revisions.ensure_watched(cwd.to_owned());
        revision
    }

    pub fn prune(&self, now: Instant) {
        if let Ok(mut entries) = self.entries.lock() {
            entries
                .retain(|_, entry| now.saturating_duration_since(entry.seen_at) <= FACT_CACHE_TTL);
        }
    }

    /// Stop scheduling work for a retired extension/window owner. In-flight
    /// commands finish under the injected runner's own cancellation policy;
    /// their results are discarded after this flag is set.
    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }

    fn spawn_live(&self, cache_key: String, identity: String, cwd: String) {
        let git = self.git.clone();
        let entries = Arc::clone(&self.entries);
        let retired = Arc::clone(&self.retired);
        thread::spawn(move || {
            if retired.load(Ordering::Acquire) {
                return;
            }
            let branch = git.head_branch(&cwd);
            if retired.load(Ordering::Acquire) {
                return;
            }
            if let Ok(mut entries) = entries.lock()
                && let Some(entry) = entries.get_mut(&cache_key)
                && entry.identity == identity
            {
                if branch.is_some() {
                    entry.facts.branch = branch;
                    entry.facts.branch_status = BranchStatus::Current;
                } else if entry.facts.branch.is_some() {
                    entry.facts.branch_status = BranchStatus::Stale;
                } else {
                    entry.facts.branch_status = BranchStatus::Unknown;
                }
                entry.live_at = Some(Instant::now());
                entry.live_running = false;
            }
        });
    }

    fn spawn_diff(&self, cache_key: String, identity: String, cwd: String) {
        let git = self.git.clone();
        let entries = Arc::clone(&self.entries);
        let retired = Arc::clone(&self.retired);
        thread::spawn(move || {
            if retired.load(Ordering::Acquire) {
                return;
            }
            let counts = git.diff_counts(&cwd);
            if retired.load(Ordering::Acquire) {
                return;
            }
            if let Ok(mut entries) = entries.lock()
                && let Some(entry) = entries.get_mut(&cache_key)
                && entry.identity == identity
            {
                (entry.facts.diff_added, entry.facts.diff_removed) = counts
                    .map_or((None, None), |(added, removed)| {
                        (Some(added), Some(removed))
                    });
                entry.diff_counted = true;
                entry.diff_running = false;
            }
        });
    }
}

/// Filesystem revision tracking for local worktrees. Remote callers get zero
/// and therefore use the bounded forced refresh interval; no local path is
/// consulted as a fallback for a remote host.
#[derive(Clone)]
pub struct WorktreeRevisionCache {
    revisions: Arc<Mutex<HashMap<PathBuf, WorktreeWatch>>>,
    aliases: Arc<Mutex<HashMap<String, PathBuf>>>,
    pending: Arc<Mutex<std::collections::HashSet<String>>>,
    watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
    watch_local: bool,
}

#[derive(Clone)]
struct WorktreeWatch {
    revision: Arc<AtomicU64>,
    paths: Vec<PathBuf>,
}

impl Default for WorktreeRevisionCache {
    fn default() -> Self {
        let revisions: Arc<Mutex<HashMap<PathBuf, WorktreeWatch>>> = Arc::default();
        let watched_revisions = Arc::clone(&revisions);
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let Ok(event) = event else {
                return;
            };
            let Ok(revisions) = watched_revisions.lock() else {
                return;
            };
            for watched in revisions.values() {
                if event
                    .paths
                    .iter()
                    .any(|path| watched.paths.iter().any(|root| path.starts_with(root)))
                {
                    watched.revision.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        Self {
            revisions,
            aliases: Arc::default(),
            pending: Arc::default(),
            watcher: Arc::new(Mutex::new(watcher.ok())),
            watch_local: true,
        }
    }
}

impl WorktreeRevisionCache {
    /// Disable all local filesystem inspection for a remote command runner.
    pub fn disabled() -> Self {
        Self {
            revisions: Arc::default(),
            aliases: Arc::default(),
            pending: Arc::default(),
            watcher: Arc::new(Mutex::new(None)),
            watch_local: false,
        }
    }

    /// Read the current revision without touching the filesystem. A zero
    /// value means the background registration has not completed or the path
    /// is unsupported.
    pub fn cached_revision(&self, cwd: &str) -> u64 {
        let Ok(aliases) = self.aliases.lock() else {
            return 0;
        };
        let Some(root) = aliases.get(cwd) else {
            return 0;
        };
        if let Ok(revisions) = self.revisions.lock()
            && let Some(watched) = revisions.get(root)
        {
            return watched.revision.load(Ordering::Relaxed);
        }
        0
    }

    /// Request local watcher registration off the caller's thread. Remote
    /// caches never call into the local filesystem because `watch_local` is
    /// false.
    pub fn ensure_watched(&self, cwd: String) {
        if !self.watch_local || self.cached_revision(&cwd) != 0 {
            return;
        }
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        if !pending.insert(cwd.clone()) {
            return;
        }
        let cache = self.clone();
        thread::spawn(move || {
            cache.register(cwd.clone());
            if let Ok(mut pending) = cache.pending.lock() {
                pending.remove(&cwd);
            }
        });
    }

    /// Keep the old polling helper usable by compatibility callers while
    /// retaining the no-filesystem-on-caller-thread guarantee.
    pub fn revision(&self, cwd: &str) -> u64 {
        let revision = self.cached_revision(cwd);
        self.ensure_watched(cwd.to_owned());
        revision
    }

    fn register(&self, cwd: String) {
        let Some(paths) = worktree_watch_paths(Path::new(&cwd)) else {
            return;
        };
        let root = paths[0].clone();
        if let Ok(aliases) = self.aliases.lock()
            && aliases.contains_key(&cwd)
        {
            return;
        }
        let Ok(mut watcher) = self.watcher.lock() else {
            return;
        };
        let Some(watcher) = watcher.as_mut() else {
            return;
        };
        let Ok(mut revisions) = self.revisions.lock() else {
            return;
        };
        if revisions.contains_key(&root) {
            drop(revisions);
            if let Ok(mut aliases) = self.aliases.lock() {
                aliases.insert(cwd, root);
            }
            return;
        }
        if revisions.len() >= MAX_WATCHED_WORKTREES {
            return;
        }
        let mut registered: Vec<PathBuf> = Vec::new();
        for path in &paths {
            if watcher.watch(path, RecursiveMode::Recursive).is_err() {
                for registered_path in registered {
                    let _ = watcher.unwatch(&registered_path);
                }
                return;
            }
            registered.push(path.clone());
        }
        revisions.insert(
            root.clone(),
            WorktreeWatch {
                revision: Arc::new(AtomicU64::new(1)),
                paths,
            },
        );
        drop(revisions);
        if let Ok(mut aliases) = self.aliases.lock() {
            aliases.insert(cwd, root);
        }
    }
}

pub fn worktree_revision(cwd: &str) -> u64 {
    static CACHE: OnceLock<WorktreeRevisionCache> = OnceLock::new();
    CACHE
        .get_or_init(WorktreeRevisionCache::default)
        .revision(cwd)
}

fn worktree_watch_paths(cwd: &Path) -> Option<Vec<PathBuf>> {
    let root = native_worktree_root(cwd)?;
    let git_dir = std::fs::canonicalize(git_dir(cwd)?).ok()?;
    let mut paths = vec![root.clone()];
    if !git_dir.starts_with(&root) {
        paths.push(git_dir);
    }
    Some(paths)
}

fn native_worktree_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .and_then(|dir| std::fs::canonicalize(dir).ok())
}

fn git_dir(cwd: &Path) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            let pointer = std::fs::read_to_string(&candidate).ok()?;
            let target = Path::new(pointer.trim().strip_prefix("gitdir:")?.trim()).to_path_buf();
            return Some(if target.is_absolute() {
                target
            } else {
                dir.join(target)
            });
        }
    }
    None
}
