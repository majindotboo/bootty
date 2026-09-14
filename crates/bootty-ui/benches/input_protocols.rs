use std::hint::black_box;

use anyhow::{Result, ensure};
use bootty_terminal::geometry::TerminalGeometry;
use bootty_terminal::{
    terminal_engine::TerminalEngine,
    terminal_input_model::{
        KeyInput, KeyMods, MouseAction, MouseButton, MouseEncoderSize, MouseInput, TerminalKey,
    },
};
use criterion::{BatchSize, Criterion};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};

#[derive(Clone, Copy)]
enum KeyboardCase {
    LegacyPrintable,
    ApplicationCursor,
    ModifyOtherKeys,
    CsiU,
    KittyKeyboard,
    AltMetaCtrlShift,
    FunctionKeys,
    RepeatKeys,
    DeadKeyText,
    AltGrText,
}

impl KeyboardCase {
    const fn name(self) -> &'static str {
        match self {
            Self::LegacyPrintable => "legacy_printable",
            Self::ApplicationCursor => "application_cursor",
            Self::ModifyOtherKeys => "modify_other_keys",
            Self::CsiU => "csi_u",
            Self::KittyKeyboard => "kitty_keyboard",
            Self::AltMetaCtrlShift => "alt_meta_ctrl_shift",
            Self::FunctionKeys => "function_keys",
            Self::RepeatKeys => "repeat_keys",
            Self::DeadKeyText => "dead_key_text",
            Self::AltGrText => "altgr_text",
        }
    }
}

#[derive(Clone, Copy)]
enum MouseCase {
    X10,
    Normal,
    ButtonEvent,
    AnyEvent,
    Sgr,
    Urxvt,
    Wheel,
    Drag,
    PixelPosition,
}

impl MouseCase {
    const fn name(self) -> &'static str {
        match self {
            Self::X10 => "x10",
            Self::Normal => "normal",
            Self::ButtonEvent => "button_event",
            Self::AnyEvent => "any_event",
            Self::Sgr => "sgr",
            Self::Urxvt => "urxvt",
            Self::Wheel => "wheel",
            Self::Drag => "drag",
            Self::PixelPosition => "pixel_position",
        }
    }
}

#[derive(Default)]
struct ProtocolStats {
    events: usize,
    bytes: usize,
    side_effects: usize,
    frame_chars: usize,
    hash: u64,
}

