use std::{hint::black_box, ops::BitXor};

use anyhow::{Context, Result};
use bootty_terminal::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion};

fn terminal_engine(cols: u16, rows: u16) -> Result<TerminalEngine> {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
    .context("terminal engine")
}

fn deterministic_bytes(len: usize, seed: u64) -> Result<Vec<u8>> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push(u8::try_from(state & 0xff).context("random byte conversion")?);
    }
    Ok(out)
}

fn invalid_utf8_payload(len: usize) -> Result<Vec<u8>> {
    let mut bytes = deterministic_bytes(len, 0x5eed_f00d)?;
    for index in (0..bytes.len()).step_by(17) {
        *bytes
            .get_mut(index)
            .context("invalid UTF-8 fixture index")? = 0xff;
    }
    for index in (7..bytes.len()).step_by(29) {
        *bytes
            .get_mut(index)
            .context("invalid UTF-8 fixture index")? = 0xc0;
    }
    Ok(bytes)
}

fn grammar_biased_escape_storm(commands: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(commands.checked_mul(24).context("fixture capacity")?);
    let fragments: [&[u8]; 14] = [
        b"\x1b[38;2;255;0;0m",
        b"\x1b[48;5;123m",
        b"\x1b[?25l",
        b"\x1b[?25h",
        b"\x1b[2J",
        b"\x1b[H",
        b"\x1b[999;999H",
        b"\x1b[1;1r",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[?2026h",
        b"\x1b[?2026l",
        b"text payload ",
        b"\r\n",
    ];
    for index in 0..commands {
        bytes.extend_from_slice(
            fragments
                .get(index.rem_euclid(fragments.len()))
                .context("escape fixture fragment")?,
        );
        if index.rem_euclid(11) == 0 {
            bytes.extend_from_slice(b"\x1b[");
        }
    }
    Ok(bytes)
}

fn query_storm(commands: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(commands.checked_mul(16).context("fixture capacity")?);
    let fragments: [&[u8]; 6] = [
        b"\x1b[c",
        b"\x1b[0c",
        b"\x1b[5n",
        b"\x1b[6n",
        b"\x1bP$qm\x1b\\",
        b"\x1bP$q q\x1b\\",
    ];
    for index in 0..commands {
        bytes.extend_from_slice(
            fragments
                .get(index.rem_euclid(fragments.len()))
                .context("query fixture fragment")?,
        );
    }
    Ok(bytes)
}

fn reset_storm(commands: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(commands.checked_mul(10).context("fixture capacity")?);
    for index in 0..commands {
        if index.rem_euclid(2) == 0 {
            bytes.extend_from_slice(b"\x1bc");
        } else {
            bytes.extend_from_slice(b"\x1b[!p");
        }
        bytes.extend_from_slice(b"ok\r\n");
    }
    Ok(bytes)
}

fn unterminated_osc_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1b]52;c;".to_vec();
    bytes.extend(std::iter::repeat_n(b'A', len));
    bytes
}

fn huge_clipboard_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1b]52;c;".to_vec();
    bytes.extend(std::iter::repeat_n(b'Q', len));
    bytes.extend_from_slice(b"\x1b\\");
    bytes
}

fn huge_hyperlink_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1b]8;id=hostile;https://example.invalid/".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', len));
    bytes.extend_from_slice(b"\x1b\\linked text\x1b]8;;\x1b\\");
    bytes
}

fn unterminated_dcs_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1bP".to_vec();
    bytes.extend(std::iter::repeat_n(b'q', len));
    bytes
}

fn malformed_kitty_graphics_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1b_Ga=T,f=100,i=9999,s=2048,v=2048;".to_vec();
    bytes.extend(std::iter::repeat_n(b'!', len));
    bytes.extend_from_slice(b"\x1b\\");
    bytes
}

fn malformed_sixel_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1bPq\"1;1;2048;2048#1;2;255;0;0".to_vec();
    bytes.extend(std::iter::repeat_n(b'?', len));
    bytes.extend_from_slice(b"\x1b\\");
    bytes
}

fn long_line_payload(len: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(len.checked_add(3).context("fixture capacity")?);
    bytes.extend(std::iter::repeat_n(b'L', len));
    bytes.extend_from_slice(b"\r\n");
    Ok(bytes)
}

fn image_quota_abuse_payload(images: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(images.checked_mul(96).context("fixture capacity")?);
    for image_id in 0..images {
        bytes.extend_from_slice(
            format!("\x1b_Ga=T,t=d,f=24,i={image_id},p={image_id},s=1,v=1,q=1;////\x1b\\")
                .as_bytes(),
        );
        if image_id.rem_euclid(17) == 0 {
            bytes.extend_from_slice(format!("\x1b_Ga=d,d=i,i={image_id}\x1b\\").as_bytes());
        }
    }
    Ok(bytes)
}

