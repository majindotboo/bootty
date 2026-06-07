# Bootty

## Run Modes

- Full app: `cargo run -p bootty-app --bin bootty`
- Bare WGPU host: `cargo run -p bootty-app --example bare`
- eframe tabs example: `cargo run -p bootty-app --example egui-tabs`

Bootty uses the macOS account login shell by default. Use `BOOTTY_SHELL=/path/to/shell`
only when a smoke test needs an explicit shell override.

## Validation

```bash
cargo fmt --check
cargo clippy
cargo test
cargo bench --no-run
cargo bench -p bootty-app --bench paint_plan -- --noplot
```

Use `cargo run` directly. If `cargo` does not resolve through mise shims, fix
the shell/mise setup instead of prefixing commands with `mise exec`.

Install the repository hooks locally when `git config --get core.hooksPath`
does not print `.githooks`:

```sh
git config core.hooksPath .githooks
```

The pre-commit hook runs `cargo fmt --check` and
`cargo clippy --workspace --all-targets -- -W clippy::all`.

## Manual Verification

- `cargo run -p bootty-app --bin bootty` must open the full Bootty window with tmux
  chrome, status metrics, and visible terminal glyphs.
- `cargo run -p bootty-app --example bare` must open a native bare terminal window;
  shell output in the launching terminal is not sufficient.
- `cargo run -p bootty-app --example egui-tabs` must open the tabs example and route
  terminal content through the shared WGPU renderer.
- For glyph smoke checks, paste and run `printf '%s\n' 'bootty glyph probe: 🥟 ABC █ ┃'`.

## Toolchain

Use the repository `mise.toml`. `cargo` should resolve through the mise shim without `mise exec`:

```sh
mise current rust
command -v cargo
cargo --version
```

## Docs

- Project overview: `README.md`
- Architecture: `docs/architecture.md`
- Egui oracle inventory: `docs/current-egui-behavior.md`
- Input encoders: `docs/input-encoders.md`
- Performance notes: `docs/performance.md`
- `libghostty-rs` dependency boundary: `docs/libghostty-rs.md`
