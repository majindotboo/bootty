# Native terminal agents

Bootty launches Codex, Claude Code and Pi in real backend terminals. Their TUIs
own prompts, transcripts, approval dialogs and authentication. Bootty retains
bounded launch metadata and observes supported provider interfaces to show
activity beside sessions, including sessions that are not selected.

`bootty-agents` owns the provider registry, observation lifetimes and retained
metadata. The mux backend owns the terminal process, tabs and splits. The
palette, keybindings, CLI, socket and integrations use the same
`CommandInvocation` path and the same host-issued resource targets.

## Launch and control

For each provider, `agents.<provider>.start` opens a session and
`agents.<provider>.tab` opens a tab in an existing session. Provider names are
`codex`, `claude` and `pi`. Both commands accept optional `cwd`, `program` and
`argv` arguments. `argv` is a JSON array of literal arguments: quotes, spaces,
newlines and shell metacharacters are passed to the executable without shell
expansion. Executable and argument bounds are checked before backend creation.

`start` accepts an exact Binding target; `tab` accepts an exact Session target.
Native, rmux and tmux use the shared backend creation path. Herdr retains its
opaque attachment boundary and does not expose inner tab or pane creation.
Palette and keybinding launches select the returned terminal. CLI and socket
launches return its target without changing desktop selection.

`agents.<provider>.state`, `prompt`, `follow_up`, `steer`, `abort`, `interrupt`
and `stop` require the exact Terminal target issued by the host. The target
must still be live and registered for that provider. A stale generation is
rejected. Commands can address a terminal in another Space without selecting
that Space. Prompt delivery uses terminal paste and submit; it does not start a
second conversation process. Interrupt sends Escape to Claude or Ctrl-C to
Codex and Pi. Stop closes the backend pane and requires the normal destructive
command confirmation.

Provider options such as model, profile, thinking, sandbox and approval settings
remain literal native CLI options in `argv`. Retained metadata keeps only
reusable options. Prompts, credentials, arbitrary config overrides and transient
session selectors are excluded from that metadata.

## Observed activity

The public activity snapshot identifies its exact Terminal target and provider.
Its states are Starting, Idle, Working, Waiting, Finished, Stopped, Error and
Unavailable. A missing connection or unavailable native query is reported as
Unavailable; process presence alone does not prove that an agent is working or
waiting. Provider identities come from actual protocol responses or native
queries, never from a working directory, a recent file or terminal text.

### Codex

For a local native backend pane, Bootty owns a private Codex app-server and
starts the interactive TUI with Codex's supported `--remote` transport. A
transparent local relay forwards the native protocol and correlates actual
thread creation, resume and fork responses. Thread and turn lifecycle events
supply activity; a successful initialization handshake supplies Idle readiness
before a thread exists. Approval requests supply Waiting without answering or
changing the user's grants.

The private server has the same lifetime as the process-local pane's app owner.
It is not attached to a persistent rmux or tmux TUI: closing Bootty must not
terminate a surviving turn. Those backends launch the original Codex TUI
arguments and report observation Unavailable. Bootty does not start, stop or
reconfigure the user's shared Codex daemon to work around this limit.

The private relay currently requires Unix sockets. A platform without that
supported observation transport reports an explicit launch error for the
observed path rather than pretending a handshake succeeded.

### Claude Code

A fresh observed Claude terminal receives a native `--session-id` UUID. An
explicit resumed identity uses the provider's exact session ID. Bootty queries
`claude agents --json --all` and matches that complete identity. Native busy,
waiting and idle states drive activity; an observed working-to-idle transition
marks Finished. If the exact session is absent or the query fails, observation
is Unavailable. An ambiguous resume selector is rejected rather than assigned
to another conversation.

The query uses the installed executable and existing provider authentication.
Launching a TUI does not establish that the account is signed in.

### Pi

Bootty supplies a private extension only to the launched Pi process using its
native extension argument. No extension is installed globally. The supported
SDK provides the actual session ID and session file, agent lifecycle events,
and prompt start/end events. These drive Working, Waiting and completed,
failed or aborted outcomes. Nested prompts remain Waiting until the final
prompt ends.

The extension publishes bounded snapshots through a private local Unix socket.
Its identity token and files belong to that launch and are removed when the
observer retires. Unsupported platforms report the observation limitation
explicitly. Dropping an observer does not terminate a backend-owned Pi process.

## Accounts

`agents.<provider>.account.status` uses the installed provider's supported
account query. It returns the provider, an authenticated boolean when the
query establishes it, and a bounded explanation. It does not return tokens or
infer authentication from an available model. Pi requires a provider ID for its
native auth check and does not refresh credentials during a status read.

`account.login` and `account.logout` open the provider's own interactive terminal
flow in an exact binding. Codex uses its device login or logout command; Claude
uses its native auth commands; Pi opens its TUI with its own `/login` and
`/logout` menus.
Completing that terminal flow is the provider's responsibility. Bootty does not
claim authentication success merely because the terminal opened.

## Retained sessions and limits

`agents.<provider>.history` currently lists bounded retained terminal records:
exact targets, safe launch configuration and observed provider identities.
`resume` and `fork` accept an explicit provider session selector and launch the
provider's own TUI with its supported native arguments. The provider owns the
conversation history and displays it in that TUI. The metadata list is not a
transcript renderer or a complete provider-wide history index.

Restored active records are marked Unavailable until a new supported observation
is established. Explicitly stopped records retain Stopped and their observed
provider identities. Bootty does not infer a surviving TUI's identity after restart.
Remote backend launches use the backend's literal argv path; native observation
remains Unavailable until a supported host bridge exists. No local query is
used to claim a remote terminal's activity or account state.

Settings does not offer bundled hook installation. Existing user provider
configuration and custom Lua or Luau files are preserved; custom scripts are
reported as unsupported and are not executed. Existing ingress wire values
remain accepted by the legacy library boundary, but primary terminal launch and
activity do not depend on that ingress.
