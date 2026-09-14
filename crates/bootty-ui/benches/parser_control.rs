use std::hint::black_box;

use anyhow::{Context, Result};
use bootty_terminal::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion};
use libghostty_vt::{
    Terminal,
    render::RenderState,
    terminal::{Mode, Point, PointCoordinate},
};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};

#[derive(Clone, Copy)]
enum Chunking {
    Whole,
    Byte,
    Prime,
}

#[derive(Clone)]
struct ParserWorkload {
    name: &'static str,
    payload: Vec<u8>,
    chunking: Chunking,
}

fn write_terminal_chunks(
    terminal: &mut Terminal<'static, 'static>,
    payload: &[u8],
    chunking: Chunking,
) -> Result<()> {
    match chunking {
        Chunking::Whole => terminal.vt_write(payload),
        Chunking::Byte => {
            for byte in payload {
                terminal.vt_write(std::slice::from_ref(byte));
            }
        }
        Chunking::Prime => {
            let mut offset = 0;
            let chunks = [1, 2, 3, 5, 8, 13, 21];
            let mut chunk_index = 0_usize;
            while offset < payload.len() {
                let len = chunks
                    .get(chunk_index.rem_euclid(chunks.len()))
                    .copied()
                    .unwrap_or_default()
                    .min(payload.len().saturating_sub(offset));
                let end = offset.checked_add(len).context("terminal chunk range")?;
                terminal.vt_write(payload.get(offset..end).context("terminal chunk")?);
                offset = end;
                chunk_index = chunk_index.checked_add(1).context("terminal chunk index")?;
            }
        }
    }
    Ok(())
}

fn write_engine_chunks(
    engine: &mut TerminalEngine,
    payload: &[u8],
    chunking: Chunking,
) -> Result<()> {
    match chunking {
        Chunking::Whole => engine.write_vt(payload),
        Chunking::Byte => {
            for byte in payload {
                engine.write_vt(std::slice::from_ref(byte));
            }
        }
        Chunking::Prime => {
            let mut offset = 0;
            let chunks = [1, 2, 3, 5, 8, 13, 21];
            let mut chunk_index = 0_usize;
            while offset < payload.len() {
                let len = chunks
                    .get(chunk_index.rem_euclid(chunks.len()))
                    .copied()
                    .unwrap_or_default()
                    .min(payload.len().saturating_sub(offset));
                let end = offset.checked_add(len).context("engine chunk range")?;
                engine.write_vt(payload.get(offset..end).context("engine chunk")?);
                offset = end;
                chunk_index = chunk_index.checked_add(1).context("engine chunk index")?;
            }
        }
    }
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn ascii_payload(lines: usize) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(lines.checked_mul(48).context("fixture capacity")?);
    for index in 0..lines {
        payload.extend_from_slice(
            format!("ascii baseline line {index:06} parser payload\r\n").as_bytes(),
        );
    }
    Ok(payload)
}

fn split_utf8_payload(repeats: usize) -> Vec<u8> {
    "utf8 split コンニチハ 🥟 e\u{301} عربى देवनागरी\r\n"
        .repeat(repeats)
        .into_bytes()
}

fn split_csi_payload(repeats: usize) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(repeats.checked_mul(64).context("fixture capacity")?);
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b[{};{}Hsplit-csi-{index:06}\x1b[0K",
                1_usize.saturating_add(index.rem_euclid(40)),
                1_usize.saturating_add(index.rem_euclid(100))
            )
            .as_bytes(),
        );
    }
    Ok(payload)
}

fn sgr_churn_payload(repeats: usize) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(repeats.checked_mul(48).context("fixture capacity")?);
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b[{};{};{}mSGR{index:06}\x1b[0m",
                30_usize.saturating_add(index.rem_euclid(8)),
                40_usize.saturating_add(index.div_euclid(3).rem_euclid(8)),
                if index.rem_euclid(2) == 0 { 1 } else { 22 }
            )
            .as_bytes(),
        );
    }
    Ok(payload)
}

