//! Repository changes and explicit index/commit operations, independent of presentation.

use serde::{Deserialize, Serialize};

use crate::{Git, runner::CommandRunner};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeGroup {
    Staged,
    Unstaged,
    Untracked,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub index: char,
    pub worktree: char,
    /// Lines the staged change adds and removes; none when nothing is staged or the file is
    /// binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_lines: Option<LineCounts>,
    /// Lines the unstaged change adds and removes, likewise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unstaged_lines: Option<LineCounts>,
}

/// Lines a change adds and removes.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LineCounts {
    pub added: u32,
    pub removed: u32,
}

impl LineCounts {
    /// Both changes together.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self {
            added: self.added.saturating_add(other.added),
            removed: self.removed.saturating_add(other.removed),
        }
    }
}

impl ChangedFile {
    pub fn groups(&self) -> impl Iterator<Item = ChangeGroup> {
        [
            (self.index == '?').then_some(ChangeGroup::Untracked),
            (!matches!(self.index, ' ' | '?')).then_some(ChangeGroup::Staged),
            (!matches!(self.worktree, ' ' | '?')).then_some(ChangeGroup::Unstaged),
        ]
        .into_iter()
        .flatten()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RepositoryChanges {
    pub root: String,
    pub branch: Option<String>,
    /// The branch's upstream, when it has one Git can still find.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<Upstream>,
    pub files: Vec<ChangedFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Upstream {
    pub name: String,
    /// Commits the branch has that the upstream lacks.
    pub ahead: u32,
    /// Commits the upstream has that the branch lacks.
    pub behind: u32,
}

impl Upstream {
    /// Read `git status --branch`'s header, such as `## main...origin/main [ahead 1, behind 2]`.
    fn from_status_header(header: &str) -> Option<Self> {
        let (_, tracking) = header.strip_prefix("## ")?.split_once("...")?;
        let (name, counts) = tracking
            .split_once(" [")
            .map_or((tracking, ""), |(name, counts)| {
                (name, counts.trim_end_matches(']'))
            });
        if counts == "gone" {
            return None;
        }
        let count = |label: &str| {
            counts
                .split(", ")
                .find_map(|part| part.strip_prefix(label)?.parse().ok())
                .unwrap_or(0)
        };
        Some(Self {
            name: name.to_owned(),
            ahead: count("ahead "),
            behind: count("behind "),
        })
    }
}

impl<R: CommandRunner> Git<R> {
    /// # Errors
    /// Returns an error if Git cannot read the repository or emits malformed status records or filenames the text transport cannot preserve.
    pub fn changes(&self, cwd: &str) -> Result<RepositoryChanges, String> {
        let root = self.checked_output(cwd, &["rev-parse", "--show-toplevel"])?;
        let root = root.strip_suffix('\n').unwrap_or(&root).to_owned();
        let output = self.checked_output(
            &root,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain=v1",
                "-z",
                "--branch",
                "--untracked-files=all",
            ],
        )?;
        // The shared process transport is UTF-8 text. Reject lossy names rather than target
        // a different file with a replacement-character name. A raw-byte runner lifts this limit.
        if output.contains('\u{fffd}') {
            return Err(
                "Git status contains a filename the text transport cannot represent unambiguously"
                    .to_owned(),
            );
        }
        let mut fields = output.split_terminator('\0').peekable();
        let upstream = fields
            .next_if(|field| field.starts_with("## "))
            .and_then(Upstream::from_status_header);
        let mut files = Vec::new();
        while let Some(field) = fields.next() {
            let [index, worktree, b' ', path @ ..] = field.as_bytes() else {
                return Err("invalid Git status record".to_owned());
            };
            if path.is_empty() {
                return Err("invalid Git status record".to_owned());
            }
            let path = std::str::from_utf8(path).map_err(|_| "invalid Git status path")?;
            let index = char::from(*index);
            let worktree = char::from(*worktree);
            let previous_path = if matches!(index, 'R' | 'C') || matches!(worktree, 'R' | 'C') {
                Some(fields.next().ok_or("missing Git rename source")?.to_owned())
            } else {
                None
            };
            files.push(ChangedFile {
                path: path.to_owned(),
                previous_path,
                index,
                worktree,
                staged_lines: None,
                unstaged_lines: None,
            });
        }
        if !files.is_empty() {
            let staged = self.line_counts(&root, true)?;
            let unstaged = self.line_counts(&root, false)?;
            for file in &mut files {
                file.staged_lines = staged.get(&file.path).copied();
                file.unstaged_lines = unstaged.get(&file.path).copied();
            }
        }
        Ok(RepositoryChanges {
            branch: self.head_branch(&root),
            root,
            upstream,
            files,
        })
    }

    /// Each changed file's line counts in the index (`cached`) or the worktree, by path. Binary
    /// files have none.
    fn line_counts(
        &self,
        root: &str,
        cached: bool,
    ) -> Result<std::collections::HashMap<String, LineCounts>, String> {
        let mut args = vec!["--no-optional-locks", "diff", "--numstat", "-z"];
        if cached {
            args.push("--cached");
        }
        let output = self.checked_output(root, &args)?;
        Ok(parse_numstat(&output))
    }

    /// # Errors
    /// Returns Git's error, such as an unreachable remote.
    pub fn fetch_remote(&self, root: &str) -> Result<(), String> {
        self.checked_output(root, &["fetch"]).map(|_| ())
    }

    /// Bring in the upstream's commits. Git refuses instead of merging when the branch has
    /// diverged.
    /// # Errors
    /// Returns Git's error, such as a diverged branch or conflicting local changes.
    pub fn pull_upstream(&self, root: &str) -> Result<(), String> {
        self.checked_output(root, &["pull", "--ff-only"])
            .map(|_| ())
    }

    /// A normal push: it never overwrites commits the upstream has.
    /// # Errors
    /// Returns Git's error, such as a rejected push or a branch without an upstream.
    pub fn push_upstream(&self, root: &str) -> Result<(), String> {
        self.checked_output(root, &["push"]).map(|_| ())
    }

    /// # Errors
    /// Returns an error if the path is invalid, no longer changed, or Git cannot produce the diff.
    pub fn file_diff(&self, root: &str, path: &str, group: ChangeGroup) -> Result<String, String> {
        self.file_diff_with(root, path, group, false)
    }

    /// A file's diff in `group`, leaving out changes to whitespace when `ignore_whitespace`.
    /// # Errors
    /// Returns an error if the path is invalid, no longer changed, or Git cannot diff it.
    pub fn file_diff_with(
        &self,
        root: &str,
        path: &str,
        group: ChangeGroup,
        ignore_whitespace: bool,
    ) -> Result<String, String> {
        let (root, file) = self.current_file(root, path)?;
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
        if ignore_whitespace {
            args.push("--ignore-all-space");
        }
        match group {
            ChangeGroup::Staged => args.push("--cached"),
            ChangeGroup::Unstaged => {}
            ChangeGroup::Untracked => args.extend(["--no-index", "--", "/dev/null", path]),
        }
        if group != ChangeGroup::Untracked {
            args.extend(["--", path]);
            if let Some(previous) = file.previous_path.as_deref() {
                args.push(previous);
            }
        }
        let output = self.changes_output(&root, &args)?;
        // `diff --no-index` reports differences as exit 1, even on success.
        if output.success
            || (group == ChangeGroup::Untracked
                && output.stderr.is_empty()
                && output.stdout.starts_with("diff --git "))
        {
            Ok(output.stdout)
        } else {
            Err(output.stderr.trim().to_owned())
        }
    }

    /// Every change since the last commit, staged or not, then each untracked file, in one diff.
    // Deliberate limit: 50 untracked files show; raise it with a lazily loaded list if more are
    // common.
    /// # Errors
    /// Returns a Git failure.
    pub fn all_diff(&self, root: &str, ignore_whitespace: bool) -> Result<String, String> {
        let changes = self.changes(root)?;
        let root = changes.root.as_str();
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
        if ignore_whitespace {
            args.push("--ignore-all-space");
        }
        let mut diff = if self.head_exists(root)? {
            args.push("HEAD");
            self.checked_output(root, &args)?
        } else {
            let unstaged = self.checked_output(root, &args)?;
            args.push("--cached");
            self.checked_output(root, &args)? + &unstaged
        };
        for file in changes
            .files
            .iter()
            .filter(|file| file.index == '?')
            .take(50)
        {
            diff.push_str(&self.file_diff_with(
                root,
                &file.path,
                ChangeGroup::Untracked,
                ignore_whitespace,
            )?);
        }
        Ok(diff)
    }

    /// What a commit message describes: the staged changes, after the latest subjects so the
    /// message can follow the project's style.
    /// # Errors
    /// Returns a directory outside Git, nothing staged, or a Git failure.
    pub fn commit_message_context(&self, root: &str) -> Result<String, String> {
        let changes = self.changes(root)?;
        let root = changes.root.as_str();
        let staged = self.checked_output(
            root,
            &[
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
            ],
        )?;
        if staged.trim().is_empty() {
            return Err("Stage the changes to describe first".to_owned());
        }
        let subjects = if self.head_exists(root)? {
            self.checked_output(root, &["log", "-n", "10", "--format=%s"])?
        } else {
            String::new()
        };
        Ok(format!(
            "Recent commits:\n{subjects}\nStaged changes:\n{staged}"
        ))
    }

    /// What a pull request into `base` describes: the branch's commits since it left `base`, and
    /// their changes. The base's upstream stands in for it when the branch has one, since the
    /// local copy may be behind.
    /// # Errors
    /// Returns a directory outside Git, a base Git doesn't know, no commits since it, or a Git
    /// failure.
    pub fn pull_request_context(&self, root: &str, base: &str) -> Result<String, String> {
        let changes = self.changes(root)?;
        let root = changes.root.as_str();
        if base.is_empty() || base.starts_with('-') {
            return Err("Choose the branch the pull request goes into".to_owned());
        }
        let base = [format!("{base}@{{upstream}}"), base.to_owned()]
            .into_iter()
            .find(|candidate| {
                self.checked_output(
                    root,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("{candidate}^{{commit}}"),
                    ],
                )
                .is_ok()
            })
            .ok_or_else(|| format!("Git doesn't know the branch {base}"))?;
        let range = format!("{base}..HEAD");
        let commits = self.checked_output(root, &["log", "--format=%s%n%n%b", &range])?;
        if commits.trim().is_empty() {
            return Err("This branch has no commits to describe yet".to_owned());
        }
        let diff = self.checked_output(
            root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                &format!("{base}...HEAD"),
            ],
        )?;
        Ok(format!("Commits:\n{commits}\nChanges:\n{diff}"))
    }

