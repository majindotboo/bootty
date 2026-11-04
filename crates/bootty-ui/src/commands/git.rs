use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind};
use bootty_git::{Git, runner::CommandRunner};

command_actions! {
    GitAction {
        CreateWorktree => ("worktree.create", "Create Worktree", ["repository", "branch", "folder", "start_ref"], Write),
        Overview => ("git.overview", "Git History, Branches and Stashes", ["repository", "limit"], Read),
        CheckoutBranch => ("git.branch-checkout", "Check Out Git Branch", ["repository", "branch"], Write),
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
            Self::CheckoutBranch => git
                .checkout_branch(root, arg(1)?)
                .map(|()| serde_json::Value::Null),
            Self::CreateWorktree => {
                Err("Worktree creation requires the configured directory".into())
            }
        }
    }
}
