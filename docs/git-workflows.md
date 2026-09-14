# Git panels

The Changes panel runs on the selected Space's host and now includes repository
history, local branches and stashes beside its stage, diff and commit workflow.
`git.overview` returns at most 200 date-ordered commits plus local branch/upstream
and stash facts. Selecting a commit opens its full patch in the existing Diff
panel. Branch creation resolves an explicit start ref to a commit first. Checkout
accepts only a currently listed local branch and requires a clean tree, so the
panel never guesses how to carry edits across branches.

Stash creation can include untracked files. Apply uses `--index` and deliberately
keeps the stash; deletion is the separate destructive `git.stash-drop` command.
A conflict is reported as Git left it, and the stash remains available. Every
branch/stash identifier is revalidated against a fresh repository snapshot just
before mutation. Local and remote workflows use the same typed commands and the
existing host runner; output appears only after Git reports completion.