fn truecolor_churn_payload(repeats: usize) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(repeats.checked_mul(72).context("fixture capacity")?);
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}mtrue{index:06}\x1b[0m",
                index.rem_euclid(256),
                index
                    .checked_mul(3)
                    .context("fixture arithmetic")?
                    .rem_euclid(256),
                index
                    .checked_mul(7)
                    .context("fixture arithmetic")?
                    .rem_euclid(256),
                index
                    .checked_mul(11)
                    .context("fixture arithmetic")?
                    .rem_euclid(256),
                index
                    .checked_mul(13)
                    .context("fixture arithmetic")?
                    .rem_euclid(256),
                index
                    .checked_mul(17)
                    .context("fixture arithmetic")?
                    .rem_euclid(256),
            )
            .as_bytes(),
        );
    }
    Ok(payload)
}

fn cursor_walk_payload(repeats: usize) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(repeats.checked_mul(28).context("fixture capacity")?);
    for index in 0..repeats {
        payload.extend_from_slice(match index.rem_euclid(8) {
            0 => b"\x1b[A",
            1 => b"\x1b[B",
            2 => b"\x1b[C",
            3 => b"\x1b[D",
            4 => b"\x1b[5C",
            5 => b"\x1b[3D",
            6 => b"\x1b[2B",
            _ => b"\x1b[1A",
        });
        payload.extend_from_slice(b"x");
    }
    Ok(payload)
}

fn public_light_cells_payload() -> Vec<u8> {
    let mut payload = b"\x1b[?1049h".to_vec();
    for ch in b'A'..=b'Z' {
        payload.extend_from_slice(b"\x1b[H");
        payload.extend(std::iter::repeat_n(
            ch,
            usize::from(GEOMETRY.cols).saturating_mul(usize::from(GEOMETRY.rows)),
        ));
    }
    payload
}

fn public_dense_cells_payload() -> Result<Vec<u8>> {
    let mut payload = b"\x1b[?1049h".to_vec();
    for (offset, ch) in (b'A'..=b'Z').enumerate() {
        let offset = u16::try_from(offset).context("cell offset conversion")?;
        payload.extend_from_slice(b"\x1b[H");
        for line in 1..=GEOMETRY.rows {
            for column in 1..=GEOMETRY.cols {
                let index = line
                    .checked_add(column)
                    .and_then(|value| value.checked_add(offset))
                    .context("cell index arithmetic")?;
                let fg_col = index.rem_euclid(156).saturating_add(100);
                let bg_col = 255_u16
                    .saturating_sub(index.rem_euclid(156))
                    .saturating_add(100);
                payload.extend_from_slice(
                    format!("\x1b[38;5;{fg_col};48;5;{bg_col};1;3;4m{}", char::from(ch)).as_bytes(),
                );
            }
        }
    }
    Ok(payload)
}

fn public_cursor_motion_payload() -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    for ch in b'A'..=b'Z' {
        let mut column_start = 1_u16;
        let mut column_end = GEOMETRY.cols;
        let mut line_start = 1_u16;
        let mut line_end = GEOMETRY.rows;
        loop {
            let mut column = column_start;
            let mut line = line_start;

            while column < column_end {
                payload.extend_from_slice(
                    format!("\x1b[{line};{column}H{}", char::from(ch)).as_bytes(),
                );
                column = column.checked_add(1).context("cursor column arithmetic")?;
            }
            while line < line_end {
                payload.extend_from_slice(
                    format!("\x1b[{line};{column}H{}", char::from(ch)).as_bytes(),
                );
                line = line.checked_add(1).context("cursor line arithmetic")?;
            }
            while column > column_start {
                payload.extend_from_slice(
                    format!("\x1b[{line};{column}H{}", char::from(ch)).as_bytes(),
                );
                column = column.checked_sub(1).context("cursor column arithmetic")?;
            }
            while line > line_start {
                payload.extend_from_slice(
                    format!("\x1b[{line};{column}H{}", char::from(ch)).as_bytes(),
                );
                line = line.checked_sub(1).context("cursor line arithmetic")?;
            }

            column_start = column_start
                .checked_add(1)
                .context("cursor column arithmetic")?;
            line_start = line_start
                .checked_add(1)
                .context("cursor line arithmetic")?;
            column_end = column_end
                .checked_sub(1)
                .context("cursor column arithmetic")?;
            line_end = line_end.checked_sub(1).context("cursor line arithmetic")?;
            if column_start > column_end || line_start > line_end {
                break;
            }
        }
    }
    Ok(payload)
}

