use std::{fmt::Write as _, hint::black_box};

use anyhow::{Context as _, Result};
use bootty_terminal::terminal_engine::TerminalEngine;
use bootty_terminal::{
    PtyBacklog, drain_pty_backlog, geometry::TerminalGeometry, terminal_session::DrainStats,
};
use criterion::{BatchSize, Criterion};

const FOUR_MIB_FRAGMENTATIONS: [(&str, usize, usize); 3] = [
    ("4096x1k", 1024, 4096),
    ("512x8k", 8 * 1024, 512),
    ("64x64k", 64 * 1024, 64),
];

fn terminal_engine(cols: u16, rows: u16) -> Result<TerminalEngine> {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
}

fn line_payload(lines: usize) -> Vec<u8> {
    let mut payload = String::new();
    for index in 0..lines {
        let _ = write!(
            payload,
            "line {index:05}: bootty pty drain benchmark output\r\n"
        );
    }
    payload.into_bytes()
}

fn chunked_payload(chunk_len: usize, chunks: usize) -> Vec<Vec<u8>> {
    let payload = line_payload((chunk_len.saturating_mul(chunks) / 48).max(1));
    (0..chunks)
        .map(|index| {
            let start = index
                .saturating_mul(chunk_len)
                .checked_rem(payload.len())
                .unwrap_or(0);
            payload
                .get(start..)
                .unwrap_or_default()
                .iter()
                .copied()
                .cycle()
                .take(chunk_len)
                .collect()
        })
        .collect()
}

fn fragmented_payload(total_len: usize, chunk_len: usize) -> Vec<Vec<u8>> {
    let source = line_payload((total_len / 48).max(1));
    let payload = source
        .iter()
        .copied()
        .cycle()
        .take(total_len)
        .collect::<Vec<_>>();
    payload.chunks(chunk_len).map(<[u8]>::to_vec).collect()
}

fn backlog_from_chunks(chunks: &[Vec<u8>]) -> PtyBacklog {
    let mut backlog = PtyBacklog::with_capacity(chunks.len());
    for chunk in chunks {
        backlog.push_back(chunk.clone());
    }
    backlog
}

fn drain_to_counter(mut backlog: PtyBacklog) -> DrainStats {
    let mut total = 0_usize;
    let stats = drain_pty_backlog(&mut backlog, |bytes| {
        total = total.saturating_add(bytes.len());
    });
    black_box(total);
    stats
}

fn drain_to_engine(mut backlog: PtyBacklog) -> Result<(DrainStats, usize)> {
    let mut engine = terminal_engine(120, 40)?;
    let stats = drain_pty_backlog(&mut backlog, |bytes| engine.write_vt(bytes));
    let frame = engine.extract_frame()?;
    Ok((stats, frame.text.len()))
}

fn catch_up_engine(mut backlog: PtyBacklog) -> Result<(usize, usize, usize, usize)> {
    let mut engine = terminal_engine(180, 80)?;
    let mut drain_slices = 0_usize;
    let mut bytes = 0_usize;
    let mut chunks = 0_usize;
    while !backlog.is_empty() {
        let stats = drain_pty_backlog(&mut backlog, |chunk| engine.write_vt(chunk));
        bytes = bytes.saturating_add(stats.bytes);
        chunks = chunks.saturating_add(stats.chunks);
        drain_slices = drain_slices.saturating_add(1);
    }
    let frame = engine.extract_frame()?;
    Ok((drain_slices, chunks, bytes, frame.text.len()))
}

fn bench_backlog_queue(c: &mut Criterion) {
    c.bench_function("pty_backlog_push_64x8k", |b| {
        let chunks = chunked_payload(8 * 1024, 64);
        b.iter(|| black_box(backlog_from_chunks(black_box(&chunks)).len()));
    });

    c.bench_function("pty_backlog_drain_counter_64x8k", |b| {
        let chunks = chunked_payload(8 * 1024, 64);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |backlog| black_box(drain_to_counter(backlog)),
            BatchSize::SmallInput,
        );
    });

    c.bench_function("pty_backlog_drain_counter_1x4mb", |b| {
        let chunks = chunked_payload(4 * 1024 * 1024, 1);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |backlog| black_box(drain_to_counter(backlog)),
            BatchSize::SmallInput,
        );
    });
}

fn bench_engine_drain(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    c.bench_function("pty_engine_drain_burst_64x8k", |b| {
        let chunks = chunked_payload(8 * 1024, 64);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |backlog| black_box(drain_to_engine(backlog).map_err(|error| failure = Some(error))),
            BatchSize::SmallInput,
        );
    });

    c.bench_function("pty_engine_drain_large_slice_1x4mb", |b| {
        let chunks = chunked_payload(4 * 1024 * 1024, 1);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |backlog| black_box(drain_to_engine(backlog).map_err(|error| failure = Some(error))),
            BatchSize::SmallInput,
        );
    });

    c.bench_function("pty_engine_backlog_catchup_4mb", |b| {
        let chunks = chunked_payload(512 * 1024, 8);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |backlog| black_box(catch_up_engine(backlog).map_err(|error| failure = Some(error))),
            BatchSize::SmallInput,
        );
    });

    for (fragmentation, chunk_len, chunk_count) in FOUR_MIB_FRAGMENTATIONS {
        let name = format!("pty_engine_backlog_catchup_4mb_{fragmentation}");
        let chunks = fragmented_payload(
            chunk_len
                .checked_mul(chunk_count)
                .context("fragmented benchmark size")?,
            chunk_len,
        );
        c.bench_function(&name, |b| {
            b.iter_batched(
                || backlog_from_chunks(&chunks),
                |backlog| {
                    black_box(catch_up_engine(backlog).map_err(|error| failure = Some(error)))
                },
                BatchSize::SmallInput,
            );
        });
    }
    failure.map_or(Ok(()), Err)
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .noise_threshold(0.15)
        .configure_from_args();
    bench_backlog_queue(&mut criterion);
    bench_engine_drain(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
