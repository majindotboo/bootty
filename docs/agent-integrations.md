# Agent integrations

Bootty manages visible agent sessions. It does not hide an agent behind a
headless RPC subprocess.

Bootty does not infer agent state from process names, terminal output, screen
content, or transcript files. An agent reports, or Bootty shows nothing.

## Which session an event belongs to

An agent runs in a session, so every reported event names the pane it came
from and the sidebar shows one row per session.

Bootty exports `BOOTTY_PANE` into every pane it spawns itself, carrying the
same pane id the mux snapshot reports as `session.pane_id`. tmux exports that
id as `TMUX_PANE` inside its own panes.

Each adapter reads `${TMUX_PANE:-${BOOTTY_PANE:-}}` and passes it as the
second argument of its `ingest` command. An event with no pane lands on no
session row.

Pane ids are only unique on one server: local tmux and local rmux can both have
a `%1`. So each adapter also passes the server its pane runs on as the fourth
argument: tmux's `$TMUX`, or the `$RMUX` Bootty's rmux panes set while leaving
`$TMUX` empty (`socket,pid,session`). Bootty derives each local binding's socket
from its configuration and compares resolved paths, since tmux names its socket
through a resolved directory such as macOS's `/private/tmp`. Among the Spaces
listing the pane, only those on the reported server decide. A hook reaches the
app only through the local `bootty`, so a server no local Space names, such as
a remote host's, resolves to no Space. Adapters installed before this argument
existed omit it and resolve by pane id alone, as before.

A pane id belongs to the Space whose own sessions contain it. Spaces bound to
one tmux server all list every session on that server, so ownership decides. A
pane that still matches several Spaces lands on no session row.

A record follows its pane. Each keeps the server its hook reported, and whenever
pane ownership changes Bootty moves records to the Space that owns their pane
now: after a session moves to another Space, or once a pane whose hook arrived
before Bootty listed it is discovered. A record is dropped only when its pane is
gone from its server's listing, or its Space closes.

State mutations claim pending requests under the state lock after scope resolution.
Cancellation before that claim leaves state unchanged; event publication failure
after the mutation returns the committed snapshot with a warning.

The native provider owns its adapter and bounded protocol state. Install or
remove it from the provider's entry in Settings. Bootty writes the adapter and
updates the tool's configuration. Existing custom Lua/Luau files are preserved
and reported as unsupported; they are never executed.

## Pi

The native Pi provider starts Pi in the selected visible terminal:

```sh
pi
```

Use these commands through the Bootty CLI or socket:

```sh
bootty command agents.pi.start /path/to/worktree
bootty command agents.pi.prompt "Inspect the failing test"
bootty command agents.pi.steer "Check the persistence path first"
bootty command agents.pi.follow_up "Run the focused contract"
bootty command agents.pi.abort --yes
bootty command agents.pi.state
bootty command agents.pi.stop --yes
```

With an explicit target, `start` launches in that visible terminal. Without a
target, it creates a new visible tab first. `prompt`, `steer`, `follow_up`, and
`abort` operate on the selected terminal, or on an explicit target in any Space
without moving focus. `abort` sends Escape to Claude Code, which interrupts the
turn, and Ctrl-C to Pi and Codex.

