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
bootty app --defaults --fullscreen non-native --titlebar hidden --window-decoration none --no-sidebar
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
title = "Bootty"
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
tabs-use-session-color = true
notched-fullscreen-black-chrome = true
gap = 0
unfocused-sidebar-dim = 0.16
unfocused-terminal-dim = 0.0

[chrome.tabs]
# Terminal and sidebar tabs share one appearance and close-button setting.
appearance = "segmented" # classic, underline, pill, outline, segmented
close-position = "right" # left or right
close-button = "always" # always, hover, hidden

# SSH placement runs the native mux server on this host. QUIC is preferred,
# then direct TCP/TLS, then SSH stdio. WSL native panes remain unavailable.
[multiplexer.remote]
host = "devbox" # ~/.ssh/config alias, hostname, or address
# user = "dev"        # when ~/.ssh/config does not name one
# port = 2222          # when ~/.ssh/config does not name one
# program = "ssh"      # the SSH client to run
# args = ["-i", "~/.ssh/devbox"] # extra flags, passed before the destination

[input]
# Every preset opens the command palette with Cmd+K (Ctrl+Shift+K outside macOS).
preset = "ghostty" # ghostty (default), bootty, or tmux — which built-in default keybind set to use
prefix = "ctrl+space" # leader for prefixed chords (bootty/tmux presets); defaults to ctrl+space / ctrl+b
keybind = ["cmd+alt+shift+r=reload_config"]
sidebar-keybind = ["Enter=activate_session", "j=next_session", "k=previous_session"]
hide-mouse-pointer-while-typing = true
macos-option-as-alt = "both" # none, left, right, or both
modifier-remap = ["right_alt=left_ctrl"]

[session]
bell = "visual" # off | visual | audio | both
command-notifications = "unfocused" # never | unfocused | always
command-notification-min-seconds = 10
shell-integration = true # optional hooks for new bash, zsh and fish PTYs
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
Space, and `none` opens the first persisted Space. Session/window selection remains owned
by the native backend's restore policy.

A cold session restore retains its layout, directories and bounded styled history.
It starts fresh shells in the recorded directories; it never replays commands.
Live processes reattach instead.

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
Terminal and UI font-size sliders move in 0.25 increments; their +/− buttons move
in 0.5 increments. The number fields also accept typed values with two decimal places.

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
Ordinary terminal text uses the host's native text rasterizer. On macOS,
terminal and interface text keep CoreText's stroke coverage without font
smoothing, so light text on dark backgrounds is not thickened and Thin faces
stay thin.
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
- Prefix a path with `?` to allow a missing file. Read and parse errors still
  reject the reload, preserving the last accepted config.
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