    /// # Errors
    /// Returns an error if the path is invalid, no longer changed, or Git cannot stage it.
    pub fn stage_file(&self, root: &str, path: &str) -> Result<(), String> {
        let (root, _) = self.current_file(root, path)?;
        self.checked_output(&root, &["add", "--", path]).map(|_| ())
    }

    /// # Errors
    /// Returns an error if the path is invalid, no longer changed, or Git cannot update the index.
    pub fn unstage_file(&self, root: &str, path: &str) -> Result<(), String> {
        let (root, file) = self.current_file(root, path)?;
        let head_exists = self
            .changes_output(&root, &["rev-parse", "--verify", "--quiet", "HEAD"])?
            .success;
        let mut args = if head_exists {
            vec!["reset", "--quiet", "HEAD", "--", path]
        } else {
            vec!["rm", "--cached", "--force", "--", path]
        };
        if let Some(previous) = file.previous_path.as_deref() {
            args.push(previous);
        }
        self.checked_output(&root, &args).map(|_| ())
    }

    /// Stages every change, untracked files included.
    /// # Errors
    /// Returns a Git failure.
    pub fn stage_all(&self, root: &str) -> Result<(), String> {
        self.checked_output(root, &["add", "-A"]).map(|_| ())
    }

    /// Unstages everything, keeping the files as they are.
    /// # Errors
    /// Returns a Git failure.
    pub fn unstage_all(&self, root: &str) -> Result<(), String> {
        if self.head_exists(root)? {
            self.checked_output(root, &["reset", "--quiet"])
        } else {
            self.checked_output(root, &["rm", "--cached", "-r", "--quiet", "--force", "."])
        }
        .map(|_| ())
    }

