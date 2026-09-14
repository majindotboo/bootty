# Shell assistance

Shells own editing. The command API exposes prompt leases and host-scoped history;
there is no separate shell editor panel.

## Commands

All callers use the same command mailbox:

- `shell.prompt`: read prompt revision, eligibility, shell, history path, cwd
  and recent metadata for the target native terminal.
- `shell.history QUERY`: read and rank that terminal's host history.
- `shell.apply REVISION TEXT SUBMIT`: atomically validate and hand off a draft;
  `SUBMIT` is `true` or `false`. Text is limited to 16 KiB; control characters other than LF and tab are
  rejected. LF is preserved in bracketed paste.
- `history.search SPEC`: host-scoped read on any binding. JSON fields are
  `shell`, `path`, `query`, `cwd`, and `recent` (normally an empty array).

The optional launch hooks preserve rc files and existing DEBUG traps. They emit
OSC 133 lifecycle plus bounded Bootty prompt/command metadata. Replayed output
never publishes these reports as live shell activity.
