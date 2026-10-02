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
| The live workspace and binding runtimes | `bootty-mux::workspace::{WorkspaceRuntime, BindingRuntime}` | A replacement appears only after validation and persistence. |
| Terminal agent identities and retained launch metadata | `bootty-agents::TerminalAgentService` | Persists exact backend targets before publishing registrations; stale generations cannot receive commands. |
| Orchestration runs and worker reports | `bootty-agents::OrchestrationService` | Persists transitions before dispatch; reports match worker generations and attempts. |
| Desktop capture and input | `bootty-computer` | User enabling and macOS permission checks precede every operation. |
| Browser child views and site data | `bootty-browser` | Wry owns navigation and identity-scoped browser profiles; host geometry and visibility arrive at the UI seam. |
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
| Paired remote command transport | `bootty-control::RemoteControlServer` | A certificate pin and per-listener credential precede every bounded request to the local owner. |
| Desktop remote-control enablement | `bootty-ui::RemoteConnections` | Only an explicit desktop action enables a listener; revoke, quit, and owner replacement invalidate the lease. |
| Native agent integration state and assets | `bootty-agents` | Native providers own bounded event parsing and integration files. |
| Terminal pane topology, ratios, and focus | `bootty-mux::BindingRuntime` | Providers remain authoritative. Hosts consume `MuxPaneLayout` and submit typed pane operations; they never persist a competing terminal tree. |
| Workspace composition | `bootty-ui::workspace_composition` | Reconciles the binding's terminal projection with Bootty-native panels. The terminal center is one locked singleton leaf retargeted at the selected mux window; it renders the whole split tree from the binding projection and never takes part in Dock drag, tabs, or documents. Documents live in the right dock. Dock never mirrors mux splits, Dock geometry never flows back into mux ratios, and native leaves never enter a backend command. |
| Native panel placement | `bootty-ui::gpui_dock` | The GPUI Kit Dock places native leaves around the reconciled terminal projection; `native-panels.json` stores one versioned layout per window, shared across Spaces. |
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
available until insertion starts. Remote daemon protocol 4 includes bounded document operations and streamed image
uploads over the existing SSH process transport.

## Workspace and mux

`bootty-mux::repository::WorkspaceRepository` owns SQLite access for Spaces and
backend bindings. It loads one validated `WorkspaceSnapshot`, applies schema
migrations and legacy import, and records binding-scoped journals before backend
mutations. A failed commit leaves the accepted snapshot active.

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
selection restore, session membership/order/name records, migration state, and
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

`bootty-mux` uses the supported SDK pipe-pane path for live rmux bytes. Each
reader has an independent bounded queue; an EOF drains its queued tail before
closing. Initial recovery seeds text and input modes while concurrent live bytes
continue through the pipe. A stalled reader disconnects and reconnects rather
than blocking another reader. Input and resize remain responsive.

The pinned SDK's pipe can lose producer bytes before Bootty's helper stdin,
including in an already-attached live stream; it exposes no sequence or gap
status for those bytes. Its separate recovery subscription also loses large
live bursts, and switching to it worsened exact-image delivery in matched runs.
The pipe transport therefore remains the live path. This preserves baseline
behavior, not a lossless rmux graphics guarantee. Historical keyframes restore
text and modes, not images beyond retained output. Upgrade this limit when the
SDK exposes a lossless per-pane stream or public producer retention controls;
no dependency internals are patched. The SDK documents bounded live retention
(256 KiB by default), not lossless unpaced producer bursts. This limitation
predates the current redesign; complete burst delivery remains unresolved.
Routine rmux acceptance sends a compressed image below 4 KiB on the wire and
checks placement, dimensions and every byte of its decoded 2 MiB pixel buffer
in both readers, followed by input after the second reader closes. This does
not prove uncompressed burst delivery. The original unpaced raw image workload
and its full pixel assertions remain runnable with
`BOOTTY_RMUX_IMAGE_BURST_STRESS=1 mise run test -- -p bootty-mux --test embedded_daemon kitty_images_reach_terminal_frames`.
It is an explicit stress diagnostic with known intermittent loss, rather than
a supported lossless transport contract. Native PTY acceptance still exercises
the unpaced raw 2 MiB image, full pixels and GPUI image primitives. Public raw
subscription acceptance separately checks reported gaps and text recovery.