`macos-option-as-alt` controls terminal input encoding, not application shortcuts.
An `alt` shortcut accepts either Option key; use `left_alt` or `right_alt` for an
explicit side constraint. The Bootty and tmux presets bind `alt+<` / `alt+>` to
move tabs left / right.

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
    "context": "Terminal",
    "bindings": {
      "ctrl-k ctrl-n": "new_tab",
    },
  },
]
```

Each section accepts only `context`, `use_key_equivalents`,
`use_builtin_defaults`, `unbind`, and `bindings`:

- `context` defaults to `Global`. Supported contexts are `Global`, `Sidebar`,
  `Command`, and `Terminal`. Other values are Zed-style key-context expressions.
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
`CommandInvocation` path as the palette, CLI, and socket callers.

Terminal find has `.*` (regular expression) and `Aa` (case sensitive) switches.
The commands `toggle_search_regex` and `toggle_search_case_sensitive` expose the
same switches to the palette, keybindings, CLI, and socket. They also apply to
copy-mode searches. Literal, case-insensitive search is the default; search
options last for the window's lifetime. Invalid expressions keep the last valid
highlights and show an error in the find bar. Matches span soft-wrapped rows;
the counter counts logical matches in the current viewport. Regex matches are
non-overlapping, and zero-length matches have no highlight.

Pasting a clipboard image stages a private PNG, up to 64 MiB, and inserts its
path. A failed, cancelled, or stale-target paste inserts nothing. Images stay in
the host's temporary directory so the receiving application can read them.
Clipboard text and copied file paths keep their existing paste behavior.

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
`[input.backend-keybind].native` arrays. It then applies each context's
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
keybind = ["cmd+alt+shift+r=reload_config"]
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

Default zoom shortcuts change terminal and UI fonts together: Command-Plus/Minus
on macOS and Ctrl-Plus/Minus on Linux and Windows. Command-0/Ctrl-0 restores each
configured size. These bind `increase_all_font_sizes:1`,
`decrease_all_font_sizes:1`, and `reset_all_font_sizes` in every preset.
The UI size also scales monospace editors. Zoom is temporary and does not write
the config.

On macOS, `SF Pro` in the font picker uses the local system font. It works without
installing Apple's design-time font download. The selected family and face are
shared by UI text, terminal text, and font fallbacks; no Apple font is bundled.

Terminal-only `increase_font_size`, `decrease_font_size`, `reset_font_size`, and
`set_font_size` retain their existing behavior. UI-only equivalents use
`ui_font_size` (for example, `increase_ui_font_size:1`). Both sets are unbound
by default. Increase, decrease, and reset are available in the palette; all
font actions are available in the keymap editor.
`set_all_font_sizes:N` sets both sizes to N points.

Wheel triggers use `scroll_up` and `scroll_down`; the default `alt+shift+scroll`
bindings adjust both font sizes in quarter-point steps. The settings keybind
recorder captures the wheel as well as the keyboard, so wheel triggers can be
bound without hand-editing the config.

Reload validates the full effective config first. If parsing, theme resolution,
modifier remap parsing, keybind parsing, or live terminal color/cursor
application fails, the current in-memory config remains active.

Libghostty-vt logs are bridged to stderr. Set `BOOTTY_LIBGHOSTTY_LOG=error|warn|info|debug|off` to adjust verbosity; unset defaults to warnings and errors.

Live-applied fields:

- `[chrome]` sidebar/status visibility, layout, and inactive panel dimming
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
style when a window has no saved state. Workspace windows remember their size,
position, display, and fullscreen state when closed or when Bootty quits, in
`windows.json` beside the configuration. Restoring a fullscreen window retains
its previous windowed size and position; disconnected displays restore onto an
available display. Saved state takes precedence over launch dimensions and
`fullscreen-enabled`. `toggle_fullscreen` never rewrites either setting.
Configurations without `fullscreen-enabled` retain the legacy
behavior: any non-disabled `fullscreen` value starts active.
Native fullscreen uses the platform fullscreen path. Non-native modes create a
borderless fullscreen-style window.
On macOS, `"non-native"` and `"non-native-padded-notch"` keep the menu bar and
Dock hidden while the borderless window is focused, so they cannot take clicks
from the top bar. `"non-native-visible-menu"` leaves the menu bar visible.

Fullscreen tabs start to the left of the camera. By default,
`[window].fullscreen-tabs-wrap-at-notch = true` continues them on a second row
below the camera when needed. The last tab on the first row may extend up to
halfway behind the camera; later tabs go below it. Set the option to `false` for
one scrollable strip confined to the left of the camera. Set
`[window].fullscreen-tabs-in-notch = false` to place the entire strip below it.
An explicit `[window].fullscreen-top-offset` overrides the reserved height.

Tab strips reveal a newly selected or keyboard-focused tab and show scroll
buttons only in directions with more tabs. Terminal tab widths grow as needed,
then wait for the title to remain unchanged for one second before shrinking.
Terminal and sidebar tabs share `[chrome.tabs]` for appearance, close-button side,
and visibility. Close buttons default to always visible on the right. Set
`close-position = "left"` or `close-button = "hover"` to change both strips.

`chrome.tabs-use-session-color` defaults to `true`: active terminal and dock tabs
use a muted tint of the selected session color. Set it to `false` to use the theme
accent.

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
Before replacement, Bootty compares the file with the draft's original bytes,
including comments. A stale draft cannot overwrite an observed external edit,
deletion, or newly created file. Settings retains rejected edits and offers
**Discard unsaved changes** to return to the accepted config before editing again.
Catalog refreshes preserve unsaved environment rows. A successful
save replaces the editor's accepted config and document together; untouched values
follow subsequent config reloads. Scalar controls and reset indicators read the
resolved accepted values, including values inherited from includes and legacy
spellings; only local edits override that display. Ordinary scalar controls are
described by their `SettingSpec` and share the same editor and writeback path.
Remote editors use that same accepted snapshot.
A remote save or removal acknowledges only its own editor: rejected remote edits
remain visible, and unrelated unsaved settings are preserved. External profile
changes refresh untouched editors while preserving unsaved drafts, even when the
saved profile was removed. Incomplete or invalid
environment rows also survive saves made by other editors and remain editable
until completed or explicitly discarded.
Connection tests report results for both saved profiles and unsaved drafts.

## Compatibility notes

- The status bar shows session tabs at the left and keep-awake, CPU, memory, power, date and time at the right.
- Sidebar modules are native implementations. `sidebar.modules` defaults to
  `["sessions"]`, and `sidebar.session-modules` to `["diffs", "process",
  "directory", "branch", "ports", "progress"]`.
- `BOOTTY_SHELL` remains a compatibility override in the session launcher.
  Configured `[session].shell` is passed as the explicit shell setting, then the
  runtime preserves its existing shell precedence.
- `BOOTTY_STABILITY_TRACE` remains a diagnostics fallback when
  `[diagnostics].stability-trace` is not set.
- `[input].keybind`, `[input].sidebar-keybind`, and
  `[input.backend-keybind].native` remain supported compatibility input. User
  overrides in `keymap.json` are applied after them.
- Bootty config is TOML with Ghostty-inspired vocabulary; it is not Ghostty's
  config syntax.


## Session list

`sidebar.group-by-project` defaults to `true`. The sidebar view button and
`ui.sidebar.toggle_grouping` switch between project groups and a flat list.
Each session row shows its branch, diff counts and the branch's pull request;
switching views preserves sessions, selection and terminal contents. Sessions are
grouped into Pinned and Sessions. A session's tooltip shows its short
name, like `bootty-7c`, which works in place of its target in commands, the CLI
and the session switcher; see `docs/automation.md`. The search box matches titles,
project names and branches. A closed session's row is dimmed and says Closed.
Double-click a row, or choose **Rename…**, to edit its title in place: Enter or
clicking away saves it, Escape keeps the old title. Command-click (Control-click
on Linux and Windows) chooses several sessions and Shift-click a range; right-click
one of them to pin or close them together. Escape, a
plain click or returning to the terminal lets go. Archive retains saved work and
offers Undo; Delete asks for confirmation, closes the session's terminals and
permanently removes its saved record. A failed close keeps the saved record.

The pull request badge comes from the host's `gh` account. Bootty asks about the
selected session's branch once a minute and the rest every five minutes; a failed
ask keeps the last answer.

Open **Settings → Projects** to add a registered folder, open its settings, or
remove its registration. Removal preserves files and existing sessions.
A project header's actions menu starts a session in the project, reveals or
copies its path, and opens **Project settings…**, the same page in a named,
closeable settings tab. Switching tabs preserves drafts; closing an unsaved draft asks
before discarding it.
The page edits its name and icon or local image.
It also keeps the project's scripts: named shell commands that a session's menu runs in
a new terminal tab, in that session's directory (`project.script CWD NAME`).
Project settings live in the workspace database and apply to new sessions.

The New Session form starts a terminal in a project's directory on a Space. A linked
checkout's sessions group under its main checkout's project in the sidebar.

`toggle_session_pin` pins the current saved session, or unpins it when already pinned.
`close_session` closes it and releases its saved membership and checkpoint. Both are in the palette. Every preset binds:

| Command              | macOS       | Linux and Windows  |
| -------------------- | ----------- | ------------------ |
| `toggle_session_pin` | `Cmd+Opt+P` | `Ctrl+Shift+Alt+P` |
| `close_session`    | `Cmd+Opt+A` | `Ctrl+Shift+Alt+A` |

`session.pin` accepts an explicit session identity with its Binding target.
`session.close` remains the wire alias for `close_session`; both accept a Session target.

`sidebar.sort-order` defaults to `"manual"`. Choose **Manual** or **Recent activity**
from the sidebar view menu or Sidebar settings. Recent activity uses persisted
accepted session input; equal or undated sessions keep their
manual order. The shared commands are `ui.sidebar.sort_manual` and
`ui.sidebar.sort_recent_activity`. Drag a session to move it, or a project
header to move all of its sessions. A drop in Recent activity order keeps the
order shown as the manual order and switches to Manual.

## Workspace dock

The window has two regions: Sessions in the left sidebar and the terminal in the
center. The mux owns terminal tabs, splits and ratios. New tabs and splits open a
terminal directly.

Each terminal pane is a Dock panel. Drag its handle to an edge to move it, or to
another pane's center to swap. Right-click the handle to extract into a tab.
Divider drags commit split weights to the mux server when released. Pane splits
restore from the mux checkpoint, independently of `native-panels.json`.
`chrome.pane-divider-width` and `chrome.pane-divider-color` style Dock's separator
line; the component owns its grab area. The line overlays the boundary and takes
no terminal grid space.

`toggle_left_dock` shows or hides the left sidebar. `toggle_sessions_panel`
toggles the Sessions panel, `show_sidebar` opens it, and `show_spaces` opens it at
the Space switcher, which sits at the bottom of the sidebar. The palette, keybinding editor, CLI and socket
expose the same commands as the header button. For example,
`[input].keybind = ["ctrl+shift+l=toggle_left_dock"]` binds the left dock.
Cmd+B in the macOS Ghostty preset and Cmd+Shift+E (Ctrl+Shift+E elsewhere) in the
Bootty and tmux presets invoke `toggle_left_dock`.

`chrome.left-dock-toggle` controls the header button; its command remains
available. The sidebar's visibility and width persist per window in
`native-panels.json`, shared across Spaces; drag its edge to resize.
`chrome.sidebar` and `chrome.sidebar-width` seed an unsaved layout.

The **Tabs** settings (`[chrome.tabs]`) select classic, underline, pill, outline,
or segmented tabs; close-button side (left/right); and close-button visibility
(always/on hover/hidden). Hover buttons occupy the tab's side padding without
reserving a separate column. Hiding a close button keeps the close command and
context menu available.

## Files

`files.reveal ABSOLUTE_PATH` shows a file or folder on this computer in its file
manager (Finder on macOS); a remote host's files have no place there. A session in
the sidebar offers it on right-click, with Copy Path. A session's menu also starts a
new session in its project and copies its branch.
`files.open_in ABSOLUTE_PATH EDITOR` opens a file or folder on this computer in an
installed editor: `vscode`, `cursor`, `zed`, `windsurf`, `sublime`, `intellij` or
`xcode`. The sidebar offers **Open in** each editor Bootty finds: an app in
`/Applications` or `~/Applications` on macOS, its launcher on `PATH` elsewhere.
Bootty looks once per run. `files.complete BASE QUERY` finds files under a
directory on the target binding's host.

Command-click (Control-click on Linux and Windows) a terminal link to open it
(`link.open`). URLs open in the default browser, and file locations open in this
computer's default application for that file.

## Notifications and shell integration

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
Replay snapshots alone cannot establish a command duration. The read-only
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
find controls and completion notifications. Other product dialogs and OS-owned UI
still need extraction before a complete additional language can ship. Terminal
output, paths, editable contents, command IDs, config keys and backend diagnostics
retain their original bytes. Chinese/Japanese Bootty translations are not bundled
yet.

## Remote placement

SSH Spaces run on the host's native mux server, so shells survive disconnects and
app restarts. WSL placements report `RemoteSpaceUnavailable`.

An SSH placement uses the table shown in the example above. A WSL placement
replaces the SSH fields with a distribution:

```toml
[multiplexer.remote]
distribution = "Ubuntu"
```

Do not combine `distribution` with SSH fields. Existing SSH tables retain their
serialized format. `bootty command wsl.list` lists installed WSL distributions on
Windows and returns Unsupported on other operating systems. Bootty never installs
or unregisters distributions.

### SSH transport

Host commands on an SSH host, such as listing its projects for New
Session, run through Bootty's daemon on that host. Bootty uploads the daemon
bundled with the app, or downloads and checksum-verifies the release daemon when
the bundle has no matching target; the full Bootty app is not required there.
The connection authenticates and bootstraps through the configured SSH client,
then prefers a persistent QUIC connection with mutual TLS. No separate login or
permanent transport key is needed. When the remote address is unreachable
directly or UDP is blocked, Bootty uses one persistent SSH TCP tunnel carrying
HTTP/2 streams with mutual TLS. This requires no firewall changes. If both setups
fail, the existing SSH command path remains available; failed persistent setup is
cached for one minute. Requests already sent are never replayed after an
ambiguous connection loss.

Bootty dials with ConnectTimeout=5, ServerAliveInterval=5 and
ServerAliveCountMax=3, so a lost connection ends in about 15 seconds instead of
hanging. Flags listed in `args` come first on the command line, so any of these
can be set differently.

On Windows, WSL host commands run through `wsl.exe --distribution NAME --cd ~
--exec`, with the matching Linux daemon installed from packaged assets or the
checksummed release cache. Linux paths never resolve against a Windows or macOS
namesake.

## Settings links

See [terminal image clipboard writes](clipboard-writes.md) for host keys, supported
formats, limits and permission scope.

Settings rows include **Reset to default**, which removes the explicit override,
and **Copy setting link**. macOS packages register links such as `bootty://settings/font.size` to open Settings
at that configuration path. The equivalent CLI command is `bootty command open_setting font.size`. Development packages use their own namespace as the
URL scheme. Fractional number controls use two decimal places; integer controls
remain integral.
