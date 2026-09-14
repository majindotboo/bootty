# Benchmarking process

Bootty keeps routine validation fast while preserving benchmark seams for deeper
performance work. This document is the stable process guide for benchmark use,
claim hygiene, result schemas, and host-specific evidence collection. Local
measurements and hillclimb findings are vault research artifacts.

## Runtime guardrails

- PTY drain work must stay bounded so large output bursts backlog and catch up
  instead of monopolizing UI frames.
- Frame extraction and paint planning should remain below an interactive frame
  budget for ordinary prompt usage.
- Text run grouping should keep run counts much lower than visible cell counts
  for normal shell output.
- Idle frames should avoid terminal work unless terminal state, cursor blink, or
  chrome repaint state changed.
- Rendering changes must preserve visual parity while separating CPU scene construction,
  GPU submission, and presentation costs.
- Startup, PTY, parser, renderer, input, image, and app-frame changes must keep
  benchmark targets compileable without making the default validation gate run
  measured Criterion suites.

## Routine validation policy

Default local validation compiles only the core paint-planning benchmark harness:

```bash
cargo test -p bootty-ui --bench pipeline_resources --no-run
```

CI keeps the blocking benchmark gate deliberately small: it validates benchmark
metadata/dashboard helpers and compile-checks the core `pipeline_resources` harness.
Local repro:

```bash
mise run bench -- --ci-smoke --output artifacts/benchmark-reproduction/ci
```

The full benchmark reproduction suite runs in the nightly benchmark workflow.
It compile-checks all checked-in benchmark targets and runs the short measured
subset that used to block push CI:

```bash
mise run bench -- --quick --output artifacts/benchmark-reproduction/overnight
```

Run task-specific compile-only checks when touching those surfaces. Keep measured
Criterion suites out of the default local gate unless validation policy changes
explicitly.

CI checks portable crates for Linux arm64 and Windows GNU using the Ubuntu
runner's cross compilers and the project's Zig toolchain. Full GUI/app coverage
runs on native Linux, macOS, and Windows runners with their platform libraries.

| Surface | Compile-only check |
| --- | --- |
| Startup/config/session-order | `cargo test -p bootty-ui --bench startup_config --no-run` |
| Graphics protocols beyond Kitty | `cargo test -p bootty-ui --bench graphics_protocols --no-run` |
| Paint planning and pipeline resources | `cargo test -p bootty-ui --bench pipeline_resources --no-run` |
| PTY drain/backpressure | `cargo test -p bootty-terminal --bench pty_drain --no-run` |
| Flood responsiveness | `cargo test -p bootty-terminal --bench flood_response --no-run` |
| Resize/reflow | `cargo test -p bootty-ui --bench resize_reflow --no-run` |
| Scrollback memory/search/copy/clear | `cargo test -p bootty-ui --bench scrollback --no-run` |
| Parser/control sequences | `cargo test -p bootty-ui --bench parser_control --no-run` |
| Render throughput/frame pacing | `cargo test -p bootty-ui --bench render_pacing --no-run` |
| GPUI terminal scene preparation | `cargo test -p bootty-ui --bench gpui_terminal_scene --no-run` |
| Idle overhead/wakeups/memory/power | `cargo test -p bootty-ui --bench idle_overhead --no-run` |
| Power-sensitive render workload models | `cargo test -p bootty-ui --bench power_thermal --no-run` |
| Keyboard/mouse/paste/clipboard/IME protocols | `cargo test -p bootty-ui --bench input_protocols --no-run` |
| Hostile input/recovery | `cargo test -p bootty-ui --bench hostile_input --no-run` |
| Panes/tabs/multi-window | `cargo test -p bootty-ui --bench panes_multiwindow --no-run` |
| Multiplexer performance/passthrough | `cargo test -p bootty-ui --bench multiplexer --no-run` |
| Remote session replay | `cargo test -p bootty-ui --bench remote_session --no-run` |
| Real application replay | `cargo test -p bootty-ui --bench real_app_replay --no-run` |

## Measured runs

Use measured Criterion runs only while investigating or validating the relevant
surface. For quick local comparisons, prefer short runs such as:

