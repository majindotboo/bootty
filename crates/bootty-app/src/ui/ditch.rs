use bootty_ui::{Theme, ThemePalette};
use eframe::egui;

use crate::git::{self, WorktreeStatus};
use crate::strings::display_path;
use crate::ui::overlay::{self, ActionItem, ActionMenu, ActionRisk, FloatingWindow, StatusLine};

/// A cleanup action chosen in the ditch window, executed by the app layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DitchAction {
    /// Close the session, leaving the worktree and branch untouched.
    KillOnly,
    /// Close the session and remove its linked worktree (`force` discards dirty state).
    RemoveWorktree { force: bool },
    /// Close the session, remove the worktree, and delete its branch. `repo` is
    /// the main worktree resolved up front, so branch deletion still works on a
    /// retry after the linked worktree (and its cwd) is already gone.
    RemoveWorktreeAndBranch {
        force: bool,
        branch: String,
        repo: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DitchSessionDialog {
    session_id: String,
    cwd: Option<String>,
    status: WorktreeStatus,
    actions: Vec<DitchAction>,
    selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DitchSessionEvent {
    None,
    Close,
    Ditch {
        session_id: String,
        cwd: Option<String>,
        action: DitchAction,
    },
}

impl DitchSessionDialog {
    pub fn open(session_id: String, cwd: Option<String>) -> Self {
        let status = cwd.as_deref().map(git::status).unwrap_or_default();
        let main = cwd.as_deref().and_then(git::main_worktree);
        let actions = actions_for(&status, main.as_deref());
        Self {
            session_id,
            cwd,
            status,
            actions,
            selected: 0,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> DitchSessionEvent {
        let lines = status_lines(&self.status, self.cwd.as_deref(), theme.palette);
        let items = action_items(&self.actions);

        let result = FloatingWindow::new("ditch-session-dialog", "Ditch Session")
            .icon("trash-2")
            .hint("Enter confirm   Esc cancel")
            .width(overlay::panel_width(ctx, 600.0, 420.0))
            .show(ctx, theme, |ui, palette| {
                let outcome = ActionMenu::new(&lines, &items, self.selected).show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(action) = self.actions.get(index).cloned()
        {
            return DitchSessionEvent::Ditch {
                session_id: self.session_id.clone(),
                cwd: self.cwd.clone(),
                action,
            };
        }
        if result.escaped || result.clicked_outside {
            return DitchSessionEvent::Close;
        }
        DitchSessionEvent::None
    }
}

/// Offer only the cleanup actions that are safe and applicable: always a plain
/// kill, plus worktree/branch removal when the cwd is a linked worktree.
fn actions_for(status: &WorktreeStatus, main: Option<&str>) -> Vec<DitchAction> {
    let mut actions = vec![DitchAction::KillOnly];
    if status.is_linked_worktree {
        actions.push(DitchAction::RemoveWorktree {
            force: status.dirty,
        });
        // Branch deletion needs the main worktree path; without it, offer only
        // the worktree removal so we never queue an un-runnable cleanup.
        if let (Some(branch), Some(repo)) = (&status.branch, main) {
            actions.push(DitchAction::RemoveWorktreeAndBranch {
                force: true,
                branch: branch.clone(),
                repo: repo.to_owned(),
            });
        }
    }
    actions
}

fn action_items(actions: &[DitchAction]) -> Vec<ActionItem> {
    actions
        .iter()
        .map(|action| match action {
            DitchAction::KillOnly => ActionItem {
                icon: Some("x".to_owned()),
                label: "Kill session".to_owned(),
                description: Some("Close the session; keep the worktree and branch".to_owned()),
                risk: ActionRisk::Safe,
            },
            DitchAction::RemoveWorktree { force } => ActionItem {
                icon: Some("trash-2".to_owned()),
                label: "Kill + remove worktree".to_owned(),
                description: Some(if *force {
                    "Discard uncommitted changes and remove the linked worktree".to_owned()
                } else {
                    "Remove the linked worktree".to_owned()
                }),
                risk: if *force {
                    ActionRisk::Danger
                } else {
                    ActionRisk::Caution
                },
            },
            DitchAction::RemoveWorktreeAndBranch { branch, .. } => ActionItem {
                icon: Some("trash-2".to_owned()),
                label: "Kill + remove worktree + delete branch".to_owned(),
                // This force-removes the worktree, so warn about both losses:
                // working-tree edits and any unmerged commits on the branch.
                description: Some(format!(
                    "Remove the worktree and delete branch '{branch}' (uncommitted changes and unmerged commits are lost)"
                )),
                risk: ActionRisk::Danger,
            },
        })
        .collect()
}

fn status_lines(
    status: &WorktreeStatus,
    cwd: Option<&str>,
    palette: ThemePalette,
) -> Vec<StatusLine> {
    let mut lines = vec![StatusLine {
        label: "path".to_owned(),
        value: cwd.map_or_else(|| "(unknown)".to_owned(), display_path),
        tint: None,
    }];
    if !status.in_repo {
        lines.push(StatusLine {
            label: "git".to_owned(),
            value: "not a git repository".to_owned(),
            tint: Some(palette.muted),
        });
        return lines;
    }
    lines.push(StatusLine {
        label: "branch".to_owned(),
        value: status
            .branch
            .clone()
            .unwrap_or_else(|| "detached".to_owned()),
        tint: None,
    });
    lines.push(StatusLine {
        label: "worktree".to_owned(),
        value: if status.is_linked_worktree {
            "linked".to_owned()
        } else {
            "main".to_owned()
        },
        tint: Some(if status.is_linked_worktree {
            palette.accent
        } else {
            palette.muted
        }),
    });
    lines.push(StatusLine {
        label: "changes".to_owned(),
        value: if status.dirty {
            "uncommitted changes".to_owned()
        } else {
            "clean".to_owned()
        },
        tint: Some(if status.dirty {
            palette.warning
        } else {
            palette.success
        }),
    });
    if status.has_upstream {
        lines.push(StatusLine {
            label: "unpushed".to_owned(),
            value: if status.unpushed > 0 {
                format!("{} commit(s)", status.unpushed)
            } else {
                "up to date".to_owned()
            },
            tint: Some(if status.unpushed > 0 {
                palette.warning
            } else {
                palette.success
            }),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked(dirty: bool, branch: Option<&str>) -> WorktreeStatus {
        WorktreeStatus {
            in_repo: true,
            is_linked_worktree: true,
            branch: branch.map(str::to_owned),
            dirty,
            ..WorktreeStatus::default()
        }
    }

    #[test]
    fn only_kill_is_offered_outside_a_linked_worktree() {
        assert_eq!(
            actions_for(&WorktreeStatus::default(), None),
            vec![DitchAction::KillOnly]
        );
        let main = WorktreeStatus {
            in_repo: true,
            branch: Some("main".to_owned()),
            ..WorktreeStatus::default()
        };
        assert_eq!(
            actions_for(&main, Some("/repo")),
            vec![DitchAction::KillOnly]
        );
    }

    #[test]
    fn clean_linked_worktree_offers_non_forced_removal_and_branch_delete() {
        let actions = actions_for(&linked(false, Some("feature")), Some("/repo"));
        assert_eq!(
            actions,
            vec![
                DitchAction::KillOnly,
                DitchAction::RemoveWorktree { force: false },
                DitchAction::RemoveWorktreeAndBranch {
                    force: true,
                    branch: "feature".to_owned(),
                    repo: "/repo".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn branch_delete_is_withheld_without_a_resolved_main_worktree() {
        // A linked worktree whose main path could not be resolved must not offer
        // branch deletion — that cleanup would have no repo to run `git branch` in.
        let actions = actions_for(&linked(false, Some("feature")), None);
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, DitchAction::RemoveWorktreeAndBranch { .. }))
        );
    }

    #[test]
    fn dirty_linked_worktree_forces_removal() {
        let actions = actions_for(&linked(true, Some("feature")), Some("/repo"));
        assert!(actions.contains(&DitchAction::RemoveWorktree { force: true }));
    }
}