fn public_unicode_payload(repeats: usize) -> Vec<u8> {
    let symbols = "¡¢£¤¥¦§¨©ª«¬®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿĀāĂąĆćĈĉĊċČčĎďĐđΓΔΘΛΞΠΣΦΨΩБГДЖЗИЙКЛПФЦЧШЩאבגדהוזחטיךכלםמןנסעףפץצקרשת가각간갈감갑값갓강개객갠갤갬갭갯갱😀😁😂😃😄😅😆😇😈😉😊😋😌😍😎😏";
    symbols.repeat(repeats).into_bytes()
}

fn scroll_insert_erase_payload(repeats: usize) -> Vec<u8> {
    let mut payload = b"\x1b[2J\x1b[1;40r".to_vec();
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b[{};1Hrow{index:06}\x1b[{}@\x1b[{}P\x1b[{}L\x1b[{}M\x1b[2K\x1b[0J",
                1_usize.saturating_add(index.rem_euclid(40)),
                1_usize.saturating_add(index.rem_euclid(4)),
                1_usize.saturating_add(index.rem_euclid(4)),
                1_usize.saturating_add(index.rem_euclid(3)),
                1_usize.saturating_add(index.rem_euclid(3)),
            )
            .as_bytes(),
        );
    }
    payload.extend_from_slice(b"\x1b[r");
    payload
}

fn top_region_scroll_payload(repeats: usize, top_row: u16) -> Vec<u8> {
    let mut payload = format!("\x1b[?1049h\x1b[{top_row};{}r", GEOMETRY.rows).into_bytes();
    for _ in 0..repeats {
        payload.extend_from_slice(b"y\n");
    }
    payload
}

fn rep_tabs_alt_payload(repeats: usize) -> Vec<u8> {
    let mut payload = b"\x1b[?1049h\x1b[2J\x1b[H".to_vec();
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "rep-tabs-{index:06}\t\x1b[{}b\r\n",
                1_usize.saturating_add(index.rem_euclid(16))
            )
            .as_bytes(),
        );
    }
    payload.extend_from_slice(b"\x1b[?1049l");
    payload
}

fn osc_dcs_query_payload(repeats: usize) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(repeats.checked_mul(128).context("fixture capacity")?);
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b]0;title-{index}\x1b\\\x1b]8;id=p{index};https://example.invalid/{index}\x1b\\link\x1b]8;;\x1b\\\x1b]52;c;SGVsbG8=\x1b\\\x1bP$qm\x1b\\\x1b[c\x1b[6n"
            )
            .as_bytes(),
        );
    }
    Ok(payload)
}

fn synchronized_update_payload(repeats: usize) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(repeats.checked_mul(64).context("fixture capacity")?);
    for index in 0..repeats {
        payload
            .extend_from_slice(format!("\x1b[?2026hupdate {index:06}\r\n\x1b[?2026l").as_bytes());
    }
    Ok(payload)
}

fn workloads() -> Result<Vec<ParserWorkload>> {
    Ok(vec![
        ParserWorkload {
            name: "ascii_whole",
            payload: ascii_payload(4_096)?,
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "utf8_byte_split",
            payload: split_utf8_payload(2_048),
            chunking: Chunking::Byte,
        },
        ParserWorkload {
            name: "csi_prime_split",
            payload: split_csi_payload(2_048)?,
            chunking: Chunking::Prime,
        },
        ParserWorkload {
            name: "sgr_churn",
            payload: sgr_churn_payload(4_096)?,
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "truecolor_churn",
            payload: truecolor_churn_payload(2_048)?,
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "cursor_walk",
            payload: cursor_walk_payload(8_192)?,
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "public_light_cells",
            payload: public_light_cells_payload(),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "public_dense_cells",
            payload: public_dense_cells_payload()?,
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "public_cursor_motion",
            payload: public_cursor_motion_payload()?,
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "public_unicode",
            payload: public_unicode_payload(512),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "scroll_insert_erase",
            payload: scroll_insert_erase_payload(1_024),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "scroll_top_region",
            payload: top_region_scroll_payload(8_192, 2),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "scroll_top_small_region",
            payload: top_region_scroll_payload(8_192, GEOMETRY.rows / 2),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "rep_tabs_alt_screen",
            payload: rep_tabs_alt_payload(2_048),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "osc_dcs_query",
            payload: osc_dcs_query_payload(512)?,
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "sync_update",
            payload: synchronized_update_payload(2_048)?,
            chunking: Chunking::Whole,
        },
    ])
}