```bash
cargo bench -p bootty-ui --bench <target> -- --sample-size 10 --measurement-time 0.2 --warm-up-time 0.1
```

Use longer runs, raw result exports, repeated randomized runs, and correctness
status before making competitive claims.

Set `BOOTTY_BENCH_FRAME_CAPTURE=/absolute/path/frame.png` when running
`gpui_terminal_scene` to save its cached terminal frame for visual comparison.
The capture runs outside the timed iterations.

## Benchmark target map

| Target | Use when changing |
| --- | --- |
| `startup_config` | config loading, theme resolution, keybind construction, font-size preference writes, and SQLite-backed session ordering |
| `graphics_protocols` | iTerm2 image OSC, Sixel, Unicode/block fallback, unsupported-feature accounting, and text/image render command preparation |
| `pty_drain` | PTY reader queueing, bounded drain slices, burst/catch-up VT writes, backlog policy, and frame publication cadence |
| `flood_response` | deterministic flood replays, visible Ctrl-C/input/scroll injection, and live PTY Ctrl-C-to-child-exit latency |
| `resize_reflow` | fixed/random resize cycles, drag model, HiDPI/monitor moves, fullscreen toggles, main-screen reflow, alternate screen, scrollback, and image-adjacent content |
| `scrollback` | append/memory snapshots, bounded/native scrollback budgets, search/copy, clear/reclaim, and reflow |
| `parser_control` | direct parser/state update and full visible frame modes for ASCII, split UTF-8/CSI, SGR/truecolor, cursor motion, scroll margins, insert/delete, erase, OSC/DCS/query storms, and synchronized updates |
| `render_pacing` | CPU-side pacing model for cursor-only, single-cell, statusline, row/column, random cells, full repaint, scroll, alternate screen, and target Hz budgets |
| `gpui_terminal_scene` | CPU-side terminal scene preparation and glyph-cache reuse for cold, warm rebuilt, and scrolling frames, plus full GPUI window draws for changing frames, cursor blinks, and unchanged cached-scene replay. The full draw reports terminal prepaint/paint CPU, glyph/image primitive counts, cache create/hit/retire counts, and GPUI dirty-to-platform-submit latency. On macOS the benchmark uses GPUI's Metal headless renderer; other platforms currently discard the scene at submission. None of these timings include display scanout. |
| `idle_overhead` | idle tick/repaint models for prompts, tabs, panes, ligatures, IME preedit, shell integration, and notifications |
| `power_thermal` | modeled idle/typing/editor/flood/animation render workloads for power-sensitive profiling; pair with external telemetry for real power or thermal claims |
| `input_protocols` | keyboard protocols, modifiers, function/repeat/dead-key/AltGr cases, mouse tracking, paste, OSC 52, and IME text handling |
| `hostile_input` | invalid bytes, malformed controls, huge OSC/DCS payloads, reset storms, long lines, fuzz streams, image quota abuse, and recovery ladders |
| `panes_multiwindow` | native/tmux-equivalent tabs and panes, active/inactive panes, all-panes-tailing updates, tab switching, create/close models, and aggregate multi-window rendering |
| `multiplexer` | terminal-alone, native mux, tmux, screen, tmux-over-SSH, nested SSH/tmux, passthrough/fallback, feature classification, latency delta, and render overhead |
| `remote_session` | virtual-network replay for SSH, mosh, docker/podman exec, ConPTY-like sessions, resize propagation, and feature-degradation classification |
| `real_app_replay` | deterministic replay streams for editors, fuzzy finders, diffs, build logs, log tails, dashboards, mux sessions, and AI/code-generation output |

## Reproduction and dashboard workflow

Run the fast blocking-CI benchmark smoke locally:

```bash
mise run bench -- --ci-smoke --output artifacts/benchmark-reproduction/ci
```

Compile checked-in benchmark targets and record command metadata:

```bash
mise run bench -- --output artifacts/benchmark-reproduction/local
```

Run the nightly/full reproduction shape, including a small measured subset:

