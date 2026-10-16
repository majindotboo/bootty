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

The Permissions settings page and `[computer]` table control local desktop computer use. `enabled`,
`capture-enabled`, and `input-enabled` default to `false`. Capture requires both
`enabled` and `capture-enabled`, plus an existing macOS Screen Recording grant.
Input requires both `enabled` and `input-enabled`, plus an existing Accessibility
grant. Changing these settings does not request or grant OS permission.
`computer.status`, `computer.targets` and `computer.icons` remain readable while disabled.
Manual browser screenshot attachments use the existing Screen Recording grant
independently of these agent-command switches.

Agents in native conversations use local apps through `computer_snapshot` and
`computer_input`, naming an app by bundle id or name. The first use of an app in
a conversation shows an approval card: **Allow for this session** grants it for
the rest of the conversation, **Always allow** also adds its bundle id to
`always-allowed-apps`, and **Deny** refuses. An `@` mention of an app window in
the prompt grants it the same way. This consent replaces the switches above for
agent app use; macOS Screen Recording and Accessibility grants still apply.
Input targets the window from the app's latest capture, so an agent captures an
app before clicking or typing. On macOS a capture also lists the window's accessibility
elements in screen order, including disabled and unlabeled ones, and `computer_input`
can press an element by its number: Bootty uses the element's own action, or clicks
its center when it has none. Listing elements needs the Accessibility grant; Chromium
and Electron apps are asked to expose their web content, which may take one more
capture to appear. Agents never use the Bootty instance they run
in, terminals, or System Settings; another Bootty instance, such as a
development build, is an ordinary app.

Computer commands use an exact window token returned by `computer.targets`.
Window or process changes invalidate the token, and input is never redirected
to another window. `computer.elements` lists the window's accessibility elements
and `computer.press` presses one of them by the element it returned; it follows
the capture switch and the input switch respectively. Computer use works on
macOS, Windows and Linux on X11, including XWayland apps in a Wayland session;
native Wayland windows are not listed. `computer.snapshot` writes a bounded PNG
to the supplied absolute local path and never replaces an existing file.

On macOS, capture needs macOS 14 or later, and input goes straight to the target
window without focusing it, so the window may sit behind other apps and the
pointer never moves; Bootty shows its own cursor briefly where each click, move
or scroll landed. Typed text and keys go to the window its app has focused,
whether or not that app is in front: `computer.focus` makes the target that
window and lifts it behind the frontmost app without activating its app. Views
that need their window active, such as web content, ignore clicks while their
app is in the background, so a left click in an app that is not frontmost
presses the accessibility element under the point when it has its own press
action. Other clicks, and every click on the frontmost app, are sent as clicks. On Windows and Linux, input drives the real pointer,
so the target must be the focused window and each point uncovered;
`computer.focus` brings it to the front, and Command shortcuts are sent as
Control.

```toml
[computer]
enabled = false
capture-enabled = false
input-enabled = false
always-allowed-apps = [] # bundle ids, such as "dev.zed.Zed"
```

The `[browser]` table chooses the search engine used for address-bar queries
and whether browser site data persists between launches. The defaults are
`duckduckgo` and `true`.

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

[browser]
search-engine = "duckduckgo" # duckduckgo, google, bing, or brave
persist-site-data = true

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
tabs-use-session-color = true
top-bar = true
bottom-bar = false
status-height = 30
status-background = "#1e1e2e"
notched-fullscreen-black-chrome = true
gap = 0
unfocused-sidebar-dim = 0.16
unfocused-terminal-dim = 0.0

[chrome.tabs]
# Terminal and sidebar tabs share one appearance and close-button setting.
appearance = "segmented" # classic, underline, pill, outline, segmented
close-position = "right" # left or right
close-button = "always" # always, hover, hidden

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
shell-integration = false # optional hooks for new bash, zsh and fish PTYs
output-archives = false # private bounded previous-session snapshots
clipboard-write-hosts = "" # opt-in host keys: local ssh:user@host:22 wsl:Ubuntu
shell = "/bin/zsh"
working-directory = "/Users/example/src"
worktree-directory = "~/.local/share/bootty/worktrees"
worktree-limit = 10 # Bootty worktrees kept per host; 0 removes only unused ones
branch-prefix = "bootty/" # start of new worktree branch names; a project's prefix wins
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

