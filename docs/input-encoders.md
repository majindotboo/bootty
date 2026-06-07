# Terminal input encoders

Bootty keeps egui input capture separate from terminal encoding. `input.rs`
converts egui events and `TerminalSurface` geometry into UI-free
`TerminalInputCommand` values. `TerminalEngine` turns those commands into bytes
using Ghostty-compatible encoders or direct UTF-8 writes.

## Command boundary

- Printable text and committed IME text become `TerminalInputCommand::Text` and
  are written as UTF-8 bytes.
- Paste becomes `TerminalInputCommand::Paste` and is encoded by
  `TerminalEngine::encode_paste_to_vec`, so bracketed-paste mode is respected.
- Focus changes become `TerminalInputCommand::Focus` and are encoded through
  `libghostty-vt`.
- Keyboard events become `TerminalInputCommand::Key` with `KeyInput`,
  `KeyMods`, and `TerminalKey`.
- Pointer move, pointer button, and mouse wheel events become
  `TerminalInputCommand::Mouse` after conversion through `TerminalSurface`.

## Keyboard policy

Non-text key events are encoded through `libghostty-vt::key::Encoder`, not
hardcoded escape sequences. Before encoding, `TerminalEngine` calls
`set_options_from_terminal(&terminal)` so terminal modes such as application
cursor/keypad and Kitty keyboard protocol can affect emitted bytes.

The complete key mapping lives in `input.rs::terminal_key`; tests are the
canonical coverage record. Do not maintain a second exhaustive key list in this
document.

## Mouse policy

Mouse input uses `TerminalSurface` for rect-local coordinates, padding, cell
size, and screen size. `TerminalEngine::encode_mouse_to_vec` delegates to
`libghostty-vt::mouse::Encoder` after loading options from terminal state.

The encoder writes bytes only when the terminal has enabled mouse tracking.
Selection and scrollback behavior must remain separate from terminal
mouse-reporting modes.

Mouse input intentionally has no ad hoc escape fallback.

## Test seams

- Unit tests cover printable-key double-send prevention, Ctrl/Alt shortcuts,
  text/paste/IME command separation, pointer coordinate conversion, and outside
  rect rejection.
- Property tests cover generated pointer coordinates inside the terminal
  surface.
- Acceptance tests exercise `TerminalEngine` key and focus encoding without
  spawning a shell.
