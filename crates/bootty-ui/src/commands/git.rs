use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind};
use bootty_git::{Git, changes::ChangeGroup, runner::CommandRunner};

command_actions! {
    GitAction {
        Open => ("git.open", "Open Git Panels", ["repository"], Write),
        Status => ("git.status", "Git Changes", ["repository"], Read),
        Diff => ("git.diff", "Git File Diff", ["repository", "path", "group", "whitespace"], Read),
        DiffAll => ("git.diff-all", "Git Diff of All Changes", ["repository", "whitespace"], Read),
        Stage => ("git.stage", "Stage File", ["repository", "path"], Write),
        Unstage => ("git.unstage", "Unstage File", ["repository", "path"], Write),
        StageAll => ("git.stage-all", "Stage All Changes", ["repository"], Write),
        UnstageAll => ("git.unstage-all", "Unstage All Changes", ["repository"], Write),
        StageHunk => ("git.stage-hunk", "Stage or Unstage Hunks", ["repository", "patch", "reverse"], Write),
        Discard => ("git.discard", "Discard File Changes", ["repository", "path"], Destructive),
        DiscardAll => ("git.discard-all", "Discard All Changes", ["repository"], Destructive),
        Commit => ("git.commit", "Commit Staged Changes", ["repository", "message"], Write),
        Amend => ("git.amend", "Amend Last Commit", ["repository", "message"], Destructive),
        WriteMessage => ("git.write-message", "Write Commit Message", ["repository"], Read),
        PullRequest => ("git.pull-request", "Branch Pull Request", ["repository"], Read),
        WritePullRequest => ("git.write-pull-request", "Write Pull Request Text", ["repository", "base"], Read),
        Fetch => ("git.fetch", "Fetch From Remote", ["repository"], Write),
        Pull => ("git.pull", "Pull From Upstream", ["repository"], Write),
        Push => ("git.push", "Push To Upstream", ["repository"], Write),
        CreateWorktree => ("worktree.create", "Create Worktree", ["repository", "branch", "folder", "start_ref"], Write),
        Overview => ("git.overview", "Git History, Branches and Stashes", ["repository", "limit"], Read),
        CommitDiff => ("git.commit-diff", "Git Commit Diff", ["repository", "commit"], Read),
        CreateBranch => ("git.branch-create", "Create Git Branch", ["repository", "branch", "start_ref"], Write),
        CheckoutBranch => ("git.branch-checkout", "Check Out Git Branch", ["repository", "branch"], Write),
        StashSwitch => ("git.branch-stash-switch", "Stash Changes and Switch Branch", ["repository", "branch"], Write),
        StashPush => ("git.stash-push", "Stash Git Changes", ["repository", "message", "include_untracked"], Write),
        StashApply => ("git.stash-apply", "Apply Git Stash", ["repository", "stash", "expected_commit"], Write),
        StashDrop => ("git.stash-drop", "Drop Git Stash", ["repository", "stash", "expected_commit"], Destructive),
        GitHubRead => ("git.github.read", "Read Pull Request", ["repository", "number"], Read),
        GitHubDiff => ("git.github.diff", "Read Pull Request File Diff", ["repository", "number", "request"], Read),
        GitHubCreationContext => ("git.github.creation-context", "Prepare Pull Request", ["repository"], Read),
        GitHubPublishBranch => ("git.github.publish-branch", "Publish Pull Request Branch", ["repository", "context"], Write),
        GitHubCreate => ("git.github.create", "Create Pull Request", ["repository", "request"], Write),
        GitHubCheckoutContext => ("git.github.checkout-context", "Prepare Pull Request Checkout", ["repository", "number", "head"], Read),
        GitHubCheckout => ("git.github.checkout", "Check Out Pull Request Revision", ["repository", "request"], Write),
        GitHubSearch => ("git.github.search", "Search Pull Requests", ["repository", "query"], Read),
        GitHubReview => ("git.github.review", "Submit Pull Request Review", ["repository", "number", "review"], Write),
        GitHubThread => ("git.github.thread", "Reply or Resolve Review Thread", ["repository", "number", "request"], Write),
        GitHubComments => ("git.github.comments", "Read Review Thread Comments", ["repository", "number", "thread", "cursor"], Read),
        GitHubAction => ("git.github.action", "Pull Request Action", ["repository", "number", "request"], Destructive),
        GitHubMetadata => ("git.github.metadata", "Update Pull Request Metadata", ["repository", "number", "request"], Write),
        GitHubViewed => ("git.github.viewed", "Pull Request Viewed Files", ["repository", "number", "cursor"], Read),
        GitHubCandidates => ("git.github.candidates", "Pull Request Labels and Reviewers", ["repository", "kind", "page"], Read),
        GitHubStack => ("git.github.stack", "Pull Request Stack", ["repository", "number"], Read),
        GitHubMergeStatus => ("git.github.merge-status", "Stack Merge Status", ["repository", "request"], Read),
        GitHubWorkflows => ("git.github.workflows", "Pull Request Workflow Approvals", ["repository", "number", "head"], Read),
        GitHubActivity => ("git.github.activity", "Pull Request Activity", ["repository", "number", "cursor"], Read),
    }
}