A cold session restore retains its layout, directories and bounded styled history.
Supported terminal agents resume their saved provider conversation in the original
pane, using the captured account and current provider enablement. Live mux processes
reattach instead. Automatic terminal-agent resume is limited to the local POSIX
host; it does not transfer credentials or permission grants to remote bindings.

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
  `SurfaceChooser`, `Composer`, `ComposerCompletion`, `Terminal`, `Herdr`, `Native`, `rmux`, and `tmux`. Backend contexts apply only
  while a terminal is focused. The Zed-style forms
  `Terminal && backend == herdr|native|rmux|tmux` are aliases.
  `Command` applies to the active command palette or picker; underlying workspace
  shortcuts are inactive while that modal surface is open.
  `SurfaceChooser` applies only to the new tab or split chooser. Every choice and
  navigation command is editable here or in the Keymap tab: `ui.surface.agent`,
  `terminal`, `claude`, `codex`, `pi`, `edit_profiles`, `previous`, `next`, `first`,
  `last`, `confirm`, and `cancel` (each with the `ui.surface.` prefix).
  Defaults include arrows and Tab/Shift-Tab, Vim J/K and gg/G, Emacs Ctrl-N/P,
  Alt-</>, Ctrl-M and Ctrl-G. A/T choose Agent/Terminal, 1–3 choose terminal
  profiles, E edits profiles, and Escape cancels. Inline keycaps show the effective
  bindings; removing a shortcut does not reveal a hidden hard-coded fallback.
  `Composer` applies while an Agent composer is shown: a native conversation or the
  new-session form. Elsewhere its keys reach the terminal.
  `ComposerCompletion` applies while an Agent composer has an active suggestion menu.
  `ui.composer.previous`, `next`, `confirm`, and `cancel` use the same editable keymap.
  Defaults include Up/Down, Emacs Ctrl-P/N, Ctrl-K/J and Vim Alt-K/J, Enter/Tab to
  insert, and Escape/Ctrl-G to dismiss. These shortcuts leave normal typing intact.
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
Catalog refreshes preserve unsaved status segments and environment rows. A successful
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

- Status and sidebar modules are native implementations. Status segments support
  `session`, `windows`, `clock`, and `sysinfo`; the sidebar includes native session
  facts and agent integrations. Existing Lua/Luau files under the config tree
  and raw `[extensions]` values are preserved, but are not executed. Native Pi,
  Codex, and Claude integration setup is documented in [agent integrations](agent-integrations.md).

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

## Session list

`sidebar.group-by-project` defaults to `true`. The sidebar view button and
`ui.sidebar.toggle_grouping` switch between project groups and a flat list.
The Settled heading and `ui.sidebar.toggle_settled` open and close the settled
shelf, saved as `sidebar.settled-collapsed`. The shelf lists the most recently
active sessions first, ten at a time.
Each session owns its branch and agent marks; switching views preserves sessions,
selection and terminal contents. Sessions are grouped into Pinned, Sessions and
Settled. Working and waiting show on the row, so a row and its numbered shortcut
stay put while its agent works; a settled session that needs an answer returns
to Sessions. A session's tooltip shows its short name, like `bootty-7c`, which
works in place of its target in commands, the CLI and the session switcher; see
`docs/automation.md`. The search box matches titles, projects, branches, agents and
short names, and
finds sessions whose conversations mention the words, showing the line that
matched; it reads the recent transcript Bootty keeps for each conversation. A
closed session's row is dimmed and says Closed. Double-click a
row, or choose **Rename…**, to edit its title in place: Enter or clicking away
saves it, Escape keeps the old title. A conversation's menu can start a side
chat from it. Command-click (Control-click on Linux and Windows) chooses several
sessions and Shift-click a range; right-click one of them to pin, settle, snooze,
mark unread, archive or delete them together. Escape, a plain click or returning to
the terminal lets go. Archive retains saved work, asks first
when the session is working, and offers Undo; Delete permanently removes a
closed session's saved record and requires confirmation.

