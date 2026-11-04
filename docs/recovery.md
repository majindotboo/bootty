# Live terminals and cold restore

The local native mux server owns pane processes, layouts and terminal state.
Quitting, crashing or updating the desktop app disconnects its VTs; the server
keeps running. Reopening attaches to the existing sessions with their history,
cursor, modes, primary screen and alternate screen intact.

After a reboot or mux server loss, saved Space metadata remains in SQLite. A
missing session can cold-restore from its last validated checkpoint: layout,
titles, directories and bounded styled history return in new shell processes.
A cold checkpoint cannot restore a running program. Saved commands are never
replayed automatically. Existing server sessions always take priority over a
cold checkpoint.

# Previous-session output

Set `session.output-archives = true` to checkpoint every attached, addressable
pane every 30 seconds. This is off by default because terminal output can contain
sensitive data. Each archive is private local application state and contains at
most 256 KiB of formatted plain scrollback plus host/backend, pane, geometry,
omitted-row and capture-time facts. Bootty keeps the newest 32 per app window.
Writes are complete-file atomic; malformed, oversized and non-regular files are
reported and skipped. `recovery.list`, `recovery.get`, `recovery.export` (no
overwrite) and `recovery.delete` expose archives through shared commands.

An archive is not a process checkpoint and is never fed back through the terminal
engine. It cannot ring bells, issue host commands, change the clipboard or send
input.
