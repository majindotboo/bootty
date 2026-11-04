# Bootty

Bootty is a native, GPU-rendered terminal workspace. It keeps your terminals,
sessions, and Git worktrees together in one desktop app.

- **Spaces** save your workspace: its sessions, tabs, and splits. Bootty's
  native mux server owns processes locally or on an SSH host. Shells and full-screen programs
  survive quitting, crashing, or updating the desktop app.
- **Sessions** sit in the left sidebar with their project, branch, pull request,
  and diff counts. Start a session on a new worktree from the New Session form.
- **Remote Spaces** run the same native mux server on the SSH host over QUIC,
  with TCP/TLS and SSH fallback. Panes survive connection loss and app restarts.
  File transfers, forwarded ports, and remote Git use the host daemon too.
- **Automation** uses the same command path as the palette and keybindings,
  exposed through the CLI and local socket.

The desktop UI uses GPUI. Ghostty powers terminal state, and the native
multiplexer server owns processes, sessions, windows, pane layouts, and focus.
The app reattaches using a full Ghostty VT checkpoint followed by live bytes.

## Run from source

With [mise](https://mise.jdx.dev/) installed:

```sh
mise install
mise run launch
```

`launch` builds and runs an isolated Development instance for this worktree.
On macOS, it creates a uniquely named `BoottyDev-<workspace-hash>.app` under
`dist/<development-namespace>/`. It does not install the app. To package the
Development app without launching it, use `mise run package:dev`.

Production and Development have separate config, state, control endpoints, and
local mux namespaces. A release build uses
Production state; use `mise run launch` for development and UI checks.

## Find your way around

| Task | Guide |
| --- | --- |
| Configure the app and keybindings | [Configuration](docs/configuration.md) · [Sample config](docs/sample-config.toml) |
| Work with projects, branches, and worktrees | [Git workflows](docs/git-workflows.md) |
| Automate Bootty | [Commands, targets, and events](docs/automation.md) |
| Move files and forward ports | [Transfers and forwards](docs/transfers.md) |
| Use multiline editing and host history | [Shell assistance](docs/shell-assistance.md) |
| Recover terminal output | [Recovery](docs/recovery.md) |
| Write terminal images to the clipboard | [Clipboard writes](docs/clipboard-writes.md) |

## Build and install

These commands package or install the **Production** app:

```sh
mise run package                       # package with the host daemon
mise run package --all-daemons          # include every remote daemon target
mise run install                       # package and install for the current OS
```

`mise run build`, `mise run launch`, and `mise run package:dev` use the dev
profile. Production packages and daemons use release.

Local packages include only the host daemon unless `--all-daemons` is passed.
CI and release packages include all five daemon targets owned by `xtasks`.
On non-macOS hosts, Apple targets require an Apple SDK in `SDKROOT`. Windows
local installs build the host daemon. For Windows release packaging, use
`mise run package:windows` with a complete staged daemon set in
`BOOTTY_DAEMON_OUTPUT_DIR`; CI stages it on target-capable runners.

On macOS, local packages use a self-signed `Bootty Dev` identity so macOS
privacy grants survive reinstalls. Packaging creates it on first
use and macOS asks for your password once to trust it. Use `mise run sign:setup`
to do this explicitly. Without a keychain, bundles are signed ad-hoc.

Installed releases check for updates on startup. Run `bootty update` to update
explicitly. See [Releases](docs/releases.md) for publishing and verified updates.

## Contribute

Read [AGENTS.md](AGENTS.md) before changing ownership or adding a feature. Each durable fact and live mutation has
one owner; terminals publish immutable frames, and every command uses one typed
invocation path.

Tests and benchmark validators require Python 3 on `PATH` (`python` on Windows,
`python3` on Unix).

Run the local validation gate:

```sh
mise run fmt
mise run clippy
mise run test
mise run bench -- --ci-smoke
```

Use targeted tests while working, for example
`cargo nextest run -p bootty-terminal <test-name>`. Run Cargo commands serially.
UI changes also need validation in a real app window.

## Technical references

- [Terminal input contracts](docs/input-encoders.md)
- [Benchmarking and performance guardrails](docs/benchmarking.md)
- [Localization](docs/localization.md)
- [Built-in theme provenance](docs/built-in-themes.md)
- [Ghostty dependency boundary](docs/libghostty-rs.md)