Open **Settings → Projects** to add a registered folder, open its settings, or
remove its registration. Removal preserves files and existing sessions.
A project header's actions menu starts a session in the project, reveals or
copies its path, and opens **Project settings…**, the same page in a named,
closeable settings tab. Switching tabs preserves drafts; closing an unsaved draft asks
before discarding it.
The page edits its name, icon or local image, default agent, and defaults for
creating worktrees (branch prefix and starting ref).
It also keeps the project's scripts: named shell commands that a session's menu runs in
a new terminal tab, in that session's directory (`project.script CWD NAME`). Scripts
marked **Setup** also run, each in its own tab, in every session that starts in a new
worktree of the project (`project.setup CWD`), such as installing dependencies.
**Before removing a worktree** runs in a project's worktree when worktree cleanup is about
to remove it; if it fails, the worktree stays and the cleanup report says what it said.
Project settings live in the workspace database and apply to new sessions.
Existing sessions keep their selected provider and model.

New worktrees go in `<session.worktree-directory>/<repository>/<name>` on the
repository's host. Unset, the directory is `~/.local/share/bootty/worktrees` on
macOS and Linux and `%LOCALAPPDATA%\bootty\worktrees` on Windows; Development
builds use their own namespace in place of `bootty`. A leading `~` is that host's
home. Generated names use the task's short slug, and branches start with
`session.branch-prefix` (default `bootty/`) unless the project sets its own. The sidebar groups a linked
worktree's sessions under its main checkout's project.

Bootty cleans up the worktrees in that directory on each host: hourly, soon after
a session is deleted, archived or settled, and when the worktree settings change.
A worktree no saved session or live terminal uses goes after its first hour.
Past `session.worktree-limit` (default 10), the worktrees of the oldest archived
sessions go first, then the oldest settled ones. Active sessions and sessions with
a running terminal keep theirs. A worktree with uncommitted changes or commits no
remote has always stays, and Bootty says so once. A removed worktree's branch goes
too when `git branch -d` accepts it. `0` removes only worktrees no session uses.
The unset directory is only cleaned by the identity's default configuration; another
configuration path must set `session.worktree-directory`.

`worktrees.clean` (**Clean Up Worktrees** in the palette) runs the cleanup now on
every host and reports what each removed and kept. It asks for confirmation from
the CLI and socket.

`settle_session` settles the exact current saved session without closing its
terminal. `toggle_session_pin` pins it, or unpins it when already pinned, and
`archive_session` archives it. All three are in the palette. Every preset binds:

| Command                | macOS         | Linux and Windows  |
| ---------------------- | ------------- | ------------------ |
| `agent_history`        | `Cmd+Shift+H` | `Ctrl+Shift+H`     |
| `toggle_changes_panel` | `Cmd+Shift+G` | `Ctrl+Shift+G`     |
| `add_selection_to_conversation` | `Cmd+Shift+L` | `Ctrl+Shift+L` |
| `toggle_session_pin`   | `Cmd+Opt+P`   | `Ctrl+Shift+Alt+P` |
| `archive_session`      | `Cmd+Opt+A`   | `Ctrl+Shift+Alt+A` |

`add_selection_to_conversation` adds the focused document's selected lines, as an
`@path:12-18` mention, or the focused terminal's selected text, as a text file
chip, to the conversation in the current session, and shows it.

Settle is `Cmd+Opt+X` (`Ctrl+Shift+Alt+X`) in the Bootty and tmux presets and
`Cmd+Shift+W` in the macOS Ghostty preset. `session.settle`, `session.pin` and
`session.archive` accept an explicit saved identity with its Binding target for
detached work and other Spaces.

