# Benchmark report

This document records measured Bootty benchmark results. It is separate from
`docs/benchmarking.md`, which defines benchmark process, validation policy,
schemas, and publication rules.

## Report status

This is a local engineering report for PTY sink-throughput work on one macOS
host. It is useful for optimization priorities and regression tracking. It is
not a public terminal ranking: the run does not include correctness gates,
randomized terminal order, visual catch-up timing, input latency, GPU counters,
power, or multi-host repetition.

## Scope

| Field | Value |
| --- | --- |
| Category | PTY sink throughput |
| Tools | `vtebench`, `termbench` |
| Platform | Local macOS developer machine |
| Terminals | Bootty, kitty, Alacritty, WezTerm |
| Fixtures | `dense_cells`, `scrolling_bottom_region`, `unicode` |
| Samples | 7 per tool/terminal/fixture |
| Payload floor | 64 MiB per sample |
| Metric | Mean sample time, milliseconds; lower is better |
| Profile | Harness-normalized 80x24 PTY |
| Bootty config | Temporary benchmark config: 560x588 window calibrated to 80x24 on this host, fullscreen off, sidebar/status chrome off, native scrollback disabled |

All rows in this report recorded `actual_pty_size = "24 80"`. Previous local
runs used mismatched GUI window sizes and produced misleading dense/scroll
resource numbers; do not compare those results to the tables below.

Ghostty is not included in the tables because the macOS `open` adapter launched
successfully on this host but reported `actual_pty_size = "53 263"`; normalized
comparisons require `actual_pty_size = "24 80"`.

## Reproduction command

```bash
cargo build --release -p bootty-app --bin bootty

scripts/run-terminal-public-benchmarks.py \
  --terminal bootty --terminal kitty --terminal alacritty --terminal wezterm \
  --tool vtebench --tool termbench \
  --fixture dense_cells --fixture scrolling_bottom_region --fixture unicode \
  --max-samples 7 \
  --max-secs 10 \
  --min-bytes 67108864 \
  --timeout 300 \
  --resource-sample \
  --output artifacts/external-benchmarks/focused-multiterminal-pty.jsonl

scripts/build-benchmark-dashboard.py \
  artifacts/external-benchmarks/focused-multiterminal-pty.jsonl \
  --output-dir artifacts/benchmark-dashboard-focused-multiterminal-pty \
  --strict
```

`termbench` accepts one benchmark root. The runner materializes a temporary
selected-fixture directory when multiple `--fixture` values are provided.

## Results

### vtebench mean sample time, ms

| Fixture | Bootty | kitty | Alacritty | WezTerm | Best in run | Bootty rank |
| --- | ---: | ---: | ---: | ---: | --- | ---: |
| dense_cells | 340.71 | 593.43 | 268.29 | 585.71 | Alacritty | 2 |
| scrolling_bottom_region | 775.00 | 1626.14 | 629.14 | 4377.67 | Alacritty | 2 |
| unicode | 299.86 | 684.71 | 323.57 | 2339.17 | Bootty | 1 |

### termbench mean sample time, ms

| Fixture | Bootty | kitty | Alacritty | WezTerm | Best in run | Bootty rank |
| --- | ---: | ---: | ---: | ---: | --- | ---: |
| dense_cells | 446.00 | 915.71 | 379.71 | 652.71 | Alacritty | 2 |
| scrolling_bottom_region | 775.29 | 1619.57 | 642.71 | 4239.00 | Alacritty | 2 |
| unicode | 297.14 | 665.14 | 332.71 | 1206.00 | Bootty | 1 |

### Focused process resource samples

These rows come from separate Bootty-vs-Alacritty `termbench` runs for each
fixture. Samples track the primary terminal process with `ps`; CPU can exceed
100% on multicore systems. They are process-level RSS/CPU counters, not GPU
memory or full power accounting.

| Fixture | Terminal | Mean time | Mean CPU | Max CPU | Peak RSS |
| --- | --- | ---: | ---: | ---: | ---: |
| dense_cells | Bootty | 440.86 ms | 55.30% | 70.30% | 161.0 MiB |
| dense_cells | Alacritty | 379.71 ms | 81.26% | 106.20% | 95.3 MiB |
| scrolling_bottom_region | Bootty | 772.14 ms | 127.77% | 154.60% | 370.3 MiB |
| scrolling_bottom_region | Alacritty | 630.14 ms | 82.69% | 105.90% | 95.2 MiB |
| unicode | Bootty | 298.71 ms | 184.81% | 259.90% | 426.4 MiB |
| unicode | Alacritty | 332.43 ms | 125.48% | 184.40% | 274.0 MiB |

## Interpretation

- Bootty leads this local subset on Unicode throughput in both public tools.
- Bootty trails Alacritty by roughly 1.27x on `termbench/dense_cells` and 1.23x
  on `termbench/scrolling_bottom_region` under corrected 80x24 normalization.
- Bootty is faster than kitty and WezTerm on all three focused fixtures in this
  run.
- Bootty still uses more primary-process RSS than Alacritty in each focused
  resource row; bottom-region scrolling has the largest RSS gap in this subset.
- `vtebench` and `termbench` measure producer/PTY sink behavior. They do not
  prove that rendered frames caught up when the producer finished.

## Before publishing competitive claims

Add these gates before using the numbers externally:

- retained raw artifacts and checksums for the focused run;
- correctness-gate status next to each terminal/tool row;
- randomized terminal order and repeated runs on at least one additional host;
- p50/p95/p99 columns from per-sample data;
- visual catch-up timing so PTY sink throughput is not confused with rendering
  completion;
- Ghostty results once the macOS adapter produces a normalized 24x80 PTY.
