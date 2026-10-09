# Bootty architecture

Bootty is a terminal workspace and local control host.

The repository contains one desktop application, one installed daemon, and
library crates with explicit owners.

The vault owns product language, plans, rationale, and durable decisions.
This file describes the current production structure.

## Architecture rules

- Each durable fact and live mutation has one owner.
- A module has a small interface and hides substantial behavior.
- Callers do not reconstruct invariants or synchronize duplicate state.
- Persistence commits before live publication.
- Backends own live processes and backend-native topology.
- Bootty owns cross-backend identity, presentation, and command safety.
- The UI thread does not block on terminal, extension, or agent work.
- Production and Development use separate local namespaces.

## Authority

| Fact or mutation | Owner | Failure rule |
| --- | --- | --- |
| Application identity and local namespace | `bootty-config` | A conflicting process identity fails startup. |
| The discoverable application process | The control instance lease | One identity publishes one generation endpoint. |
| Persistent Space and binding metadata | `bootty-mux::repository::WorkspaceRepository` | A failed commit leaves the prior state active. |
| Registered project paths, defaults and disclosure | `bootty-mux::WorkspaceRuntime` and `WorkspaceRepository` | Binding-scoped `workspace_projects` records survive without sessions. Registration, customization and collapse commit before publication. Names, icons and new-session provider/worktree defaults are stored together; custom image paths reference local presentation assets. |
| Saved session identity, title and task lifecycle | `bootty-mux::WorkspaceRuntime` and `WorkspaceRepository` | Persist state before publication; lifecycle changes never terminate or rename backend processes. |
| Saved terminal topology and bounded styled history | `bootty-mux::session_snapshot` and `WorkspaceRepository` | Complete, generation-scoped captures commit together; partial captures and failed writes retain the previous checkpoint. Logical identities remain separate from observed backend IDs. |
| Saved terminal presentation contract | `bootty-control::terminal_history` | Bounded text and SGR styles only; headless and desktop readers reject the same unsafe controls. |
| The live workspace and binding runtimes | `bootty-mux::workspace::{WorkspaceRuntime, BindingRuntime}` | A replacement appears only after validation and persistence. |
| Backend processes and native topology | The selected provider under `bootty-mux` | Bootty reports backend failure and does not invent success. |
| Git project, worktree, branch, and bounded diff facts | `bootty-git` | Git commands run through the owning host runner; remote paths never use local filesystem state. |
| The installed remote Space catalog | `bootty-mux::remote_catalog::Catalog` | A backend-scoped lease serializes mutations; membership follows backend session tags. |
| PTY and child lifecycle | `TerminalSession` and `TerminalWorker` | Runtime health reports asynchronous core failure. |
| VT state | `TerminalEngine` | Consumers read published frames. |
| Accepted product configuration, schema, document, and reload revision | `bootty-config::ConfigRuntime` | Invalid candidates do not replace the accepted policy. |
| Keymap JSONC model, matching, and atomic edits | `bootty-config::KeymapRuntime` | A failed edit does not replace the document or effective snapshot. |
| UI-derived config and terminal/input effects | `bootty-ui::AppConfigRuntime` | Derived validation runs before a UI-facing policy is published. |
| Command values and transport | `bootty-control` | All callers use one typed invocation path. |
| Mux command preflight, execution, and persistence ordering | `bootty-mux::executor` | Unsupported, stale, canceled, or failed work reports its typed outcome. |
| Desktop command resolution and UI policy | `bootty-ui::commands` | UI adapters resolve intents and delegate domain work to its owner. |
| Clipboard image transfer | `bootty-host::clipboard_image` | The daemon verifies the complete byte count and SHA-256 digest before publishing a private temporary PNG path. |
| Local control transport and instance ownership | `bootty-control` | The singleton lease publishes one owner-local endpoint. |
| Terminal agent observation and launch metadata | `bootty-agents::TerminalAgentService` | Observers own exact terminal identity, bounded activity and retained provider session metadata. |
| Seekable file and media reads | `bootty-host::file_reader` | Files are opened on their owning host with bounded ranges and revision checks; account-scoped history rejects paths outside its captured root. |
| Private stdio framing and remote endpoint relays | `bootty-host::private_stdio` | Bounded bytes use the owning host's encrypted process stream; tool grants and command authority stay with `bootty-agents`. |
| Native conversations and provider subprocesses | `bootty-agents::NativeAgentService` | Commit catalog snapshots before publication; resume only the captured task, account and provider conversation. |
| Persisted agent runs and dispatch attempts | `bootty-agents::OrchestrationService` | Commit before publication; recovery interrupts outstanding attempts without replaying prompts. |
| Desktop capture and input | `bootty-computer` | Disabled, denied, stale or occluded targets fail before input; the platform helper validates the exact observed process and window. |
| Native browser views and validated web content | `bootty-browser` | Native window systems report capability errors; navigation and bounded page events pass through the host adapter. |
| Local browser annotations and credential origin checks | `bootty-browser` | Bounded annotations persist atomically. Retained credential library APIs validate the document and exact origin; platform-store records remain separate from browser-managed passwords. |
| Browser page identity, selection and saved placement | `bootty-ui::BrowserPanel` and `gpui_dock` | A stale page ID fails rather than navigating another page; page content and placement persist in `native-panels.json`. |
| Terminal pane topology, ratios, and focus | `bootty-mux::BindingRuntime` | Providers remain authoritative. Hosts consume `MuxPaneLayout` and submit typed pane operations; they never persist a competing terminal tree. |
| Workspace composition | `bootty-ui::workspace_composition` | Projects real backend windows and pane layouts. A pane carrying a native conversation identity renders its agent view in that pane's rectangle. Numbered tabs, navigation, splits and close operations use the same backend targets for terminals and agents. Documents live in the right sidebar; the center has no inner tab strip. |
| Native panel placement | `bootty-ui::gpui_dock` | The GPUI Kit Dock composes Sessions on the left, the selected backend window in the center, and tools/documents on the right. `native-panels.json` stores sidebar sizes, visibility and documents. Center projections retain logical task/window keys and resolve through the current binding mapping. Agent placement belongs to real mux panes, never a separate Dock conversation tab or split tree. Transient creation choosers are not saved. |
| Per-window coordination and GPUI projections | `bootty-ui::{AppState, GpuiWorkspace}` | Views consume snapshots and emit typed intents; they do not become domain owners. |
| Preserved custom script diagnostics | `bootty-ui` | Existing Lua and Luau files remain on disk and are reported as unsupported. |

## Process composition

`ApplicationIdentity` has two values: Production and Development. Production uses
one fixed namespace. Development derives a stable namespace from the canonical Git
worktree so independently built and installed `BoottyDev` applications can coexist.

The identity and local Development worktree select the config tree, state tree,
control descriptor, local daemon catalog, rmux endpoint, tmux server, application
bundle identifier, and development CLI name.

The `bootty` executable launches the GUI only when the selected identity has no
live owner. An argumented invocation uses the owner-local control endpoint.

The installed daemon defaults to Production. Local development identity does not
change remote assets or remote backend namespaces.

Clipboard image pastes run as pending commands. Native clipboard reading stays in
`bootty-ui::platform`; PNG staging and transfer run on a worker. The command keeps
the original terminal target and revalidates its generation before inserting the
path. Switching focus never redirects a pending paste. Cancellation remains
available until insertion starts. Bounded remote document operations and streamed
image uploads use the shared host transport.

## Workspace and mux

`bootty-mux::repository::WorkspaceRepository` owns SQLite access for Spaces and
backend bindings. It creates fresh databases or loads the current schema as one
validated `WorkspaceSnapshot`, and records binding-scoped journals before backend
mutations. Unsupported schema revisions return a load error without conversion
or deletion. A failed commit leaves the accepted snapshot active.

`bootty-mux::workspace::WorkspaceRuntime` owns the live committed workspace and
its Space bindings. `BindingRuntime` owns one realized binding, its
`MuxController`, persisted membership and order, backend selection, reconnect
state, pane layout, focus, split ratios, titles, progress, ports, and terminal
attachments. Providers own the real processes and backend-native topology;
bindings reconcile their snapshots and publish only the facts the UI needs.

