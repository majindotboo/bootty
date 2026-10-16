## Stable Bootty presentation keys. Insert user data as variables, never into message source.
menu-settings = Settings…
menu-about = About Bootty
menu-quit = Quit Bootty
settings-search = Search settings
settings-navigation = Settings Navigation
panel-files = Files
panel-changes = Changes
panel-diff = Diff
common-save = Save
common-reload = Reload
common-close = Close
common-cancel = Cancel
common-edit = Edit
common-preview = Preview
common-working = Working…
common-refresh = Refresh
files-parent = Parent
files-filter = Filter files
files-more = Load more…
files-loading = Loading…
files-load-failed = Could not load folder; collapse and expand to retry
files-expand = Expand to load
files-empty = Empty folder
files-no-matches = No matching files
files-depth-limit = Directory nesting limit reached
git-no-commits = No commits yet
git-staged = Staged
git-unstaged = Unstaged
git-untracked = Untracked
git-stage = Stage
git-unstage = Unstage
git-commit = Commit staged
git-amend = Amend…
git-message = Commit message
git-file-count = { $count ->
    [one] { $group } · { $count } file
   *[other] { $group } · { $count } files
    }
command-finished = Command finished
command-finished-status = Command finished with exit status { $status }
command-duration = { $seconds ->
    [one] Completed in { $seconds } second
   *[other] Completed in { $seconds } seconds
    }
document-unsaved = Save changes to { $name }?
document-discard = Discard changes
settings-search-placeholder = Search settings…
document-save-before-close = Save document before closing?
document-discard-reload = Discard this draft and reload the file?
common-discard = Discard
git-amend-confirm = Amend the last commit?
git-amend-detail = This replaces the last commit with the currently staged changes and this message.
git-amend-action = Amend

## Settings, sections, pages and commands keep their English with their schema and catalog.
## Translations add keys derived from stable paths (see `presentation_key`), never from labels.
find-title = Find
find-previous = Previous
find-next = Next
find-regex = Regular expression
find-case = Case sensitive
palette-title = Commands
palette-search = Search commands…
palette-empty = No matching commands
palette-hint = ↑ ↓ navigate   Enter run   Esc close
palette-count = { $total ->
    [one] { $shown } / { $total } command
   *[other] { $shown } / { $total } commands
    }

document-revert = Revert from Disk…
document-format = Format Document
document-format-short = Format
document-formatting = Formatting…
document-format-shortcut = Format Document · Alt/Option+Shift+F
document-multi-cursor-hint = Alt/Option+click: cursors
document-changed-during-format = Document changed while formatting; run Format again.
document-external-change = This file changed outside Bootty. Your unsaved changes are preserved.

panel-agents = Agents
panel-jobs = Jobs
panel-transfers = Transfers
panel-recovery = Previous sessions
panel-menu = Panels
panel-hide = Hide panels

