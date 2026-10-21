# Bootty

Bootty is a native, GPU-rendered terminal workspace for local and remote work.
Keep terminals, coding agents, Git worktrees, and browser pages together in one
desktop app.

- **Spaces** save your workspace and its backend bindings. Use native, rmux, or
  tmux backends locally or over SSH; connect to Herdr through opaque attachments.
- **Terminals and agents** share the workspace. Work with Codex, Claude, and Pi
  through native integrations or their terminal interfaces.
- **Git tools** bring projects, branches, worktrees, and diffs into the app.
- **Automation** uses the same command path as the palette and keybindings,
  exposed through the CLI and local socket.

The desktop UI uses GPUI. Ghostty powers terminal state, and mux backends own
processes, sessions, windows, and panes.

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
local mux namespaces. A release build without the `bootty-dev` feature uses
Production state; use `mise run launch` for development and UI checks.

## Find your way around

| Task | Guide |
| --- | --- |
| Configure the app and keybindings | [Configuration](docs/configuration.md) · [Sample config](docs/sample-config.toml) |
| Set up coding agents | [Agent integrations](docs/agent-integrations.md) |
| Work with projects, branches, and worktrees | [Git workflows](docs/git-workflows.md) |
| Automate Bootty | [Commands, targets, and events](docs/automation.md) |
| Move files and forward ports | [Transfers and forwards](docs/transfers.md) |
| Use multiline editing and host history | [Shell assistance](docs/shell-assistance.md) |
| Recover output and agent sessions | [Recovery](docs/recovery.md) |
| Write terminal images to the clipboard | [Clipboard writes](docs/clipboard-writes.md) |

## Build and install

These commands package or install the **Production** app:

```sh
mise run package                       # package with the host daemon
mise run package --static              # static package with the host daemon
mise run package --all-daemons --static # include every remote daemon target
mise run install                       # package and install for the current OS
```

Use `mise run build --fast` or `mise run install --fast` for the
`fast-release` profile with dynamic linkage.

Local packages include only the host daemon unless `--all-daemons` is passed.
CI and release packages include all five daemon targets owned by `xtasks`.
On non-macOS hosts, Apple targets require an Apple SDK in `SDKROOT`. Windows
packaging uses `mise run package:windows` and requires a complete staged daemon
set in `BOOTTY_DAEMON_OUTPUT_DIR`; CI stages it on target-capable runners.

On macOS, local packages use a self-signed `Bootty Dev` identity so Accessibility
and Screen Recording grants survive reinstalls. Packaging creates it on first
use and macOS asks for your password once to trust it. Use `mise run sign:setup`
to do this explicitly. Without a keychain, bundles are signed ad-hoc.

Installed releases check for updates on startup. Run `bootty update` to update
explicitly. See [Releases](docs/releases.md) for publishing and verified updates.

## Contribute

Read [AGENTS.md](AGENTS.md) and the [architecture](docs/architecture.md) before
changing ownership or adding a feature. Each durable fact and live mutation has
one owner; terminals publish immutable frames, and every command uses one typed
invocation path.

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

## Crate map

- `bootty` - executable startup, CLI dispatch, and native packaging.
- `bootty-ui` - GPUI application and windows, presentation and input adapters,
  terminal paint plans, text policy, and sprites.
- `bootty-terminal` - Ghostty-backed terminal state, input, geometry, PTY sessions,
  shell selection, drain scheduling, and immutable frame publication.
- `bootty-config` - product configuration, accepted keymaps, identity namespaces,
  OpenType feature values, and safe configuration edits.
- `bootty-host` - host process execution, SSH, remote daemon installation, and
  command framing.
- `bootty-git` - Git project, worktree, branch, favorite, and diff facts.
- `bootty-mux` - backend contracts, native/rmux/tmux/Herdr providers, remote
  Spaces and persistence, daemon catalog, and live binding orchestration.
- `bootty-daemon` - installed headless catalog and remote command executable.
- `bootty-control` - invocation envelopes, command transport, cancellation, and
  owner-local tasks and subscriptions.
- `bootty-agents` - native Pi, Codex, and Claude integrations and protocol state.
- `bootty-browser` - native browser views, page identity, and web content policy.
- `bootty-computer` - desktop capture, accessibility, and validated input.
- `bootty-write` - shared locked atomic replacement and durability outcomes.
- `bootty-test-support` - test-only helpers, such as fake provider scripts that start
  without macOS checking each new copy.

## Technical references

- [Architecture and crate ownership](docs/architecture.md)
- [Terminal input contracts](docs/input-encoders.md)
- [Benchmarking and performance guardrails](docs/benchmarking.md)
- [Localization](docs/localization.md)
- [Built-in theme provenance](docs/built-in-themes.md)
- [Ghostty dependency boundary](docs/libghostty-rs.md)
