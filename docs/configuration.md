# Bootty configuration

Bootty loads a native TOML config from:

```text
$XDG_CONFIG_HOME/bootty/config.toml
```

If `XDG_CONFIG_HOME` is not set, Bootty uses
`$HOME/.config/bootty/config.toml`. If neither environment variable is set, the
fallback path is `bootty/config.toml` relative to the process working directory.

The application identity scopes this directory. Production uses `bootty`, while
each Development build uses its stable `bootty-dev-...` namespace. The preferred
user keybinding file is `keymap.json` beside the selected `config.toml`, so
Production and Development keymaps cannot affect one another. An explicit
`--config /path/to/config.toml` selects `/path/to/keymap.json` as its sibling.

An absent config file is not an error; Bootty starts with built-in defaults.
Invalid startup config currently fails startup with a parse/load error. Runtime
reload is non-destructive: invalid reloads keep the last-good in-memory config
and show the error in the status bar.

For a one-off run that ignores live config and sidecar state, use:

```sh
bootty app --defaults
```

To load a specific config file instead of the XDG path, use:

```sh
bootty app --config /path/to/config.toml
```

Startup flags can override common config values after the file is loaded:

```sh
bootty app --defaults --backend native --fullscreen non-native --titlebar hidden --window-decoration none --no-sidebar
```

Run `bootty --help` for the full flag list.

## Example

See `docs/sample-config.toml` for a complete sample with every supported
configuration field. The inline example below shows the same shape.

```toml
version = 1
locale = "en"
theme = "One Dark"
include = ["?local.toml"]
restore_on_startup = "last_session"
cli_default_open_behavior = "existing_window"
default_open_behavior = "existing_window"
when_closing_with_no_tabs = "platform_default"
on_last_window_closed = "platform_default"

[window]
title = "Agent Shell"
width = 1220
height = 760
fullscreen = "native"
fullscreen-enabled = false
window-decoration = "auto"
macos-titlebar-style = "transparent"

[font]
family = [".ZedMono"] # Bundled Lilex, matching Zed's virtual font name.
size = 15
ui-family = [".ZedSans"] # Bundled IBM Plex Sans for all non-terminal UI.
ui-size = 16
# Optional fixed cell metric overrides; omit to derive from font size.
# cell-width = 10
# cell-height = 22
fit-cell-height = true

[chrome]
top-bar = true
bottom-bar = false
panel-tab-style = "icons"
panel-tabs = "automatic"
status-height = 30
status-background = "#1e1e2e"
notched-fullscreen-black-chrome = true
gap = 0
unfocused-sidebar-dim = 0.16
unfocused-terminal-dim = 0.0

[chrome.dock-tabs]
appearance = "segmented" # classic, underline, pill, outline, segmented
close-position = "right" # left, right
close-button = "hover" # always, hover, hidden

[chrome.terminal-tabs]
appearance = "classic"
close-position = "right"
close-button = "always"

[multiplexer]
backend = "rmux"

# native keeps mux state and terminals inside Bootty. rmux renders through
# rmux-sdk. herdr 0.8+ and tmux attach through their own backend UIs.

# Attach a multiplexer running on another host. Herdr keeps its UI and inner
# topology inside its attached client and must already be installed on the
# remote host. Bootty reaches the remote host over SSH and renders its sessions
# here. For remote Space catalog operations, Bootty uploads the
# matching daemon bundled with the app. If the bundle has no matching target,
# Bootty downloads and checksum-verifies the release daemon. New Session uses
# that daemon to discover projects and Git worktrees on the remote filesystem.
# The full Bootty app is not required on the remote host.
[multiplexer.remote]
host = "devbox" # ~/.ssh/config alias, hostname, or address
# user = "dev"        # when ~/.ssh/config does not name one
# port = 2222          # when ~/.ssh/config does not name one
# program = "ssh"      # the SSH client to run
# args = ["-i", "~/.ssh/devbox"] # extra flags, passed before the destination

# `bootty app --backend herdr --ssh-remote devbox` does the same for one run.
# Herdr's composite remote UI accepts an SSH host or `user@host`; put ports,
# jump hosts, alternate SSH programs, and other connection options in ~/.ssh/config.
#
# This is the default every space inherits. A single space can name its own host
# in the space editor, next to its backend, so remote and local spaces sit side
# by side; that override lives in the workspace database, not in this file.
#
# Bootty dials with ConnectTimeout=5, ServerAliveInterval=5 and
# ServerAliveCountMax=3, so a lost connection ends in about 15 seconds instead of
# hanging. The pane then reconnects on its own, with a growing delay between
# attempts; the sessions stay on the remote host throughout. Flags listed in
# `args` come first on the command line, so any of these can be set differently.

[input]
preset = "ghostty" # ghostty (default), bootty, or tmux — which built-in default keybind set to use
prefix = "ctrl+space" # leader for prefixed chords (bootty/tmux presets); defaults to ctrl+space / ctrl+b
keybind = ["cmd+shift+,=reload_config"]
sidebar-keybind = ["Enter=activate_session", "j=next_session", "k=previous_session"]
hide-mouse-pointer-while-typing = true
macos-option-as-alt = "both" # none, left, right, or both
modifier-remap = ["right_alt=left_ctrl"]

[session]
bell = "visual" # off | visual | audio | both
command-notifications = "unfocused" # never | unfocused | always
command-notification-min-seconds = 10
shell-integration = false # optional hooks for new bash, zsh and fish PTYs
output-archives = false # private bounded previous-session snapshots
clipboard-write-hosts = "" # opt-in host keys: local ssh:user@host:22 wsl:Ubuntu
shell = "/bin/zsh"
working-directory = "/Users/example/src"
env = [{ name = "EDITOR", value = "vim" }]
term = "xterm-bootty"
colorterm = "truecolor"
max-scrollback = 320000000
scrollbar = "auto" # auto-hides when idle; also hover, always, never
glyph-protocol = true

[cursor]
style = "block" # block, bar, underline, or hollow-block
blink = false
dim-inactive-pane = true

[diagnostics]
stability-trace = "/tmp/bootty-stability.csv"

[colors]
background = "#1e1e2e"
foreground = "#cdd6f4"
cursor = "#f5e0dc"
palette = [
  "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
  "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
  "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
  "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
]
```