fn nested_sync_reset_payload(rounds: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(rounds.checked_mul(32).context("fixture capacity")?);
    for round in 0..rounds {
        bytes.extend_from_slice(b"\x1b[?2026h");
        if round.rem_euclid(3) == 0 {
            bytes.extend_from_slice(b"\x1b[?2026h");
        }
        bytes.extend_from_slice(format!("sync round {round}\r\n").as_bytes());
        if round.rem_euclid(5) == 0 {
            bytes.extend_from_slice(b"\x1b[!p");
        }
        bytes.extend_from_slice(b"\x1b[?2026l");
    }
    Ok(bytes)
}

fn grammar_biased_fuzz_payload(len: usize) -> Result<Vec<u8>> {
    let atoms: [&[u8]; 16] = [
        b"\x1b[",
        b"\x1b]",
        b"\x1bP",
        b"\x1b_G",
        b"999999999",
        b";",
        b"?2026h",
        b"?1049l",
        b"38;2;1;2;3m",
        b"\x1b\\",
        b"\x07",
        b"\r\n",
        b"payload",
        b"\xff\xc0\x80",
        b"\x1bc",
        b"\x1b[!p",
    ];
    let mut out = Vec::with_capacity(len);
    let mut state = 0x0ddc_0ffe_u64;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let index = usize::try_from(state).context("fuzz fixture index conversion")?;
        out.extend_from_slice(
            atoms
                .get(index.rem_euclid(atoms.len()))
                .context("fuzz fixture atom")?,
        );
    }
    out.truncate(len);
    Ok(out)
}

fn mixed_hostile_corpus() -> Result<Vec<Vec<u8>>> {
    Ok(vec![
        invalid_utf8_payload(16 * 1024)?,
        grammar_biased_escape_storm(512)?,
        query_storm(512)?,
        reset_storm(128)?,
        unterminated_osc_payload(16 * 1024),
        unterminated_dcs_payload(16 * 1024),
        malformed_kitty_graphics_payload(16 * 1024),
        huge_hyperlink_payload(16 * 1024),
        huge_clipboard_payload(16 * 1024),
        malformed_sixel_payload(16 * 1024),
    ])
}

fn extended_hostile_corpus() -> Result<Vec<Vec<u8>>> {
    let mut corpus = mixed_hostile_corpus()?;
    corpus.extend([
        image_quota_abuse_payload(512)?,
        nested_sync_reset_payload(512)?,
        grammar_biased_fuzz_payload(128 * 1024)?,
        long_line_payload(2 * 1024 * 1024)?,
    ]);
    Ok(corpus)
}

fn write_and_extract(mut engine: TerminalEngine, payload: &[u8]) -> Result<usize> {
    engine.write_vt(payload);
    let frame = engine
        .extract_frame()
        .context("extract frame after hostile input")?;
    black_box((
        frame.cells.len(),
        frame.text.len(),
        frame.images.placements.len(),
    ));
    Ok(frame.text.len())
}

fn write_reset_and_extract(mut engine: TerminalEngine, payload: &[u8]) -> Result<(usize, usize)> {
    engine.write_vt(payload);
    engine.write_vt(b"\x1bcafter hostile reset\r\n");
    let frame = engine
        .extract_frame()
        .context("extract frame after reset")?;
    Ok((frame.cells.len(), frame.text.len()))
}

fn bench_extract(c: &mut Criterion, name: &str, payload: &[u8]) -> Result<()> {
    let mut failure = None;
    c.bench_function(name, |b| {
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| match engine.and_then(|engine| write_and_extract(engine, black_box(payload))) {
                Ok(value) => {
                    black_box(value);
                }
                Err(error) => failure = Some(error),
            },
            BatchSize::SmallInput,
        );
    });
    failure.map_or(Ok(()), Err)
}

fn bench_write(c: &mut Criterion, name: &str, payload: &[u8]) -> Result<()> {
    let mut failure = None;
    c.bench_function(name, |b| {
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| match engine {
                Ok(mut engine) => {
                    engine.write_vt(black_box(payload));
                    black_box(engine);
                }
                Err(error) => failure = Some(error),
            },
            BatchSize::SmallInput,
        );
    });
    failure.map_or(Ok(()), Err)
}

fn bench_soak(c: &mut Criterion, name: &str, corpus: &[Vec<u8>]) -> Result<()> {
    let mut failure = None;
    c.bench_function(name, |b| {
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| {
                let result = (|| -> Result<_> {
                    let mut engine = engine?;
                    for round in 0_usize..256 {
                        let payload = corpus
                            .get(round.rem_euclid(corpus.len()))
                            .context("soak corpus entry")?;
                        engine.write_vt(black_box(payload));
                        if round.rem_euclid(31) == 0 {
                            engine.write_vt(b"\x1b[!psoak checkpoint\r\n");
                        }
                    }
                    let frame = engine.extract_frame().context("extract frame after soak")?;
                    Ok((
                        frame.cells.len(),
                        frame.text.len(),
                        frame.images.placements.len(),
                    ))
                })();
                match result {
                    Ok(value) => {
                        black_box(value);
                    }
                    Err(error) => failure = Some(error),
                }
            },
            BatchSize::SmallInput,
        );
    });
    failure.map_or(Ok(()), Err)
}

