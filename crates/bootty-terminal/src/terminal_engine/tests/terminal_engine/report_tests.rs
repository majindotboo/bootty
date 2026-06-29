use super::super::super::*;
#[cfg(unix)]
use super::super::{SharedMemoryFixture, is_shared_memory_unavailable};
use super::support::*;

#[test]
fn terminal_engine_answers_size_queries_from_current_geometry() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    for (query, expected) in [
        (b"\x1b[14t".as_slice(), b"\x1b[4;480;800t".as_slice()),
        (b"\x1b[16t".as_slice(), b"\x1b[6;20;10t".as_slice()),
        (b"\x1b[18t".as_slice(), b"\x1b[8;24;80t".as_slice()),
    ] {
        engine.write_vt(query);
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_orders_osc_color_query_responses_before_later_da1() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b]11;?\x1b\\\x1b[c");

    assert_eq!(
        take_pty_output(&output),
        b"\x1b]11;rgb:1a1a/1b1b/2525\x1b\\\x1b[?62;22;52c"
    );
    Ok(())
}

#[test]
fn terminal_engine_answers_kitty_graphics_probes() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b_Gi=31,s=1,v=1,a=q,t=s,f=24;AAAA\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b_Gi=31;EINVAL: invalid data\x1b\\"
    );

    engine.write_vt(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    assert_eq!(take_pty_output(&output), b"\x1b_Gi=31;OK\x1b\\");
    Ok(())
}

#[cfg(unix)]
#[test]
fn terminal_engine_answers_valid_tuie_shared_memory_kitty_probe() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;
    let fixture = match SharedMemoryFixture::write(&[0, 0, 0]) {
        Ok(fixture) => fixture,
        Err(err) if is_shared_memory_unavailable(&err) => return Ok(()),
        Err(err) => return Err(err),
    };
    let payload = fixture.payload()?;

    engine.write_vt(format!("\x1b_Gi=32,s=1,v=1,a=q,t=s,f=24;{payload}\x1b\\").as_bytes());

    assert_eq!(take_pty_output(&output), b"\x1b_Gi=32;OK\x1b\\");
    Ok(())
}

#[cfg(unix)]
#[test]
fn terminal_engine_answers_tuie_startup_capability_batch_before_da1_sentinel() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;
    let (kitty_query, _fixture) = match SharedMemoryFixture::write(&[0, 0, 0]) {
        Ok(fixture) => (
            format!(
                "\x1b_Gi=31,s=1,v=1,a=q,t=s,f=24;{}\x1b\\",
                fixture.payload()?
            ),
            Some(fixture),
        ),
        Err(err) if is_shared_memory_unavailable(&err) => (
            "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\".to_owned(),
            None,
        ),
        Err(err) => return Err(err),
    };
    let query = [
        b"\x1b[22;2t".as_slice(),
        kitty_query.as_bytes(),
        b"\x1b[23;2t\x1b[14t\x1b[16t".as_slice(),
        b"\x1b]11;?\x1b\\\x1b[c".as_slice(),
    ]
    .concat();

    engine.write_vt(&query);
    let output = take_pty_output(&output);

    let kitty =
        find_subslice(&output, b"\x1b_Gi=31;OK\x1b\\").expect("kitty graphics probe response");
    let window_px = find_subslice(&output, b"\x1b[4;480;800t").expect("window pixel size response");
    let cell_px = find_subslice(&output, b"\x1b[6;20;10t").expect("cell pixel size response");
    let background = find_subslice(&output, b"\x1b]11;rgb:1a1a/1b1b/2525\x1b\\")
        .expect("background color query response");
    let da1 = find_subslice(&output, b"\x1b[?62;22;52c").expect("DA1 sentinel response");

    assert!(kitty < da1, "kitty probe response must arrive before DA1");
    assert!(
        window_px < da1,
        "window pixel size response must arrive before DA1"
    );
    assert!(
        cell_px < da1,
        "cell pixel size response must arrive before DA1"
    );
    assert!(
        background < da1,
        "background color response must arrive before DA1"
    );
    Ok(())
}