impl ProtocolStats {
    fn add_bytes(&mut self, bytes: &[u8]) {
        self.events = self.events.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.hash = bytes
            .iter()
            .fold(self.hash ^ 0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
    }

    fn checksum(&self) -> Result<u64> {
        Ok(self.hash
            ^ u64::try_from(self.events)?
            ^ u64::try_from(self.bytes)?.rotate_left(17)
            ^ u64::try_from(self.side_effects)?.rotate_left(31)
            ^ u64::try_from(self.frame_chars)?.rotate_left(47))
    }
}

fn engine() -> Result<TerminalEngine> {
    TerminalEngine::new(GEOMETRY)
}

fn configure_keyboard(engine: &mut TerminalEngine, case: KeyboardCase) {
    match case {
        KeyboardCase::ApplicationCursor => engine.write_vt(b"\x1b[?1h"),
        KeyboardCase::ModifyOtherKeys => engine.write_vt(b"\x1b[>4;2m"),
        KeyboardCase::CsiU => engine.write_vt(b"\x1b[>4;1m"),
        KeyboardCase::KittyKeyboard => engine.write_vt(b"\x1b[>1u"),
        _ => {}
    }
}

fn key_event(case: KeyboardCase, index: usize) -> Result<KeyInput> {
    Ok(match case {
        KeyboardCase::ApplicationCursor => KeyInput {
            key: *[
                TerminalKey::ArrowUp,
                TerminalKey::ArrowDown,
                TerminalKey::ArrowRight,
                TerminalKey::ArrowLeft,
            ]
            .get(index.rem_euclid(4))
            .ok_or_else(|| anyhow::anyhow!("keyboard key index"))?,
            mods: KeyMods::default(),
            repeat: false,
            utf8: None,
            unshifted: None,
        },
        KeyboardCase::FunctionKeys => KeyInput {
            key: *[
                TerminalKey::F1,
                TerminalKey::F5,
                TerminalKey::F12,
                TerminalKey::PageDown,
            ]
            .get(index.rem_euclid(4))
            .ok_or_else(|| anyhow::anyhow!("function key index"))?,
            mods: KeyMods {
                shift: index.is_multiple_of(2),
                ctrl: index.is_multiple_of(3),
                alt: index.is_multiple_of(5),
                ..KeyMods::default()
            },
            repeat: false,
            utf8: None,
            unshifted: None,
        },
        KeyboardCase::AltMetaCtrlShift => KeyInput {
            key: TerminalKey::A,
            mods: KeyMods {
                shift: true,
                alt: index.is_multiple_of(2),
                ctrl: index.is_multiple_of(3),
                command: index.is_multiple_of(5),
                ..KeyMods::default()
            },
            repeat: false,
            utf8: Some("A"),
            unshifted: Some('a'),
        },
        KeyboardCase::RepeatKeys => KeyInput {
            key: TerminalKey::J,
            mods: KeyMods::default(),
            repeat: index > 0,
            utf8: Some("j"),
            unshifted: Some('j'),
        },
        KeyboardCase::DeadKeyText => KeyInput {
            key: TerminalKey::E,
            mods: KeyMods::default(),
            repeat: false,
            utf8: Some("é"),
            unshifted: Some('e'),
        },
        KeyboardCase::AltGrText => KeyInput {
            key: TerminalKey::Q,
            mods: KeyMods {
                alt: true,
                ctrl: true,
                right_alt: true,
                ..KeyMods::default()
            },
            repeat: false,
            utf8: Some("@"),
            unshifted: Some('q'),
        },
        _ => KeyInput {
            key: TerminalKey::A,
            mods: KeyMods::default(),
            repeat: false,
            utf8: Some("a"),
            unshifted: Some('a'),
        },
    })
}

fn run_keyboard_case(case: KeyboardCase, events: usize) -> Result<u64> {
    let mut engine = engine()?;
    configure_keyboard(&mut engine, case);
    let mut out = Vec::new();
    let mut stats = ProtocolStats::default();
    for index in 0..events {
        engine.encode_key_to_vec(key_event(case, index)?, &mut out)?;
        stats.add_bytes(&out);
    }
    ensure!(stats.events == events, "keyboard event count mismatch");
    stats.checksum()
}

fn configure_mouse(engine: &mut TerminalEngine, case: MouseCase) {
    let sequence = match case {
        MouseCase::X10 => b"\x1b[?9h".as_slice(),
        MouseCase::Normal => b"\x1b[?1000h".as_slice(),
        MouseCase::ButtonEvent => b"\x1b[?1002h".as_slice(),
        MouseCase::AnyEvent => b"\x1b[?1003h".as_slice(),
        MouseCase::Sgr | MouseCase::PixelPosition | MouseCase::Wheel => {
            b"\x1b[?1000h\x1b[?1006h".as_slice()
        }
        MouseCase::Urxvt => b"\x1b[?1000h\x1b[?1015h".as_slice(),
        MouseCase::Drag => b"\x1b[?1002h\x1b[?1006h".as_slice(),
    };
    engine.write_vt(sequence);
}

fn mouse_event(case: MouseCase, index: usize) -> Result<MouseInput> {
    let button = match case {
        MouseCase::Wheel => Some(if index.is_multiple_of(2) {
            MouseButton::Four
        } else {
            MouseButton::Five
        }),
        _ => Some(MouseButton::Left),
    };
    Ok(MouseInput {
        action: if matches!(case, MouseCase::Drag) && !index.is_multiple_of(3) {
            MouseAction::Motion
        } else if index.is_multiple_of(5) {
            MouseAction::Release
        } else {
            MouseAction::Press
        },
        button,
        mods: KeyMods {
            shift: index.is_multiple_of(7),
            alt: index.is_multiple_of(11),
            ctrl: index.is_multiple_of(13),
            ..KeyMods::default()
        },
        x: f32::from(u16::try_from(index.rem_euclid(100))?).mul_add(7.0, 10.0),
        y: f32::from(u16::try_from(index.rem_euclid(30))?).mul_add(13.0, 8.0),
        pixel_x: f32::from(u16::try_from(index.rem_euclid(100))?).mul_add(7.0, 10.0),
        pixel_y: f32::from(u16::try_from(index.rem_euclid(30))?).mul_add(13.0, 8.0),
        size: MouseEncoderSize {
            screen_width: u32::from(GEOMETRY.cols).saturating_mul(GEOMETRY.cell_width),
            screen_height: u32::from(GEOMETRY.rows).saturating_mul(GEOMETRY.cell_height),
            cell_width: GEOMETRY.cell_width,
            cell_height: GEOMETRY.cell_height,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        },
    })
}

fn run_mouse_case(case: MouseCase, events: usize) -> Result<u64> {
    let mut engine = engine()?;
    configure_mouse(&mut engine, case);
    let mut out = Vec::new();
    let mut stats = ProtocolStats::default();
    for index in 0..events {
        engine.encode_mouse_to_vec(mouse_event(case, index)?, &mut out)?;
        stats.add_bytes(&out);
    }
    ensure!(stats.events == events, "mouse event count mismatch");
    stats.checksum()
}

fn run_paste_clipboard_ime() -> Result<u64> {
    let mut engine = engine()?;
    let mut out = Vec::new();
    let mut stats = ProtocolStats::default();
    let large_paste = "x".repeat(1024usize.saturating_mul(1024));
    for text in ["small paste", "line1\nline2\n", large_paste.as_str()] {
        engine.encode_paste_to_vec(text, &mut out)?;
        stats.add_bytes(&out);
    }
    engine.write_vt(b"\x1b[?2004h");
    engine.encode_paste_to_vec("bracketed\ncontrol\x1b[201~", &mut out)?;
    stats.add_bytes(&out);
    engine.write_vt(b"\x1b]52;c;Ym9vdHR5\x1b\\");
    stats.side_effects = stats
        .side_effects
        .saturating_add(engine.drain_side_effects().len());
    engine.write_vt("IME preedit かな漢字 dead-key e\u{301}\r\n".as_bytes());
    let frame = engine.extract_frame()?;
    stats.frame_chars = stats.frame_chars.saturating_add(frame.text.len());
    stats.hash ^= frame
        .text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
            (hash ^ u64::from(u32::from(*ch))).wrapping_mul(0x0000_0100_0000_01b3)
        });
    stats.checksum()
}