The rmux pane worker owns both remote transport process trees. Closing the
terminal ends them even while its output reader waits on a quiet SSH stream;
transport lifetime does not depend on another output line arriving.

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
runtime health.

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

Pi, Codex, and Claude launch as terminal programs through the selected mux backend. The backend owns their processes, tabs, splits, and terminal state. `TerminalAgentService` owns bounded provider identities, retained launch metadata, and exact terminal targets. History and account queries use provider files and commands on a background worker. Resume and fork launch the provider's terminal flow. Agents appear within backend sessions in the Sessions sidebar.

Terminal registrations persist before publication. Provider session IDs remain distinct from backend target handles and generations; stale targets cannot redirect input to another pane.

`OrchestrationService` owns durable runs, tasks, worker attachments, and messages. It delegates prompts through the same command mailbox to existing sessions and never launches a second worker process. An accepted prompt is running, not completed: completion requires a report from the captured worker target and dispatch attempt. Interrupted work requires explicit retry.

`bootty-ui` composes these owners and routes the palette, keybindings, CLI, socket, and agent commands through one catalog and invocation path. Native panels have fixed homes: Sessions on the left and labeled tools on the right. The center displays the selected backend terminal window and its split tree.

`bootty-computer` owns the macOS desktop helper and rechecks OS permissions and secure input before capture or input. `computer-use` must be enabled by a user action; agents cannot enable it or request permission. Capture metadata remains bounded and images use private files. `bootty-browser` owns Wry child-view lifetime, navigation events, and host geometry; the shell hides native views beneath GPUI dialogs and sheets. Saved logins use the host platform credential store, keyed by app identity and exact origin. The host checks the selected tab and navigation revision before filling; the page script checks origin again and never submits the form. Element annotations accept bounded main-document metadata only while the host enables selection. A native editor reviews the comment before copying it or submitting a `terminal.paste` invocation; pasting does not submit terminal input. Its Linux adapter borrows the existing Xcb window as an Xlib handle on the same X server. Desktop startup selects X11 for both GPUI and GTK, using XWayland when launched from a Wayland desktop.

Existing custom integrations remain unsupported and preserved. Built-in terminal launches require no installed hooks. The prior pane event service remains available for explicitly configured terminal adapters.

Desktop pairing starts an explicitly enabled TLS listener on the chosen local interface. The pairing code contains its certificate pin and a random credential; remote requests then use the same local command owner and exact issued targets. Public status never exposes the credential. Listener lifetime follows the desktop owner, and revoke closes the listener. The phone owns its connection credential and presentation, while the desktop remains authoritative for sessions, terminal frames, and commands.

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
  subscription capabilities, discovery, and the singleton lease. It has no
  product command implementations.
- `bootty-config` owns product configuration, defaults, includes, validation,
  writeback, identity namespaces, keymap JSONC parsing/editing/matching, and
  framework-free font and modifier values.
- `bootty-daemon` owns the installed headless executable entrypoint, argv and
  identity parsing, endpoint lifetime, and protocol serving. It composes
  `bootty-host` and the mux catalog; it does not duplicate their policy.
- `bootty-agents` owns terminal agent metadata, accounts, history, and orchestration.
- `bootty-computer` owns native desktop capture and input.
- `bootty-browser` owns embedded Wry browser views.
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

`bootty-ui::gpui_dock` composes one terminal workspace, documents, and tools into a
GPUI Kit Dock. The terminal stays in the center; the binding owns its backend
windows, pane topology, processes, and input. Documents initially open in the right
dock. Sessions has a fixed home on the left; tools and documents have fixed homes
on the right. All panel toggles use the same command path.
Opaque client attachments retain their backend-owned
layout. Builders resolve existing panel entities by Dock-area identity; they cannot
restore another window's host or editor. Registration is removed when its workspace
is released. Layout reads run off the UI thread. One application-owned writer orders
atomic, locked saves across windows. Unknown panel names use
the library's placeholder and retain their saved state.

One DockArea owns geometry across Space and session switches. Files, Changes, and
Diff follow the selected terminal and its directory. Captured Git drafts and
in-flight writes retain their original context; late replies cannot open panels
in another context. Open documents retain their own host and path identities.
Layout version 10 restores the fixed panel arrangement from earlier custom layouts.

