use std::hint::black_box;

use anyhow::Result;
use bootty_terminal::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion};

#[derive(Clone, Copy)]
enum RemoteKind {
    Ssh,
    Mosh,
    DockerExec,
    PodmanExec,
    Conpty,
}

#[derive(Clone, Copy)]
struct RemoteProfile {
    rtt_ms: u32,
    jitter_ms: u32,
    loss_per_10k: u32,
    reorder_every: Option<usize>,
    kind: RemoteKind,
}

#[derive(Clone)]
struct RemoteChunk {
    ready_ms: u32,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
struct RemoteStats {
    virtual_elapsed_ms: u32,
    key_echo_p95_ms: u32,
    resize_echo_p95_ms: u32,
    delivered_bytes: usize,
    dropped_chunks: usize,
    backlog_high_water: usize,
    feature_degraded: bool,
}

fn terminal_engine(cols: u16, rows: u16) -> Result<TerminalEngine> {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
}

fn profile_latency(profile: RemoteProfile, sequence: usize) -> u32 {
    let base = profile.rtt_ms / 2;
    let jitter = if profile.jitter_ms == 0 {
        0
    } else {
        deterministic_u32(
            u64::try_from(sequence)
                .unwrap_or(u64::MAX)
                .wrapping_add(0x9e37_79b9),
        )
        .checked_rem(profile.jitter_ms.saturating_add(1))
        .unwrap_or(0)
    };
    base.saturating_add(jitter)
}

const fn deterministic_u32(seed: u64) -> u32 {
    let mut state = seed ^ 0xa076_1d64_78bd_642f;
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    let [_, _, a, b, c, d, _, _] = state.to_le_bytes();
    u32::from_le_bytes([a, b, c, d])
}

fn should_drop(profile: RemoteProfile, sequence: usize) -> bool {
    if profile.loss_per_10k == 0 {
        return false;
    }
    deterministic_u32(
        u64::try_from(sequence)
            .unwrap_or(u64::MAX)
            .wrapping_add(0xfeed_beef),
    ) % 10_000
        < profile.loss_per_10k
}

const fn remote_prompt(profile: RemoteProfile) -> &'static str {
    match profile.kind {
        RemoteKind::Ssh => "ssh",
        RemoteKind::Mosh => "mosh",
        RemoteKind::DockerExec => "docker-exec",
        RemoteKind::PodmanExec => "podman-exec",
        RemoteKind::Conpty => "conpty",
    }
}

fn remote_feature_probe(profile: RemoteProfile) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"\x1b[?2026h");
    bytes.extend_from_slice(
        b"\x1b]8;id=remote;https://example.invalid/remote\x1b\\link\x1b]8;;\x1b\\\r\n",
    );
    bytes.extend_from_slice(b"\x1b[38;2;116;199;236mtruecolor probe\x1b[0m\r\n");
    match profile.kind {
        RemoteKind::Mosh => {
            bytes.extend_from_slice(b"graphics disabled over mosh fallback\r\n");
        }
        RemoteKind::Conpty => {
            bytes.extend_from_slice(b"windows conpty clipboard/osc feature fallback\r\n");
        }
        _ => {
            bytes.extend_from_slice(b"\x1b_Ga=T,f=24,i=91,s=1,v=1;////\x1b\\\r\n");
        }
    }
    bytes.extend_from_slice(b"\x1b[?2026l");
    bytes
}

fn key_echo_chunks(profile: RemoteProfile, keys: usize) -> Vec<RemoteChunk> {
    let mut chunks = Vec::with_capacity(keys.saturating_add(2));
    chunks.push(RemoteChunk {
        ready_ms: profile_latency(profile, 0),
        bytes: format!("{} remote shell ready\r\n", remote_prompt(profile)).into_bytes(),
    });
    for sequence in 0..keys {
        if should_drop(profile, sequence) {
            continue;
        }
        let sequence_u32 = u32::try_from(sequence).unwrap_or(u32::MAX);
        let ready_ms = sequence_u32
            .saturating_mul(8)
            .saturating_add(profile_latency(profile, sequence.saturating_add(1)));
        let bytes = format!(
            "\x1b[32mkey-{sequence:03}\x1b[0m echo from {}\r\n",
            remote_prompt(profile)
        )
        .into_bytes();
        chunks.push(RemoteChunk { ready_ms, bytes });
    }
    chunks.push(RemoteChunk {
        ready_ms: u32::try_from(keys)
            .unwrap_or(u32::MAX)
            .saturating_mul(8)
            .saturating_add(profile.rtt_ms)
            .saturating_add(profile.jitter_ms),
        bytes: remote_feature_probe(profile),
    });
    maybe_reorder(profile, chunks)
}

fn remote_size(sequence: usize) -> (u16, u16) {
    (
        80_u16.saturating_add(u16::try_from(sequence % 5).unwrap_or(0).saturating_mul(20)),
        24_u16.saturating_add(u16::try_from(sequence % 4).unwrap_or(0).saturating_mul(8)),
    )
}

