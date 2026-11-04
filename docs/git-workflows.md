# Git workflows

Bootty reads Git on the selected Space's host through the existing host runner.
Local and remote work use the same typed commands.

The sidebar shows each session's branch, its pull request number and status, and
the lines its checkout adds and removes. These facts refresh in the background;
the UI never waits on Git.

The New Session form starts a session in the project's current checkout or in a
new worktree. A new worktree takes a branch, an optional base branch, and an
optional folder name. See `session.worktree-directory` and
`session.branch-prefix` in [Configuration](configuration.md) for where worktrees
go and how their branches are named.

The shared commands are:

- `git.overview` returns at most `limit` date-ordered commits plus local
  branch/upstream and stash facts for a repository.
- `git.branch-checkout` checks out a currently listed local branch. It requires
  a clean tree, so Bootty never guesses how to carry edits across branches.
- `worktree.create` creates a worktree from a branch, an optional folder and an
  optional start ref. An explicit start ref resolves to a commit first.

Every branch identifier is revalidated against a fresh repository snapshot just
before mutation. Output appears only after Git reports completion.