fn bench_keyboard(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let cases = [
        KeyboardCase::LegacyPrintable,
        KeyboardCase::ApplicationCursor,
        KeyboardCase::ModifyOtherKeys,
        KeyboardCase::CsiU,
        KeyboardCase::KittyKeyboard,
        KeyboardCase::AltMetaCtrlShift,
        KeyboardCase::FunctionKeys,
        KeyboardCase::RepeatKeys,
        KeyboardCase::DeadKeyText,
        KeyboardCase::AltGrText,
    ];
    for case in cases {
        c.bench_function(&format!("input_protocol_keyboard_{}", case.name()), |b| {
            b.iter_batched(
                || case,
                |case| {
                    black_box(
                        run_keyboard_case(case, 10_000).map_err(|error| failure = Some(error)),
                    )
                },
                BatchSize::SmallInput,
            );
        });
    }
    failure.map_or(Ok(()), Err)
}

fn bench_mouse(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let cases = [
        MouseCase::X10,
        MouseCase::Normal,
        MouseCase::ButtonEvent,
        MouseCase::AnyEvent,
        MouseCase::Sgr,
        MouseCase::Urxvt,
        MouseCase::Wheel,
        MouseCase::Drag,
        MouseCase::PixelPosition,
    ];
    for case in cases {
        c.bench_function(&format!("input_protocol_mouse_{}", case.name()), |b| {
            b.iter_batched(
                || case,
                |case| {
                    black_box(run_mouse_case(case, 10_000).map_err(|error| failure = Some(error)))
                },
                BatchSize::SmallInput,
            );
        });
    }
    failure.map_or(Ok(()), Err)
}

fn bench_paste_clipboard_ime(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    c.bench_function("input_protocol_paste_clipboard_ime", |b| {
        b.iter(|| black_box(run_paste_clipboard_ime().map_err(|error| failure = Some(error))));
    });
    failure.map_or(Ok(()), Err)
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .sample_size(10)
        .noise_threshold(0.20)
        .configure_from_args();
    bench_keyboard(&mut criterion)?;
    bench_mouse(&mut criterion)?;
    bench_paste_clipboard_ime(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