#[test]
fn terminal_engine_reports_resizes_when_window_ops_are_enabled() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.resize(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 9,
        cell_height: 18,
    })?;

    engine.write_vt(b"\x1b[14t\x1b[16t\x1b[18t");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b[4;432;720t\x1b[6;18;9t\x1b[8;24;80t",
    );

    engine.write_vt(b"\x1b[?2048h");
    engine.resize(TerminalGeometry {
        cols: 100,
        rows: 40,
        cell_width: 9,
        cell_height: 18,
    })?;
    assert_eq!(take_pty_output(&output), b"\x1b[48;40;100;720;900t");

    Ok(())
}

type CapturedDeviceAttributesEngine = (
    TerminalEngine,
    Arc<Mutex<Vec<u8>>>,
    Arc<Mutex<DeviceAttributes>>,
);

fn captured_device_attributes_engine() -> Result<CapturedDeviceAttributesEngine> {
    let (mut engine, output) = captured_pty_engine()?;
    let attributes = Arc::new(Mutex::new(default_device_attributes()));
    let callback_attributes = attributes.clone();
    engine
        .terminal
        .on_device_attributes(move |_terminal| callback_attributes.lock().ok().map(|a| *a))?;
    Ok((engine, output, attributes))
}

#[test]
fn terminal_engine_reports_clipboard_in_production_primary_device_attributes() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b[c");

    assert_eq!(take_pty_output(&output), b"\x1b[?62;22;52c");
    Ok(())
}

#[test]
fn terminal_engine_reports_primary_device_attributes() -> Result<()> {
    let (mut engine, output, attributes) = captured_device_attributes_engine()?;

    for (primary, expected) in [
        (
            PrimaryDeviceAttributes::new(
                ConformanceLevel::VT220,
                &[DeviceAttributeFeature::ANSI_COLOR],
            ),
            b"\x1b[?62;22c".as_slice(),
        ),
        (
            PrimaryDeviceAttributes::new(
                ConformanceLevel::VT220,
                &[
                    DeviceAttributeFeature::ANSI_COLOR,
                    DeviceAttributeFeature::CLIPBOARD,
                ],
            ),
            b"\x1b[?62;22;52c".as_slice(),
        ),
        (
            PrimaryDeviceAttributes::new(
                ConformanceLevel::VT420,
                &[
                    DeviceAttributeFeature::COLUMNS_132,
                    DeviceAttributeFeature::SELECTIVE_ERASE,
                    DeviceAttributeFeature::ANSI_COLOR,
                ],
            ),
            b"\x1b[?64;1;6;22c".as_slice(),
        ),
        (
            PrimaryDeviceAttributes::new(ConformanceLevel::VT100, &[]),
            b"\x1b[?1c".as_slice(),
        ),
    ] {
        attributes.lock().expect("attributes lock").primary = primary;
        engine.write_vt(b"\x1b[c");
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_reports_secondary_and_tertiary_device_attributes() -> Result<()> {
    let (mut engine, output, attributes) = captured_device_attributes_engine()?;

    engine.write_vt(b"\x1b[>c");
    assert_eq!(take_pty_output(&output), b"\x1b[>1;0;0c");

    engine.write_vt(b"\x1b[=c");
    assert_eq!(take_pty_output(&output), b"\x1bP!|00000000\x1b\\");

    attributes.lock().expect("attributes lock").tertiary = TertiaryDeviceAttributes {
        unit_id: 0xAABBCCDD,
    };
    engine.write_vt(b"\x1b[=c");
    assert_eq!(take_pty_output(&output), b"\x1bP!|AABBCCDD\x1b\\");

    Ok(())
}

#[test]
fn terminal_engine_updates_mode_state_without_pty_output() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;
    assert!(engine.terminal.mode(Mode::WRAPAROUND)?);
    engine.terminal.set_mode(Mode::DECCKM, true)?;
    assert!(engine.terminal.mode(Mode::DECCKM)?);
    engine.terminal.set_mode(Mode::DECCKM, false)?;
    assert!(!engine.terminal.mode(Mode::DECCKM)?);
    engine.terminal.set_mode(Mode::INSERT, true)?;
    assert!(engine.terminal.mode(Mode::INSERT)?);
    assert_eq!(take_pty_output(&output), b"");
    Ok(())
}

#[test]
fn terminal_engine_reports_mode_query_status() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    for (input, expected) in [
        (b"\x1b[?7$p".as_slice(), b"\x1b[?7;1$y".as_slice()),
        (b"\x1b[?7l\x1b[?7$p".as_slice(), b"\x1b[?7;2$y".as_slice()),
        (b"\x1b[?2026$p".as_slice(), b"\x1b[?2026;2$y".as_slice()),
        (
            b"\x1b[?2026h\x1b[?2026$p".as_slice(),
            b"\x1b[?2026;1$y".as_slice(),
        ),
        (b"\x1b[?9999$p".as_slice(), b"\x1b[?9999;0$y".as_slice()),
        (
            b"\x1b[?7l\x1b[?7s\x1b[?7h\x1b[?7r\x1b[?7$p".as_slice(),
            b"\x1b[?7;2$y".as_slice(),
        ),
    ] {
        engine.write_vt(input);
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_answers_status_queries() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    for (input, expected) in [
        (b"\x1b[5n".as_slice(), b"\x1b[0n".as_slice()),
        (b"\x1b[6n".as_slice(), b"\x1b[1;1R".as_slice()),
        (
            b"\x1b[5;20r\x1b[?6h\x1b[3;5H\x1b[6n".as_slice(),
            b"\x1b[3;5R".as_slice(),
        ),
        (b"\x1b[19t".as_slice(), b"".as_slice()),
    ] {
        engine.write_vt(input);
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_answers_color_queries_from_active_palette() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b]10;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]10;rgb:c0c0/caca/f5f5\x1b\\"
    );

    engine.write_vt(b"\x1b]11;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]11;rgb:1a1a/1b1b/2525\x1b\\"
    );

    engine.write_vt(b"\x1b]10;#123456;#223344;#334455\x1b\\");
    for (code, expected) in [
        (10, b"\x1b]10;rgb:1212/3434/5656\x1b\\".as_slice()),
        (11, b"\x1b]11;rgb:2222/3333/4444\x1b\\".as_slice()),
        (12, b"\x1b]12;rgb:3333/4444/5555\x1b\\".as_slice()),
    ] {
        engine.write_vt(format!("\x1b]{code};?\x1b\\").as_bytes());
        assert_eq!(take_pty_output(&output), expected, "OSC {code} override");
    }

    engine.write_vt(b"\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\");
    engine.write_vt(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]10;rgb:c0c0/caca/f5f5\x1b\\\x1b]11;rgb:1a1a/1b1b/2525\x1b\\\x1b]12;rgb:c0c0/caca/f5f5\x1b\\"
    );

    engine.write_vt(b"\x1b]10;?;?;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]10;rgb:c0c0/caca/f5f5\x1b\\\x1b]11;rgb:1a1a/1b1b/2525\x1b\\\x1b]12;rgb:c0c0/caca/f5f5\x1b\\"
    );

    engine.write_vt(b"\x1b]4;232;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]4;232;rgb:0808/0808/0808\x1b\\"
    );

    engine.write_vt(b"\x1b]4;0;?;232;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]4;0;rgb:1515/1616/1e1e\x1b\\\x1b]4;232;rgb:0808/0808/0808\x1b\\"
    );

    engine.write_vt(b"\x1b]4;1;#123456\x1b\\\x1b]4;1;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]4;1;rgb:1212/3434/5656\x1b\\"
    );

    engine.write_vt(b"\x1b]104;1\x1b\\\x1b]4;1;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]4;1;rgb:f7f7/7676/8e8e\x1b\\"
    );

    Ok(())
}