The General lifecycle settings use Zed's stable names and choice tokens, adapted to Bootty's
model. A workspace is a durable Bootty Space. `last_session` restores the Space selected for the
application window's persistence key, `last_workspace` restores the primary window's selected
Space, and `none` opens the first persisted Space. Backend session/window selection remains owned
by each mux backend's restore policy.

Only set values you want to override. Unknown fields are rejected.

`[font].baseline-adjustment` defaults to `0`: glyphs use the font's centered
baseline. Positive values move text up in logical pixels; negative values move it
down. Ghostty uses device pixels for `adjust-font-baseline`: divide that value
by the display scale to use the same offset here (for example, `3` becomes `1.5`
on a 2x Retina display). Explicit overrides remain active when the font or display scale changes.

`[font].size` is the font's em size in logical pixels. Shaping, outline rendering,
and native font fallbacks use that same size, including on scaled displays. Font
names accept families, full face names (such as `Maple Mono NF Light`), and PostScript
names. The settings editor exposes each stack entry as a family picker and a
named style picker, saving the selected face name in the family array.

Automatic weights preserve their offset from the base: Bold requests 300 weight
units above the selected base, then chooses the nearest available family style.
For example, a Thin base also makes Bold lighter than a Regular base. Requests
are limited to the font weight range 1–1000.

`[font].style-bold`, `style-italic`, and `style-bold-italic` accept an advertised
style name (such as `SemiBold`), `"auto"` (the default), or `false` to use the base
style. A missing named style falls back to the base. Explicit assignments are
never synthetically thickened. Automatic Bold synthesizes weight only when the
family cannot supply a heavier face.