`bootty-mux::executor` is the common mux operation path. It performs capability
preflight, captures the binding scope, starts cancellation/deadline tracking,
submits the provider command, commits membership before publishing an
authoritative result, and reports stale, unsupported, unavailable, canceled,
and partial outcomes. `workflow` owns mux-level Git cleanup sequencing while
`bootty-git` owns the Git operations it calls. UI actions, CLI requests, socket
requests, and native integrations all enter this owner path rather than
reimplementing persistence or backend safety.

A durable workspace mutation uses this order:

```text
intent
  -> candidate
  -> validation
  -> one SQLite transaction
  -> live replacement
  -> typed outcome
```

Backend membership cannot share the SQLite transaction. Bootty writes a
binding-scoped journal before create, rename, or ditch; the next authoritative
backend snapshot resolves partial completion. Space identity, remote placement,
selection restore, session membership/order/name records, and
reconnect policy remain in mux. The UI receives binding session groups and
immutable projections; it does not traverse or mutate repository storage.

`bootty-config` owns serialized backend, SSH, and binding values. `bootty-mux`
owns live backend-neutral snapshots, remote Space transport, the installed
remote catalog, the provider contract, capability registry, and providers.
Its `terminal-runtime` facet owns pane policy, terminal attachment, and the
desktop-only controller/workspace realization. The facet is optional so the
installed daemon keeps core providers and catalog operations without Ghostty VT
or local PTY support. `bootty-host` owns SSH and WSL commands, daemon installation,
remote framing, and generic process execution. `bootty-git` owns project
discovery, favorites, worktrees, status, and Git facts through its host-runner
seam. Worktree creation resolves a branch, optional sibling folder name and optional
starting ref on that same host. It resolves the ref to a commit before creating
the checkout, then creates the branch. A branch failure removes only the clean
checkout it created; failed cleanup reports the retained path. The New Session
form and `worktree.create` command share this policy. Remote daemon protocol 7
carries the typed request, preventing older daemons from silently dropping options.

The native, rmux, tmux, and Herdr implementations are internal `bootty-mux`
providers registered through one registry. Herdr is opaque: Bootty attaches to
its ordinary local or SSH client and does not model its inner workspace, tabs,
panes, processes, or agents. The daemon uses mux's core registry for
catalog-backed remote Spaces.

## Terminal path

`bootty-mux` delivers rmux live pane bytes through its public `pipe-pane` API.
A Bootty daemon helper shares each pane's stream over local IPC with bounded
buffers and exits when its last reader leaves. No output is spooled to disk.
The SDK keyframe restores the initial text and modes. The pinned rmux v0.10 patch
gives each pipe a bounded one-MiB byte queue instead of an overwritable cursor.
PTY publication runs on the blocking pool while a pipe is attached, so pressure
reaches the producer without holding the server's output-state lock. Pipe closure
releases blocked publication. A pane closed during helper startup is normal
teardown, observed through its stable SDK identity. Remote readers use the same
host-side path. Return to registry dependencies when rmux releases this contract.

```text
TerminalSession
  -> TerminalWorker
  -> TerminalEngine
  -> PublishedFrame
  -> per-pane GPUI TerminalView
  -> PaintPlanner
  -> TerminalRenderFrame
  -> GPUI terminal element
```

`bootty-terminal` owns host-neutral terminal geometry.
The `bootty-ui` GPUI adapter converts host coordinates at its seam.
Per-pane terminal views adapt published scrollback geometry to GPUI Kit's scrollbar.
The component owns hover, dragging, and auto-hide; requested offsets go back to the
pane runtime. The renderer does not paint or hit-test a second scrollbar.

`bootty-terminal` owns the Ghostty VT adapter, input encoding, terminal
effects, images, and immutable terminal frames. Each frame carries its extraction
lineage; consumers reuse row damage only against its predecessor, and rebuild
after skipped publications.

`bootty-terminal` owns shell selection, environment construction, PTY creation,
child cleanup, worker scheduling, command delivery, frame publication, and
runtime health. Unix PTY input uses an owned duplicate of the master descriptor;
closing it sends no synthetic newline or EOF key to a surviving backend pane.

`bootty-ui` owns semantic paint planning, text policy, vector sprite geometry, and GPUI scene
lowering. `GpuiTerminalView` consumes immutable frames and owns view-local focus, IME, cursor
blink, and input handoff. Text commands carry their ordered font-feature policy. No UI render
path reads a terminal engine behind a lock.

`GpuiTerminalZoom` prepares cold magnified scenes on a background worker, with one
running job and one replaceable request per pane. The view draws current content
at the last available resolution until preparation completes. Warm resolutions
use the normal incremental renderer; output, cursor blinking, selection, and IME
do not wait for another worker round trip. Image retirement stays on the UI thread,
including when a pane closes during preparation.

## Configuration

`bootty-config` owns the TOML schema, defaults, includes, reload dependency
tracking, validation, and atomic round-trip writeback. `ConfigRuntime` owns the
accepted `BoottyConfig`, editable document, settings schema, reload stamp, and
revision. Validation is supplied at the boundary that knows the derived policy;
the candidate is published only after that validation succeeds.

`bootty-ui` uses the published `gpui-kit` package for GPUI, styled components,
behavior primitives, platform support, and icons. `gpui::theme_integration` projects
Bootty's palette and configured fonts into Kit. The settings controls, keymap table,
and menus use Kit directly; the application does not import Zed's UI, theme, menu,
or asset crates. Dependencies use their unmodified published packages.
`native_platform` supplies Bootty's font policy through GPUI's public platform
interface and delegates native windows unchanged. `font_database` selects concrete faces;
`font_mapping` translates UI weight roles relative to the configured base and
keeps exact native faces through shaping and rasterization. Immutable mappings
keep cached text valid when settings change.
Appearance resolution retains each selected theme's colors before overrides,
so Settings can hide redundant reset actions without loading themes during rendering.

`bootty-config` also owns the Zed-compatible `keymap.json` JSONC model, typed
add/replace/remove edits, per-context built-in-default policy, and atomic writes.
Its framework-free `KeymapRuntime` parses and matches the effective keymap. The
UI supplies command resolution and GPUI context/action conversion; config never
imports GPUI or control transport.

`bootty-config` selects the identity-scoped config directory. `keymap.json`
is derived as a sibling of the selected `config.toml`, so Production and each
Development namespace have separate user keymaps.

`bootty-ui::AppConfigRuntime` wraps `bootty-config::ConfigRuntime` to validate
keybindings and project accepted configuration into terminal, renderer, mux, and
host-input effects. `bootty-ui::keymap_runtime` supplies UI command descriptors,
focus/context state, and GPUI action conversion to the config matcher. Legacy
TOML keybinds are projected as built-ins and the user keymap is applied last.
Whole-file parse failure keeps the prior effective bindings; invalid sections and
entries are diagnosed without hiding valid neighbors.

`AppConfigRuntime` validates every derived input policy before workspace
construction or reload publication.

Settings and the Keymap editor are disposable `bootty-ui` sessions. They own
drafts, search, selection, focus, and editor entities. They emit typed edit
intents; the UI host sends those intents to `bootty-config` writeback and then
publishes the accepted runtime revision.

Live terminal publication happens after config acceptance.
A dead runtime produces a scoped warning.
It does not roll back the accepted config.

## Commands and control

`bootty-control` owns command descriptors, invocations, outcomes, cancellation,
and the bounded app command mailbox.

`bootty-ui::commands` owns the desktop command catalog, GPUI action mapping,
argument presentation, and UI-only policy. It resolves a command invocation and
delegates mux operations to `bootty-mux::executor` and agent operations to the
injected `bootty-agents::AgentService`. The owner of each operation performs
target resolution, capability checks, destructive confirmation, execution, and
result mapping.

The command palette, keybindings, CLI, local socket, and native agent
integrations submit the same `CommandInvocation`. CLI and socket callers use
control transport; UI callers may invoke the same owner executor directly when
transport would add no value. A caller with a response channel receives its
failure, including one that lands after dispatch, and the window shows no error
notice for it. `bootty-ui` decides that: a binding's controller never records an
authoritative command's failure as its own error.

`bootty-control` owns the local transport, read-only `ControlCatalog` metadata,
detached task and subscription state, and the singleton lease.
The initial desktop window owns the control server and its mailbox. Reopening
after that window closes can establish the next primary owner, but external
control has no router for targeting arbitrary secondary windows. Window and
binding scope are captured before asynchronous work so focus changes cannot
silently retarget a command.

