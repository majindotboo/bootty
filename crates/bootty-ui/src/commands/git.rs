use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind};
use bootty_git::{Git, changes::ChangeGroup, runner::CommandRunner};

command_actions! {
    GitAction {
        Open => ("git.open", "Open Git Panels", ["repository"], Write),
        Status => ("git.status", "Git Changes", ["repository"], Read),
        Diff => ("git.diff", "Git File Diff", ["repository", "path", "group"], Read),
        Stage => ("git.stage", "Stage File", ["repository", "path"], Write),
        Unstage => ("git.unstage", "Unstage File", ["repository", "path"], Write),
        Commit => ("git.commit", "Commit Staged Changes", ["repository", "message"], Write),
        Amend => ("git.amend", "Amend Last Commit", ["repository", "message"], Destructive),
        CreateWorktree => ("worktree.create", "Create Worktree", ["repository", "branch", "folder", "start_ref"], Write),
        Overview => ("git.overview", "Git History, Branches and Stashes", ["repository", "limit"], Read),
        CommitDiff => ("git.commit-diff", "Git Commit Diff", ["repository", "commit"], Read),
        CreateBranch => ("git.branch-create", "Create Git Branch", ["repository", "branch", "start_ref"], Write),
        CheckoutBranch => ("git.branch-checkout", "Check Out Git Branch", ["repository", "branch"], Write),
        StashPush => ("git.stash-push", "Stash Git Changes", ["repository", "message", "include_untracked"], Write),
        StashApply => ("git.stash-apply", "Apply Git Stash", ["repository", "stash", "expected_commit"], Write),
        StashDrop => ("git.stash-drop", "Drop Git Stash", ["repository", "stash", "expected_commit"], Destructive),
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
                arg.required = !matches!(name, "folder" | "start_ref");
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

    pub(super) fn worktree_request(args: &[String]) -> Result<bootty_git::WorktreeRequest, String> {
        Ok(bootty_git::WorktreeRequest {
            branch: args.get(1).ok_or("Worktree branch is required")?.clone(),
            name: args.get(2).filter(|value| !value.is_empty()).cloned(),
            start_ref: args.get(3).filter(|value| !value.is_empty()).cloned(),
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
            Self::CreateBranch => git
                .create_branch(root, arg(1)?, args.get(2).map_or("HEAD", String::as_str))
                .map(|()| serde_json::Value::Null),
            Self::CheckoutBranch => git
                .checkout_branch(root, arg(1)?)
                .map(|()| serde_json::Value::Null),
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
            Self::CreateWorktree => git
                .create_worktree(root, &Self::worktree_request(args)?)
                .map(serde_json::Value::String),
            Self::Open => Err("Opening Git panels requires a window".to_owned()),
            Self::Status => {
                serde_json::to_value(git.changes(root)?).map_err(|error| error.to_string())
            }
            Self::Diff => {
                let group = match arg(2)? {
                    "staged" => ChangeGroup::Staged,
                    "unstaged" => ChangeGroup::Unstaged,
                    "untracked" => ChangeGroup::Untracked,
                    _ => return Err("Invalid Git change group".to_owned()),
                };
                git.file_diff(root, arg(1)?, group)
                    .map(serde_json::Value::String)
            }
            Self::Stage => git
                .stage_file(root, arg(1)?)
                .map(|()| serde_json::Value::Null),
            Self::Unstage => git
                .unstage_file(root, arg(1)?)
                .map(|()| serde_json::Value::Null),
            Self::Commit | Self::Amend => git
                .commit_index(root, arg(1)?, self == Self::Amend)
                .map(|()| serde_json::Value::Null),
        }
    }
}
