# Bootty

Bootty is a native GPU-rendered terminal workspace and a local control host. One
desktop app, one installed daemon, and a set of library crates with explicit
owners.

Four things shape almost every decision in this repo:

1. **One owner per fact.** Every durable value and every live mutation has
   exactly one crate that owns it.
2. **The terminal is not the multiplexer.** Bootty renders terminals; the
   native multiplexer in `bootty-mux` owns session/window/pane topology and pane
   runtimes.
3. **Frames, not blocking.** The UI thread never waits on terminal or Git
   work. Terminal state is published as immutable frames and painted from a
   plan.
4. **One invocation path.** The palette, keybindings, CLI, and local socket all
   submit the same `CommandInvocation`. There is no second way to do a thing.

## A small glossary

Use this language when you write code and when you talk to us.

- **you** - the agent doing the work. **we/us** - the maintainers. **user** -
  the person directing you.
- **Space** - a persisted workspace with a native multiplexer binding. Lives in SQLite,
  owned by `bootty-mux`.
- **binding** - one Space's native sessions, pane layout,
  focus, and titles. The native mux server owns the live one; `BindingRuntime` projects it.
- **multiplexer** - Bootty's concrete native implementation. It owns real pane
  processes and session/window/pane topology in a long-lived server. There are no selectable backends or providers.
- **pane** / **session** / **window** - native multiplexer topology, owned by
  `bootty-mux`. Hosts project it and submit commands to it.
- **surface** - host-neutral terminal geometry (`bootty-terminal`). Host
  coordinates convert at the adapter seam, not in the middle.
- **frame** - an immutable published snapshot of VT state. Consumers read
  frames; they do not read the engine.
- **identity** - Production (`bootty`) or Development (`bootty-dev`). Selects
  config tree, state tree, control endpoint, and versioned native mux server endpoint.

## The two ways to hurt yourself

**A release build from your tree is the user's real app.** Identity is
`Development` only under `debug_assertions`. So
`cargo run` is safely `BoottyDev`, but a release-profile build takes the Production namespace: the user's `config.toml`,
their workspace SQLite, their control endpoint, and their native mux server. A Production app launch
exits when that identity already has a live owner. A Production command
invocation instead targets that live owner and can mutate the user's real
Spaces. Use `mise run launch` for normal development
and UI acceptance. On macOS it packages and runs this worktree's uniquely named
`BoottyDev-<workspace-hash>.app` under `dist/<development-namespace>/`, so native
automation can address the app. Other platforms run the isolated development
binary directly. Nothing is installed. Use `mise run package:dev` to package
without launching. There is intentionally no `install:dev`
task because installing per-worktree builds would litter `/Applications`.
`launch` and `package:dev` use the dev profile and keep Production state apart.

**Never patch `libghostty-rs` in-tree.** It is a pinned external binding crate.
Anything you can do by preprocessing input, postprocessing frames, or calling
public `libghostty-vt` APIs belongs in `bootty-terminal`. Anything that needs
Ghostty internals the C API does not expose is unsupported. See
`docs/libghostty-rs.md`.

## Hit every surface

Bootty fans out. Fixing one arm is not fixing the feature.

- **Commands** reach the same `CommandInvocation` from the palette, keybindings,
  CLI, socket, and internal callers. A command that only
  works from the palette is unfinished. Preserve wire caller values for existing clients.
- **The native multiplexer** owns session, window, and pane changes. Use its
  concrete command path and serializable `MuxCommand`/`MuxSnapshot`; hosts do not
  invent topology. SSH Spaces use the same native server through the daemon remote link.
  Unsupported placements such as WSL report typed `RemoteSpaceUnavailable`.
- **Config fields** are not one edit. A new field needs a typed config value, a
  default, and one `SettingSpec` whose path the loader reads and whose page owns
  its editor. Scalar specs render their own settings row; only non-scalar
  settings need a custom editor. Update `docs/sample-config.toml` and
  `docs/configuration.md`. Tests in `bootty-config` enforce this contract.
- **The GUI host is GPUI.** Renderer and input changes must work through the full
  application and must be exercised in its real window.
  GPUI Kit (`longbridge/gpui-kit`) supplies the styled `gpui-component` crate
  and `gpui-base` behavior layer. Read the local `gpui` and `gpui-component`
  skills before UI changes; verify APIs against the pinned checkout.
- **Identities** are two. Anything touching paths, endpoints, or namespaces must
  keep Production and Development apart.

## Releases

When Luan asks you to make a release, read and follow `docs/releases.md`. Build
the release notes from commits, then include them in the body of the prepare
commit made directly on `main`. Do not create a release branch or pull request.

## Verifying

Run targeted tests while you work: `cargo nextest run -p bootty-terminal
<test-name>`. Do not run Cargo commands in parallel - concurrent Rust builds
fight over CPU, memory, disk, and the Cargo lock, and the "parallel" run is
slower than the serial one.

Before you hand work back, the local gate is:

```sh
mise run fmt
mise run clippy
mise run test
mise run bench -- --ci-smoke
```

- **Benchmarks are compile gates, not measurement gates.** `--ci-smoke` is
  enough for any non-performance change. Run measured Criterion suites only for
  rendering and performance work, or when asked. `docs/benchmarking.md` has the
  guardrails and the claim hygiene rules.

Nextest does not run doctests, so executable documentation examples live in
integration tests.

**GUI changes need a window.** Output in the launching terminal does not prove a
render path works. `cargo run -p bootty --bin bootty` should show the tab bar,
status metrics, and real glyphs; the two examples should each open their own
native window. For a glyph smoke check, run
`printf '%s\n' 'bootty glyph probe: 🥟 ABC █ ┃'` inside the app.

## Taste

- Complexity belongs behind a small interface, not spread across callers. A
  caller should never reconstruct an invariant or mirror state it does not own.
- Persistence commits before live publication. A failed commit leaves the prior
  state active. Never publish a half-applied mutation and reconcile later.
- Multiplexer failure is reported, never invented. Bootty does not fake success it
  did not observe.
- `mod.rs` and `lib.rs` hold module declarations and re-exports. Nothing else.
- Keep test code out of production source: no `#[cfg(test)]`, fixtures, or test
  support in a non-test module, and no path bridge into `tests/`. Tests are
  Cargo integration targets under the crate's `tests/`. The root `tests/` is
  only for cross-crate product behavior and executable boundaries.
- Large files are allowed while each keeps one cohesive owner and a small
  interface. File size is a signal for review, not proof of depth.
- `unsafe` is denied workspace-wide. Clippy runs `all`, `pedantic`, and `nursery`
  at deny.
- Reuse the names already in this repo. New vocabulary for an existing concept
  is a bug.

If a rule here fights the task in front of you, say so out loud rather than
quietly working around it. These are the defaults we hold, not law.

## Plans and work artifacts

Do not commit implementation plans, research notes, or scratch files. The vault
owns product language, plans, rationale, and durable decisions. The code is the
source of truth for structure; `docs/` holds process and user reference only.

## Where to read next

`README.md` has the run and packaging commands and the doc index. Beyond that:

- `docs/configuration.md` - config path, schema, reload, and writeback.
- `docs/input-encoders.md` - the terminal input contract.