Detached tasks and event subscriptions use opaque owner-local capability IDs.

## Agents

`bootty-agents::TerminalAgentService` owns native Pi, Codex and Claude observation,
exact terminal targets, retained launch metadata and provider account queries.
The backend owns each interactive TUI, its tabs, splits and process lifetime.
Codex uses a bounded local app-server relay, Claude queries its exact native
session identity, and Pi receives events from a per-launch native extension.
These observers retire with their owner. Hook installation is no longer offered;
existing integration files and legacy event records are preserved.

`bootty-ui` composes the service into the same command catalog used by the palette,
CLI and socket. Terminal launch/resume/fork open a backend window in the selected
task. Terminal actions receive literal argv and return an issued terminal target.
Nested commands preserve the original caller. Palette and keybinding Open actions
focus that returned target; other callers remain detached.

`NativeAgentService` owns direct Codex app-server, Pi RPC and Claude stream-JSON
subprocesses, bounded transcript snapshots, first-hand approvals/questions and a private `conversations.json`
catalog beside terminal observation state. The full workspace start view submits
`agents.native.start` with a frozen account/profile, directory and saved task
identity. Its real backend pane carries the conversation identity and renders the native agent instead of the backing terminal. The backing process supplies scoped terminal tools; it is not an extra visible tab.
The conversation catalog retains the resolved Bootty profile ID with the account directory;
resume and fork reuse both even after profile preferences change. Older records keep
their captured account without borrowing a current profile name.
Enter opens the conversation; Cmd+Enter leaves current focus alone. Cmd+N opens the
start view. Two empty Enters create a shell; editing resets that confirmation.
Codex can defer saving an empty provider thread until its first accepted turn.
If that empty reservation has no rollout after restart, Bootty creates a fresh
provider thread under the same conversation identity. Accepted turns and seeded
side chats retain their captured provider identity; missing history remains an error.

Remote native launches freeze the executable, account directory and canonical project
on the captured host. Their provider pipes use the same encrypted process transport
as terminal and control streams; resume never resolves an account on the desktop.
Per-launch MCP files use a private remote socket relayed to the original local tool
lease. Credentials stay on the provider host. Pi uses its public MCP registration
API, or its public tool registration API on versions predating that API. Native
child spawning creates a real mux task on the same host and starts a fresh native
provider identity with the parent's captured account, project and configuration.
`NativeAgentService` persists the exact spawning conversation. Child tool leases
retain their live ancestor and lose spawning, supervision, sibling reads and capture authority.
native parents can interrupt or stop only children whose persisted `spawn_parent`
matches the exact live parent generation. Both operations repeat the private
attachment, caller, Binding and cancellation checks through the shared command path.
Cold provider restoration does not renew a parent's former tool grant.
MCP creation receipts return issued targets and compact child metadata; reusable
launch configuration, account paths and transcripts remain in the host's records.
Images travel as provider payloads. Other admitted files use the host's checksummed
transfer stream into a private directory under the captured provider account. The
directory is scoped by application identity and a digest of the provider conversation
identity, so pane numbers and other desktops cannot collide. Repeated uploads verify
the existing bytes without replacing them. Only the received owning-host path reaches
the provider; desktop paths are never sent. These copies remain alongside the provider's
durable history across disconnects, restarts, and deletion of the local conversation.
Established foreground and background conversations reconnect through the shared resume
command with bounded backoff. Recovery preserves provider identity and never replays input.
Stopping a conversation or disabling its provider cancels pending recovery. Failed creation
returns its exact durable reservation target, so the error remains in its own mux pane.

New tab and split open a chooser in their captured destination before starting a
process. Agent selects a provider; Terminal and terminal profiles use the same
shared creation path. Browser and Diff remain right-sidebar tools. The chooser
captures the focused terminal or conversation, task and Binding, so later focus
changes cannot redirect its selection. Cancel discards only that empty chooser.

Choosing Agent and the provider submits `agents.native.tab` against an existing
captured task and Binding, or `agents.native.start` in an empty Space. Each launch
creates a distinct conversation in a real backend window. Native forks create a real backend split to the right. `MuxPaneAnchor.native_agent` carries the stable conversation ID: native stores it on the pane, tmux stores `@bootty_native_agent` as a pane option, and rmux stores it under the stable pane ID in its server options. Saved terminal checkpoints retain the same marker and reapply it to restored backend pane IDs. Provider credentials and thread IDs remain in the private agent catalog. Closing an agent closes its exact backend pane and stops its provider while preserving its catalog and transcript. Opening it restores its task and resumes that provider identity.
An Agent created in a split belongs to that backend window and has no duplicate global tab. Cmd+T from an Agent creates a new outer chooser; cancelling restores
the previous tab. Completing it replaces that chooser in the same destination.

The composer's worktree options edit the starting ref and optional branch/folder
overrides without leaving the draft. Worktree creation submits `worktree.create`
against that draft's captured Binding. After Git confirms the checkout, the
chooser submits session creation in it; failed provider startup retains the
checkout for retry. Project discovery workers never perform Git mutations.

The GPUI conversation view publishes streamed markdown and tool output. Its request
view owns approval and question controls and emits responses with the captured
target through the existing conversation dispatcher. Send,
interrupt and pending requests use separate command paths so cancellation does
not wait for prompt acknowledgement. Every operation validates the captured
conversation generation and Space. Resume retains the original provider thread
and account. Opening a stopped conversation automatically resumes that exact
provider identity through the shared focus command. Unexpected remote EOF retries
the captured conversation with bounded backoff and one pending resume, without
resending its prompt, stealing focus or repeating error notifications. The transcript gutter indexes
user turns, with cached virtual-list measurements retained while streaming;
copy actions and returning to the live tail act on that same transcript. Transcript
refresh runs on a worker and follows service revisions, without an idle polling loop.
Active-turn follow-ups use Codex's exact `turn/steer` acknowledgement or Pi's
`steer` disposition; rejection preserves the active turn and the composer draft.
Claude's transport currently admits prompts between turns. Context occupancy is
shown only from reported token counts and provider-advertised capacity. File
changes render the provider's diff rather than its raw JSON envelope.
Pi history uses bounded seekable reads on its captured host and rejects files outside
the captured account's session store. Source revisions are checked across pages.
Pi's persisted parent tool results restore their bounded nested execution details;
refreshing or reopening history retains the observed command input and outcome.

`NativeAgentService` also owns per-conversation attachment manifests and private
file copies. Transcripts retain typed attachment IDs and presentation metadata;
source paths never enter saved messages. Image previews are bounded reads from
that store, and deleting a conversation removes its local copies. Inline response
comments retain the exact assistant message ID, UTF-8 source range, and inline
prompt token range. Sent quotes stay inline and navigate to the referenced
response. Provider context includes those references while authored text stays intact.

Codex and Pi model menus use their live protocol catalogs; Claude uses the model
capabilities advertised by its initialized transport. Creation discovery
initializes a transient transport without starting a turn, then closes it.
Selected models and reasoning persist before the next prompt is submitted.
Provider permission modes are retained in the same conversation configuration.
The default preserves the captured account/profile policy. Supervised, automatic
edit acceptance, provider auto review (Codex/Claude), and full access use the
provider's own policy. Pi uses its public blocking tool hook for supervised
and edit modes. Changing policy saves it before stopping the idle provider,
then resumes the exact conversation with a fresh scoped tool lease; it never
replays a prompt. Provider policy cannot widen Bootty's application/tool grants.
The private parent tool lease retains at most 128 exact shell targets returned by
its accepted creations. Terminal input, capture, interruption and close use the
shared commands against those targets without changing selection. This authority
is transient: catalog history and terminal listings never grant it, and renewal,
revocation or disabling child tools removes it. Agent carrier panes are excluded.
Codex MCP confirmations with an empty form schema use first-hand, once-only
approval in the conversation. Structured MCP forms and URL/device authentication
remain pending and can be declined; they never invent answers or terminate the
provider merely for asking for input.
Model favorites live in the same agent catalog, scoped to the provider and exact
account directory and owning host. Both composer pickers read that accepted catalog; starring a
model commits before publication and does not configure or prompt a conversation.
Claude applies model and effort changes through its validated control protocol.
Its CLI-only maximum effort is offered at creation; live pickers expose the
efforts accepted by flag settings. Loading and retry states retain the draft.