fn bench_recovery(c: &mut Criterion, name: &str, corpus: &[u8]) -> Result<()> {
    let mut failure = None;
    c.bench_function(name, |b| {
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| match engine
                .and_then(|engine| write_reset_and_extract(engine, black_box(corpus)))
            {
                Ok(value) => {
                    black_box(value);
                }
                Err(error) => failure = Some(error),
            },
            BatchSize::SmallInput,
        );
    });
    failure.map_or(Ok(()), Err)
}

fn bench_ladder(c: &mut Criterion, name: &str, corpus: &[Vec<u8>]) -> Result<()> {
    let mut failure = None;
    c.bench_function(name, |b| {
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| {
                let result =
                    (|| -> Result<_> {
                        let mut engine = engine?;
                        let mut checksum = 0_usize;
                        for payload in corpus {
                            engine.write_vt(black_box(payload));
                            engine.write_vt(b"\x1bcafter hostile step\r\n");
                            let frame = engine
                                .extract_frame()
                                .context("recover after hostile step")?;
                            checksum =
                                checksum.bitxor(frame.cells.len().bitxor(
                                    frame.text.len().bitxor(frame.images.placements.len()),
                                ));
                        }
                        Ok(checksum)
                    })();
                match result {
                    Ok(value) => {
                        black_box(value);
                    }
                    Err(error) => failure = Some(error),
                }
            },
            BatchSize::SmallInput,
        );
    });
    failure.map_or(Ok(()), Err)
}

fn register_extract(criterion: &mut Criterion, name: &str, payload: Result<Vec<u8>>) -> Result<()> {
    let payload = payload?;
    bench_extract(criterion, name, &payload)
}

fn register_core_benches(criterion: &mut Criterion) -> Result<()> {
    register_extract(
        criterion,
        "hostile_invalid_utf8_256kb_extract",
        invalid_utf8_payload(256 * 1024),
    )?;
    register_extract(
        criterion,
        "hostile_random_bytes_256kb_extract",
        deterministic_bytes(256 * 1024, 0xfeed_face_cafe_babe),
    )?;
    register_extract(
        criterion,
        "hostile_grammar_escape_storm_4096_extract",
        grammar_biased_escape_storm(4096),
    )?;
    register_extract(
        criterion,
        "hostile_query_storm_4096_extract",
        query_storm(4096),
    )?;
    register_extract(
        criterion,
        "hostile_reset_storm_1024_extract",
        reset_storm(1024),
    )?;
    Ok(())
}

fn register_control_benches(criterion: &mut Criterion) -> Result<()> {
    register_extract(
        criterion,
        "hostile_unterminated_osc_256kb_extract",
        Ok(unterminated_osc_payload(256 * 1024)),
    )?;
    register_extract(
        criterion,
        "hostile_unterminated_dcs_256kb_extract",
        Ok(unterminated_dcs_payload(256 * 1024)),
    )?;
    register_extract(
        criterion,
        "hostile_osc8_hyperlink_256kb_extract",
        Ok(huge_hyperlink_payload(256 * 1024)),
    )?;
    register_extract(
        criterion,
        "hostile_osc52_clipboard_256kb_extract",
        Ok(huge_clipboard_payload(256 * 1024)),
    )?;
    register_extract(
        criterion,
        "hostile_malformed_kitty_graphics_256kb_extract",
        Ok(malformed_kitty_graphics_payload(256 * 1024)),
    )?;
    register_extract(
        criterion,
        "hostile_malformed_sixel_256kb_extract",
        Ok(malformed_sixel_payload(256 * 1024)),
    )?;
    Ok(())
}

fn register_recovery_benches(criterion: &mut Criterion) -> Result<()> {
    register_extract(
        criterion,
        "hostile_long_line_1mb_extract",
        long_line_payload(1024 * 1024),
    )?;
    let long_line = long_line_payload(16 * 1024 * 1024)?;
    bench_write(criterion, "hostile_long_line_16mb_write", &long_line)?;
    let corpus = mixed_hostile_corpus()?;
    bench_soak(criterion, "hostile_mixed_soak_256_rounds", &corpus)?;
    let recovery = corpus.concat();
    bench_recovery(criterion, "hostile_recovery_after_mixed_corpus", &recovery)?;
    Ok(())
}

fn register_extended_benches(criterion: &mut Criterion) -> Result<()> {
    register_extract(
        criterion,
        "hostile_grammar_biased_fuzz_512kb_extract",
        grammar_biased_fuzz_payload(512 * 1024),
    )?;
    register_extract(
        criterion,
        "hostile_image_quota_abuse_4096_extract",
        image_quota_abuse_payload(4096),
    )?;
    register_extract(
        criterion,
        "hostile_nested_sync_reset_4096_extract",
        nested_sync_reset_payload(4096),
    )?;
    let corpus = extended_hostile_corpus()?;
    bench_ladder(criterion, "hostile_extended_recovery_ladder", &corpus)
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .noise_threshold(0.15)
        .configure_from_args();
    register_core_benches(&mut criterion)?;
    register_control_benches(&mut criterion)?;
    register_recovery_benches(&mut criterion)?;
    register_extended_benches(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