`[font.ui-weights]` assigns named styles independently to `thin`, `extra-light`,
`light`, `normal`, `medium`, `semibold`, `bold`, `extra-bold`, and `black`.
The same `"auto"` and `false` values apply. UI emphasis remains independent of the
weight assignment. These controls appear under Interface weights in Settings
and update all windows immediately.
Ordinary terminal text uses the host's native text rasterizer. On macOS this
preserves CoreText's stroke coverage and foreground-dependent font smoothing,
including the system font-smoothing preference.
By default, cell width and height are derived from the selected font and
`[font].size`; set `cell-width` or `cell-height` only when you want a fixed
override. `[font].fit-cell-height = true` is also enabled by default. It keeps
the row count implied by the current cell height, then stretches row spacing just
enough for those rows to fill the terminal area's available height. It does not
change the glyph font size.

## Includes

`include` is a top-level array of paths:

```toml
include = ["shared.toml", "?local.toml"]
```

- Paths are relative to the containing config file.
- Included files are applied after the containing file, so included values can
  override earlier values.
- Prefix a path with `?` to make it optional.
- Include cycles are rejected.

## Terminal backgrounds

The Appearance settings page controls `window.background-opacity` (0–1),
`background-image`, `background-image-opacity` (0–1), `background-gradient-start`
and `background-gradient-end` (`#RRGGBB` or `#RRGGBBAA`), and
`background-gradient-angle` (0–360 degrees). Both gradient colors enable the
continuous workspace gradient. Images cover the terminal area without changing
aspect ratio and load asynchronously. Relative image paths resolve beside the
configuration file. Image load failures appear in the background layer.

Reduce terminal background opacity to reveal the gradient or image beneath it.
Text, cursors, selections and explicit application-painted backgrounds retain
their opacity. A single backdrop spans split panes. Changes apply live.

`window.background-material` selects `opaque` (default), `transparent`, `blurred`,
`mica`, or `mica-alt`. Desktop transparency requires a transparent material and
reduced terminal background opacity. Blur depends on the window system and
compositor. Mica variants require Windows 11; on other platforms they use plain
transparency. Window chrome remains readable and opaque.

## Themes

`theme = "name"` resolves in this order:

1. user themes in the config directory, under `themes/<name>` or
   `themes/<name>.toml`
2. the built-in catalog

User themes shadow built-ins with the same name. Explicit `[colors]` values in
`config.toml` override the selected theme.

User and built-in theme files use a restricted schema:

```toml
[metadata]
name = "My Theme"
source = "local"
license = "personal"

[colors]
background = "#000000"
foreground = "#ffffff"
cursor = "#ffffff"
palette = ["#000000", "#ff0000"]
```

The **Edit Theme** command loads built-in or user themes, imports local TOML and
iTerm2 XML/binary plist schemes, and edits named colors and palette entries.
Preview changes the active window until the editor closes. **Save and Apply**
saves a user theme and selects it for the current light/dark appearance, removing
color overrides for that branch so they do not mask the authored theme. Rename
the draft to duplicate it. Built-in themes default to a copy. Existing user
files are replaced only when their revision still matches the loaded revision.
Source and license metadata are retained and editable.

CLI and socket clients use the same operations: `theme.read name`,
`theme.import absolute-path`, `theme.save name toml-source [revision]`,
`theme.preview toml-source light|dark`, `theme.apply name light|dark`, and
`theme.restore`. These operate on the local application's configuration tree.
Imports and theme sources are limited to 64 KiB.

Theme files must not configure shell, input, window, font, or chrome settings.
Built-in theme source and license notes are tracked in
`docs/built-in-themes.md`.

## Keymaps and keybindings

`keymap.json` is Bootty's preferred user override and editing path. It uses a
Zed-compatible JSONC shape: the root is an ordered array of section objects,
line and block comments are allowed, and trailing commas are allowed.

```jsonc
[
  // A missing context means Global.
  {
    "context": "Global",
    "use_key_equivalents": false,
    "use_builtin_defaults": true,
    "unbind": {
      "cmd-n": "new_mux_session",
    },
    "bindings": {
      "ctrl-x": null,
      "ctrl-k": "new_tab",
      "cmd-1": ["select_session", 1],
    },
  },
  {
    "context": "Terminal && backend == native",
    "bindings": {
      "ctrl-k ctrl-n": "new_tab",
    },
  },
]
```