Both Agent composers share retained completion state. Commands and skills come
from the selected provider/account's live protocol; probing a creation draft does
not start a conversation. File search runs through `bootty-host::files` on the captured host, with bounded
ranked Git indexes and ignore rules. Outside Git it completes the typed directory
without recursively scanning a home tree.
Selecting a file resolves `files.source` on that same captured binding. The
seekable host reader verifies its revision while copying at most 50 MB into a
private local staging directory. The original name and staged bytes stay alive
until attachment admission; changing the draft's host discards a pending result.
OS file-picker and clipboard attachments remain local user-selected sources.
App choices come from current exact local window tokens. Atomic editor tokens
preserve skill, file, and app references through undo and draft restoration.
Only authored text can invoke skills; appended browser annotations, files and
assistant quotes remain reference material. Codex receives typed skill inputs;
Pi uses its native leading `/skill:` command.

Native launch binds the persisted conversation target to its private tool lease
before provider initialization. Automatic agent listing reads compact accepted
activity only in that lease's Binding; status and model discovery use its exact
conversation and provider account. These reads share terminal/capture cancellation
and revocation, and never accept caller-supplied commands, targets or account paths.
Parent terminal and native integrations can inspect their captured Space's name,
backend and host label through `spaces.inspect`; child tools cannot inspect parent
Space metadata. `terminal.activities` lists up to 128 observed terminal panes in the
same captured Space, with names and opaque targets. It excludes native agent
carriers and process/directory fields, and does not start detached tasks.
These reads follow the lease rather than the selected Space.

Side chats copy history through a completed assistant response, retain the exact
project/account/model, and create a fresh provider session. `bootty-agents` owns
the durable source/boundary, immutable copied transcript and context-delivery receipt. The copied prefix and
attachment files are saved before publication; historical approvals, app grants
and provider writer identities are never copied. The first ordinary prompt seeds
that prefix once per provider identity. Quotes and nested forks read that saved
prefix even after live history is trimmed. A fork from an older Codex or Pi
provider page reads through that response without moving the displayed page;
lookback is bounded to 64 pages and copied context to 2 MiB. Large provider echoes receive extra
bytes only for the exact history submitted in their owned turn; unrelated fields
retain the ordinary control budget. `bootty-ui` places the child in its own
split and keeps source links without modifying the parent transcript.

Provider-reported subagent lifecycle rows preserve identity, model, observed
status and timestamps across transcript restoration. Child updates may outlive a
parent turn, but only that parent session can introduce a child. Codex child
transcripts use `thread/read` after verifying reported ownership; viewing never
resumes a child or acquires its writer. Pi task IDs are output/lifecycle identities,
not invented conversation IDs. Claude Agent/Task notifications must name an
observed parent tool. Unknown or foreign children remain inaccessible.

An explicit application mention grants that conversation access to its selected
window for the next submission. The lease owns opaque reference IDs, process
incarnation and geometry. A new prompt replaces that scope; stop/revocation
cancels pending application work. Grants and pixels are not persisted or inherited
by subagents. Screenshot, focus and input tools reenter the shared command path;
the signed helper still checks existing OS permissions, secure input and exact
window identity. Input requires the selected window to be focused; the focus
command raises only that window. Global feature defaults cannot widen a mention's
scope or substitute another application.

The native transcript folds completed thinking and tool work by turn while keeping
final responses visible. Live work retains its elapsed timer independently of the
fold. Selected assistant text supports `C` to comment, `R` to choose a reaction
with arrows and Enter, and `Y` to copy. These shortcuts apply only to the focused
response selection; the prompt editor keeps ordinary typing. Comments and Pi's
reaction meanings share the validated citation payload and editable inline tokens.

Native tool attachments expose `get_agent_activity` for the exact captured
conversation, including stopped retained history. `bootty-agents` projects up to
32 recent entries, most recent first, with 8 KiB of UTF-8 text per entry and an
explicit truncation flag. Launch configuration, account paths and tool inputs are
excluded. It uses the same read grant, generation checks and revocation as status
and model discovery; it does not load older provider history pages.

`bootty-agents` also owns the path-free provider-selection projection behind
`agents.native.provider`, `list_providers` and `inspect_provider`. These read the
captured conversation's persisted model, effort, fast mode and permission choices.
`agents.native.profiles` and the root-only `list_profiles` tool project at most
sixteen configured profile IDs and display names for that provider, alongside the
retained profile ID. Names do not grant account access; paths and launch arguments
stay with the host.
The tool catalog covers only the attached provider/account; it does not broaden
the launch grant or inspect authentication.

Native conversations support local Codex, Pi and Claude on Unix. Resume keeps
Codex's thread ID, Pi's exact session file or Claude's observed session UUID, plus
the frozen account and directory. Claude retains recent transcript snapshots;
its protocol provides no full-history query. Remote conversations and other providers
require a supported direct protocol implementation;
unsupported requests fail before shell creation. Native conversations receive
their private Bootty tool attachment under the existing provider policy. Each
attachment retains its exact terminal, caller, Binding, account and captured launch;
a sibling terminal observer cannot substitute its own identity. Closing the
associated terminal or revoking its authority invalidates inherited child
authority without widening grants. A failed provider launch retains the saved task
and conversation; opening it retries the captured resume path. Failed launches
are never reported as successful.

Ordinary creation, forks and spawned children publish the reserved `Starting`
conversation into its real mux pane before provider initialization. Placement
and provider failures retain that identity and report an error; they cannot
substitute a shell or change the parent conversation.

Provider account inspection uses the selected account store and supported native
protocols. Codex's read-only `account/read` reports account type and ChatGPT plan;
Claude's JSON account status reports authentication and subscription metadata.
Pi reports only readiness and authentication type for the selected model provider.
Unavailable metadata remains unknown. Checks never sign in, refresh credentials
or infer subscription tiers from credential presence.

Agent prompt, follow-up and interrupt commands require a live observation in
Idle, Working, Waiting, Approval or Input state. Restored metadata is readable but cannot authorize
input. Explicit stop still closes its exact retained terminal. Worker-side
`shutdown_and_wait` joins currently owned observations without closing backend
terminals; the GPUI teardown path remains nonblocking.

`OrchestrationService` owns bounded dependent runs and immutable snapshots.
`bootty-ui` freezes provider, account, caller and destination at creation, then
dispatches through the shared mailbox. Each accepted node retains its
exact native conversation generation and first-turn receipt ID. Only that receipt's
Succeeded outcome releases dependencies; idle, later turns, failed or interrupted
turns and unavailable observations never imply success. Failed initial prompts
retain their created conversation and error for explicit recovery.
Explicit restart requires the original durable window, Space and caller; it
obtains a fresh binding target without expanding tool authority. Old runs lacking
that destination metadata remain interrupted rather than using current focus.

Bootty does not infer agent state from process names, terminal output, screen
contents, or transcripts. The service persists what agents reported to a
per-window private file injected by `bootty-ui` and restores it, marked
`restored`, on the next start.

## Crates

- `bootty` owns executable composition, CLI grammar and output, config overrides,
  login-shell environment hydration, control startup selection, release updates,
  and native packaging. It constructs the services and launches `bootty-ui`;
  product domain policy does not live in the binary.
- `bootty-ui` owns GPUI initialization, windows, views, dialogs, terminal scene
  lowering, native input/IME/focus, per-window coordination, settings/keymap
  drafts, and presentation projections. It emits typed intents and delegates
  persistence, mux, Git, host, and agent operations to their owners.
- `bootty-control` owns command descriptors and invocation envelopes,
  cancellation, mailbox backpressure, local transport, detached task and event
  subscription capabilities, discovery, the singleton lease, and validation of
  the shared saved terminal presentation format. It has no
  product command implementations.
- `bootty-config` owns product configuration, defaults, includes, validation,
  writeback, identity namespaces, keymap JSONC parsing/editing/matching, and
  framework-free font and modifier values.
- `bootty-daemon` owns the installed headless executable entrypoint, argv and
  identity parsing, endpoint lifetime, and protocol serving. It composes
  `bootty-host` and the mux catalog; it does not duplicate their policy.
