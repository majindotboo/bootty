use bootty_terminal::{
    geometry::TerminalGeometry,
    terminal_engine::{TerminalColorConfig, TerminalEngine},
    terminal_input::{ModifierSideState, TerminalInputCommand, TerminalInputEffects},
    terminal_input_model::{
        KeyInput, KeyMods, MouseAction, MouseButton, MouseEncoderSize, MouseInput, TerminalKey,
    },
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::{fixture, rstest};

const fn key_input(key: TerminalKey, mods: KeyMods) -> KeyInput {
    KeyInput {
        key,
        mods,
        repeat: false,
        utf8: None,
        unshifted: None,
    }
}

#[test]
fn modifier_side_state_applies_physical_sides_to_terminal_input() {
    let sides = ModifierSideState {
        right_shift: true,
        right_alt: true,
        left_ctrl: true,
        ..ModifierSideState::default()
    };
    let mut input = key_input(
        TerminalKey::Tab,
        KeyMods {
            shift: true,
            alt: true,
            ..KeyMods::default()
        },
    );

    sides.apply_to_key_input(&mut input);

    assert_eq!(
        input.mods,
        KeyMods {
            shift: true,
            alt: true,
            ctrl: true,
            right_shift: true,
            right_alt: true,
            ..KeyMods::default()
        }
    );
}

#[fixture]
fn engine() -> anyhow::Result<TerminalEngine> {
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 80,
            rows: 4,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        1024 * 1024,
    )?;
    engine.write_vt("line\r\n".repeat(20).as_bytes());
    engine.scroll_viewport_to(4);
    Ok(engine)
}

fn mouse(action: MouseAction, button: Option<MouseButton>) -> MouseInput {
    MouseInput {
        action,
        button,
        mods: KeyMods::default(),
        x: 0.0,
        y: 0.0,
        pixel_x: 0.0,
        pixel_y: 0.0,
        size: MouseEncoderSize {
            screen_width: 800,
            screen_height: 80,
            cell_width: 10,
            cell_height: 20,
            padding_top: 0,
            padding_bottom: 0,
            padding_left: 0,
            padding_right: 0,
        },
    }
}

#[derive(Clone, Copy, Debug, Arbitrary)]
enum Typing {
    Text,
    Paste,
    Enter,
}

proptest! {
    #[test]
    fn typing_encodes_current_modes_and_returns_to_live_viewport(
        typing in any::<Typing>(),
        text in "[a-zA-Z0-9 λ😀]{0,80}",
        bracketed in any::<bool>(),
    ) {
        let mut engine = engine().unwrap();
        if bracketed {
            engine.write_vt(b"\x1b[?2004h");
        }
        let (command, expected) = match typing {
            Typing::Text => (TerminalInputCommand::Text(text.clone()), text.into_bytes()),
            Typing::Paste => {
                let expected = if bracketed {
                    format!("\x1b[200~{text}\x1b[201~").into_bytes()
                } else {
                    text.as_bytes().to_vec()
                };
                (TerminalInputCommand::Paste(text), expected)
            }
            Typing::Enter => (
                TerminalInputCommand::Key(key_input(TerminalKey::Enter, KeyMods::default())),
                b"\r".to_vec(),
            ),
        };
        let mut output = b"stale input must never be delivered again".to_vec();
        let effects = command.apply(&mut engine, &mut output).unwrap();
        assert_eq!(output, expected);
        assert_eq!(effects, TerminalInputEffects { viewport_changed: true, force_publish: true });
        let scrollbar = engine.extract_frame().unwrap().scrollbar.unwrap();
        assert_eq!(scrollbar.offset.saturating_add(scrollbar.len), scrollbar.total);
    }
}

#[rstest]
#[case::untracked(false, b"".as_slice())]
#[case::tracked(true, b"\x1b[<35;1;1M".as_slice())]
fn only_reported_mouse_motion_forces_publication(
    engine: anyhow::Result<TerminalEngine>,
    #[case] tracking: bool,
    #[case] expected: &[u8],
) {
    let mut engine = engine.unwrap();
    if tracking {
        engine.write_vt(b"\x1b[?1003h\x1b[?1006h");
    }
    let mut output = b"old output".to_vec();
    let effects = TerminalInputCommand::Mouse(mouse(MouseAction::Motion, None))
        .apply(&mut engine, &mut output)
        .unwrap();
    assert_eq!(output, expected);
    assert_eq!(
        effects,
        TerminalInputEffects {
            viewport_changed: false,
            force_publish: tracking
        }
    );
    assert_eq!(engine.extract_frame().unwrap().scrollbar.unwrap().offset, 4);
}

#[rstest]
fn wheel_scrolls_the_viewport_or_reports_each_notch(
    engine: anyhow::Result<TerminalEngine>,
    #[values(false, true)] tracking: bool,
    #[values(-3, 0, 3)] delta: isize,
) {
    let mut engine = engine.unwrap();
    if tracking {
        engine.write_vt(b"\x1b[?1000h\x1b[?1006h");
    }
    let (button, report) = if delta < 0 {
        (MouseButton::Four, b"\x1b[<64;1;1M")
    } else {
        (MouseButton::Five, b"\x1b[<65;1;1M")
    };
    let mut output = b"old output".to_vec();
    let effects = TerminalInputCommand::MouseWheel {
        input: mouse(MouseAction::Press, Some(button)),
        scroll_delta: delta,
    }
    .apply(&mut engine, &mut output)
    .unwrap();
    let expected = if tracking {
        report.repeat(delta.unsigned_abs().max(1))
    } else {
        Vec::new()
    };
    assert_eq!(output, expected);
    assert_eq!(
        effects,
        TerminalInputEffects {
            viewport_changed: !tracking && delta != 0,
            force_publish: tracking || delta != 0,
        }
    );
    let expected_offset = if tracking {
        4
    } else {
        u64::try_from(4_isize.saturating_add(delta)).unwrap()
    };
    assert_eq!(
        engine.extract_frame().unwrap().scrollbar.unwrap().offset,
        expected_offset
    );
}

#[rstest]
fn focus_reports_respect_terminal_mode_and_preserve_the_viewport(
    engine: anyhow::Result<TerminalEngine>,
    #[values(false, true)] tracking: bool,
    #[values(false, true)] gained: bool,
) {
    let mut engine = engine.unwrap();
    if tracking {
        engine.write_vt(b"\x1b[?1004h");
    }
    let mut output = b"old output".to_vec();
    let effects = TerminalInputCommand::Focus(gained)
        .apply(&mut engine, &mut output)
        .unwrap();
    let expected: &[u8] = if !tracking {
        b""
    } else if gained {
        b"\x1b[I"
    } else {
        b"\x1b[O"
    };
    assert_eq!(output, expected);
    assert_eq!(
        effects,
        TerminalInputEffects {
            viewport_changed: false,
            force_publish: true
        }
    );
    assert_eq!(engine.extract_frame().unwrap().scrollbar.unwrap().offset, 4);
}
