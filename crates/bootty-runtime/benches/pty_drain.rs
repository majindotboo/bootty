use std::{hint::black_box, time::Duration};

use bootty_runtime::{
    PtyBacklog, drain_pty_backlog,
    geometry::TerminalGeometry,
    terminal_session::{
        DrainStats, should_publish_frame_after_work, sync_output_suppresses_publish,
    },
};
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

fn terminal_engine(cols: u16, rows: u16) -> TerminalEngine {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
    .expect("terminal engine")
}

fn line_payload(lines: usize) -> Vec<u8> {
    (0..lines)
        .map(|index| format!("line {index:05}: bootty pty drain benchmark output\r\n"))
        .collect::<String>()
        .into_bytes()
}

fn chunked_payload(chunk_len: usize, chunks: usize) -> Vec<Vec<u8>> {
    let payload = line_payload((chunk_len * chunks / 48).max(1));
    (0..chunks)
        .map(|index| {
            let start = (index * chunk_len) % payload.len();
            let mut chunk = Vec::with_capacity(chunk_len);
            while chunk.len() < chunk_len {
                let available = (payload.len() - start).min(chunk_len - chunk.len());
                chunk.extend_from_slice(&payload[start..start + available]);
            }
            chunk
        })
        .collect()
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
    let stats = drain_pty_backlog(&mut backlog, |bytes| total += bytes.len());
    black_box(total);
    stats
}

fn drain_to_engine(mut backlog: PtyBacklog) -> (DrainStats, usize) {
    let mut engine = terminal_engine(120, 40);
    let stats = drain_pty_backlog(&mut backlog, |bytes| engine.write_vt(bytes));
    let frame = engine.extract_frame().expect("render frame");
    (stats, frame.text.len())
}

fn bench_backlog_queue(c: &mut Criterion) {
    c.bench_function("pty_backlog_push_64x8k", |b| {
        let chunks = chunked_payload(8 * 1024, 64);
        b.iter(|| black_box(backlog_from_chunks(black_box(&chunks)).len()))
    });

    c.bench_function("pty_backlog_drain_counter_64x8k", |b| {
        let chunks = chunked_payload(8 * 1024, 64);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |backlog| black_box(drain_to_counter(backlog)),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("pty_backlog_drain_counter_1x4mb", |b| {
        let chunks = chunked_payload(4 * 1024 * 1024, 1);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |backlog| black_box(drain_to_counter(backlog)),
            BatchSize::SmallInput,
        )
    });
}

fn bench_engine_drain(c: &mut Criterion) {
    c.bench_function("pty_engine_drain_burst_64x8k", |b| {
        let chunks = chunked_payload(8 * 1024, 64);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |backlog| black_box(drain_to_engine(backlog)),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("pty_engine_drain_large_slice_1x4mb", |b| {
        let chunks = chunked_payload(4 * 1024 * 1024, 1);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |backlog| black_box(drain_to_engine(backlog)),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("pty_engine_backlog_catchup_4mb", |b| {
        let chunks = chunked_payload(512 * 1024, 8);
        b.iter_batched(
            || backlog_from_chunks(&chunks),
            |mut backlog| {
                let mut engine = terminal_engine(180, 80);
                let mut frames = 0_usize;
                let mut bytes = 0_usize;
                while !backlog.is_empty() {
                    let stats = drain_pty_backlog(&mut backlog, |chunk| engine.write_vt(chunk));
                    bytes += stats.bytes;
                    frames += 1;
                }
                let frame = engine.extract_frame().expect("render frame");
                black_box((frames, bytes, frame.text.len()))
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_publish_policy(c: &mut Criterion) {
    c.bench_function("pty_publish_policy_backlog_mixed", |b| {
        let elapsed = [
            Duration::ZERO,
            Duration::from_millis(8),
            Duration::from_millis(16),
            Duration::from_millis(64),
        ];
        b.iter(|| {
            let mut publishes = 0_usize;
            for pending in black_box([0, 4 * 1024, 512 * 1024]) {
                for force in black_box([false, true]) {
                    for sync in black_box([false, true]) {
                        for change_elapsed in black_box(elapsed) {
                            for publish_elapsed in black_box(elapsed) {
                                if should_publish_frame_after_work(
                                    black_box(true),
                                    force,
                                    sync_output_suppresses_publish(sync, change_elapsed),
                                    pending,
                                    change_elapsed,
                                    publish_elapsed,
                                ) {
                                    publishes += 1;
                                }
                            }
                        }
                    }
                }
            }
            black_box(publishes)
        })
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.15);
    targets = bench_backlog_queue, bench_engine_drain, bench_publish_policy
);
criterion_main!(benches);