- `bootty-agents` owns native Pi, Codex, and Claude provider state, explicit
  event parsing, command forwarding, lifecycle, and integration files/assets.
- `bootty-host` owns host identity/path interpretation, local, SSH, and WSL process
  execution, shell quoting, daemon installation/bootstrap, and generic remote
  framing/forwarding.
- `bootty-mux` owns Space and binding persistence, live workspace/binding
  reconciliation, mux command execution, snapshots, remote Space transport and
  catalog, the provider contract/registry, and internal native, rmux, tmux, and
  Herdr providers. Its optional `terminal-runtime` facet owns desktop terminal
  attachment and pane policy.
- `bootty-git` owns project discovery, favorites, worktree operations, status,
  branch/diff facts, and safe Git mutations through a host runner; it does not
  own Space persistence or UI state.
- `bootty-terminal` owns terminal semantics, input protocol encoding,
  host-neutral geometry, PTY sessions/workers, and immutable frames.
- `bootty-write` owns atomic write targets, locking, permissions, durability,
  and commit outcomes.

## UI modules

- `state.rs` owns per-window coordination: accepted config projections, mux and
  command owner handles, input/focus/dialog state, frame scheduling, and typed
  host effects. It does not own repository, provider, Git, or agent state.
- `native_host.rs` owns GPUI application and window lifetimes. It creates the
  primary workspace and its control server; secondary windows remain local UI
  surfaces and are not externally addressable through a new router.
- `gpui_workspace.rs` is the root GPUI entity. It coordinates child views,
  converts domain snapshots into immutable view snapshots, applies typed intents,
  and runs host effects after the update pass.
- `gpui_actions.rs` converts configured bindings into typed GPUI actions that
  submit the existing `CommandInvocation`; it does not execute domain work.
- `gpui_terminal_view.rs` owns one pane's GPUI focus, keyboard/IME input,
  retained terminal adapter, cursor blink, and frame presentation.
- `chrome_frame.rs` owns chrome intent reduction and presentation effects;
  `chrome_projection.rs` owns immutable window/session/agent view values derived
  from mux and native-agent facts.
- `config_runtime.rs` wraps `bootty_config::ConfigRuntime` to validate
  UI-facing derived policy and turn accepted changes into terminal, renderer,
  input, and window effects.
- `keymap_runtime.rs` supplies command descriptors, UI context/focus state, and
  GPUI action conversion to `bootty_config::KeymapRuntime`.
- `gpui/settings` owns settings navigation, input focus, and its `SettingsSession`
  draft. `gpui_settings.rs` applies typed edits and derives controls from config;
  the workspace only bridges document effects and external commands.
  `settings_runtime.rs` runs config and integration operations. Accepted writes
  go through `bootty-config` and native agent integration APIs.
- `gpui_keymap_editor.rs` owns the separate keymap editor's persistence adapter.
- `commands.rs` and `commands/runtime.rs` own the desktop command catalog and
  adapter lifecycle. Mux commands delegate to `bootty-mux::executor`; agent
  commands delegate to the injected `AgentService`.
- `new_session.rs` and `remote_catalog.rs` own picker/task presentation and
  cancellation. Project, worktree, remote transport, and catalog policy remain
  in `bootty-git`, `bootty-host`, and `bootty-mux`.
- `terminal_interaction.rs`, `input`, `presentation`, and `state/dialog*`
  route native events exactly once to a modal/component, domain command, or
  terminal encoder and retain UI-only focus/selection state. Modal command context
  and input blocking derive from the live dialog, not a second mutable focus flag.
  GPUI-component's dialog trap and Root own widget focus confinement and restoration;
  Bootty retains only the underlying terminal, sidebar, or find-input route.
  Save, discard, reload, and Git confirmations use the shared themed alert dialog in
  `gpui/dialogs.rs`; their callers retain ownership of the confirmed operation.
- `gpui/terminal.rs`, `paint_plan.rs`, `terminal_render.rs`, text-atlas, and
  sprite modules own terminal paint planning, shaping/raster caches, clipping,
  and direct GPUI scene lowering. They consume `bootty-terminal` frames. Text atlas
  records retain natural ink extents and bearings independently of grid dimensions;
  only explicit symbol constraints fit glyphs, while viewport masks clip terminal ink.
  Color glyphs shape complete graphemes through the application's shared GPUI text
  provider; the terminal atlas retains cell fitting and pixel composition.

These files can remain large only while each keeps one cohesive owner and a
small interface. File size is a signal for review. It is not proof of depth.

### Native tool panels

`bootty-ui::gpui_dock` composes fixed homes: Sessions on the left, a terminal
singleton in the center, and one labeled tool/document tab group on the right.
Dock edges resize; panels cannot relocate or create nested groups. The binding
owns terminal windows, panes, processes and input. Native panels never reconstruct
mux topology. One application-owned writer persists dimensions, visibility and
content in the window's `native-panels.json`.

Files, Changes and Diff follow the selected terminal and directory. Captured Git
operations retain their original binding and repository; late replies cannot
retarget another context. Documents retain their own host and path identities.
Each browser page is a peer tab in this same right group, with an exact page ID;
there is no nested browser tab strip. The browser owner hides native views when
the dock or page is hidden and while GPUI overlays require keyboard focus.

`browser.snapshot` reads bounded visible text for an exact page ID and host window.
An optional document-start token pins the read to an earlier host observation;
without it, the owner captures the current document before reading. It never
focuses or navigates. The browser owner rechecks
the native view, load revision, address and document before publication; cancelled
or expired reads publish nothing. Form fields and editable content are excluded.
Traversal stops at 10,000 text nodes or 16,384 UTF-16 units and reports truncation.
This read API does not itself grant agents access: a browser MCP capture requires
a host-issued exact window, page and document. URLs and annotation JSON cannot
issue that grant.

The native browser's **Attach page** action submits `agents.native.browser-attach`
for the exact selected conversation. The live tool lease owns one attached
window/page/document; **Detach page** clears it. Its MCP catalog advertises
`browser_snapshot` before the first prompt so SDKs can cache the tool, but reads
fail until explicitly attached. Replacing or detaching a grant cancels queued
reads and withholds already accepted results from the prior grant. Child leases
cannot acquire this access. Restoring or resuming a conversation starts without
browser grants; live UI projections are not serialized in the conversation store.

`browser.input` accepts a positive page ID and a typed click, scroll, type or key
action. On macOS the browser owner sends native events directly to its visible
WKWebView content view without activating the application or posting global
input. Hidden pages, overlays, out-of-bounds points and invalid actions fail.
Other platforms report that native background input is unavailable.
Wry's macOS child-view Command-shortcut override also remains unsupported;
such input returns an error rather than claiming that the shortcut ran.

Built-in browser-managed password save/fill is unsupported through the current
public webview API. The GUI has no password manager. Existing app-specific
credential storage and origin-checked library APIs remain intact; native
platform-store runtime acceptance has not been performed.

The sidebar owns saved task presentation, project grouping/search and the compact
usage footer. Space switching remains separate from task tabs. Working and
waiting agent status remains visible on unselected tasks. Lifecycle filters and
context actions capture the saved identity and Binding; process status never
changes durable task lifecycle.
Pinned tasks form a separate section. Manual order remains owned by binding
membership; recent activity orders accepted terminal input and native prompts.
Focus, repaint and process output do not stamp activity. The attention filter
currently includes approvals, unanswered input and failures; unseen completions
require persisted per-conversation read markers before joining this filter.

Terminal and native panel tabs share `gpui::tabs` and typed visual preferences.
Every tab has a close action. The right group's Add menu offers unopened tools
and new browser pages. The terminal Add menu offers shells and enabled native
providers within the same task. Both menus submit `CommandInvocation`s.
Dock requests validate captured destination IDs and complete after application;
stale groups fail rather than using a newly selected destination.

Changes and Diff use `bootty-git::changes` through the shared `git.*` command catalog.
Invocations capture a binding generation and repository path. Local and SSH runners
execute the same Git argv; local refreshes reuse the chrome's Git watcher, and remote
refreshes run only while Changes or Diff is active. Index mutations and commit/amend remain
Git-owned. Starting a Git mutation commits its cancellation token; Bootty waits for
Git's actual result rather than interrupting a commit hook and inventing its outcome.
Tool-panel focus selects the existing `Other` keymap context and bypasses raw terminal
key capture, while global application bindings remain available.

