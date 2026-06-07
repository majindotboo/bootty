# Bootty Tauri WebGL demo

This crate is a small example of embedding Bootty's non-egui terminal stack in a
Tauri application with a React frontend.

## Backend boundary

`src/lib.rs` owns the Tauri bridge:

- starts a `bootty_runtime::TerminalSession`
- resizes the PTY/grid from browser canvas dimensions
- writes raw terminal input bytes
- serializes `RenderFrame` into a small web DTO

The backend does not depend on egui.

## Frontend boundary

`src-ui/src` is intentionally split into focused pieces:

- `terminal-api.ts` selects the active frontend backend
- `tauri-terminal-backend.ts` wraps the Tauri commands
- `rust-site-backend.ts` wraps the Rust/WASM ratatui + tuirealm site app
- `main.tsx` owns only the canvas host and polling/input loop
- `webgl-terminal.ts` renders terminal frames with WebGL2

The WebGL renderer uses instanced quads for backgrounds, text, underlines, and
cursor. Text glyphs are cached in a DPR-aware atlas; Canvas2D is only used when
rasterizing a new glyph into that atlas.

The default build uses the Tauri backend. `npm run build:pages` uses
`.env.github-pages` to select the Rust/WASM tuirealm site backend and writes
`pages-dist`, which can be hosted directly by GitHub Pages.

## Run

```sh
npm install
npm run tauri -- dev
```

## Build check

```sh
npm run build
npm run build:pages
cargo test -p bootty-tauri
npm run tauri -- build --no-bundle
```