    /// Throws away a file's changes: staged and unstaged ones go back to the last commit, and a
    /// file the last commit lacks is deleted.
    /// # Errors
    /// Returns an error if the path is invalid, no longer changed, or Git cannot discard it.
    pub fn discard_file(&self, root: &str, path: &str) -> Result<(), String> {
        let (root, file) = self.current_file(root, path)?;
        if file.groups().any(|group| group == ChangeGroup::Staged) {
            self.unstage_file(&root, path)?;
        }
        let committed = self.head_exists(&root)?
            && self
                .changes_output(&root, &["cat-file", "-e", &format!("HEAD:{path}")])?
                .success;
        if committed {
            self.checked_output(&root, &["restore", "--worktree", "--", path])
        } else {
            self.checked_output(&root, &["clean", "--force", "--", path])
        }
        .map(|_| ())
    }

    /// Throws away every change: files go back to the last commit and untracked ones are
    /// deleted. Ignored files stay.
    /// # Errors
    /// Returns a Git failure partway.
    pub fn discard_all(&self, root: &str) -> Result<(), String> {
        self.unstage_all(root)?;
        if self.head_exists(root)? {
            self.checked_output(root, &["restore", "--worktree", "--", "."])?;
        }
        self.checked_output(root, &["clean", "-d", "--force", "--", "."])
            .map(|_| ())
    }