`sidebar.sort-order` defaults to `"manual"`. Choose **Manual** or **Recent activity**
from the sidebar view menu or Sidebar settings. Recent activity uses persisted
accepted session input and native prompts; equal or undated sessions keep their
manual order. The shared commands are `ui.sidebar.sort_manual` and
`ui.sidebar.sort_recent_activity`. Drag a session to move it, or a project
header to move all of its sessions. A drop in Recent activity order keeps the
order shown as the manual order and switches to Manual.

`sidebar.animate-working` defaults to `true`. Turn off **Animate Working ring**
on the Sidebar settings page to keep the Working ring still. Reduced motion also
keeps it still. Changes apply when configuration reloads.

`sidebar.auto-settle-after-days` defaults to `3`; set it to `0` to disable
automatic settlement. **Auto-settle behavior** in a session's menu can keep that
session active. Pinned, snoozed and busy sessions are excluded. Settlement keeps
the terminal and history; accepted input returns the session to the active list.
**Snooze → Custom…** chooses a future date and time in the local time zone.

## Workspace docks

`toggle_left_dock` and `toggle_right_dock` show or hide their respective docks.
The palette, keybinding editor, CLI and socket expose the same commands as the
header buttons. Panel commands are `show_sidebar`, `show_files`, `show_changes`,
and `show_agents`.
For example, `[input].keybind = ["ctrl+shift+l=toggle_left_dock"]` binds the left dock.

Sessions occupy the left sidebar. Files, Changes, Diff and documents open as
labeled peer tabs in the right tool area. Its plus menu and empty state open
tools; closing the last tab returns to the empty state. Tools cannot be dragged
into another region or split into nested groups. The mux owns terminal tabs,
splits and ratios independently.

`chrome.left-dock-toggle` and `chrome.right-dock-toggle` control the header
buttons; their commands remain available. Side visibility, widths, tool tabs
and document positions persist per window, shared across Spaces. Older layouts
are flattened into these fixed homes while retaining their documents.
The retired `chrome.panel-tab-style`, `chrome.panel-tabs`, `toggle_tab_bar` and
`toggle_hidden_tabs` no longer hide tool labels or navigation.
`show_agents` still provides the existing agent controls through the palette;
Agents has no primary navigation entry. `show_spaces` opens the left sidebar,
whose Space switcher remains at the bottom.

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

Settings → Panels offers an optional status bar button (none, top, bottom) for
Sessions, Files, Changes and Diff. Panel placement is fixed. Legacy `dock`
preferences and Agents button preferences remain loadable but have no effect.

The **Dock Tabs** and **Terminal Tabs** settings independently select classic,
underline, pill, outline, or segmented tabs; close-button side (left/right); and
close-button visibility (always/on hover/hidden). Hover buttons occupy the tab's
side padding without reserving a separate column. Hiding a close button keeps the
close command and context menu available.

Side visibility and widths are saved in `native-panels.json`; drag a side edge
to resize. Each session keeps its own right side: its tools, documents and
subagent tabs, and whether it shows. The sessions sidebar on the left is shared. The legacy `chrome.sidebar` and `chrome.sidebar-width` keys seed an
unsaved layout. `sidebar.position` no longer moves the sidebar.

## Git tool panels

Use **Show Git Changes** in the palette (`show_changes`) or click a sidebar diff
count. Changes and Diff are peer tabs in the right tool area: switch or close their
tabs, resize the side, or toggle its visibility. Opening Changes again
restores closed Changes; selecting a file restores and activates Diff. Layouts are
saved per window, shared across Spaces, in `native-panels.json` beside the active config file.
Older tile groups reopen as tabs with the frontmost tile selected; every panel remains available.

Changes separates staged, unstaged, and untracked files, each grouped by folder.
Stage/Unstage updates the index without changing working files; a group's header
stages or unstages everything at once. Selecting lines in a file's diff stages or
unstages their hunk alone. **Discard Changes…** on a file, or **Discard all** on the
unstaged group, asks first and then puts files back to the last commit, deleting
untracked ones; ignored files stay. **All changes** shows every change in one diff.
The diff switches between unified and side by side, wraps long lines, and can hide
whitespace changes. Commit uses only the index and needs a message and something staged;
Amend explicitly replaces the last commit, needs a message, and asks before proceeding.
The sparkle beside them asks the quick model to write the message from the staged changes
and recent subjects (`git.write-message`); a message typed meanwhile stays. A new pull
request's **Write with Codex** fills an empty title and description from the branch's
commits and changes since its base (`git.write-pull-request REPOSITORY BASE`).