Each section accepts only `context`, `use_key_equivalents`,
`use_builtin_defaults`, `unbind`, and `bindings`:

- `context` defaults to `Global`. Supported contexts are `Global`, `Sidebar`, `Command`,
  `Terminal`, `Herdr`, `Native`, `rmux`, and `tmux`. Backend contexts apply only
  while a terminal is focused. The Zed-style forms
  `Terminal && backend == herdr|native|rmux|tmux` are aliases.
  `Command` applies to the active command palette or picker; underlying workspace
  shortcuts are inactive while that modal surface is open.
- `use_key_equivalents` is optional, defaults to `false`, and is preserved as
  Zed-compatible section metadata.
- `use_builtin_defaults` is optional and defaults to `true`. Set it to `false`
  to suppress built-in bindings for exactly that context without deleting user
  bindings. When several sections set it, the last declaration wins.
- `bindings` maps a keystroke or whitespace-separated keystroke sequence to
  `null`, a Bootty action name, or `[name, input]`. `null` consumes the
  keystroke. The input can be any JSON value accepted by that command's typed
  arguments.
- `unbind` maps a keystroke to the named action being suppressed; an unbind
  target cannot be `null`.

Zed-style `ctrl-k` and Bootty-style `ctrl+k` modifiers are both accepted.
Sections and entries are evaluated in file order, and later matching entries
win. Action names resolve through Bootty's command catalog and enter the same
`CommandInvocation` path as the palette, CLI, socket, and native integration callers.

Terminal find has `.*` (regular expression) and `Aa` (case sensitive) switches.
The commands `toggle_search_regex` and `toggle_search_case_sensitive` expose the
same switches to the palette, keybindings, CLI, and socket. They also apply to
copy-mode searches. Literal, case-insensitive search is the default; search
options last for the window's lifetime. Invalid expressions keep the last valid
highlights and show an error in the find bar. Matches span soft-wrapped rows;
the counter counts logical matches in the current viewport. Regex matches are
non-overlapping, and zero-length matches have no highlight.

Pasting a clipboard image stages a private PNG and inserts its path. In an SSH
binding, Bootty uploads it through the matching remote daemon first and inserts
the remote path only after checksum verification. Uploads accept PNGs up to
64 MiB. A failed, cancelled, or stale-target paste inserts nothing; it never
substitutes a local path into the remote terminal. Images stay in the destination
host's temporary directory so the receiving application can read them. Clipboard
text and copied file paths keep their existing paste behavior.

The `Command` context exposes `ui.command.previous` (Up, Ctrl+P),
`ui.command.next` (Down, Ctrl+N), `ui.command.confirm` (Enter),
`ui.command.cancel` (Escape), and `ui.command.toggle_favorite` (Ctrl+Shift+F).
These bindings can be replaced or removed in the Keymap editor like other
commands. For example:

```jsonc
[
  {
    "context": "Command",
    "unbind": { "ctrl-n": "ui.command.next" },
    "bindings": { "ctrl-j": "ui.command.next" },
  },
]
```

Bootty first resolves the selected `[input].preset` and the existing TOML
`[input].keybind`, `[input].sidebar-keybind`, and
`[input.backend-keybind]` arrays. It then applies each context's
`use_builtin_defaults` policy and `keymap.json` user bindings as the final
layer. Those TOML fields remain supported, live-reloaded compatibility input;
existing configurations do not need to migrate. New keybinding edits should
use the Keymap editor or edit `keymap.json` directly.

Bootty watches `keymap.json` independently of `config.toml`. A valid saved
candidate is published without restarting the app. Invalid whole-file JSONC
keeps the last-good bindings active. In a structurally valid section array,
invalid sections or entries produce diagnostics while valid neighbors still
load. The Keymap editor writes changes atomically and preserves unrelated JSONC
comments and ordering where practical.

## Reload behavior

Bootty automatically checks the config file and any current includes for changes
and reloads after saves. `keymap.json` has the independent live-reload behavior
described above. The config reload keybinding is still available for manual
retry or edge cases:

```toml
[input]
keybind = ["cmd+shift+,=reload_config"]
```

