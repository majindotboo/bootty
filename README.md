# Bootty

Bootty is a native GPU-rendered terminal and a set of reusable terminal crates.

## Run

```sh
cargo run -p bootty --bin bootty
```

The default app opens the full Bootty shell with terminal rendering, status
metrics, and tmux session chrome through GPUI.

## Workspace

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
- `bootty-write` - shared locked atomic replacement and durability outcomes.

## Native app bundles

Native Bootty app bundles are built from `bootty --bin bootty`.

```sh
mise run package          # local dynamic package with the host daemon
mise run package --static # static package with the host daemon
mise run package --all-daemons --static # static package with every remote daemon
mise run package:windows  # Windows zip from a staged complete daemon set
mise run install          # local dynamic package and install for the current OS
mise run build --fast     # dynamic build with --profile fast-release
mise run install --fast   # dynamic install using --profile fast-release
```

CI and release packages contain the five daemon targets owned by `xtasks`.

On macOS, local packages are signed with a self-signed `Bootty Dev` identity so
Accessibility and Screen Recording grants survive reinstalls. `package` creates
it on first use; macOS asks for your password once to trust it. Run
`mise run sign:setup` to do that step explicitly. Without a keychain (CI) the
bundle is signed ad-hoc.
Local package and install tasks build only
the host daemon unless `--all-daemons` is passed. On non-macOS hosts, Apple
targets require an installed Apple SDK in `SDKROOT`. Windows packaging requires
a complete staged daemon directory through `BOOTTY_DAEMON_OUTPUT_DIR`. CI builds
that directory on target-capable runners.

The CI workflow runs full Rust validation on pull requests and pushes. Pushing
a version tag matching `Cargo.toml` creates a GitHub Release with native macOS,
Windows, and Linux bundles. Installed Bootty releases check for updates on
startup; use `bootty update` to update explicitly. See `docs/releases.md`.

## Validation

```sh
mise run fmt
mise run clippy
mise run test
mise run bench -- --ci-smoke
```

## Docs

- Architecture and crate boundaries: `docs/architecture.md`
- [Automation](docs/automation.md) — command targets, event waits and condition snapshots.
- [Transfers and forwards](docs/transfers.md) — streaming files and managed SSH listeners.
- [Shell assistance](docs/shell-assistance.md) — optional multiline editing and host history.
- Pi and Codex integration setup: `docs/agent-integrations.md`
- Configuration path, schema, reload, and writeback: `docs/configuration.md`
- Translation catalogs and locale behavior: `docs/localization.md`
- Input encoder contracts: `docs/input-encoders.md`
- Benchmark process and performance guardrails: `docs/benchmarking.md`
- Built-in theme provenance: `docs/built-in-themes.md`
- `libghostty-rs` dependency boundary: `docs/libghostty-rs.md`
- Release publishing and verified updates: `docs/releases.md`

- [Terminal image clipboard writes](docs/clipboard-writes.md)

- [Previous-session output and agent recovery](docs/recovery.md)

- [Git panel workflows](docs/git-workflows.md)
