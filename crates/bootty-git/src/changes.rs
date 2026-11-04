//! Repository status, history, branches and stashes, independent of presentation.

use serde::{Deserialize, Serialize};

use crate::{Git, runner::CommandRunner};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub index: char,
    pub worktree: char,
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
            });
        }
        Ok(RepositoryChanges {
            branch: self.head_branch(&root),
            root,
            upstream,
            files,
        })
    }

    fn changes_output(
        &self,
        root: &str,
        args: &[&str],
    ) -> Result<crate::runner::CommandOutput, String> {
        let args = ["--literal-pathspecs", "-C", root]
            .into_iter()
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

    pub(crate) fn require_clean(&self, root: &str) -> Result<String, String> {
        let changes = self.changes(root)?;
        if !changes.files.is_empty() {
            return Err("working tree must be clean before switching branches; commit or stash your changes first".into());
        }
        Ok(changes.root)
    }
}