Bootty parses compatibility TOML keybind strings with the shared Ghostty-style
binding parser and rejects actions that the app cannot execute. Supported global
app actions today include reload/ignore, terminal byte writes (`csi:...`,
`esc:...`, `text:...`), font size changes, clipboard paste/copy, window
lifecycle, fullscreen/sidebar chrome, mux session navigation, native tab/pane
actions, and terminal scroll actions. `[input].sidebar-keybind` is sidebar-local
and supports `ignore`, `previous_session`, `next_session`, `activate_session`,
and `focus_terminal`.
`focus_terminal` returns keyboard focus to the active terminal from any panel.
The default shortcut is Command-Shift-J on macOS and Ctrl-Shift-J on Linux and
Windows. Escape returns from Sessions panel navigation to the terminal; Escape
in the terminal remains terminal input.

Wheel triggers use `scroll_up` and `scroll_down`; the default `alt+shift+scroll`
bindings adjust font size in quarter-point steps. The settings keybind recorder
captures the wheel as well as the keyboard, so wheel triggers can be bound
without hand-editing the config.

Reload validates the full effective config first. If parsing, theme resolution,
modifier remap parsing, keybind parsing, or live terminal color/cursor
application fails, the current in-memory config remains active.

Libghostty-vt logs are bridged to stderr. Set `BOOTTY_LIBGHOSTTY_LOG=error|warn|info|debug|off` to adjust verbosity; unset defaults to warnings and errors.

Live-applied fields:

- `[chrome]` sidebar/status visibility, layout, and inactive panel dimming
- `[multiplexer]` backend selection and backend UI mode
- `[input]` modifier remaps, macOS Option-as-Meta mode, global keybinds, and sidebar keybinds
- `theme`, `[colors]` terminal defaults, `[cursor]` defaults, `[font]` metrics,
  and `[session].glyph-protocol`
- `[window].title`
- `[diagnostics].stability-trace`

New-session/new-window-only fields:

- `[session]` shell, working directory, environment, `TERM`, `COLORTERM`, and max scrollback
- `[window].width` and `[window].height`
- `[window].fullscreen`
- `[window].window-decoration`
- `[window].macos-titlebar-style`

When a reload includes new-session/new-window-only changes, Bootty keeps the
current terminal session alive and shows a status message. Open a new Bootty
window, or restart Bootty, for those settings to take effect.

## Window chrome and fullscreen

`[window].macos-titlebar-style = "hidden"` hides the titlebar and titlebar
buttons for new windows. `window-decoration = "none"` disables native window
decorations.

On Linux, `window-decoration = "client"` requests Bootty's title bar, window controls,
and resize borders. `"server"` requests the window manager's decorations; `"auto"`
uses that same platform preference. When the compositor cannot draw decorations
(for example, GNOME on Wayland), Bootty supplies them. X11 environments without
client-decoration support use system decorations. Settings windows follow the
decoration preference and keep their own controls even when the workspace is
borderless or fullscreen. Decoration changes apply to newly opened windows.

`[window].fullscreen` accepts `false`, `true`/`"native"`, `"non-native"`,
`"non-native-visible-menu"`, or `"non-native-padded-notch"` and records the
style to use. `[window].fullscreen-enabled` is the launch preference for that
style. `toggle_fullscreen` changes only the running window; it never rewrites
either setting. Configurations without `fullscreen-enabled` retain the legacy
behavior: any non-disabled `fullscreen` value starts active.
Native fullscreen uses the platform fullscreen path. Non-native modes create a
borderless fullscreen-style window.
On macOS, `"non-native"` and `"non-native-padded-notch"` auto-hide the menu bar
and Dock so they reappear when the pointer reaches the screen edge;
`"non-native-visible-menu"` intentionally leaves the menu bar visible.

## Preference writeback

Bootty has a round-trip TOML editing path for preference writeback. It edits the
user's `config.toml` rather than writing a generated full config, preserving
unrelated comments, ordering, includes, and tables. The settings UI, sidebar
width, appearance mode, active theme, and font-size actions use this path.