fn burst_chunks(profile: RemoteProfile, lines: usize, chunk_lines: usize) -> Vec<RemoteChunk> {
    let mut chunks = Vec::with_capacity(lines.div_ceil(chunk_lines));
    let mut line = 0_usize;
    let mut sequence = 0_usize;
    let prompt = remote_prompt(profile);
    let repeated_payload = "payload ".repeat(6);
    while line < lines {
        let mut bytes = Vec::with_capacity(chunk_lines.saturating_mul(96));
        for _ in 0..chunk_lines {
            if line >= lines {
                break;
            }
            bytes.extend_from_slice(
                format!(
                    "{prompt} log line {line:05}: cargo/test/kubectl stream {repeated_payload}\r\n"
                )
                .as_bytes(),
            );
            line = line.saturating_add(1);
        }
        if !should_drop(profile, sequence) {
            chunks.push(RemoteChunk {
                ready_ms: u32::try_from(sequence)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(3)
                    .saturating_add(profile_latency(profile, sequence)),
                bytes,
            });
        }
        sequence = sequence.saturating_add(1);
    }
    maybe_reorder(profile, chunks)
}

fn resize_chunks(profile: RemoteProfile, count: usize) -> Vec<RemoteChunk> {
    let mut chunks = Vec::with_capacity(count);
    for sequence in 0..count {
        if should_drop(profile, sequence) {
            continue;
        }
        let (cols, rows) = remote_size(sequence);
        let ready_ms = u32::try_from(sequence)
            .unwrap_or(u32::MAX)
            .saturating_mul(16)
            .saturating_add(profile_latency(profile, sequence));
        let bytes =
            format!("\x1b[8;{rows};{cols}tremote resize ack {cols}x{rows}\r\n").into_bytes();
        chunks.push(RemoteChunk { ready_ms, bytes });
    }
    maybe_reorder(profile, chunks)
}

fn maybe_reorder(profile: RemoteProfile, mut chunks: Vec<RemoteChunk>) -> Vec<RemoteChunk> {
    if let Some(every) = profile.reorder_every {
        for index in (1..chunks.len()).step_by(every) {
            chunks.swap(index.saturating_sub(1), index);
        }
    }
    chunks.sort_by_key(|chunk| chunk.ready_ms);
    chunks
}

fn replay_chunks(engine: &mut TerminalEngine, chunks: &[RemoteChunk]) -> Result<RemoteStats> {
    let mut stats = RemoteStats::default();
    let mut last_ready = 0_u32;
    for chunk in chunks {
        stats.backlog_high_water = stats.backlog_high_water.max(chunk.bytes.len());
        engine.write_vt(&chunk.bytes);
        stats.delivered_bytes = stats.delivered_bytes.saturating_add(chunk.bytes.len());
        last_ready = last_ready.max(chunk.ready_ms);
    }
    let frame = engine.extract_frame()?;
    stats.virtual_elapsed_ms = last_ready;
    stats.feature_degraded = frame
        .text
        .windows(8)
        .any(|window| window == ['f', 'a', 'l', 'l', 'b', 'a', 'c', 'k']);
    black_box((
        frame.cells.len(),
        frame.text.len(),
        frame.images.placements.len(),
    ));
    Ok(stats)
}

fn replay_key_echo(profile: RemoteProfile, keys: usize) -> Result<RemoteStats> {
    let chunks = key_echo_chunks(profile, keys);
    let mut engine = terminal_engine(120, 40)?;
    let mut stats = replay_chunks(&mut engine, &chunks)?;
    stats.key_echo_p95_ms =
        profile_latency(profile, keys.saturating_mul(95) / 100).saturating_mul(2);
    stats.dropped_chunks = keys.saturating_sub(chunks.len().saturating_sub(2));
    Ok(stats)
}

fn replay_output_burst(
    profile: RemoteProfile,
    lines: usize,
    chunk_lines: usize,
) -> Result<RemoteStats> {
    let chunks = burst_chunks(profile, lines, chunk_lines);
    let mut engine = terminal_engine(180, 60)?;
    let mut stats = replay_chunks(&mut engine, &chunks)?;
    stats.dropped_chunks = lines.div_ceil(chunk_lines).saturating_sub(chunks.len());
    Ok(stats)
}

fn replay_resize(profile: RemoteProfile, count: usize) -> Result<RemoteStats> {
    let chunks = resize_chunks(profile, count);
    let mut engine = terminal_engine(120, 40)?;
    for sequence in 0..count {
        let (cols, rows) = remote_size(sequence);
        engine.resize(TerminalGeometry {
            cols,
            rows,
            cell_width: 9,
            cell_height: 22,
        })?;
    }
    let mut stats = replay_chunks(&mut engine, &chunks)?;
    stats.resize_echo_p95_ms =
        profile_latency(profile, count.saturating_mul(95) / 100).saturating_mul(2);
    stats.dropped_chunks = count.saturating_sub(chunks.len());
    Ok(stats)
}

const fn localhost_ssh() -> RemoteProfile {
    RemoteProfile {
        rtt_ms: 1,
        jitter_ms: 0,
        loss_per_10k: 0,
        reorder_every: None,
        kind: RemoteKind::Ssh,
    }
}

