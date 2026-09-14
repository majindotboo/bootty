use crate::presentation::dialogs::DitchAction;

/// Translate the dialog's presentation value into the mux workflow input.
pub fn mux_ditch_action(action: &DitchAction) -> bootty_mux::workflow::DitchAction {
    match action {
        DitchAction::DetachWorktree => bootty_mux::workflow::DitchAction::DetachWorktree,
        DitchAction::KillOnly => bootty_mux::workflow::DitchAction::KillOnly,
        DitchAction::RemoveWorktree { force } => {
            bootty_mux::workflow::DitchAction::RemoveWorktree { force: *force }
        }
        DitchAction::RemoveWorktreeAndBranch {
            force,
            branch,
            repo,
        } => bootty_mux::workflow::DitchAction::RemoveWorktreeAndBranch {
            force: *force,
            branch: branch.clone(),
            repo: repo.clone(),
        },
    }
}