Keybindings use a separate round-trip JSONC path. The Keymap editor displays the
effective preset, compatibility TOML, and user bindings, but emits only typed
edit intents. `bootty-ui` applies those intents through `bootty-config` and
publishes the accepted result. The editor view performs no file I/O; the UI
file editor uses the shared `bootty-host` safe text-file contract.
Editing or removing a preset/TOML binding records a user override or targeted
unbind in `keymap.json` rather than mutating the compatibility input.

Bootty writes a complete temporary file beside the config. It synchronizes the
file before it atomically replaces the config. Existing symlinks remain links.
Existing Unix permission bits remain unchanged. A new Unix config uses mode
`0600` because it can contain SSH connection metadata.

If a writeback target file does not exist, Bootty creates it. If an existing
file cannot be parsed as TOML, writeback fails rather than replacing the file.
Bootty writers for one config path are serialized. External editors do not use
the Bootty writer lease, so writeback is not a cross-program compare-and-swap.

## Compatibility notes

- Status and sidebar modules are native implementations. Status segments support
  `session`, `windows`, `clock`, and `sysinfo`; the sidebar includes native session
  facts and agent integrations. Existing Lua/Luau files under the config tree
  and raw `[extensions]` values are preserved, but are not executed. Settings
  reports unsupported custom sources and module IDs. Native Pi, Codex, and Claude
  integration setup remains available; see [agent integrations](agent-integrations.md).

- `BOOTTY_SHELL` remains a compatibility override in the session launcher.
  Configured `[session].shell` is passed as the explicit shell setting, then the
  runtime preserves its existing shell precedence.
- `BOOTTY_STABILITY_TRACE` remains a diagnostics fallback when
  `[diagnostics].stability-trace` is not set.
- `[input].keybind`, `[input].sidebar-keybind`, and
  `[input.backend-keybind]` remain supported compatibility input. User overrides
  in `keymap.json` are applied after them.
- Bootty config is TOML with Ghostty-inspired vocabulary; it is not Ghostty's
  config syntax.

## Workspace docks

`toggle_left_dock` and `toggle_right_dock` show or hide their respective docks.
The palette, keybinding editor, CLI and socket expose the same commands as the
header buttons. Panel commands are `show_sidebar`, `show_files`, `show_changes`,
and `show_agents`.
For example, `[input].keybind = ["ctrl+shift+l=toggle_left_dock"]` binds the left dock.

Right-click a tab or empty group header and choose **Add panel** to open or move a
panel into that group. Panel commands accept an optional live `group` ID; an expired
ID reports a stale target. Without a group, a panel opens in its existing location,
or its default dock if closed. Document and diff tabs open from their file actions.

The Appearance → Docks settings control each dock button independently and select
icons only (default), icons and text, or text-only tab labels. `chrome.left-dock-toggle`
and `chrome.right-dock-toggle` hide only their buttons; commands still work.
`chrome.panel-tab-style` accepts `icons`, `icons-and-text`, or `text` for fixed left/right docks;
the default is `icons`. Main and bottom Dock groups use icons and text. `chrome.panel-tabs` accepts
`automatic`, `always`, or `never` for fixed docks. Main and bottom Dock groups keep their tabs
visible unless that individual group is switched to command-only navigation.

Single-panel groups hide their tabs automatically. **Always show tabs** in the group
context menu overrides this; **Always hide tabs** enables command-only panel switching for that group; `toggle_tab_bar` toggles the same preference for the
focused group and also accepts a group ID. Layout and tab preferences persist per
window, shared across Spaces. `toggle_hidden_tabs` toggles the per-group hidden override. `show_codexbar` opens Agents with usage meters; `show_spaces` opens Sessions with the Space switcher. Dock controls and the status row remain available when tabs hide.

## Panel controls

Every tool panel has a `toggle_<name>_panel` command, available in the palette,
keymap, CLI, and control socket. Names are `sessions`, `files`, `changes`, `diff`,
`agents`, `jobs`, `transfers`, `recovery`, and `shell`. A toggle closes an active,
visible panel; otherwise it opens and selects that panel. Terminal topology and
contextual document tabs retain their own close and navigation commands.