A script can also start an agent in a session of its own, in any Space, without
moving focus: `spaces.list` returns each Space's Binding target, and
`session.create NAME CWD '["claude","..."]' --target BINDING` returns the new
pane's `terminal` target for `terminal.capture` and
`agents.<provider>.prompt --target`. See
[architecture](architecture.md#scripted-sessions).

Install the Pi extension from the `agents.pi` module in Settings to publish
native events from existing interactive Pi sessions.

A project can use `.pi/extensions/bootty.ts` after Pi trusts that project.

The adapter calls `agents.pi.ingest` through the live Bootty owner, with the
pane Pi runs in as the second argument and its server as the fourth.

The adapter uses one active publisher and a bounded event queue.

It coalesces `tool_execution_update` events for the same tool call.

It reports any dropped event count through `extension_error`.

## Codex

The native Codex provider starts Codex in the selected visible
terminal:

```sh
codex
```

Use these commands through the Bootty CLI or socket:

```sh
bootty command agents.codex.start /path/to/worktree
bootty command agents.codex.prompt "Inspect the failing test"
bootty command agents.codex.steer "Check the persistence path first"
bootty command agents.codex.interrupt --yes
bootty command agents.codex.state
bootty command agents.codex.stop --yes
```

Install the Codex hooks from the `agents.codex` module in Settings. The module
owns both the hook script and the native hook configuration Bootty merges.

The hook reads one native hook JSON object from stdin.

The hook calls `agents.codex.ingest` through the live Bootty owner, with the
pane it ran in as the second argument and its server as the fourth.

## Claude Code

Claude Code reports through command hooks, like Codex, and can also be started
in the selected visible terminal.

`agents.claude.state` inspects what the hooks reported:

```sh
bootty command agents.claude.state
bootty command agents.claude.state %3
bootty command agents.claude.start /path/to/worktree
```

Install the Claude Code hooks from the `agents.claude` module in Settings. The
module owns both the hook script and the native hook configuration Bootty
merges.

The hook reads one native hook JSON object from stdin and calls
`agents.claude.ingest` through the live Bootty owner.

A permission or question `Notification` is what tells Bootty that Claude Code is
waiting on the person. Claude Code also notifies when an idle session has waited
for input (`idle_prompt`); that reports idle, not waiting. `Stop` records the
turn's final text from `last_assistant_message`, and every event records the
transcript path.

## Resume, fork, and launch context

All three providers expose `agents.<provider>.resume [session] [cwd] [program]
[argv-json]` and `agents.<provider>.fork` with the same arguments. Pi uses
`--session`/`--fork`, Codex uses its `resume`/`fork` subcommands, and Claude uses
`--resume` with optional `--fork-session`. An omitted session ID is accepted only
when the selected pane reported one. Resume and fork always create a new visible
tab in the captured parent session and never type into the source agent.

The optional argv value is a JSON string array, so arguments retain their exact
boundaries. Launch values are bounded and reject controls and option-shaped
session IDs. POSIX launches use single-quoted argv; local Windows launches use a
UTF-16LE encoded PowerShell command. The adapter reports working directory,
session identity and reusable launch options. Bootty retains only known model,
profile, sandbox, approval and UI options; it drops prompts, arbitrary config,
credentials and old session selectors. Sessions launched with a persistence-off
flag cannot be resumed or forked from reported context. Explicit arguments can
still choose a different executable or options.

Start accepts the same optional argv JSON after cwd and program. The returned
success includes the target and sanitized launch context and means that the
command was submitted to its visible PTY; native events remain the authority for
agent lifecycle state.

## Limits and cleanup

Agent processes are owned by the visible mux pane that launched them. Closing
that pane stops its agent process tree. Reloading an integration does not stop
the interactive session.

Agent-specific JSON schemas, lifecycle rules, and installed hook adapters are
owned by the native `bootty-agents` crate. Existing custom Lua and Luau files
are preserved but unsupported; Settings reports them.

## Attention and navigation

The Agents Dock panel lists reported sessions across live local, SSH and WSL
bindings. Focus, resume, fork and mark-read actions use the same command path as
`agents.list`, `agents.focus`, `agents.next` and `agents.<provider>.acknowledge`.
Targets include a pane generation; closed or replaced panes fail as stale.
Entries also carry `source` (`existing`, or `restored` after a restart), the
provider `session_id`, its transcript or session file
(`session_file`), `last_event`, the start of the last turn's final text
(`last_message`, at most 1 KiB, with `last_message_truncated` when cut) and when
that turn ended (`turn_ended_at`, Unix milliseconds as a string). The listing is
one control response, so `agents.<provider>.state` on an entry's target returns
that pane's whole state, including the full final text (up to 64 KiB).

Reported state survives a restart. Each window keeps it in a private
`agent-state-<window>.json` beside the workspace database, written at most once
a second and on shutdown. Restored entries report `source: restored` until
their pane reports again, because the agent may have moved on while Bootty was
down. A restored entry names the pane id it was reported for; if the backend
reuses that id after its own server restarts, the entry shows until the new
occupant reports or the pane closes.
Resume and fork first focus the captured host, then create the new tab there.

Completion, input requests and errors receive monotonically increasing attention
sequences. Acknowledging a displayed sequence cannot clear a newer event. A
focused visible terminal acknowledges its own events; background panes retain an
unread marker. `session.agent_notifications` controls desktop alerts (`never`,
`unfocused`, or `always`). Duplicate status reports do not notify again.

Codex hooks include permission requests, tool completion and interruption. A
permission request reports an approval boundary; another hook may approve it
without displaying a human prompt, so the state clears when execution resumes.
See the [Codex hook contract](https://learn.chatgpt.com/docs/hooks).

The application tray aggregates reported agents across open Bootty windows. Its
unread count and menu update from the same projection as the Dock panel. A menu
entry focuses its captured pane in its originating window; entries from an older
menu revision are discarded. The native menu is capped at 128 agent rows, with
links to each window's full Agents panel. Closing the last reporting window
removes the tray; the tray never changes close or quit behavior.

macOS and Windows use native status/tray icons. Linux uses StatusNotifierItem on
a background service thread, with coalesced updates and shutdown when the owner
drops. A desktop without a tray service retains all window controls.
