# Previous-session output

Set `session.output-archives = true` to checkpoint every attached, addressable
pane every 30 seconds. This is off by default because terminal output can contain
sensitive data. Each archive is private local application state and contains at
most 256 KiB of formatted plain scrollback plus host/backend, pane, geometry,
omitted-row and capture-time facts. Bootty keeps the newest 32 per app window.
Writes are complete-file atomic; malformed, oversized and non-regular files are
reported and skipped. The `Previous sessions` Dock panel labels output as an
archive. `recovery.list`, `recovery.get`, `recovery.export` (no overwrite) and
`recovery.delete` expose the same data through shared commands.

An archive is not a process checkpoint and is never fed back through the terminal
engine. It cannot ring bells, issue host commands, change the clipboard or send
input. tmux and Herdr archives contain Bootty's retained client-view history, not
backend-native history.

When an integration reported a persistent Pi, Codex or Claude session, Bootty
stores its session ID and the already-filtered reusable launch context. The panel
can explicitly resume or fork it in a new tab. At execution time Bootty requires
the original Space and a matching hash of its current host transport, validates
the launch and session ID again, and uses `new_tab`, `terminal.paste`, and
`terminal.submit` through the shared command mailbox. Prompts, credentials,
ephemeral sessions and arbitrary launch overrides are never archived. A replaced
host binding or missing session disables relaunch; Bootty never redirects it to
the focused terminal.