The former Sessions shortcuts now invoke `toggle_left_dock`, including Cmd+B in
the macOS Ghostty preset and Cmd+Shift+E in the macOS Bootty/Tmux presets.
Toggle Right Dock defaults to Cmd+Option+B on macOS and Ctrl+Alt+B elsewhere.
Existing custom keybindings are preserved.

Settings → Panels offers a dock dropdown (left, right, bottom) and a status bar
button dropdown (none, top, bottom) for each tool panel. For example:

```toml
[panels.sessions]
dock = "left"
button = "top"

[panels.agents]
dock = "right"
button = "bottom"
```

Sessions defaults to the left dock and the other tool panels to the right, with
no status buttons. Selecting a dock moves an open panel there; hidden panels use
that dock when reopened. Dragging can override placement for the current workspace; explicit dock
settings apply again when reopening it. Status buttons also make their chosen bar visible.

The **Dock Tabs** and **Terminal Tabs** settings independently select classic,
underline, pill, outline, or segmented tabs; close-button side (left/right); and
close-button visibility (always/on hover/hidden). Hover buttons occupy the tab's
side padding without reserving a separate column. Hiding a close button keeps the
close command and context menu available.

Dock visibility and widths are saved in the workspace layout. Use each panel's
Dock setting for placement and drag dock edges to resize. The legacy
`chrome.sidebar`, `chrome.sidebar-width`, and `sidebar.position` keys are accepted
only to seed an unsaved layout or migrate an older layout; changing them does not
alter an open workspace. They are no longer settings controls.

## Git tool panels

Use **Show Git Changes** in the palette (`show_changes`) or click a sidebar diff
count. Changes and Diff are native Dock panels: drag their tabs to split or combine
groups, resize the split, close panels, or toggle the right dock. Opening Changes again
restores closed Changes; selecting a file restores and activates Diff. Layouts are
saved per window, shared across Spaces, in `native-panels.json` beside the active config file.

Changes separates staged, unstaged, and untracked files. Stage/Unstage updates the
index without changing working files. Commit uses only the index; Amend explicitly
replaces the last commit and asks before proceeding. Git errors stay visible in the
panel. The message box remains disabled while its command is running.

CLI/socket/keybinding callers use the same commands: `git.open REPOSITORY`,
`git.status REPOSITORY`, `git.diff REPOSITORY PATH staged|unstaged|untracked`,
`git.stage REPOSITORY PATH`, `git.unstage REPOSITORY PATH`, `git.commit REPOSITORY MESSAGE`,
and `git.amend REPOSITORY MESSAGE`. Repository/file paths belong to the target binding's
host. Paths are literal rather than Git pathspec patterns; invalid UTF-8 filenames are
currently rejected by the text transport. External amend callers use the existing
command confirmation mechanism. SSH runs through the existing daemon transport.

### Files and document panels

Open Files through the Tools toolbar or `files.browse ABSOLUTE_DIRECTORY`.
`files.open ABSOLUTE_PATH [LINE [COLUMN]]` opens a document at a one-based
location on the target binding's host. The Files tree expands directories,
filters loaded entries, shares Git decorations with Changes, and pages large
directories. Refresh rereads the displayed directory; reopening a directory
refreshes its children.

Documents support Save (including Command/Ctrl-S), Reload, and Markdown Preview.
A dirty document's Close button offers Save, Discard, and Cancel. Closing a
window or quitting with unsaved documents also asks; Save all keeps the window
open so save errors remain visible. Saving over an external edit is rejected.
Reload asks before replacing a draft. UTF-8 files up to 512 KiB are supported;
binary files and larger documents report an explicit error.

Files commands also expose `files.list PATH [OFFSET]`, `files.read PATH`, and
`files.save PATH EXPECTED_SHA256 CONTENT_BASE64` for the CLI and control socket.
All paths belong to the selected binding's host, including remote paths.

Shell completion notifications require live OSC 133 command-start and finish
markers. Bootty measures worker-observed time, ignores replayed markers, and emits
at most one notification per observed command. `unfocused` includes another pane,
window or Space. Notification delivery follows the operating system's permissions.
The bell uses the platform system sound and/or a 250 ms terminal outline; bursts
are limited to one bell per 100 ms.