### Files and documents

`bootty-host::files` owns bounded filesystem reads, directory pages and
revision-checked writes. Local callers and the `bootty-daemon file` endpoint
execute the same typed request. Documents are UTF-8, at most 512 KiB; transport
uses base64 so JSON escaping cannot overflow the control frame limit. The
read sniffs a bounded header and returns metadata-only `MediaDescriptor` values
for images and video containers. `bootty-host::media::MediaReader` owns a seekable
local file or persistent SSH/WSL daemon channel. Encoded media has no document-size
cap; each binary range and reader cache is bounded to 1 MiB. Open and range reads
check an opaque file-metadata revision, separate from text-save SHA-256 revisions.
Detected edits reject the source; metadata checks are not immutable snapshots.
Remote reads have operation deadlines and explicit cancellation terminates their
process tree. The UI decodes images off-thread with dimension and memory limits,
applies EXIF orientation, fits the first frame, and retains host-bound refresh.
Decoded pixel limits remain independent of the encoded media transport.
On macOS, AVPlayer requests ranges from the same reader and owns video timing,
decoding, orientation and audio. GPUI retains the current GPU frame. Hiding a
preview pauses playback; closing it cancels its source. Other platforms retain
image and Markdown previews.
Markdown documents retain editable source and an optional selectable preview.
The existing atomic-write owner preserves target permissions and symlinks. A save
compares the loaded SHA-256 revision under the writer lease before replacement.
Formatting runs a selected formatter on the host with the bounded draft on
standard input and returns bounded output. It never writes the file.

`bootty-ui` owns document drafts and Files/Document panel presentation. Media discovery and document operations
enter `files.*` through `CommandInvocation`, capturing a binding generation. Media
readers use the resulting descriptor and captured remote configuration on a worker;
large binary bodies never enter the control response.
Document identity combines the host configuration digest with the absolute
host path. Restore never silently adopts a different host. Dock persistence
stores path, host identity, caret and preview mode, never document contents.
Dirty editors survive tool-context switches and require a close decision;
failed writes retain the draft. Local parent-directory watches invalidate
snapshots across atomic replacement; visible remote documents poll every five
seconds. Polling and filesystem work stay off the GPUI thread.

### Pane arrangement

The shared session-targeted commands `pane.swap SOURCE TARGET`,
`pane.move SOURCE TARGET left|right|up|down`, `pane.extract SOURCE`, and
`pane.merge SOURCE_WINDOW TARGET_WINDOW` preserve backend pane IDs and processes.
Native transfers stay within a session so terminal runtime keys remain unchanged.
`bootty-mux::workspace::pane_arrangement` captures the binding's split trees before
backend dispatch and applies the resulting placement only after authoritative
success, checking the final pane set. A merge retains each tree's internal ratios.
The desktop serializes arrangement against pending mux commands in the same binding.

Native split surfaces have a corner drag grip: drop near an edge to insert beside
that pane, or in its center to swap. Right-click the grip to extract. Tab menus offer
extraction, movement to another tab and merging into the active tab. These gestures
submit the same scoped `CommandInvocation` used by CLI and socket callers.

Native supports all four operations. tmux supports individual stable-ID swaps,
joins and breaks; it exposes all real pane identities while retaining its single
attach surface. Whole-window merge is unavailable there because tmux cannot make
multiple joins atomic. The pinned rmux protocol addresses transfers by mutable
indices, so transfers stay unavailable until stable-identity requests exist.
Herdr's inner panes remain opaque. Unsupported operations report capability failure.
Remote daemon protocol 5 carries the new operations through the existing transport.

### Terminal links and selection

`bootty-terminal::terminal_links` recognizes bounded logical lines in published
frames, including soft wrapping, file locations and contiguous OSC hyperlinks.
Explicit hyperlinks take precedence. Semantic double-click selection uses this
same mapping, quoted/bracketed text, email addresses and Unicode word boundaries;
ordinary words keep the terminal engine's selection behavior.

Command-click (macOS) or Control-click opens through `link.open LOCATION [CWD]`.
The command captures a terminal target and resolves files on that binding's host,
then invokes `files.open` or `files.browse`. Relative paths require a known pane
directory; an opaque multi-pane attachment cannot infer a clicked inner pane's
working directory. Host I/O and forwarding run on workers, and stale or cancelled
results cannot open panels or URLs. Ordinary terminal mouse input stays with the
application; the link gesture suppresses both its press and release.

Remote HTTP(S) loopback links use a private OpenSSH control master owned by the
binding generation. Bootty waits for authentication and listener creation before
opening the rewritten URL, reuses live forwards and closes them when the binding
ends. Public URLs open directly. The forward uses existing SSH configuration and
never closes the user's control master. This capability currently requires Unix
OpenSSH control-master support; unsupported platforms report failure.

### Shell lifecycle and notifications

`bootty-terminal` parses OSC 133 into typed shell events and stamps live delivery
on the terminal worker. Recovery drops these events alongside bells and other
one-shot effects. Optional bash/zsh/fish launch hooks live with the terminal
launch owner, retain private startup files for the child lifetime, and preserve
user rc files. Fish's native markers take precedence. Explicit launch arguments
and backend-owned shell launchers retain their own configuration.

`WorkspaceRuntime` drains notification events from active, inactive and parked
terminal owners and retains them across Space changes. Inactive clipboard and
other host-control effects remain discarded. `bootty-ui` owns transient command
timers keyed by binding generation and pane, completion policy, bell rate limiting,
and OS delivery. Finish consumes the timer; duplicate or orphan finishes cannot
notify. GPUI owns the visual bell deadline and uses the pinned platform system-bell
API for audio. Desktop delivery runs on a worker through the existing `notify-rust`
dependency, using the current application identity.

### Interface language

`bootty-config` persists `locale`; `bootty-ui::i18n` owns message catalogs, per-message
English fallback and formatting. `AppState` updates its localizer when a config
commit/reload is accepted. The GPUI host publishes locale changes once per local
revision to the component library and refreshes windows. The library retains its
own catalog ownership. Translation touches presentation fields after identifiers
are formed, never command invocation data, user text or backend facts. See
[localization](localization.md) for catalog keys and current coverage.

WSL is a host transport, not a mux backend. `bootty-config::RemoteConfig` preserves
legacy SSH tables and distinguishes a validated WSL distribution.
`bootty-host::RemoteHost` dispatches structured process, daemon, file, and Git
requests; `bootty-mux` retains backend and Space authority. WSL uses Linux rmux/tmux
servers. Herdr retains its SSH public-client boundary. Distro discovery is an
asynchronous UI/command projection; it does not mutate or install distributions.

### Terminal capture and export

`bootty-terminal` formats bounded selections on its existing terminal worker.
The selection is temporary and never replaces the user's selection. Both native
PTY and rmux workers implement the same asynchronous capture request; the UI
waits through its command mailbox without blocking a frame.

