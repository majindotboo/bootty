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
    pub files: Vec<ChangedFile>,
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
        let mut fields = output.split_terminator('\0');
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
            });
        }
        Ok(RepositoryChanges {
            branch: self.head_branch(&root),
            root,
            files,
        })
    }

    /// # Errors
    /// Returns an error if the path is invalid, no longer changed, or Git cannot produce the diff.
    pub fn file_diff(&self, root: &str, path: &str, group: ChangeGroup) -> Result<String, String> {
        let (root, file) = self.current_file(root, path)?;
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
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

    fn checked_output(&self, root: &str, args: &[&str]) -> Result<String, String> {
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
    pub upstream: Option<String>,
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
                "--count=500",
                "--sort=-committerdate",
                "--format=%(HEAD)%00%(refname:short)%00%(objectname)%00%(upstream:short)%00",
                "refs/heads",
            ],
        )?;
        let mut fields = refs.trim_end_matches('\n').split_terminator('\0');
        let mut branches = Vec::new();
        while let Some(current) = fields.next() {
            let name = fields.next().ok_or("invalid Git branch list")?;
            let commit = fields.next().ok_or("invalid Git branch list")?;
            let upstream = fields.next().ok_or("invalid Git branch list")?;
            branches.push(GitBranch {
                name: name.trim_start_matches('\n').into(),
                commit: commit.into(),
                current: current.trim_start_matches('\n') == "*",
                upstream: (!upstream.is_empty()).then(|| upstream.into()),
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
    fn require_clean(&self, root: &str) -> Result<String, String> {
        let changes = self.changes(root)?;
        if !changes.files.is_empty() {
            return Err("working tree must be clean before switching branches".into());
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