`session.shell-integration` applies to new native PTYs with a supported shell and
no explicit command arguments. Bash/zsh rc files still run, existing Bash DEBUG
traps are preserved, and Fish's built-in `mark-prompt` support takes precedence.
Bootty does not replace shell editing, history, prompts or user rc files. Bash
shells with an existing DEBUG trap need their existing integration to emit markers.
Backend-owned shells (rmux, tmux, Herdr and remote attachments) retain their launch
configuration. Their notifications require the backend to forward live OSC 133
markers; replay snapshots alone cannot establish a command duration. The read-only
`shell.integration bash|zsh|fish` command returns the scripts for optional manual
installation, through the same CLI/socket command catalog.

### Interface language

The top-level `locale` is a language tag (`"en"` by default). `"en-XA"` enables
pseudo-localization: longer accented interface labels help reveal clipped controls.
English remains the fallback for languages without a bundled Bootty translation.
GPUI Kit receives the language tag for its own controls; those controls use the
library's catalogs and fallback. Native menus update after restarting. Existing
command palettes and input placeholders update when reopened; settings labels and
new notifications follow accepted configuration changes.

The foundation covers settings labels/help/choices, command palette text, terminal
find controls, Files/Changes/Document controls and completion notifications. Other
product dialogs and OS-owned UI still need extraction before a complete additional
language can ship. Terminal output, paths, editable contents, command IDs, config
keys and backend diagnostics retain their original bytes. Chinese/Japanese Bootty
translations are not bundled yet.

### WSL workspaces (Windows)

New/Edit Space lists installed WSL distributions alongside local and SSH locations.
Selecting a distribution uses the rmux backend by default; tmux is also supported
and must be available in the distribution. Native and Herdr bindings are not
supported over this transport. Bootty never installs or unregisters distributions.

The same command interface is available to CLI and socket callers:

```sh
bootty command wsl.list
bootty command space.wsl Ubuntu "Linux project" rmux
```

`space.wsl` saves and activates a Space; its response reports `activated` and the
saved `space_id`. The binding runtime connects afterward, so successful Space
creation is not a claim that a Linux process has started. Existing backend sessions
remain owned by the selected distribution's rmux or tmux server across reconnects.

For a default WSL placement, replace the entire SSH remote table with:

```toml
[multiplexer]
backend = "rmux"
[multiplexer.remote]
distribution = "Ubuntu"
```

Do not combine `distribution` with SSH fields. Existing SSH tables retain their
serialized format. `--ssh-remote` explicitly selects SSH and preserves SSH options
when the configured target is already SSH.

Bootty executes Linux arguments through `wsl.exe --distribution NAME --cd ~ --exec`.
The host layer installs the matching Linux daemon from packaged assets or the
existing checksummed release cache. It verifies a private upload's digest and
protocol before publishing its versioned path. Files, Git operations, project and
worktree discovery, and clipboard-image uploads all run in that distribution;
Linux paths never resolve against a Windows or macOS namesake. Loopback URLs rely
on Windows/WSL localhost forwarding. See Microsoft's [command reference](https://learn.microsoft.com/en-us/windows/wsl/basic-commands)
and [networking behavior](https://learn.microsoft.com/en-us/windows/wsl/networking).

WSL commands return Unsupported on other operating systems. Configuration and
transport tests run across platforms; the full Windows/WSL GUI workflow has not
been certified from the macOS development environment.

Agent alerts use `session.agent-notifications = "unfocused"` by default. Set
`"always"` to include the focused terminal or `"never"` to disable desktop alerts.
Unread markers and sequence-safe acknowledgement remain available in all modes.

See [terminal image clipboard writes](clipboard-writes.md) for host keys, supported
formats, limits and permission scope.

Settings rows include **Reset to default**, which removes the explicit override,
and **Copy setting link**. macOS packages register links such as `bootty://settings/font.size` to open Settings
at that configuration path. The equivalent CLI command is `bootty command open_setting font.size`. Development packages use their own namespace as the
URL scheme. Fractional number controls use two decimal places; integer controls
remain integral.