Sessions includes the Space switcher, project groups, backend sessions and windows,
agent state, and account usage meters. Published Git facts group repository
subdirectories and linked worktrees under the owning host's main repository root.
Projects appear in first-seen backend order; sessions keep their backend order
within each project. Branches and working directories belong to session children.
Project disclosure is transient chrome state; switching to a hidden session expands
its project, and removed projects discard their disclosure state. Legacy
`show_spaces` and `show_codexbar` invocations both open Sessions.

`gpui_dock_skin` presents right-side tools as labelled Kit buttons with icons.
Their labels remain visible at every width, wrapping into rows as needed. Base
owns selection, focus, and geometry. Panels cannot be dragged into another region.
The application header reads mux
window tabs through the shared chrome projection and tab renderer, including scoped
selection, pane closing, navigation, reordering, and context actions. Window-scoped
pane actions resolve the binding's retained focus rather than a stale backend anchor.
Dock groups keep their own native panel tabs; their render order does not determine
the application header. Open side docks own their title rows and collapse buttons. The left title row
reserves the native window controls and shows Bootty's mascot and title. The right
row receives whole status controls that fit its measured width; the center titlebar
keeps the remaining controls and mux tabs. Closed docks expose their toggle in the
center titlebar. Dock edges use an inset grab area and Base's clamped dock geometry; the workspace skin applies the first
motion and final drop and saves the completed resize. Terminal dividers also commit
the drop position, including when one motion starts and ends the drag.
Bottom status segments, including mux tabs, occupy the center dock's footer; side
docks keep their full height. On Linux, the window-level title bar and resize frame
sit outside this dock layout, with Bootty controls whenever client decorations are
selected or required by the compositor.
Dock toggles and panel opening use registered `CommandInvocation`s. The
window completes these requests after applying them, or reports a stale group;
requests arriving during layout restoration wait for it to finish.
The Sessions panel and terminal center do not show native panel tabs.
Mux and document tab bars use Kit's tab variants. Bootty supplies panel and mux
commands, tab content, and close affordances; the shared `gpui::tabs` layout keeps
close buttons in side padding. Typed chrome settings independently control each
surface's tab appearance and close-button side and visibility.
Dock visibility and dimensions belong to the saved layout. Legacy sidebar config
values only seed unsaved or migrated layouts; live config reload does not show or
hide Sessions. Legacy sidebar commands submit dock requests.
`gpui_sidebar_panel` owns Sessions, agent rows, account usage and its Space switcher.
Agents belong to backend sessions; there is no separate Agents panel. Chrome
settings control dock-toggle visibility and tab styling; panel placement and
tool-navigation labels are fixed.
`status_fit` measures the status controls and prepaints only whole controls that
fit the available header width. Omitted controls receive no hitboxes.
The notch inset places the strip's bottom border below the camera exclusion band.

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
daemon protocol 14 carries these operations and explicit-create argv. The
daemon executable path is versioned by that protocol, so a client never reaches
an older daemon that would drop a field it does not know.

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
Space's membership under the caller's name, so generated-name reconciliation never
renames the session. The name must be free on the binding's server; the create
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
persisted or replayed: a restored native pane starts the default shell.

`session.close` is destructive and kills a session its Space holds, in any Space,
without Git cleanup and without changing selection. `spaces.list` returns every
Space with its backend, host, Binding target and the sessions it holds, each with
its Session target. `pane.close` is destructive and closes one pane by its
Terminal target, in any Space, without changing selection: tmux and rmux kill
the backend pane; native closes it in the local topology and then ends its
runtime wherever it lives, hidden or shown. A window's last pane takes the
window with it.

### Theme authoring

`bootty-config::config::theme_file` owns bounded theme import and revision-checked
file replacement. It accepts native TOML and iTerm2 XML/binary plists and keeps
source/license metadata. `bootty-ui` projects the draft as generic dialog fields
with the same GPUI color picker used by Settings. Loading, import, save, preview,
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

`bootty-agents::AgentLaunch` owns bounded argv and provider session syntax.
Native provider start, resume, fork and sign-in commands create a backend-owned
terminal and select it through the shared command mailbox. Account and session
history queries run off the UI thread. The local history dialog searches provider
metadata and submits the same start/resume/fork commands as CLI/socket callers;
it does not render conversations or own terminal topology. Remote hosts retain
the provider terminal picker until remote history queries are available.
Mailbox callers receive the authoritative mux completion target.

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
