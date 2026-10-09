# Agent integrations

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

## Native conversations

New session opens a prompt-first native conversation. New native agent tab adds
one to the current saved session. Codex, Pi and Claude use their installed
providers' public protocols: Codex app-server, Pi RPC and Claude stream-json.
The selected account, project, task and conversation identity stay captured for
the lifetime of that conversation. Local and remote conversations use the same
captured provider protocol; the remote daemon owns the provider process transport.

Session naming also updates the exact newly created conversation's tab title.
Delayed names apply only while each captured title remains unchanged; sibling
conversations retain their own titles. Submitting a prompt preserves the title
supplied at creation or by a later rename.

Native Codex launches use a process-local `default_mode_request_user_input` feature
override so supported SDKs can ask questions in Default mode. This does not change
the provider account's configuration or permission policy.

The transcript, composer, approval requests and questions live in a selectable
backend pane beside terminals. Close stops that pane and provider while retaining
the saved conversation and history.
Interrupt cancels its current turn. Opening a stopped conversation automatically
reattaches its exact provider selector: a Codex thread, Pi session file or Claude
session UUID. Pi's launch extension checkpoints its public session header and
entries before a fresh identity is published, then Pi reattaches that same file.
This preserves side chats that have not received their first message. The private
checkpoint command is excluded from composer completion. A rejected
prompt retains the draft and does not change the saved title. Claude history is
bounded to the turns retained by Bootty until a public history interface exists.

Codex approvals show the command, working directory and reason. Allow once retains
no grant. Allow for this session uses Codex's session approval cache; Always allow
applies only the exact proposed command-prefix rule displayed in the request.
Provider-restricted decisions, altered rules and expired requests are rejected.

Permission changes during a turn are saved for the next turn. The current prompt
and approval keep their captured policy. After the turn becomes idle, Bootty
reattaches the same provider conversation with the saved policy and a fresh tool
lease before accepting its next prompt.

Provider request waits pause while an owned question or approval is pending.
Answering or cancelling resumes the remaining transport budget. The provider's
actual reply still owns prompt acceptance; a visible dialog never invents turn success.
Pi extension commands can open a question before acknowledging their prompt.
Bootty returns the observed waiting state to the command caller immediately and
keeps an owned reply worker until answer, cancellation or shutdown. This prevents
the caller's deadline from expiring during human input; a later provider rejection
still appears as a conversation error.

Bootty tools attach privately at launch using the existing provider policy and
captured caller, Binding and terminal identity. Focus changes do not retarget
them. Native conversations also attach `list_agents`, `get_agent_status` and `list_models`
before provider initialization. These read the persisted conversation identity
and its exact provider account. Agent listing returns compact lifecycle metadata
only in the captured Space; status/model reads cannot choose another agent or account.
`list_providers` and `inspect_provider` expose only that captured provider's persisted
model, effort, fast mode and permission choices. They omit executable, account and
project paths, and do not authenticate or change policy. Legacy unspecified policy
remains unspecified; available choices use explicit modes supported by the provider.
Terminal agents keep their terminal-scoped catalog. Inherited child leases cannot
list siblings or advertise computer input/capture. Root resume renews the lease;
restored children keep their provider identity without renewing the former parent's grant.
Disabling the provider or revoking its authority
revokes the lease and child access. Private attachment arguments and credentials
are excluded from saved launch metadata. Computer capture and input each require
their existing grants.

`computer.capture` returns the exact application window as a validated PNG MCP
image with capture geometry. `computer.snapshot` retains its file export behavior.
Image responses have a separate bounded envelope; ordinary tool requests and
text responses retain their smaller limit.
Codex and Pi accept correlated image-tool results and bounded provider history.
Pi codemode image echoes must match the pixels from an observed capture in that
exact parent call. Only fingerprints are retained for the live turn; historical
image replies come from the correlated provider-owned history response and confer
no capture authority.
Claude tool-result echoes above 1 MiB remain unsupported until its public protocol
has a verified correlated format.

Native prompts accept host-admitted PNGs through the same conversation owner.
Captured image references remain in history; encoded pixels are transport-only.
Provider rejection retains the pending draft and its attachments.

Typed spawning tools create child work in the captured destination. Native parents
create native provider children in real mux tasks on their same local or remote
host. The provider executable, account, project, model and permissions come from
the accepted parent record; children start with fresh provider identities. Dependent work
starts only after its prerequisite's exact first turn succeeds. Failed or
interrupted turns retain their conversation for review. Recovery goes through
the shared invocation owner once. A stale destination requires explicit Restart.

## Retained sessions and limits

`agents.<provider>.history` reads bounded conversation metadata from the selected
provider account, scoped to the current project or all projects. Stored titles
take precedence; Claude and Pi conversations without a title use a short first-user-message
preview from the bounded file prefix. Discovery does not rewrite or launch a conversation.
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