    /// Stages the hunks in `patch`, or unstages them when `reverse`.
    /// # Errors
    /// Returns a patch that no longer applies to the index.
    pub fn apply_to_index(&self, root: &str, patch: &str, reverse: bool) -> Result<(), String> {
        if patch.len() > 8 * 1024 * 1024 {
            return Err("The selected changes are too large to stage this way".to_owned());
        }
        let mut args = vec!["-C", root, "apply", "--cached", "--recount"];
        if reverse {
            args.push("--reverse");
        }
        args.push("-");
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let output = self
            .runner
            .run_with_input("git", &args, patch.as_bytes().to_vec())
            .map_err(|error| error.to_string())?;
        if output.success {
            Ok(())
        } else {
            Err(output.stderr.trim().to_owned())
        }
    }

    fn head_exists(&self, root: &str) -> Result<bool, String> {
        Ok(self
            .changes_output(root, &["rev-parse", "--verify", "--quiet", "HEAD"])?
            .success)
    }

    /// # Errors
    /// Returns an error for an empty or invalid message, or if Git cannot create or amend the commit.
    pub fn commit_index(&self, root: &str, message: &str, amend: bool) -> Result<(), String> {
        if message.trim().is_empty() || message.contains('\0') {
            return Err("a commit message is required".to_owned());
        }
        let mut args = vec!["commit", "-m", message];
        if amend {
            args.push("--amend");
        }
        self.checked_output(root, &args).map(|_| ())
    }

    fn current_file(&self, root: &str, path: &str) -> Result<(String, ChangedFile), String> {
        validate_path(path)?;
        let changes = self.changes(root)?;
        changes
            .files
            .into_iter()
            .find(|file| file.path == path)
            .map(|file| (changes.root, file))
            .ok_or_else(|| "file is no longer in the repository changes".to_owned())
    }