The header shows the branch's pull request, with its state and checks, from the host's
`gh` account (`git.pull-request`); it opens among the pull requests. Sidebar rows and a
conversation's context card show the same pull request, asked about once a minute for
the selected session and every five minutes for the rest. The branch list has a search,
shows each branch's distance from its upstream, and switches from a tree with changes by
stashing them first, untracked files included (`git.branch-stash-switch`); the stash
stays for you to apply. A conversation's composer shows its branch and switches the
checkout from a menu of recent local branches. Git errors stay visible in the
panel. The message box remains disabled while its command is running. When the
branch has an upstream, the header fetches, and shows ↓ and ↑ counts that pull
(fast-forward only) and push (never forced) the commits the two don't share.

CLI/socket/keybinding callers use the same commands: `git.open REPOSITORY`,
`git.status REPOSITORY`, `git.diff REPOSITORY PATH staged|unstaged|untracked [show|ignore]`,
`git.diff-all REPOSITORY [show|ignore]`, `git.stage REPOSITORY PATH`,
`git.unstage REPOSITORY PATH`, `git.stage-all REPOSITORY`, `git.unstage-all REPOSITORY`,
`git.stage-hunk REPOSITORY PATCH [reverse]`, `git.discard REPOSITORY PATH`,
`git.discard-all REPOSITORY`, `git.write-message REPOSITORY`,
`git.write-pull-request REPOSITORY BASE`, `git.pull-request REPOSITORY`,
`git.branch-stash-switch REPOSITORY BRANCH`, `git.commit REPOSITORY MESSAGE`,
`git.amend REPOSITORY MESSAGE`, `git.fetch REPOSITORY`, `git.pull REPOSITORY` and
`git.push REPOSITORY`. `git.status` reports the upstream with its ahead and behind
counts. Repository/file paths belong to the target binding's host. Paths are literal rather than Git pathspec patterns; invalid UTF-8 filenames are
currently rejected by the text transport. External amend callers use the existing
command confirmation mechanism. SSH runs through the existing daemon transport.

### Files and document panels

Open Files through the Tools toolbar or `files.browse ABSOLUTE_DIRECTORY`.
`files.open ABSOLUTE_PATH [LINE [COLUMN]]` opens a document at a one-based
location on the target binding's host. The Files tree expands directories,
filters loaded entries, shares Git decorations with Changes, and pages large
directories. Refresh rereads the displayed directory; reopening a directory
refreshes its children.
`files.reveal ABSOLUTE_PATH` shows a file on this computer in its file manager
(Finder on macOS); a remote host's files have no place there. A document's tab,
a file link in a conversation, a changed file in Changes, and a session in the
sidebar offer it on right-click, with Copy Path. A session's menu also starts a
new session in its project and copies its branch.
`files.open_in ABSOLUTE_PATH EDITOR` opens a file or folder on this computer in an
installed editor: `vscode`, `cursor`, `zed`, `windsurf`, `sublime`, `intellij` or
`xcode`. The same places offer **Open in** each editor Bootty finds: an app in
`/Applications` or `~/Applications` on macOS, its launcher on `PATH` elsewhere.
Bootty looks once per run.

PNG, JPEG, GIF, and WebP files open as read-only images fitted to the panel.
Animated images show their first frame. Encoded media has no document-size cap;
decoded images are limited to 8192 pixels per side and 16 megapixels.
On macOS, video previews support playback, seeking, and audio through the same
seekable media reader used for local, SSH, and WSL sources.

Documents support Save (including Command/Ctrl-S), Reload, Markdown Preview, and
Format Document (Alt/Option-Shift-F). Formatting uses `rustfmt` for Rust, `taplo`
for TOML, and `prettier` for JavaScript, TypeScript, JSON/JSONC, Markdown,
HTML, CSS, and YAML. The formatter must be installed on the document's host.
Formatting reads the current draft through standard input and returns an
undoable edit; it does not save the file. If the draft changes while formatting
runs, Bootty leaves it alone and asks you to run Format again.