`terminal.capture [format] [scope] [max_lines]` returns text and metadata.
Formats are `plain`, `ansi` and `html`; scope is `screen` (active screen) or
`history` (latest retained rows including the screen). Defaults are plain,
screen and 10000 rows. Captures report omitted physical rows, dimensions,
alternate-screen state, the exact target and the host/backend source. Native
and rmux captures represent a pane. A tmux or Herdr pane on screen captures the
attached client's VT state, not backend-owned inner pane history or original
PTY bytes. A backend pane Bootty has no render state for, in any Space, is read
by the backend: a tmux pane off screen (`capture-pane`), or an rmux pane nothing
has shown yet (the SDK's capture). That source is `backend_pane`, reports only
its text and line counts, and does not support `html`. Captures never change
focus.

`terminal.write`, `terminal.paste` and `terminal.submit` with an explicit target
reach that pane in any Space without selecting its window or moving focus.
A pane with a terminal runtime, native or rmux, takes the input there, so keys
are encoded for its keyboard mode. Other backend panes are addressed by stable
pane id: tmux with `send-keys`, and `load-buffer` plus `paste-buffer -p`; rmux,
for a session never shown, with the SDK's pane input and a private buffer pasted
with `bracketed` set. Either way a paste is bracketed only when the application
asked for it. Without a target they write to the focused terminal. The mux
completes the command only after the backend acted. Backend input and capture
run on a worker and claim the binding's command-config fence right before
acting: work for a Space closed or moved to another backend in the meantime
fails as stale.

Catalog-backed remote Spaces and remote rmux run the same pane operations in
the remote daemon with its own backend implementation. The request travels on
the daemon's stdin, since a paste can exceed a remote command line, and the
catalog refuses a pane whose session does not carry the Space's tag. Remote
daemon protocol 19 carries these operations, native-agent pane markers and exact
pane activation, explicit-create argv, filesystem
completion and captured working directories for host commands. The daemon
executable path is versioned by that protocol, so a client never reaches
an older daemon that would drop a field it does not know.

`bootty-host::remote_link` owns the remote process transport. SSH authenticates
the host, starts the versioned daemon and exchanges public certificates. The
daemon and client then require mutual TLS over QUIC; private keys remain in
memory. If direct UDP is unavailable, the same framing and flow control run over
HTTP/2 with mutual TLS through one owned SSH TCP tunnel. The TCP listener is
loopback-only and requires the certificate exchanged during SSH bootstrap. One
connection carries separate bounded streams for terminal input,
output, resize, control commands, files and Git. PTY clients enter raw mode so
single keys do not wait for a newline. A private local relay keeps existing
process/PTY adapters on this same path. Local clients pin its TLS certificate
before sending authorization or commands. Its endpoint descriptor is private and
isolated by application identity and SSH target. Connections are acknowledged
before relay publication. Development daemon calls inherit the invoking client's
Development identity and namespace,
and Development tmux attachments and control queries share their named server;
explicit daemon caller identities retain their wire values.

A direct connection gets a short head start; an owned SSH tunnel starts while
a blocked UDP dial is still pending. Only an authenticated readiness reply can
select the winning transport. If neither connection can be established before
submission, execution
falls back to the existing SSH path. Once a request is submitted, connection loss is
reported rather than replaying a possible mutation. Disconnect terminates the
execution or attach client; the selected mux backend retains its owned sessions.
Remote Unix PTY writers also close without injecting input into those sessions.
Detached macOS commands retain their real stdout, stderr and exit status through
private launchd capture files, checked against the same 16 MiB output limit.
The relay expires after five idle minutes and remains alive while streams are
attached. WSL keeps its local distribution process transport.

`terminal.export <local-path> [format] [scope] [max_lines]` atomically publishes
a new private file. The destination must be an absolute local path. It never
replaces an existing destination. The palette's
**Export Terminal** form uses this same command with a captured target and
retains the form on failure. Closing the form cancels before publication when
possible. The default form includes history. Commands accept up to 100000
physical rows; capture responses allow 128 KiB of formatted text to leave room
for JSON escaping, and local exports allow 2 MiB. Oversized output fails with
its required size so callers can request fewer rows; it never cuts UTF-8 or
style sequences in half.

### Scripted sessions

`session.create NAME CWD [ARGV]` creates a detached backend session in the target
Binding's Space, which need not be active. It never changes the selected session,
the selected window or the active Space: it is submitted with
`CommandSelection::Preserve`, which keeps whatever the binding has selected when
the result lands. `bootty-mux::workspace` validates the request and journals the
Space's membership under a stable identity. The backend name is a locator hint,
independent of the saved display title. The name must be free on the binding's server; the create
fails rather than adopt an existing session, locally and in a remote Space. ARGV is a JSON array of strings for
the first pane: absent or empty starts the default shell, one element runs through
the backend's default shell, and more elements run directly. It is bounded to 64
elements and 12 KiB, because tmux carries one client command in about 16 KiB. The
result carries the `created` Session target and the first pane's `terminal`
target.

tmux passes ARGV after `--` in the same `new-session` invocation as the stamps,
escaping a trailing `;` so tmux does not end the command there. rmux uses a
create-only `EnsureSession` with the SDK command vector. A native pane otherwise
starts its shell the first time it is shown, so once a native create lands the
workspace starts the first pane with ARGV at once, hidden, in the terminal owner
that will show it: the active binding's shared native owner, or the parked one
while another backend's Space is active. Before any native Space has been active
nothing is parked, so that start parks the target binding's own owner and the first
native activation adopts it. The pane starts within the create's dispatch, so no
frame can show the pane with a shell first; showing it presents that runtime. The
command answers once the pane's process has started. A program that cannot start,
such as one missing from `PATH`, fails the command and closes the session through
`session.close`, so the name is free again. A native cwd must be an existing
directory: the PTY layer would otherwise start in the home directory. ARGV is never
persisted or replayed. A saved terminal checkpoint can restore a task after its
backend process has exited; this starts shells in the recorded directories rather
than replaying commands. Supported terminal agents resume separately from retained
provider session metadata.

`session.close` is destructive and kills a session its Space holds, in any Space,
without Git cleanup and without changing selection. Closing its last attachment
retains its saved identity, title, directory and order in `workspace_sessions`.
`session.saved` lists those records and their observed attachments in an exact
Binding. `session.set_title ID TITLE` commits a metadata title before publication
and never renames a backend session. `session.reopen ID` first selects the exact
surviving, identity-tagged attachment. When it has exited, a validated checkpoint
restores the saved windows, panes, titles, directories, layout and focus under the
same logical identity. Creation is exclusive: a name collision never adopts an
unrelated session. Restored backend IDs are observed from the accepted creation;
the binding retains their mapping to the stable saved window and pane keys.
Without a checkpoint, reopening starts a fresh default shell in the saved project
directory with the same logical identity, title and order. Unsupported providers
and inaccessible directories fail without replacing saved data.

Checkpoints retain at most 256 KiB of text and SGR colors/styles per pane and
8 MiB per session. Oversized captures retain fewer complete recent rows; a single
row beyond the byte limit fails without replacing the prior checkpoint. Fresh
captures resolve indexed palette colors to RGB and discard OSC metadata. Saved
history rejects queries, cursor movement, modes and all
non-style escapes. History enters an output-only renderer, never shell stdin; styles reset
before fresh process output. Native history is seeded before the new shell produces
output. Older plain checkpoints remain valid. Snapshots do not read executable arguments,
environment variables, credential stores or arbitrary process memory. Remote creation
carries topology and validated presentation. tmux seeds empty panes through its
output-only `display-message -I` API before starting fresh login shells; its own
copy mode and capture retain those rows. Native and rmux readers seed their local
renderers. Opaque
attachments without identity stamping cannot restore inner topology.

The GUI captures immutable terminal history on workers and commits it through the
mux repository. Capture runs independently of optional output archives. Closing
a started terminal waits for its own complete checkpoint receipt before the
backend mutation. Creation, tab and pane success also wait for startup and that
session's committed checkpoint. OSC 7 file URIs are decoded into host paths at
the persistence adapter; reports that cannot name a supported host path cannot
replace a prior checkpoint. Shutdown retains capture and persistence owners until the final
save finishes or the platform shutdown deadline expires. Startup restores only
the last selected saved task; other tasks restore on activation. Native agent
resume remains a separate command owned by `NativeAgentService`: it restores the
captured task attachment, then resumes the same provider conversation and account
with fresh scoped tools, without replaying its prompt. An older conversation
without a terminal checkpoint starts a fresh shell in its saved directory before
resuming the same provider conversation. Failed restoration keeps saved data intact
and does not replay commands or prompts.

Cold terminal-agent recovery uses `TerminalAgentService`'s retained provider session
ID, account directory and saved task/window/pane location. That logical location
survives backend ID changes and interrupted checkpoint/catalog writes. Only a new
pane created by validated saved-topology
restoration admits this recovery; live backend reattachments do not restart agents.
The host prepares the provider-native resume command on a worker, then consumes a
one-use destination before replacing the process in that same pane. The launch
uses literal argv through the process owner, never terminal input. Disabled
providers and changed destinations fail before launch. Recovery does not replay the
original prompt, copy credentials to a remote host, or recreate child-spawn and
computer-capture grants. An uncertain launch is reported and never retried.

The private Codex terminal observer starts its provider in an isolated process
group under a supervisor with a parent-held lifetime pipe. Parent exit closes
that pipe; the supervisor terminates only its owned group, including launcher
descendants. This prevents an orphaned observer from retaining the conversation's
writer lock after an application crash. Ordinary stop also reaps that group.

Saved session lifecycle is independent of process activity. Active and Settled
are durable task states; archive, hide, soft deletion and an optional UTC snooze
deadline control discovery. `session.settle`, `activate`, `archive`, `unarchive`,
`snooze`, `unsnooze`, `hide`, `show`, `delete` and `restore` use the exact Binding
and saved identity. These commands preserve all terminal attachments and content.
Soft-deleted sessions must be restored before reopening. The repository updates
the supported identity schema transactionally; malformed or unsupported formats
fail without replacing existing records.

Pinning activates a task and clears its snooze deadline; settling clears its pin.
Archive, snooze and soft deletion retain pin metadata for restoration.
`session.activity` accepts only host-issued input receipts against an exact
Binding and saved identity. Its nonnegative UTC timestamp advances monotonically
and commits before publication; rejected input, stale targets and failed writes
leave the previous ordering intact.

Sessions attach by their exact identity and Space tag; backend names never recover
an attachment. Observed backend renames update the locator hint, never the saved
purpose title. Explicit Space ownership transfers still move membership;
attachment loss alone never prunes it.

`spaces.list` returns every
Space with its backend, host, Binding target and the sessions it holds, each with
its Session target. `pane.close` is destructive and closes one pane by its
Terminal target, in any Space, without changing selection: tmux and rmux kill
the backend pane; native closes it in the local topology and then ends its
runtime wherever it lives, hidden or shown. A window's last pane takes the
window with it.

### Theme authoring

`bootty-config::config::theme_file` owns bounded theme import and revision-checked
file replacement. It accepts native TOML and iTerm2 XML/binary plists and keeps
source/license metadata. The Settings window has Settings, Keymap and Theme tabs.
Theme groups workspace colors and named-theme authoring beside a live preview,
using the same draft and color picker as Settings. Loading, import, save, preview,
apply and restore travel through `CommandInvocation`; apply uses the existing
configuration commit owner. Preview is temporary and closing the editor restores
the accepted appearance. A save conflict leaves the editable draft visible.

### Workspace backgrounds

`bootty-config` owns typed opacity, image, gradient and material settings. The
GPUI workspace paints one continuous gradient/image backdrop; image decoding uses
GPUI's asynchronous asset loader. Each terminal element applies opacity only to
its surface fill at paint time, retaining the cached scene. Explicit cell fills,
text, selections and cursors remain opaque. `paint_plan` keeps explicitly painted
cells even when their RGB equals the default background. Native window materials
are configured at creation and refreshed only when the requested material changes.

### Native agent launch context

`bootty-agents::AgentLaunch` owns bounded literal argv and provider session syntax.
The host captures the exact binding/session and host cwd before dispatch. Native
panes start their explicit command before being shown; rmux and tmux create the
process through their supported backend interfaces. A failed tab launch closes
only its newly created pane. Resume and fork require an observed session identity;
retained metadata strips credentials and initial prompts.

Agent attention sequences and acknowledgement cursors belong to `bootty-agents`.
`bootty-ui` projects only panes found in live bindings, captures generation-scoped
targets, and owns unread presentation and notification delivery. Agents Dock
actions and automatic acknowledgement submit the shared command catalog.

The process-wide agent tray in `bootty-ui` aggregates window projections and
retains window-bound command senders. Native menu callbacks enqueue shared
commands; they never mutate GPUI entities or provider state. Linux DBus work
runs outside the UI thread. Tray lifetime never controls application lifetime.

Control subscriptions own their event queue, revision and wake signal. `event.wait`
checks the queue and subscribes to that signal under the same state lock; no UI
thread is parked. Client condition waits capture a command target once and
reconcile read-only snapshots after event wakeups or queue gaps.

Batch job process trees and retained stdout/stderr belong to `bootty-host::jobs`.
They are separate from backend-owned interactive terminals. The window command
runtime owns one bounded registry and retires it on close. Its event publisher
coalesces notifications off the UI thread; the native catalog validates the
registry generation before publishing `jobs.changed`. CLI clients read that registry through `CommandInvocation`.

Remote job execution runs in the versioned daemon and shares the local process
owner implementation. The daemon protocol and executable path derive from one
version constant. Output packets carry process status separately from transport
status; an interrupted stream cannot fabricate a successful job exit.

Shell assistance keeps live prompt revisions and bounded recent command metadata
in `bootty-terminal`'s worker. `bootty-host` reads and ranks shell-owned history on
its executing host; it never writes shell history. Handoff enters `CommandInvocation` and
validates the prompt lease at the PTY owner. Shared mux attachments cannot grant
that exclusive lease; their history reader remains available.

Explicit semantic history searches retain this host-owned candidate retrieval.
`bootty-host::semantic_history` bounds the candidate upload, validates TypeSafe
answers, and ranks original commands. The desktop command worker owns the API
credential and deadline; remote daemons receive neither the credential nor an
inference request. Local search remains the default.

File transfers reuse `bootty-host::jobs::JobRegistry` for deadlines, cancellation,
progress and window ownership. Their binary stream and atomic file publication
belong to `bootty-host`; the daemon only exposes that service. SSH forwarding remains binding-owned in the app's
command runtime, with unique `ForwardLease` identities and acknowledged close /
replacement before live registry updates.

OSC 5522 framing belongs to `bootty-terminal`; the GUI host owns per-host permission,
exclusive image assembly, bounded asynchronous decode, clipboard publication and
pane-generation reply routing. Recovery/replay cannot publish clipboard effects.

The GUI host owns bounded previous-session archives in identity-scoped state.
`bootty-terminal` supplies formatted capture only; archives never enter its replay
path. Agent recovery stores only `bootty-agents` retained launch facts and submits
new-tab/paste/submit commands after host fingerprint validation.

`bootty-git` owns history, local-branch and stash validation and mutations. The
Git Dock panel presents them, while every local or remote action still enters the
shared command path and runs on the captured binding host.

Agent executable, enabled state and named account/launch profile preferences belong to `bootty-config`. `bootty-agents` owns bounded installation/readiness inspection and live observation; provider credential stores remain external. All provider starts, account flows, updates and resume use shared command invocations and backend-owned terminal tabs.

## GitHub review

`bootty-git::GitHub` owns repository-host identity, viewer permissions, source diff
anchors, review submission and PR metadata/lifecycle operations. Remote Git work
uses the same host-bound runner as local Changes. `bootty-ui::gpui_git_panel` owns
only retained presentation and draft edits. Review comments capture old/new file
side and source line ranges independently of rendered editor rows. Submission
rechecks the head and selected source quote. Accepted writes remove only the
submitted draft IDs and unchanged text; newer edits and failures retain drafts.
Native GitHub stack actions capture every affected layer's revision. Rebase
checks branch permissions before the first write, then checks preceding heads
before each next layer. A partial failure reports completed layers; merge
requests preserve GitHub's pending/enqueued distinction. Pending merges
are observed through the read-only `git.github.merge-status` command after a
pending merge acknowledgement. The review view retains its repository, PR and
operation identity, polls with bounded backoff, and disables another merge
until the operation completes. A timeout or failed read retains that identity;
Refresh continues observing it rather than repeating the merge write. Fork workflow approvals
verify both commit identity and unique PR ownership before approving actual runs.
Large diff preparation runs off the UI thread. Missing or truncated host data is
reported rather than represented as a complete review.

Repository selection runs `gh repo view` in the captured local, SSH or WSL
checkout, preserving gh's default/upstream/fork policy. PR creation captures both
the base repository and origin's head repository, branch and commit. A normal
push names that exact commit on origin, and creation verifies its published revision. The
creation form retains its draft on failure. Full-file expansion reads immutable
base/head revisions, rejects binary and oversized contents, and compares private
temporary copies without changing either checkout or object database. Review
submission rechecks both revisions after validating source quotes.

PR review checkout captures both the displayed PR revision and local HEAD,
fetches GitHub's PR head, then rechecks both before switching. It uses a detached
checkout to preserve local branches, rejects dirty worktrees, and refuses to
overwrite ignored files. Branch creation remains an explicit separate command.