```bash
mise run bench -- --quick --output artifacts/benchmark-reproduction/overnight
```

Validate launcher/profile manifests:

```bash
scripts/validate-benchmark-manifests.py
```

Normalize measured JSONL rows and build dashboard artifacts:

```bash
scripts/build-benchmark-dashboard.py artifacts/results/*.jsonl \
  --output-dir artifacts/benchmark-dashboard \
  --strict
```

The dashboard writes `raw-normalized.jsonl`, `summary.csv`, and `dashboard.md`.
The row schema lives in `benchmarks/result-schema.json`; launcher/profile inputs
live in `benchmarks/launcher-matrix.json` and `benchmarks/profiles.json`.

## Host-specific evidence

These scripts are opt-in and may depend on installed tools, live services, GUI
sessions, or privileges:

```bash
scripts/run-terminal-public-benchmarks.py \
  --terminal bootty --terminal kitty --terminal alacritty --terminal wezterm \
  --tool vtebench --tool termbench \
  --min-bytes 67108864 \
  --resource-sample \
  --output artifacts/external-benchmarks/terminal-public.jsonl

scripts/run-external-benchmark-adapters.py --output artifacts/external-benchmarks/results.jsonl
scripts/run-external-benchmark-adapters.py \
  --typometer-csv artifacts/latency/typometer.csv \
  --software-latency-csv artifacts/latency/software.csv \
  --hardware-latency-csv artifacts/latency/hardware.csv \
  --output artifacts/external-benchmarks/latency-import.jsonl

mise run bench:live-remote -- artifacts/live-remote/results.jsonl
mise run bench:hostile-soak -- artifacts/hostile-soak/local
mise run bench:power-thermal -- artifacts/power/local -- cargo run -p bootty --bin bootty
mise run bench:record-replay -- <fixture-name> artifacts/replays -- <command> [args...]
```

`run-terminal-public-benchmarks.py` launches tools inside actual terminals. Keep
benchmark stdout attached to the terminal PTY; redirecting stdout measures file
output and invalidates the result. `run-external-benchmark-adapters.py` is for
probing/importing public benchmark tools and CSV artifacts unless its command is
already running inside a terminal emulator.

Latency imports preserve `terminal`, `profile`, and `benchmark`/`case` columns
from CSV input as top-level normalized fields. Numeric latency columns default to
milliseconds for `--typometer-csv`, `--software-latency-csv`, and
`--hardware-latency-csv`; headers ending in `_us`, `_ns`, or `_s` override the
unit. Typometer rows are labeled `typometer_software_visual`, software event or
frame-counter imports are labeled `software_event_visual`, and hardware rig rows
are labeled `hardware_key_to_pixel`. Publishable latency claims still require
the capture method, device/display settings, run counts, and raw CSV artifacts.

The public terminal runner normalizes the focused PTY profile to an actual
80x24 terminal grid and records `actual_pty_size` in each terminal result row.
Rows whose measured PTY size does not match the target are marked
`invalidated`, with metrics retained only for diagnosis. Bootty uses a
harness-generated config by default: a calibrated non-fullscreen window,
sidebar/status chrome disabled, and native scrollback disabled. Competitor
launchers must likewise avoid user config and set an equivalent 80x24 initial
grid when their CLIs support it. Pass `--use-user-bootty-config` only for
explicitly labeled user-profile runs.

By default, the runner also executes a small terminal-response correctness gate
per terminal/profile and copies `correctness_status`, detail, and artifact paths
onto each benchmark row; failed gates invalidate otherwise passing timing rows.

By default, each public PTY benchmark then emits a visible sentinel and a DSR
query after the benchmark process exits. The resulting
`terminal_response_catch_up/post_producer_response_time` metric measures how long
the terminal takes to accept the post-producer output and answer after any PTY
backlog. This is not a visual-present timestamp; it is a terminal-agnostic
catch-up lower bound and a synchronization artifact for external video/OCR
capture. Use `--skip-catch-up-probe` to disable it or `--catch-up-timeout-ms` to
change the probe timeout.