The document editor supports multiple cursors: Alt/Option-click adds one,
Alt/Option-Shift-drag selects a column, and Escape returns to one cursor.
Use Alt-Shift-Up/Down on Linux, Command-Option-Up/Down on macOS, or
Control-Alt-Up/Down on Windows to add cursors above or below.
A dirty document's Close button offers Save, Discard, and Cancel. Closing a
window or quitting with unsaved documents also asks; Save all keeps the window
open so save errors remain visible. Saving over an external edit is rejected.
Reload asks before replacing a draft. UTF-8 files up to 512 KiB are editable;
unsupported binary formats and larger text documents report an error.

Files commands also expose `files.list PATH [OFFSET]`, `files.read PATH`, and
`files.save PATH EXPECTED_SHA256 CONTENT_BASE64` for the CLI and control socket.
`files.format PATH CONTENT_BASE64` returns formatted document bytes without
writing the host file.
`files.source PATH ROOT` returns a seekable source descriptor after checking that
the regular file stays inside the captured root. A composer's `@` file mention
inserts the file's project path instead (`@src/main.rs`, quoted when it has
spaces), so the agent reads and edits the real file. Choosing a folder opens it;
**This folder** names the folder itself.
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

### SSH transport

Packaged SSH workspaces authenticate and bootstrap through the configured SSH
client, then prefer a persistent QUIC connection with mutual TLS for terminals
and host commands. No separate login or permanent transport key is needed.
When the remote address is unreachable directly or UDP is blocked, Bootty uses
one persistent SSH TCP tunnel carrying HTTP/2 streams with mutual TLS. This
requires no firewall changes. If both setups fail, the existing SSH command path
remains available; failed persistent setup is cached for one minute. Requests
already sent
are never replayed after an ambiguous connection loss.

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
Alerts cover native conversations, agents reporting through hooks, and agents
Bootty starts in a terminal; each also lists in the Agents panel. Alerts play the
system notification sound, subject to the system's notification settings, and on
macOS the Dock icon shows how many agents wait on you.

See [terminal image clipboard writes](clipboard-writes.md) for host keys, supported
formats, limits and permission scope.

Settings rows include **Reset to default**, which removes the explicit override,
and **Copy setting link**. macOS packages register links such as `bootty://settings/font.size` to open Settings
at that configuration path. The equivalent CLI command is `bootty command open_setting font.size`. Development packages use their own namespace as the
URL scheme. Fractional number controls use two decimal places; integer controls
remain integral.

## Agent providers

Settings → AI groups new-session defaults and shared conversation behavior,
including the default provider, response streaming, composer collapse, send shortcut
and follow-up behavior, agent notifications and child-agent access. AI also
contains each provider's executable, enabled state, model and
thinking defaults, available thinking levels and named account/launch profiles.
The default account uses the provider's existing store. A named profile can select
an absolute account directory and literal launch arguments. Credentials and
conversation history remain provider-owned; Bootty never imports or copies them.

Pi profiles retain literal `--extension` / `-e` paths on creation and resume.
Provider credentials, prompt arguments and prior session selectors are still
excluded from reusable launch metadata.

```toml
[agents.codex]
enabled = true
program = "codex"
selected = "work"

[agents.codex.profiles.work]
name = "Work"
directory = "/absolute/path/to/codex-work"
arguments = ["--model", "gpt-6.1-sol"]
```

The same keys apply to `agents.claude` and `agents.pi`. Account directories use `CODEX_HOME`, `CLAUDE_CONFIG_DIR` and `PI_CODING_AGENT_DIR`, respectively. Directory selection currently requires a local POSIX terminal; remote accounts belong to the remote host. It does not change provider permission or credential-store policies. Leaving the directory empty reuses the default account with different launch arguments.