const fn lan_ssh() -> RemoteProfile {
    RemoteProfile {
        rtt_ms: 4,
        jitter_ms: 1,
        loss_per_10k: 0,
        reorder_every: None,
        kind: RemoteKind::Ssh,
    }
}

const fn wan_ssh(rtt_ms: u32) -> RemoteProfile {
    RemoteProfile {
        rtt_ms,
        jitter_ms: rtt_ms / 10,
        loss_per_10k: 0,
        reorder_every: None,
        kind: RemoteKind::Ssh,
    }
}

const fn lossy_ssh(loss_per_10k: u32) -> RemoteProfile {
    RemoteProfile {
        rtt_ms: 100,
        jitter_ms: 12,
        loss_per_10k,
        reorder_every: Some(9),
        kind: RemoteKind::Ssh,
    }
}

const fn mosh_profile() -> RemoteProfile {
    RemoteProfile {
        rtt_ms: 100,
        jitter_ms: 35,
        loss_per_10k: 100,
        reorder_every: Some(5),
        kind: RemoteKind::Mosh,
    }
}

const fn docker_exec_profile(kind: RemoteKind) -> RemoteProfile {
    RemoteProfile {
        rtt_ms: 2,
        jitter_ms: 1,
        loss_per_10k: 0,
        reorder_every: None,
        kind,
    }
}

const fn conpty_profile() -> RemoteProfile {
    RemoteProfile {
        rtt_ms: 6,
        jitter_ms: 2,
        loss_per_10k: 0,
        reorder_every: None,
        kind: RemoteKind::Conpty,
    }
}

fn bench_key_echo_latency(c: &mut Criterion) -> Result<()> {
    for (name, profile) in [
        ("localhost_ssh", localhost_ssh()),
        ("lan_ssh", lan_ssh()),
        ("wan_20ms_ssh", wan_ssh(20)),
        ("wan_100ms_ssh", wan_ssh(100)),
        ("wan_200ms_ssh", wan_ssh(200)),
        ("mosh_100ms_lossy", mosh_profile()),
    ] {
        let mut failure = None;
        c.bench_function(&format!("remote_{name}_key_echo_replay_128"), |b| {
            b.iter(|| {
                black_box(replay_key_echo(black_box(profile), 128).map_err(|error| {
                    failure = Some(error);
                }))
            });
        });
        if let Some(error) = failure {
            return Err(error);
        }
    }
    Ok(())
}

fn bench_output_burst_handling(c: &mut Criterion) -> Result<()> {
    for (name, profile) in [
        ("localhost_ssh", localhost_ssh()),
        ("wan_100ms_ssh", wan_ssh(100)),
        ("loss_0_1pct_ssh", lossy_ssh(10)),
        ("loss_1pct_ssh", lossy_ssh(100)),
        ("loss_3pct_ssh", lossy_ssh(300)),
        ("mosh_100ms_lossy", mosh_profile()),
        ("docker_exec", docker_exec_profile(RemoteKind::DockerExec)),
        ("podman_exec", docker_exec_profile(RemoteKind::PodmanExec)),
    ] {
        let mut failure = None;
        c.bench_function(&format!("remote_{name}_output_burst_4096_lines"), |b| {
            b.iter(|| {
                black_box(
                    replay_output_burst(black_box(profile), 4096, 16).map_err(|error| {
                        failure = Some(error);
                    }),
                )
            });
        });
        if let Some(error) = failure {
            return Err(error);
        }
    }
    Ok(())
}

fn bench_resize_and_feature_degradation(c: &mut Criterion) -> Result<()> {
    for (name, profile) in [
        ("lan_ssh", lan_ssh()),
        ("wan_200ms_ssh", wan_ssh(200)),
        ("mosh_100ms_lossy", mosh_profile()),
        ("conpty", conpty_profile()),
    ] {
        let mut failure = None;
        c.bench_function(&format!("remote_{name}_resize_propagation_64"), |b| {
            b.iter(|| {
                black_box(replay_resize(black_box(profile), 64).map_err(|error| {
                    failure = Some(error);
                }))
            });
        });
        if let Some(error) = failure {
            return Err(error);
        }
    }

    let mut failure = None;
    c.bench_function("remote_feature_degradation_matrix", |b| {
        let profiles = [localhost_ssh(), mosh_profile(), conpty_profile()];
        b.iter_batched(
            || profiles,
            |profiles| {
                let mut degraded = 0_usize;
                let result = (|| -> Result<usize> {
                    for profile in profiles {
                        if replay_key_echo(black_box(profile), 8)?.feature_degraded {
                            degraded = degraded.saturating_add(1);
                        }
                    }
                    Ok(black_box(degraded))
                })();
                black_box(result.map_err(|error| failure = Some(error)))
            },
            BatchSize::SmallInput,
        );
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .noise_threshold(0.15)
        .configure_from_args();
    bench_key_echo_latency(&mut criterion)?;
    bench_output_burst_handling(&mut criterion)?;
    bench_resize_and_feature_degradation(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