Use `--resource-sample` when a run is intended to compare efficiency; it records
primary terminal-process RSS and CPU samples beside the timing metrics. Treat it
as process-level evidence, not full GPU/power accounting.

## Internal trace mode

### Live tab, session, and Space switching

`scripts/benchmark-switching.py` drives the running development app through its
normal command owner and waits for the destination terminal's GPUI paint hook.
Unlike the `panes_multiwindow` synthetic workloads, this includes control dispatch,
backend activation, persistence, Dock reconciliation, terminal preparation, and CPU
painting. It does **not** measure display scanout or OS keyboard-event delivery.
`command_to_paint_ms` starts before CLI process launch; `command_ms` separately
records command round-trip time. `owner_to_paint_ms` starts when the app owner
receives the invocation, excluding CLI discovery/startup and transport overhead.
Use the owner metric to optimize switching within the UI. A successful command
alone is never a successful paint sample.

Launch a development window with:

```bash
BOOTTY_SWITCH_BENCH_TRACE=/tmp/switch-paint.jsonl \
BOOTTY_TRACE_LATENCY=/tmp/switch-phases.log mise run launch
```

Keep that window foreground, with terminal panels selected. Set up two destinations
for each case, switch to each once, and use their `target` values from the paint
trace. The trace contains identities, focus, and a nonblank-content flag, never
terminal text. Use real shell content in both destinations. Example case file:

```json
{
  "cases": [{
    "name": "native_session_warm",
    "setup": [["command", "select_space", "1"]],
    "destinations": [
      {"command": ["command", "select_session", "1"], "target": "EXACT_FIRST_TARGET"},
      {"command": ["command", "select_session", "2"], "target": "EXACT_SECOND_TARGET"}
    ]
  }],
  "restore": [["command", "select_space", "1"], ["command", "select_session", "1"]]
}
```

Use `select_tab` and `select_space` for the other cases. Label backend, local/remote,
terminal count, workload, and warm/cold policy in the case names and recipe. The
runner measures **warm alternating switches**; cold startup/first attachment needs
separate evidence. Existing development sessions are reused, never created or
deleted. The optional `restore` sequence restores the initial selection.

```bash
python3 scripts/benchmark-switching.py \
  --binary /absolute/path/to/development/bundle/Contents/MacOS/bootty \
  --namespace bootty-dev-0123456789abcdef \
  --trace /tmp/switch-paint.jsonl --cases /tmp/switch-cases.json \
  --output artifacts/switching/baseline --samples 30 --warmups 3
```

Run cases serially, with builds and other benchmark workloads stopped. Each
iteration measures both directions in seeded randomized order, after establishing
the opposite destination and a 100ms settling interval outside the timed region.
The runner requires the requested target to paint with keyboard focus and nonblank
content. A timeout, command failure, or clock adjustment invalidates the run; no
outliers are discarded. Results include raw samples, p50/p95/p99/max, variance,
binary hash, recipe, and owner diagnostics. Keep failed rows as evidence.

Tracing writes synchronously **after** taking the paint timestamp. Its overhead
can affect subsequent frames; compare candidates with identical instrumentation.
Scene paint is earlier than GPU submission, and nonblank content is not a pixel
equivalence check. Confirm real window content and input routing independently.
Opaque tmux/Herdr attachment keys may not distinguish inner tab changes: do not
use identical keys as a tab benchmark or call those no-ops a fast result.

Bootty can emit internal JSONL trace records for optimization-only runs. This is
not apples-to-apples competitive evidence because competitors cannot expose the
same internal milestones.

```bash
BOOTTY_BENCH_TRACE=/tmp/bootty-trace.jsonl cargo run -p bootty --bin bootty
BOOTTY_BENCH_TRACE=/tmp/bootty-trace.jsonl \
BOOTTY_BENCH_TRACE_SAMPLE_EVERY=10 \
cargo run -p bootty --bin bootty
```

Trace records include `schema_version`, `ts_ns`, and `event`. Current events
include `worker_start`, `worker_stop`, `input_commands`, `pty_read`,
`pty_collect_done`, `parse_start`, `parse_done`, `frame_submitted`, and
`frame_presented`. Consumers must ignore unknown fields/events. When tracing is
disabled the worker stores `None`; when enabled records are written
synchronously so crash evidence can be recovered.

