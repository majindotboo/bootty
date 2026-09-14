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
transport would add no value.

`bootty-control` owns the local transport, read-only `ControlCatalog` metadata,
detached task and subscription state, and the singleton lease.
The initial desktop window owns the control server and its mailbox. Reopening
after that window closes can establish the next primary owner, but external
control has no router for targeting arbitrary secondary windows. Window and
binding scope are captured before asynchronous work so focus changes cannot
silently retarget a command.

Detached tasks and event subscriptions use opaque owner-local capability IDs.

## Agents and preserved custom scripts

`bootty-agents` owns the native Pi, Codex, and Claude providers: provider state,
explicit event parsing, command forwarding, lifecycle generations, hook and
integration file installation, and typed agent snapshots. Providers run in visible
terminal panes. Pi reports events through its installed adapter; Codex and Claude
report native command-hook events. The service accepts a narrow pane-scope resolver from the UI and a
control event publisher; it does not import mux or GPUI.

`bootty-ui` composes one agent service per desktop owner, exposes its descriptors
through the desktop command catalog, and projects typed agent facts into chrome
and panels. Agent workers and event publication remain asynchronous and are
retired before a replacement can publish stale state.

Existing `.lua` and `.luau` files under `<config>/extensions` are preserved and
reported as unsupported. Bootty does not execute those files or delete them.
Bootty does not infer agent state from process names, terminal output, screen
contents, or transcripts.

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
dock. Typed panel preferences select each tool's home dock and optional top or bottom
status button. All panel toggles use the same command path.
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
Layout version 8 removes the retired Jobs, Transfers, Recovery, and Shell panels.

Sessions includes the compact Space switcher; Agents includes the usage and quota
meters. Neither section is a standalone Dock leaf, so neither inherits the split
minimum height. Saved standalone Spaces and CodexBar panels merge into those owners;
existing destination panels keep their locations. Legacy `show_spaces` and
`show_codexbar` invocations open Sessions and Agents respectively.

`gpui_dock_skin` presents icon-labelled Kit segmented tabs with per-tab close
controls and context menus, while Base owns selection, dragging, splitting, and
close dispatch. Add-panel menus capture the destination group; selecting an
existing tool panel moves it into that group. The application header reads mux
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
Dock toggles, panel opening, and tab visibility use registered `CommandInvocation`s.
Context menus supply the live destination node ID as an optional argument. The
window completes these requests after applying them, or reports a stale group;
requests arriving during layout restoration wait for it to finish.
Single-panel groups hide their tabs automatically. Each group's context menu can
keep tabs visible; this preference follows the live node and is serialized by
its path alongside the same layout snapshot, then resolved after restoration.
Dock and mux tab bars both use Kit's tab variants. Bootty supplies panel and mux
commands, tab content, and close affordances; the shared `gpui::tabs` layout keeps
close buttons in side padding. Typed chrome settings independently control each
surface's tab appearance and close-button side and visibility.
Dock visibility and dimensions belong to the saved layout. Legacy sidebar config
values only seed unsaved or migrated layouts; live config reload does not show or
hide Sessions. Legacy sidebar commands submit dock requests.
`gpui_sidebar_panel` owns Sessions and its Space switcher. `gpui_agents_panel`
owns the Agents view and usage section; the existing chrome projection retains
usage data and Space actions. Layout version 7 merges the former standalone
navigation panels while preserving the destination panels and unrelated geometry. Panel labels and
dock-button visibility come from typed chrome settings. Each group can override
automatic tab visibility with always-show or always-hide, including command-only
switching.
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
existing atomic-write owner preserves target permissions and symlinks. A save
compares the loaded SHA-256 revision under the writer lease before replacement.

`bootty-ui` owns document drafts and Files/Document panel presentation. All I/O
enters `files.*` through `CommandInvocation`, capturing a binding generation.
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
and rmux captures represent a pane. tmux and Herdr captures represent the
attached client's VT state, not backend-owned inner pane history or original
PTY bytes. Unattached targets fail without changing focus.

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

`bootty-agents::AgentLaunch` owns bounded argv, session operation syntax and
shell serialization. The app captures the exact source pane, parent mux session,
working directory and host shell before dispatch. Start, resume and fork create
or use a visible terminal, then submit through `terminal.paste` and
`terminal.submit`. Hook adapters return session/cwd and a sanitized launch
context. Resume/fork always use a new tab and fail before mutation without an
explicit or pane-reported session. Mailbox callers receive the same authoritative
mux completion target as CLI/socket callers.

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