#[test]
fn terminal_engine_answers_xterm_dynamic_color_queries_from_config() -> Result<()> {
    let colors = TerminalColorConfig {
        background: RgbColor { r: 1, g: 2, b: 3 },
        foreground: RgbColor { r: 4, g: 5, b: 6 },
        cursor: Some(RgbColor { r: 7, g: 8, b: 9 }),
        pointer_foreground: Some(RgbColor {
            r: 10,
            g: 11,
            b: 12,
        }),
        pointer_background: Some(RgbColor {
            r: 13,
            g: 14,
            b: 15,
        }),
        tektronix_foreground: Some(RgbColor {
            r: 16,
            g: 17,
            b: 18,
        }),
        tektronix_background: Some(RgbColor {
            r: 19,
            g: 20,
            b: 21,
        }),
        highlight_background: Some(RgbColor {
            r: 22,
            g: 23,
            b: 24,
        }),
        tektronix_cursor: Some(RgbColor {
            r: 25,
            g: 26,
            b: 27,
        }),
        highlight_foreground: Some(RgbColor {
            r: 28,
            g: 29,
            b: 30,
        }),
        ..Default::default()
    };
    let mut engine = TerminalEngine::new_with_colors(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        colors,
    )?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let capture = output.clone();
    engine.on_pty_write(move |_terminal, bytes| {
        capture
            .lock()
            .expect("pty output lock")
            .extend_from_slice(bytes);
    })?;

    for (code, expected) in [
        (10, b"\x1b]10;rgb:0404/0505/0606\x1b\\".as_slice()),
        (11, b"\x1b]11;rgb:0101/0202/0303\x1b\\".as_slice()),
        (12, b"\x1b]12;rgb:0707/0808/0909\x1b\\".as_slice()),
        (13, b"\x1b]13;rgb:0a0a/0b0b/0c0c\x1b\\".as_slice()),
        (14, b"\x1b]14;rgb:0d0d/0e0e/0f0f\x1b\\".as_slice()),
        (15, b"\x1b]15;rgb:1010/1111/1212\x1b\\".as_slice()),
        (16, b"\x1b]16;rgb:1313/1414/1515\x1b\\".as_slice()),
        (17, b"\x1b]17;rgb:1616/1717/1818\x1b\\".as_slice()),
        (18, b"\x1b]18;rgb:1919/1a1a/1b1b\x1b\\".as_slice()),
        (19, b"\x1b]19;rgb:1c1c/1d1d/1e1e\x1b\\".as_slice()),
    ] {
        engine.write_vt(format!("\x1b]{code};?\x1b\\").as_bytes());
        assert_eq!(take_pty_output(&output), expected, "OSC {code} reply");
    }

    Ok(())
}