The historical `frame_presented` event is emitted when the terminal worker
publishes an immutable frame, before GPUI prepaint, paint, platform submission,
or display scanout. Its `presenter` field is `published_frame`; do not treat it
as a visual-present timestamp. The `gpui_terminal_scene` full-window case is the
checked-in source for GPUI draw and platform-submit evidence.

When `run-terminal-public-benchmarks.py --bootty-trace` is used for Bootty, the
runner also imports trace-derived `bootty_trace` metrics. The legacy
`visual_catch_up_time` field is the time from the last `parse_done` event to the
last published-frame event despite its name. Treat it as Bootty-owned
parse-to-publication diagnostic evidence, not visual evidence. For
competitors, use the public runner's post-producer sentinel plus compositor,
video, OCR, or frame-counter capture when a true visual-present timestamp is
required.

## Competitive claim rules

Do not publish one global fastest-terminal score. Publish independent,
category-specific claims. Every candidate claim starts as `insufficient_data` and
must become `candidate` before it can become `publishable`.

| State | Meaning |
| --- | --- |
| `unsupported` | The competitor or Bootty does not support the feature; report separately from speed. |
| `invalidated` | Correctness, visual parity, crash, hang, or timeout failure makes numbers non-comparable. |
| `insufficient_data` | Data lacks enough runs, metadata, opponents, or tail statistics. |
| `candidate` | Raw data and metadata exist but still need review for caveats/outliers/reproducibility. |
| `publishable` | Correctness, raw data, metadata, tail statistics, confidence data, and caveats are present. |

A publishable claim needs correctness status, raw JSON/CSV, command lines,
benchmark commit/workload hashes, terminal versions/config hashes, profile,
`TERM`, shell, font/grid/scrollback settings, platform/display/GPU metadata, run
counts, warmups, outlier policy, p50/p95/p99/min/max/stddev/CI/CV, producer vs
visible catch-up timing where applicable, and resource counters when speed can
trade against memory or power.

Hillclimb work is complete only when the benchmark has a stable baseline and
high-tail sentinel, at least one profiled optimization attempt or evidence that
no meaningful Bootty-owned hot path remains, correctness still passes, and the
report states what was tried, what was rejected, and the next suspected
bottleneck.

## Strong opponents by claim area

| Category | Strong opponents |
| --- | --- |
| Low input latency | xterm, st, Alacritty, foot, tuned kitty |
| Parser throughput | kitty, Alacritty, Ghostty |
| Wayland efficiency | foot, kitty, Alacritty, Ghostty |
| Render/frame pacing | foot, Alacritty, kitty, Ghostty, WezTerm |
| Graphics protocols | kitty, WezTerm, Ghostty, Konsole |
| Memory and scrollback efficiency | foot, xterm, st, Alacritty |
| macOS native behavior | Terminal.app, iTerm2, Ghostty |
| Multiplexer workflows | WezTerm, tmux inside kitty/Alacritty/Ghostty |
| Cross-platform feature coverage | WezTerm, kitty, Ghostty |
| Fault resistance | xterm, kitty, Ghostty, WezTerm, Alacritty |

## Result hygiene

- Store raw benchmark outputs, plots, and summaries under artifacts or PR/task
  evidence, not in this document.
- Record benchmark command lines, commit hashes, platform metadata, terminal
  config hashes, run counts, warmups, and outlier policy with the result.
- Separate producer completion, parse/update completion, and visible render
  catch-up when measuring output-heavy workloads.
- Report p50/p95/p99/max and variance for publishable results; median-only
  summaries are not enough.
- Mark unsupported features as unsupported, not slow.
- Invalidate performance claims for any feature class with failed correctness,
  visual parity, crash, hang, or timeout evidence.
- Do not compare tuned Bootty against default competitors without labeling both
  profiles.
- Do not mix Wayland, X11, macOS, Windows, refresh rates, DPI scales, or power
  profiles in one chart without labels.