impl GitAction {
    #[must_use]
    pub fn descriptor(self) -> CommandDescriptor {
        let (id, title, names, mutation) = self.metadata();
        let arguments = names
            .iter()
            .copied()
            .map(|name| {
                let mut arg = super::argument(name, bootty_control::ValueType::String);
                arg.required = !matches!(name, "folder" | "start_ref" | "whitespace");
                if name == "whitespace" {
                    arg.choices = ["show", "ignore"].map(str::to_owned).to_vec();
                }
                if name == "group" {
                    arg.choices = ["staged", "unstaged", "untracked"]
                        .map(str::to_owned)
                        .to_vec();
                }
                arg
            })
            .collect();
        CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description: format!("{title} on the target binding's host."),
            arguments: CompactSchema { arguments },
            mutation,
            target: Some(ResourceKind::Binding),
            palette: false,
        }
    }

    /// `directory` is the configured worktree parent; wire arguments never carry it.
    pub(super) fn worktree_request(
        args: &[String],
        directory: Option<String>,
    ) -> Result<bootty_git::WorktreeRequest, String> {
        Ok(bootty_git::WorktreeRequest {
            branch: args.get(1).ok_or("Worktree branch is required")?.clone(),
            name: args.get(2).filter(|value| !value.is_empty()).cloned(),
            start_ref: args.get(3).filter(|value| !value.is_empty()).cloned(),
            directory,
        })
    }

    pub(super) fn execute(
        self,
        runner: impl CommandRunner,
        args: &[String],
    ) -> Result<serde_json::Value, String> {
        let git = Git::with_runner(runner);
        let arg = |index: usize| {
            args.get(index)
                .map(String::as_str)
                .ok_or_else(|| format!("Missing Git argument {index}"))
        };
        let root = arg(0)?;
        if self.metadata().0.starts_with("git.github.") {
            return self.execute_github(&git, root, args);
        }
        if matches!(
            self,
            Self::PullRequest | Self::WriteMessage | Self::WritePullRequest
        ) {
            return self.execute_description(&git, root, args);
        }
        match self {
            Self::Overview => serde_json::to_value(
                git.overview(
                    root,
                    args.get(1)
                        .map_or("100", String::as_str)
                        .parse()
                        .map_err(|_| "history limit must be a number")?,
                )?,
            )
            .map_err(|e| e.to_string()),
            Self::CommitDiff => git
                .commit_diff(root, arg(1)?)
                .map(serde_json::Value::String),
            Self::CreateBranch | Self::CheckoutBranch | Self::StashSwitch => {
                self.execute_branch(&git, root, arg(1)?, args)
            }
            Self::StashPush => git
                .stash_push(
                    root,
                    arg(1)?,
                    arg(2)?
                        .parse()
                        .map_err(|_| "include_untracked must be true or false")?,
                )
                .map(|()| serde_json::Value::Null),
            Self::StashApply => git
                .stash_apply(root, arg(1)?, arg(2)?)
                .map(|()| serde_json::Value::Null),
            Self::StashDrop => git
                .stash_drop(root, arg(1)?, arg(2)?)
                .map(|()| serde_json::Value::Null),
            Self::CreateWorktree => {
                Err("Worktree creation requires the configured directory".into())
            }
            Self::Open => Err("Opening Git panels requires a window".to_owned()),
            Self::Status => {
                serde_json::to_value(git.changes(root)?).map_err(|error| error.to_string())
            }
            Self::Fetch => git.fetch_remote(root).map(|()| serde_json::Value::Null),
            Self::Pull => git.pull_upstream(root).map(|()| serde_json::Value::Null),
            Self::Push => git.push_upstream(root).map(|()| serde_json::Value::Null),
            Self::Diff => {
                let group = match arg(2)? {
                    "staged" => ChangeGroup::Staged,
                    "unstaged" => ChangeGroup::Unstaged,
                    "untracked" => ChangeGroup::Untracked,
                    _ => return Err("Invalid Git change group".to_owned()),
                };
                git.file_diff_with(
                    root,
                    arg(1)?,
                    group,
                    args.get(3).is_some_and(|value| value == "ignore"),
                )
                .map(serde_json::Value::String)
            }
            Self::DiffAll => git
                .all_diff(root, args.get(1).is_some_and(|value| value == "ignore"))
                .map(serde_json::Value::String),
            Self::Stage => git
                .stage_file(root, arg(1)?)
                .map(|()| serde_json::Value::Null),
            Self::Unstage => git
                .unstage_file(root, arg(1)?)
                .map(|()| serde_json::Value::Null),
            Self::StageAll => git.stage_all(root).map(|()| serde_json::Value::Null),
            Self::UnstageAll => git.unstage_all(root).map(|()| serde_json::Value::Null),
            Self::StageHunk => git
                .apply_to_index(root, arg(1)?, arg(2)? == "true")
                .map(|()| serde_json::Value::Null),
            Self::Discard => git
                .discard_file(root, arg(1)?)
                .map(|()| serde_json::Value::Null),
            Self::DiscardAll => git.discard_all(root).map(|()| serde_json::Value::Null),
            Self::Commit | Self::Amend => git
                .commit_index(root, arg(1)?, self == Self::Amend)
                .map(|()| serde_json::Value::Null),
            _ => Err("GitHub commands require the GitHub host dispatcher".into()),
        }
    }
    /// Creating `branch`, or switching to it, from a clean tree or by stashing the changes.
    fn execute_branch<R: CommandRunner>(
        self,
        git: &Git<R>,
        root: &str,
        branch: &str,
        args: &[String],
    ) -> Result<serde_json::Value, String> {
        match self {
            Self::CreateBranch => {
                git.create_branch(root, branch, args.get(2).map_or("HEAD", String::as_str))
            }
            Self::CheckoutBranch => git.checkout_branch(root, branch),
            Self::StashSwitch => git.stash_and_switch(root, branch),
            _ => Err("Not a branch command".to_owned()),
        }
        .map(|()| serde_json::Value::Null)
    }

    /// What describes the work: the branch's pull request, and what Codex writes commit messages
    /// and pull request text from, which the runtime hands over.
    fn execute_description<R: CommandRunner>(
        self,
        git: &Git<R>,
        root: &str,
        args: &[String],
    ) -> Result<serde_json::Value, String> {
        match self {
            Self::PullRequest => {
                let branch = git.changes(root)?.branch.unwrap_or_default();
                serde_json::to_value(git.branch_pull_request(root, &branch)?)
                    .map_err(|error| error.to_string())
            }
            Self::WriteMessage => git
                .commit_message_context(root)
                .map(serde_json::Value::String),
            Self::WritePullRequest => git
                .pull_request_context(
                    root,
                    args.get(1)
                        .ok_or("Missing the pull request's base branch")?,
                )
                .map(serde_json::Value::String),
            _ => Err("Not a describing command".to_owned()),
        }
    }

    fn execute_github(
        self,
        git: &Git<impl CommandRunner>,
        root: &str,
        args: &[String],
    ) -> Result<serde_json::Value, String> {
        if matches!(self, Self::GitHubCheckoutContext | Self::GitHubCheckout) {
            return self.execute_checkout(git, root, args);
        }
        let arg = |index: usize| {
            args.get(index)
                .map(String::as_str)
                .ok_or_else(|| format!("Missing GitHub argument {index}"))
        };
        match self {
            Self::GitHubCreationContext => {
                serde_json::to_value(git.pull_request_creation_context(root)?)
                    .map_err(|e| e.to_string())
            }
            Self::GitHubDiff => serde_json::to_value(git.github(root)?.file_diff(
                arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                &serde_json::from_str(arg(2)?).map_err(|e| e.to_string())?,
            )?)
            .map_err(|e| e.to_string()),
            Self::GitHubPublishBranch => git
                .publish_pull_request_branch(
                    root,
                    &serde_json::from_str(arg(1)?).map_err(|e| e.to_string())?,
                )
                .map(|()| serde_json::Value::Null),
            Self::GitHubCreate => serde_json::to_value(git.create_pull_request(
                root,
                &serde_json::from_str(arg(1)?).map_err(|e| e.to_string())?,
            )?)
            .map_err(|e| e.to_string()),
            Self::GitHubStack => serde_json::to_value(
                git.github(root)?
                    .stack(arg(1)?.parse().map_err(|_| "Invalid pull request number")?)?,
            )
            .map_err(|e| e.to_string()),
            Self::GitHubMergeStatus => {
                serde_json::to_value(git.github(root)?.stack_merge_status(
                    &serde_json::from_str(arg(1)?).map_err(|e| e.to_string())?,
                )?)
                .map_err(|e| e.to_string())
            }
            Self::GitHubWorkflows => serde_json::to_value(git.github(root)?.workflow_approvals(
                arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                arg(2)?,
            )?)
            .map_err(|e| e.to_string()),
            Self::GitHubActivity => serde_json::to_value(git.github(root)?.activity(
                arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                arg(2)?,
            )?)
            .map_err(|e| e.to_string()),
            Self::GitHubMetadata => git.github(root)?.metadata(
                arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                &serde_json::from_str(arg(2)?).map_err(|e| e.to_string())?,
            ),
            Self::GitHubViewed => serde_json::to_value(git.github(root)?.viewed_files(
                arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                arg(2)?,
            )?)
            .map_err(|e| e.to_string()),
            Self::GitHubCandidates => serde_json::to_value(git.github(root)?.candidates(
                match arg(1)? {
                    "labels" => bootty_git::github::CandidateKind::Labels,
                    "reviewers" => bootty_git::github::CandidateKind::Reviewers,
                    "teams" => bootty_git::github::CandidateKind::Teams,
                    _ => return Err("Invalid candidate kind".into()),
                },
                arg(2)?.parse().map_err(|_| "Invalid candidate page")?,
            )?)
            .map_err(|e| e.to_string()),
            Self::GitHubComments => serde_json::to_value(git.github(root)?.thread_comments(
                arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                arg(2)?,
                arg(3)?,
            )?)
            .map_err(|e| e.to_string()),
            Self::GitHubRead => serde_json::to_value(
                git.github(root)?
                    .read(arg(1)?.parse().map_err(|_| "Invalid pull request number")?)?,
            )
            .map_err(|e| e.to_string()),
            Self::GitHubSearch => {
                serde_json::to_value(git.github(root)?.search(arg(1)?)?).map_err(|e| e.to_string())
            }
            Self::GitHubReview => git.github(root)?.review(
                arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                &serde_json::from_str(arg(2)?).map_err(|e| e.to_string())?,
            ),
            Self::GitHubThread => git.github(root)?.thread(
                arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                &serde_json::from_str(arg(2)?).map_err(|e| e.to_string())?,
            ),
            Self::GitHubAction => git.github(root)?.action(
                arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                &serde_json::from_str(arg(2)?).map_err(|e| e.to_string())?,
            ),
            _ => Err("Unknown GitHub command".into()),
        }
    }
    fn execute_checkout(
        self,
        git: &Git<impl CommandRunner>,
        root: &str,
        args: &[String],
    ) -> Result<serde_json::Value, String> {
        let arg = |index: usize| {
            args.get(index)
                .map(String::as_str)
                .ok_or_else(|| format!("Missing GitHub argument {index}"))
        };
        match self {
            Self::GitHubCheckoutContext => {
                serde_json::to_value(git.pull_request_checkout_context(
                    root,
                    arg(1)?.parse().map_err(|_| "Invalid pull request number")?,
                    arg(2)?,
                )?)
                .map_err(|e| e.to_string())
            }
            Self::GitHubCheckout => git
                .checkout_pull_request(
                    root,
                    &serde_json::from_str(arg(1)?).map_err(|e| e.to_string())?,
                )
                .map(|()| serde_json::Value::Null),
            _ => Err("Unknown pull request checkout command".into()),
        }
    }
}