    fn changes_output(
        &self,
        root: &str,
        args: &[&str],
    ) -> Result<crate::runner::CommandOutput, String> {
        // Stash uses Git's internal pathspec magic when cleaning untracked files.
        // Its public arguments here contain no pathspecs; forcing literal mode breaks -u.
        let literal = (args.first() != Some(&"stash")).then_some("--literal-pathspecs");
        let args = literal
            .into_iter()
            .chain(["-C", root])
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        self.runner
            .run("git", &args)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn checked_output(&self, root: &str, args: &[&str]) -> Result<String, String> {
        let output = self.changes_output(root, args)?;
        if output.success {
            Ok(output.stdout)
        } else {
            Err(output.stderr.trim().to_owned())
        }
    }
}

fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains('\0')
        || path.starts_with('/')
        || path.split('/').any(|part| part == "..")
    {
        return Err("expected a repository-relative file path".to_owned());
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GitCommit {
    pub id: String,
    pub parents: Vec<String>,
    pub author: String,
    pub timestamp: u64,
    pub decorations: String,
    pub subject: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GitBranch {
    pub name: String,
    pub commit: String,
    pub current: bool,
    /// The checkout that already owns this branch, if any.
    #[serde(default)]
    pub worktree_path: Option<String>,
    pub upstream: Option<String>,
    /// Commits the branch has that its upstream lacks.
    #[serde(default)]
    pub ahead: u32,
    /// Commits its upstream has that the branch lacks.
    #[serde(default)]
    pub behind: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GitStash {
    pub reference: String,
    pub commit: String,
    pub timestamp: u64,
    pub subject: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RepositoryOverview {
    pub root: String,
    pub history: Vec<GitCommit>,
    pub branches: Vec<GitBranch>,
    pub stashes: Vec<GitStash>,
}
impl<R: CommandRunner> Git<R> {
    /// # Errors
    /// Returns an error if Git cannot read the repository or emits malformed history, branch, or stash records.
    pub fn overview(&self, cwd: &str, limit: u16) -> Result<RepositoryOverview, String> {
        let root = self.changes(cwd)?.root;
        let limit = limit.clamp(1, 200).to_string();
        let log = self.checked_output(
            &root,
            &[
                "log",
                "--all",
                "--date-order",
                "--format=%H%x00%P%x00%an%x00%at%x00%D%x00%s%x00",
                "-n",
                &limit,
            ],
        )?;
        let mut fields = log.trim_end_matches('\n').split_terminator('\0');
        let mut history = Vec::new();
        while let Some(id) = fields.next() {
            let parents = fields.next().ok_or("invalid Git history")?;
            let author = fields.next().ok_or("invalid Git history")?;
            let timestamp = fields
                .next()
                .ok_or("invalid Git history")?
                .parse()
                .map_err(|_| "invalid Git timestamp")?;
            let decorations = fields.next().ok_or("invalid Git history")?;
            let subject = fields.next().ok_or("invalid Git history")?;
            history.push(GitCommit {
                id: id.trim_start_matches('\n').into(),
                parents: parents.split_whitespace().map(str::to_owned).collect(),
                author: author.into(),
                timestamp,
                decorations: decorations.into(),
                subject: subject.into(),
            });
        }
        let refs = self.checked_output(
            &root,
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(HEAD)%00%(refname:short)%00%(objectname)%00%(upstream:short)%00%(upstream:track,nobracket)%00%(worktreepath)%00",
                "refs/heads",
            ],
        )?;
        let mut fields = refs.trim_end_matches('\n').split_terminator('\0');
        let mut branches = Vec::new();
        while let Some(current) = fields.next() {
            let name = fields.next().ok_or("invalid Git branch list")?;
            let commit = fields.next().ok_or("invalid Git branch list")?;
            let upstream = fields.next().ok_or("invalid Git branch list")?;
            let track = fields.next().ok_or("invalid Git branch list")?;
            let worktree_path = fields.next().ok_or("invalid Git branch worktree")?;
            // Like "ahead 2, behind 1"; empty when even, and "gone" when the upstream is.
            let count = |word: &str| {
                track
                    .split(", ")
                    .find_map(|part| part.strip_prefix(word)?.trim().parse().ok())
                    .unwrap_or(0)
            };
            branches.push(GitBranch {
                name: name.trim_start_matches('\n').into(),
                commit: commit.into(),
                current: current.trim_start_matches('\n') == "*",
                worktree_path: (!worktree_path.is_empty()).then(|| worktree_path.into()),
                upstream: (!upstream.is_empty()).then(|| upstream.into()),
                ahead: count("ahead"),
                behind: count("behind"),
            });
        }
        let stash = self.checked_output(
            &root,
            &[
                "stash",
                "list",
                "-n",
                "200",
                "--format=%gd%x00%H%x00%at%x00%s%x00",
            ],
        )?;
        let mut fields = stash.trim_end_matches('\n').split_terminator('\0');
        let mut stashes = Vec::new();
        while let Some(reference) = fields.next() {
            let commit = fields.next().ok_or("invalid Git stash list")?;
            let timestamp = fields
                .next()
                .ok_or("invalid Git stash list")?
                .parse()
                .map_err(|_| "invalid stash timestamp")?;
            let subject = fields.next().ok_or("invalid Git stash list")?;
            stashes.push(GitStash {
                reference: reference.trim_start_matches('\n').into(),
                commit: commit.into(),
                timestamp,
                subject: subject.into(),
            });
        }
        Ok(RepositoryOverview {
            root,
            history,
            branches,
            stashes,
        })
    }
    /// # Errors
    /// Returns an error for a dirty worktree, invalid branch or starting commit, or a failed Git switch.
    pub fn create_branch(&self, root: &str, name: &str, start: &str) -> Result<(), String> {
        self.require_clean(root)?;
        self.checked_output(root, &["check-ref-format", "--branch", name])?;
        let commit = self.checked_output(
            root,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{start}^{{commit}}"),
            ],
        )?;
        self.checked_output(
            root,
            &[
                "switch",
                "--no-overwrite-ignore",
                "--create",
                name,
                commit.trim(),
            ],
        )
        .map(|_| ())
    }
    /// # Errors
    /// Returns an error for a dirty worktree, an unknown local branch, or a failed Git switch.
    pub fn checkout_branch(&self, root: &str, name: &str) -> Result<(), String> {
        self.require_clean(root)?;
        let overview = self.overview(root, 200)?;
        if !overview.branches.iter().any(|branch| branch.name == name) {
            return Err("branch is not a local repository branch".into());
        }
        self.checked_output(&overview.root, &["switch", "--no-overwrite-ignore", name])
            .map(|_| ())
    }
    /// Switches to local branch `name`, first stashing uncommitted changes, untracked files
    /// included, under a message naming both branches. The stash stays for the person to apply
    /// where they want the changes.
    /// # Errors
    /// Returns an unknown local branch, or Git failing to stash or switch; a failed switch after
    /// stashing says where the changes went.
    pub fn stash_and_switch(&self, root: &str, name: &str) -> Result<(), String> {
        let changes = self.changes(root)?;
        let overview = self.overview(&changes.root, 1)?;
        if !overview.branches.iter().any(|branch| branch.name == name) {
            return Err("branch is not a local repository branch".into());
        }
        let stashed = !changes.files.is_empty();
        if stashed {
            let from = changes.branch.as_deref().unwrap_or("a detached HEAD");
            self.checked_output(
                &changes.root,
                &[
                    "stash",
                    "push",
                    "--include-untracked",
                    "--message",
                    &format!("Stashed switching from {from} to {name}"),
                ],
            )?;
        }
        self.checked_output(&changes.root, &["switch", "--no-overwrite-ignore", name])
            .map(|_| ())
            .map_err(|error| {
                if stashed {
                    format!("{error} Your changes are in the newest stash.")
                } else {
                    error
                }
            })
    }

    /// # Errors
    /// Returns an error for an invalid message or if Git cannot create the stash.
    pub fn stash_push(
        &self,
        root: &str,
        message: &str,
        include_untracked: bool,
    ) -> Result<(), String> {
        let root = self.changes(root)?.root;
        if message.contains('\0') {
            return Err("invalid stash message".into());
        }
        let mut args = vec!["stash", "push", "--message", message];
        if include_untracked {
            args.push("--include-untracked");
        }
        self.checked_output(&root, &args).map(|_| ())
    }
    /// # Errors
    /// Returns an error if the stash identity changed or Git cannot restore it, including conflicts.
    pub fn stash_apply(&self, root: &str, reference: &str, expected: &str) -> Result<(), String> {
        let root = self.valid_stash(root, reference, expected)?;
        self.checked_output(&root, &["stash", "apply", "--index", reference])
            .map(|_| ())
    }
    /// # Errors
    /// Returns an error if the stash identity changed or Git cannot remove it.
    pub fn stash_drop(&self, root: &str, reference: &str, expected: &str) -> Result<(), String> {
        let root = self.valid_stash(root, reference, expected)?;
        self.checked_output(&root, &["stash", "drop", reference])
            .map(|_| ())
    }
    /// # Errors
    /// Returns an error for an invalid or missing commit, or if Git cannot render its diff.
    pub fn commit_diff(&self, root: &str, id: &str) -> Result<String, String> {
        if !(id.len() == 40 || id.len() == 64) || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid commit ID".into());
        }
        let root = self.changes(root)?.root;
        self.checked_output(&root, &["cat-file", "-e", &format!("{id}^{{commit}}")])?;
        self.checked_output(
            &root,
            &[
                "show",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--format=fuller",
                "--stat",
                "--patch",
                id,
            ],
        )
    }
    pub(crate) fn require_clean(&self, root: &str) -> Result<String, String> {
        let changes = self.changes(root)?;
        if !changes.files.is_empty() {
            return Err("working tree must be clean before switching branches; commit or stash your changes first".into());
        }
        Ok(changes.root)
    }
    fn valid_stash(&self, root: &str, reference: &str, expected: &str) -> Result<String, String> {
        let overview = self.overview(root, 200)?;
        if !overview
            .stashes
            .iter()
            .any(|stash| stash.reference == reference && stash.commit == expected)
        {
            return Err("stash is no longer present".into());
        }
        Ok(overview.root)
    }
}

/// `git diff --numstat -z` records, `added\tremoved\tpath`, by path. A rename's record has no
/// path and is followed by its source and destination. Binary files count `-` and are left out.
fn parse_numstat(output: &str) -> std::collections::HashMap<String, LineCounts> {
    let mut fields = output.split_terminator('\0');
    let mut counts = std::collections::HashMap::new();
    while let Some(record) = fields.next() {
        let mut columns = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) =
            (columns.next(), columns.next(), columns.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            match (fields.next(), fields.next()) {
                (Some(_), Some(destination)) => destination,
                _ => break,
            }
        } else {
            path
        };
        if let (Ok(added), Ok(removed)) = (added.parse(), removed.parse()) {
            counts.insert(path.to_owned(), LineCounts { added, removed });
        }
    }
    counts
}