Start and resume open terminal tabs in the selected session. Changes affect future launches. Sign-in opens the provider's own terminal flow; Pi uses `/login`. Refresh status runs bounded, read-only version and account queries on a worker. Codex reports its account type and plan through its app server; Claude reports its own login state and subscription type; Pi reports the selected provider's existing authentication without inferring a subscription. An older refresh cannot replace a newer account selection. Update runs the proven owning native, Bun-global or npm-global installer in a local POSIX terminal tab. Its exit result stays visible until Enter closes the tab. Unknown installations require a manual update; active agent tabs prevent updates. Bootty does not update agents automatically.

Codex's **Quick model** (`agents.quick-model`) names sessions and worktrees, and writes
commit messages and pull request text. It defaults to `gpt-6-luna` and uses the selected
Codex account. Each query runs only when asked, in an ephemeral read-only process without
tools, and does not create a conversation.

`agents.allow-spawn = false` is the default. Enabling it in Settings → Permissions
allows an attached agent to create child sessions only in its captured workspace,
project directory, provider and account. Native parents can interrupt or stop only
native children created under that exact live grant; stopping retains the history
and backend pane. Children from an older parent generation remain outside the grant.
Parents can read, paste, submit, interrupt and close only shell panes created under
their current grant. Listed terminals and agent carrier panes do not receive input
authority. Closing, revoking or renewing the parent removes that shell authority;
historical creation receipts cannot restore it. At most 128 shells are retained
per live grant, including queued creations.
Child tools retain their own terminal read access and lose spawning, supervision
and capture access. Closing or disabling the parent revokes the inherited tool
authority; existing sessions and processes remain.
Native parents create native children on either their local or remote host.
A restored child keeps its provider identity and account but does not regain the
former parent's tool grant.
Native conversations retain their resolved profile ID with the account directory.
Resuming or forking after a profile preference changes keeps that captured pair;
`inspect_provider` reports the captured ID without exposing account paths. Older
conversations keep their account and report no profile ID.
The root-only `list_profiles` tool lists the captured provider's configured IDs
and display names. It does not switch profiles or authorize another account.

The sidebar's **History** opens a full view of saved conversations across Codex,
Claude and Pi on the captured host. Provider and project filters are inside the
view. Sessions group under Today, Yesterday, Previous 7 days, Previous 30 days
and Older; a search ranks them instead. Each provider lists its 200 most recent
sessions, and **Load older sessions** fetches the next 200. Opening a result
resumes its original provider session and account rather than starting a new
conversation.

`agents.follow-up-behavior` defaults to `"queue"`. Messages sent during an active
turn wait for it to finish; `"steer"` sends them into the active turn instead,
waiting in the queue until the turn can take them; `"interrupt"` stops the turn and
sends them next.
`agents.send-shortcut` decides which keys send one message with the opposite
behavior. The queued messages appear above the composer and remain with the
conversation when its tab closes. Arrows move a waiting message sooner or later.
Stopping the agent pauses the queue so the next message doesn't go out at once, and
so does a failed turn; Resume, or sending a message, lets it go again, moving on from
a failure without restarting the agent. A message that couldn't be sent waits for
Retry without holding the ones behind it.

`agents.default-provider` selects the provider for a fresh composer. Each provider's
`default-model` and `default-effort` prefer advertised selections; empty values resolve
to the provider's configured model and supported effort. The Settings pickers use
the same account catalog as the composer. Retained drafts keep their
explicit selection. Model and permission catalogs persist under the captured
provider settings, account, remote target and project directory; Bootty stores
hashed cache identities, and changing that context selects a different entry.
`agents.codex.fast-mode` requests Codex's fast service tier;
`agents.claude.fast-mode` enables Claude's fast mode. These are defaults for new
conversations. Both composers expose **Fast mode** in the model menu; its bolt
indicates the selected speed. When Codex describes its fast tier, such as "2x
speed, increased usage", that description shows under the switch. Existing
conversations save speed changes for the next turn without interrupting active work. Switching Fast mode off explicitly
clears the provider's previous fast selection. Pi has no fast-mode setting.

