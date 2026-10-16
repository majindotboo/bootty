//! The conversation's branch beside the composer, and switching its checkout to another local
//! branch: directly from a clean tree, or by stashing the changes first.
use super::*;
use bootty_git::changes::{GitBranch, RepositoryChanges, RepositoryOverview};

/// Branches the menu lists at most, most recently committed first.
// The Changes panel searches all of them; raise this if the menu turns out to be browsed.
const MAX_LISTED_BRANCHES: usize = 20;

/// The checkout's local branches and whether it has changes, as Git last said.
#[derive(Default)]
pub(super) struct BranchChoices {
    /// The binding and branch these were read for.
    read_for: Option<(CommandTarget, Option<String>)>,
    branches: Vec<GitBranch>,
    dirty: bool,
}

impl NativeAgentSessionView {
    /// Reads the checkout's branches again when the conversation's binding or branch changed.
    pub(super) fn refresh_branches(&mut self, cx: &Context<Self>) {
        let Some((binding, branch)) = self
            .context_card
            .as_ref()
            .and_then(|context| Some((context.binding.clone()?, context.branch.clone())))
        else {
            return;
        };
        let key = Some((binding.clone(), branch));
        if self.branch_choices.read_for == key {
            return;
        }
        self.branch_choices.read_for = key;
        let cwd = self.record.config.cwd.to_string_lossy().into_owned();
        let submit = |command: &str, arguments: Vec<String>| {
            let mut invocation = CommandInvocation::from_action(command, Caller::Internal);
            invocation.arguments = arguments;
            invocation.target = Some(binding.clone());
            self.sender
                .submit(
                    invocation,
                    Instant::now()
                        .checked_add(Duration::from_secs(30))
                        .unwrap_or_else(Instant::now),
                    CommandCancellation::new(),
                )
                .ok()
        };
        let (Some(overview), Some(status)) = (
            submit("git.overview", vec![cwd.clone(), "1".to_owned()]),
            submit("git.status", vec![cwd]),
        ) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let (overview, status) = cx
                .background_executor()
                .spawn(async move { (overview.recv(), status.recv()) })
                .await;
            let read = |outcome| match outcome {
                Ok(CommandOutcome::Success { value, .. }) => Some(value),
                _ => None,
            };
            let overview = read(overview)
                .and_then(|value| serde_json::from_value::<RepositoryOverview>(value).ok());
            let status = read(status)
                .and_then(|value| serde_json::from_value::<RepositoryChanges>(value).ok());
            _ = this.update(cx, |this, cx| {
                if let (Some(overview), Some(status)) = (overview, status) {
                    this.branch_choices.branches = overview.branches;
                    this.branch_choices.dirty = !status.files.is_empty();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Switches the checkout to `branch`, stashing its changes first when it has any.
    fn switch_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some((binding, _)) = self.branch_choices.read_for.clone() else {
            return;
        };
        let command = if self.branch_choices.dirty {
            "git.branch-stash-switch"
        } else {
            "git.branch-checkout"
        };
        let mut invocation = CommandInvocation::from_action(command, Caller::Internal);
        invocation.arguments = vec![
            self.record.config.cwd.to_string_lossy().into_owned(),
            branch,
        ];
        invocation.target = Some(binding);
        let Ok(receiver) = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) else {
            self.error = Some("Could not reach the repository's host".into());
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(CommandOutcome::Success { .. }) => {
                        // Read the branches again for the checkout's new state.
                        this.branch_choices.read_for = None;
                        this.refresh_branches(cx);
                    }
                    Ok(outcome) => {
                        this.error = Some(diagnostic(
                            "Could not switch branches",
                            crate::commands::command_outcome_message(&outcome)
                                .unwrap_or_else(|| "Git refused".to_owned()),
                        ));
                    }
                    Err(_) => this.error = Some("The repository's host disconnected".into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The conversation's branch, with a menu of local branches to switch the checkout to.
    pub(super) fn render_branch_picker(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let context = self.context_card.as_ref()?;
        let branch = context.branch.clone()?;
        context.binding.as_ref()?;
        let others = self
            .branch_choices
            .branches
            .iter()
            .filter(|choice| !choice.current)
            .take(MAX_LISTED_BRANCHES)
            .map(|choice| {
                let distance = crate::gpui_git_panel::branch_distance(choice);
                let label = if distance.is_empty() {
                    choice.name.clone()
                } else {
                    format!("{} {distance}", choice.name)
                };
                (choice.name.clone(), label)
            })
            .collect::<Vec<_>>();
        let dirty = self.branch_choices.dirty;
        let target = context.target.clone();
        let owner = cx.weak_entity();
        Some(
            Button::new("native-branch-picker")
                .debug_selector(|| "native-branch-picker".into())
                .icon(gpui_kit::assets::IconName::GitBranch)
                .label(branch)
                .ghost()
                .small()
                .disabled(is_busy(self.record.snapshot.status))
                .tooltip(if dirty {
                    "Switch branches; your changes are stashed first"
                } else {
                    "Switch branches"
                })
                .dropdown_menu(move |mut menu, _, _| {
                    for (name, label) in others.clone() {
                        let owner = owner.clone();
                        menu = menu.item(
                            gpui_kit::component::menu::PopupMenuItem::new(label).on_click(
                                move |_, _, cx| {
                                    _ = owner.update(cx, |this, cx| {
                                        this.switch_branch(name.clone(), cx);
                                    });
                                },
                            ),
                        );
                    }
                    if let Some(target) = target.clone() {
                        let owner = owner.clone();
                        menu = menu.separator().item(
                            gpui_kit::component::menu::PopupMenuItem::new("All branches…")
                                .on_click(move |_, _, cx| {
                                    _ = owner.update(cx, |_, cx| {
                                        cx.emit(OpenNativeSession::Changes {
                                            target: target.clone(),
                                        });
                                    });
                                }),
                        );
                    }
                    menu
                }),
        )
    }
}