#[test]
fn terminal_engine_tracks_non_rendered_xterm_dynamic_color_overrides() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b]13;rgb:01/02/03;rgb:04/05/06;rgb:07/08/09;rgb:0a/0b/0c;rgb:0d/0e/0f;rgb:10/11/12;rgb:13/14/15\x1b\\");

    for (code, expected) in [
        (13, b"\x1b]13;rgb:0101/0202/0303\x1b\\".as_slice()),
        (14, b"\x1b]14;rgb:0404/0505/0606\x1b\\".as_slice()),
        (15, b"\x1b]15;rgb:0707/0808/0909\x1b\\".as_slice()),
        (16, b"\x1b]16;rgb:0a0a/0b0b/0c0c\x1b\\".as_slice()),
        (17, b"\x1b]17;rgb:0d0d/0e0e/0f0f\x1b\\".as_slice()),
        (18, b"\x1b]18;rgb:1010/1111/1212\x1b\\".as_slice()),
        (19, b"\x1b]19;rgb:1313/1414/1515\x1b\\".as_slice()),
    ] {
        engine.write_vt(format!("\x1b]{code};?\x1b\\").as_bytes());
        assert_eq!(
            take_pty_output(&output),
            expected,
            "OSC {code} override reply"
        );
    }

    engine.write_vt(b"\x1b]113\x1b\\\x1b]119\x1b\\");
    engine.write_vt(b"\x1b]13;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]13;rgb:c0c0/caca/f5f5\x1b\\"
    );
    engine.write_vt(b"\x1b]19;?\x1b\\");
    assert_eq!(
        take_pty_output(&output),
        b"\x1b]19;rgb:1a1a/1b1b/2525\x1b\\"
    );

    Ok(())
}
#[test]
fn terminal_engine_reports_color_scheme_when_requested() -> Result<()> {
    for (scheme, expected) in [
        (None, b"".as_slice()),
        (
            Some(libghostty_vt::terminal::ColorScheme::Dark),
            b"\x1b[?997;1n".as_slice(),
        ),
        (
            Some(libghostty_vt::terminal::ColorScheme::Light),
            b"\x1b[?997;2n".as_slice(),
        ),
    ] {
        let (mut engine, output) = captured_pty_engine()?;
        engine.write_vt(b"\x1b[?2031h");
        match scheme {
            Some(scheme) => {
                engine
                    .terminal
                    .on_color_scheme(move |_terminal| Some(scheme))?;
            }
            None => {
                engine.terminal.on_color_scheme(|_terminal| None)?;
            }
        }
        engine.write_vt(b"\x1b[?996n");
        assert_eq!(take_pty_output(&output), expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_reports_production_color_scheme_from_active_background() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b[?2031h\x1b[?996n");
    assert_eq!(take_pty_output(&output), b"\x1b[?997;1n");

    let light_colors = TerminalColorConfig {
        background: RgbColor {
            r: 0xf8,
            g: 0xf8,
            b: 0xf2,
        },
        ..Default::default()
    };
    engine.set_colors(light_colors)?;
    engine.write_vt(b"\x1b[?996n");
    assert_eq!(take_pty_output(&output), b"\x1b[?997;2n");
    Ok(())
}

#[test]
fn terminal_engine_ignores_invalid_titles_and_malformed_sequences() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;
    let title_changes = Arc::new(Mutex::new(0_u32));
    let callback_title_changes = title_changes.clone();
    engine.terminal.on_title_changed(move |_terminal| {
        *callback_title_changes.lock().expect("title lock") += 1;
    })?;

    engine.write_vt(b"\x1b]2;outer\x1b\\");
    assert_eq!(engine.terminal.title()?, "outer");
    engine.write_vt(b"\x1b]2;bad\xc0\x1b\\");
    assert_eq!(engine.terminal.title()?, "outer");
    assert_eq!(*title_changes.lock().expect("title lock"), 1);

    let too_many_params = format!("\x1b[{}C", ["1"; 40].join(";"));
    engine.write_vt(too_many_params.as_bytes());
    engine.write_vt(b"\x1b[111111111111111111111111111111111111111111111111111111111111111111C");
    engine.write_vt(b"\x1bP6;;;;;;;;;;;;;;;;;;pignored\x1b\\");
    assert_eq!(take_pty_output(&output), b"");

    Ok(())
}

#[test]
fn terminal_engine_reports_production_xtversion() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b[>q");

    assert_eq!(
        take_pty_output(&output),
        format!("\x1bP>|{TERMINAL_XTVERSION}\x1b\\").as_bytes()
    );
    Ok(())
}

#[test]
fn terminal_engine_emits_bell_side_effect_for_bel() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x07");

    assert_eq!(take_pty_output(&output), b"");
    assert_eq!(engine.drain_side_effects(), vec![TerminalSideEffect::Bell]);
    Ok(())
}

#[test]
fn terminal_engine_handles_query_side_effects_and_keyboard_reports() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    let bell_count = Arc::new(Mutex::new(0_u32));
    let callback_bells = bell_count.clone();
    engine.terminal.on_bell(move |_terminal| {
        *callback_bells.lock().expect("bell lock") += 1;
    })?;
    engine
        .terminal
        .on_enquiry(|_terminal| Some("bootty-enquiry"))?;
    engine
        .terminal
        .on_xtversion(|_terminal| Some("Bootty 1.0"))?;

    engine.write_vt(b"\x07\x05\x1b[>q");
    assert_eq!(*bell_count.lock().expect("bell lock"), 1);
    assert_eq!(
        take_pty_output(&output),
        b"bootty-enquiry\x1bP>|Bootty 1.0\x1b\\",
    );

    engine.write_vt(b"\x1b[?u");
    assert_eq!(take_pty_output(&output), b"\x1b[?0u");
    engine.write_vt(b"\x1b[>1u\x1b[?u");
    assert_eq!(take_pty_output(&output), b"\x1b[?1u");

    engine.write_vt(b"\x1b]2;My Title\x1b\\");
    assert_eq!(take_pty_output(&output), b"");
    engine.write_vt(b"\x1b[21t");
    assert_eq!(take_pty_output(&output), b"\x1b]lMy Title\x1b\\");

    Ok(())
}