New composers inherit the selected provider account's approval policy. The
permissions control displays the effective policy reported by the provider;
unknown policies remain inherited. Choosing an explicit policy overrides that
default. Switching provider or account returns to that account's policy.
Pi does not advertise approval modes, so its composer hides the permissions
control and submits the provider default.


Provider settings include `agents.<provider>.available-efforts`, an array of effort identifiers.
An empty array exposes all levels supported by each model. A nonempty array limits
the picker and the Alt+, / Alt+. shortcuts to those levels, in the model's advertised
order. The shortcuts decrease or increase effort without wrapping at either end.
Their commands are `ui.composer.effort-down` and `ui.composer.effort-up`.

On macOS, text fields take the Control keys Cocoa text fields do: Control-B and
Control-F move by character, Control-D and Control-H delete forward and back, and
Control-A and Control-E go to the line's ends. In the composer, Control-P and
Control-N move by line and Control-K deletes to the line's end; an open completion
menu takes Control-P, Control-N and Control-K for itself.

Composer controls have bindable commands: `ui.composer.focus-space`,
`ui.composer.focus-provider`, `ui.composer.focus-model`, `ui.composer.focus-effort`,
`ui.composer.focus-project`, `ui.composer.focus-worktree`, and `ui.composer.focus-permissions`.
Their default shortcuts are Ctrl+Alt+S/M/P/W/A for space, model,
project, worktree, and permissions. Provider and effort have no separate default binding.
Shift+Tab toggles plan mode (`ui.composer.toggle-plan`) when the provider offers it,
Ctrl+Alt+F toggles fast mode (`ui.composer.toggle-fast`) for a model that offers it,
Ctrl+Alt+D and Ctrl+Alt+V start dictation (`ui.composer.dictate`) and voice mode
(`ui.composer.voice`), and Ctrl+Alt+I moves focus back to the composer
(`ui.composer.focus-composer`). Cmd+S (Ctrl+S elsewhere, `ui.composer.stash`) puts
the composer's words aside, in a stash any conversation can use; on an empty
composer it brings the newest stashed prompt back. The bookmark badge beside the
controls lists the stash, and choosing a prompt brings it back. Attachments stay in
the composer. The stash keeps fifty prompts, in `prompt-stash.json` beside the
configuration.
These defaults and the effort shortcuts live in the `Composer` context.
Provider, model, and effort share one picker; their focus commands open it. Control
tooltips describe their purpose and show the active configured shortcut.

`agents.send-shortcut` accepts `"enter"` (default), `"mod-enter-multiline"`, or
`"mod-enter"`. It applies to both the new-session and conversation composers.
With the default shortcut, Shift+Enter inserts a newline and Command/Control+Enter
sends with the opposite follow-up behavior. When Command/Control+Enter is required,
adding Shift uses the opposite follow-up behavior for that message.


`agents.response-streaming` selects `"live"` (default), `"paragraph"`, or `"complete"`.
Live mode shows words as they arrive and fades them in, and holds back an inline span
until its closing marker arrives, so `**`, backticks and links never show raw and then
change. Paragraph mode waits for completed Markdown blocks and keeps unfinished fenced
code blocks together. Final replies always display in full.

A reply's image on its own line that names a file shows that file: absolute paths as
written, relative paths from the conversation's directory, read on the conversation's
host, remote ones included, up to 32 MiB. One that can't load says why in its place. Web
and data URL images load as usual. A finished ```` ```mermaid ```` fence shows the drawn
diagram in the theme's colors; one still streaming, or one Mermaid can't draw, shows its
source, with the reason. An ```` ```html ```` block offers to open the page in your
browser from a private file. Code blocks highlight the languages agents write most,
SQL included.
`agents.transcript-width` sets how wide messages run: `"wide"` (default, 72rem),
`"normal"` (48rem), or `"full"`. In a wide window the conversation details card
sits in the margin beside centered messages, and moves them aside only when it
would cover them.
`agents.collapse-composer-on-scroll` defaults to true; an empty, unfocused composer
folds to one line while reading older turns, and opens when focused. Drafts and
attachments keep the composer expanded.