fn state_terminal() -> Result<Terminal<'static, 'static>> {
    let mut terminal = Terminal::new(GEOMETRY.cols, GEOMETRY.rows).context("terminal")?;
    terminal
        .set_scrollback_max_bytes(Some(0))
        .context("scrollback limit")?;
    Ok(terminal)
}

fn run_parse_state(workload: &ParserWorkload) -> Result<u64> {
    let mut terminal = state_terminal()?;
    write_terminal_chunks(&mut terminal, &workload.payload, workload.chunking)?;
    let mut render_state = RenderState::new().context("render state")?;
    let snapshot = render_state.update(&terminal).context("state snapshot")?;
    let cursor_hash = snapshot
        .cursor_viewport()
        .context("cursor viewport")?
        .map_or(0, |cursor| u64::from(cursor.x) << 32 | u64::from(cursor.y));
    let mode_hash = u64::from(terminal.mode(Mode::WRAPAROUND).context("wrap mode")?);
    let cell_hash = terminal
        .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: 0 }))
        .map_or(0, |_| 0x9e37_79b9_7f4a_7c15);
    Ok(hash_bytes(&workload.payload)
        ^ u64::from(snapshot.cols().context("snapshot cols")?)
        ^ (u64::from(snapshot.rows().context("snapshot rows")?) << 8)
        ^ cursor_hash
        ^ mode_hash
        ^ cell_hash)
}

fn run_full_visible(workload: &ParserWorkload) -> Result<u64> {
    let mut engine = TerminalEngine::new(GEOMETRY).context("terminal engine")?;
    write_engine_chunks(&mut engine, &workload.payload, workload.chunking)?;
    let frame = engine.extract_frame().context("visible parser frame")?;
    let text_hash = frame
        .text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
            (hash ^ u64::from(u32::from(*ch))).wrapping_mul(0x0000_0100_0000_01b3)
        });
    anyhow::ensure!(
        (frame.cols, frame.rows) == (GEOMETRY.cols, GEOMETRY.rows),
        "visible parser frame geometry changed"
    );
    Ok(text_hash
        ^ u64::try_from(frame.cells.len()).context("cell count conversion")?
        ^ (u64::try_from(frame.text.len()).context("text count conversion")? << 32))
}

fn bench_parse_state(c: &mut Criterion) -> Result<()> {
    for workload in workloads()? {
        let mut failure = None;
        c.bench_function(&format!("parser_state_{}", workload.name), |b| {
            b.iter_batched(
                || workload.clone(),
                |workload| match run_parse_state(&workload) {
                    Ok(value) => {
                        black_box(value);
                    }
                    Err(error) => failure = Some(error),
                },
                BatchSize::SmallInput,
            );
        });
        failure.map_or(Ok(()), Err)?;
    }
    Ok(())
}

fn bench_full_visible(c: &mut Criterion) -> Result<()> {
    for workload in workloads()? {
        let mut failure = None;
        c.bench_function(&format!("parser_visible_{}", workload.name), |b| {
            b.iter_batched(
                || workload.clone(),
                |workload| match run_full_visible(&workload) {
                    Ok(value) => {
                        black_box(value);
                    }
                    Err(error) => failure = Some(error),
                },
                BatchSize::SmallInput,
            );
        });
        failure.map_or(Ok(()), Err)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .sample_size(10)
        .noise_threshold(0.20)
        .configure_from_args();
    bench_parse_state(&mut criterion)?;
    bench_full_visible(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
