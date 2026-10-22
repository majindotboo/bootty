# libghostty-rs dependency boundary

Bootty uses `libghostty-rs` as an external binding crate for Ghostty terminal
state and parsing.

- Source: `https://github.com/Uzaaft/libghostty-rs.git`
- Ref: `0ef80413de1326685743378efbfa06d849e041a7`
- Dependency: workspace `libghostty-vt` Git dependency in `Cargo.toml`
- License: see the upstream repository

The native Ghostty source is pinned separately in `mise.toml` to upstream commit
`3d07ef9e28c4af05065b9e2e546b8ac8f9b62df0`, the Zig 0.17 migration. Mise downloads
the unchanged source archive and supplies it through the binding crate's public
`GHOSTTY_SOURCE_DIR` override. Zig 0.17 and `ReleaseFast` provide the native
Windows ARM64 build; the latter uses Ghostty's minimal release panic path.
The binding's `ZIG` override names mise's selected executable explicitly so an
older compiler inherited through `PATH` cannot change the native build.
Keep the source pin and binding revision compatible when updating either.

Bootty must not patch or extend `libghostty-rs` in-tree. Functionality that can
be implemented by preprocessing terminal input, postprocessing frame data, or
using public `libghostty-vt` APIs belongs in Bootty crates, primarily
`crates/bootty-terminal`.

Functionality that requires Ghostty internals not exposed through the
`libghostty-vt` C API is unsupported unless it can be approximated entirely in
Bootty without modifying the binding crate.

`bootty-config` owns neutral parsed RGBA values. `bootty-mux::terminal_config`
owns the conversion from those values into `libghostty-vt` terminal RGB values.
The conversion intentionally drops alpha because the terminal RGB type has no
alpha channel.
