# Bootty render-pipeline performance — session handoff

> **TEMPORARY DOC. DELETE THIS FILE (`benchmarks.tmp.md`) WHEN THE WORK BELOW IS DONE,**
> **then commit and push the branch (see "Closing protocol" at the bottom).**

This captures an in-progress, measurement-first effort to make bootty's per-frame
render pipeline *incremental* (do work proportional to what changed, not the whole
screen), inspired by a deep comparison against `1jehuang/handterm`. A prior session
fixed the first and biggest layer; this doc hands off the remaining layers plus the
measurement framework still to be built.

---

## 0. Mission & guiding discipline

- **Goal:** drive each pipeline layer toward a *theoretical floor* where a localized
  edit costs work ∝ `dirty_rows / rows`, and steady-state allocations → 0/frame.
- **Hard rule (the user's):** **do not make optimizations without measurements.**
  Replicate the issue in a benchmark, measure a baseline, change, re-measure, prove
  the win or revert. A previous attempt that skipped this (see §6.1) shipped a
  regression and had to be reverted.
- **Style:** Rust edition 2024. Zero clippy warnings (`cargo clippy -- -W clippy::all`),
  no `#[allow(...)]` unless instructed, delete dead code, all tests green before
  committing. Conventional-commit messages, impersonal, no em-dashes, no first person.

---

## 1. Branch & working-tree state (verify with `git status` first thing)

- Branch: **`luan/perf`** (created with git-spice; `agents.git-tool=git-spice`).
  - NOTE: `spice.branchCreate.prefix` is `luan` (no slash); the branch was manually
    renamed to `luan/perf`. Consider `git config spice.branchCreate.prefix luan/`.
- Commits already on the branch (do **not** rewrite these):
  - `6672d88 test(app): add paint-plan dirty-scope benchmark`
  - `93dac0e fix(terminal): reset render-state dirty so edits report partial dirtiness`
- Uncommitted changes in the tree at handoff time:
  - `crates/bootty-app/benches/pipeline_resources.rs` — **(this session)** new resource
    bench (untracked). Commit it.
  - `crates/bootty-app/Cargo.toml` — **(this session)** registers that bench (`[[bench]]
    name = "pipeline_resources"`). Commit it.
  - `crates/bootty-app/src/extensions.rs` (~125 lines) — **NOT from this session.**
  - `crates/bootty-app/src/sidebar_defaults/codexbar.luau` (~65 lines) — **NOT from this session.**
  - `crates/bootty-app/src/sidebar_defaults/sessions.luau` (~198 lines) — **NOT from this session.**
  - The three "NOT from this session" files are the user's own in-progress Luau/extension
    work. **Per the user: include them when pushing. NEVER discard them.** They look
    unrelated to perf, so prefer committing them as their own commit (e.g. a
    `feat(extensions)` / `chore(sidebar)` commit) distinct from the perf work.

---

## 2. What is already DONE (commit `93dac0e`) — the dirty-reset fix

**Root cause found:** libghostty's render-state dirty tracking is *caller-managed*.
`RenderState::update` consumes the terminal's dirty state but **does not unset the
render state's own per-row / global dirty flags** (see the crate docs in
`~/.cargo/git/checkouts/libghostty-rs-*/c1fe97a/crates/libghostty-vt/src/render.rs:40-51`,
and the canonical renderer `example/ghostling_rs/src/main.rs:463` `row.set_dirty(false)`
and `:511` `snapshot.set_dirty(Dirty::Clean)`).

bootty's `extract_frame` never reset those flags, so after the first paint they
accumulated to all-rows-dirty and **every edit reported `Dirty::Full`** → full-screen
extract + plan + render on every keystroke. (Idle frames looked `Clean` only because
bootty's own `content_epoch` guard masked it.)

**Fix** in `crates/bootty-terminal/src/terminal_engine.rs` `extract_frame` (~line 2410):
added, in both the Partial path (~2485) and the Full path (~2536):
- `row.set_dirty(false)?` after consuming each row, and
- `snapshot.set_dirty(Dirty::Clean)?` once per frame.

This **activated bootty's previously-dormant incremental-extraction path** (`row_cache`
+ `assemble_cached_frame`, gated by `dirty == Dirty::Partial`).

**Measured result** (`extract_frame_one_row_mutate`, clean A/B via `git stash`):
a localized edit reports ~2 dirty rows instead of 40, and extraction dropped **80–87%**:

| Scenario | before | after |
|---|---:|---:|
| simple_shell 120×40 | 88µs | 16µs |
| complex_shell 180×80 | 312µs | 42µs |
| ai_agent 220×70 | 323µs | 46µs |
| tmux_images 240×90 | 426µs | 62µs |

**Correctness:** 194 bootty-terminal tests + bootty-render (164) + bootty-app suites all
pass; clippy clean. Guarded by characterization test
`crates/bootty-terminal/tests/dirty_tracking.rs` (`localized_edit_reports_partial_dirtiness`)
— it is a **tripwire**: if the reset regresses, every edit dirties all rows and it fails.

Memory note for this finding: `~/.claude/projects/-Users-luan-santos-src-bootty/memory/bootty-dirty-tracking-gap.md`.

---

## 3. The pipeline and the layer map (current measured state)

Per-frame pipeline for a localized edit:

```
PTY bytes → libghostty parse → extract_frame → PaintPlanner::plan
          → TerminalRenderFrame::from_plan → wgpu prepare (vertex build + upload) → draw
```

`extract_frame` is now incremental. **Everything downstream still does full-screen work
regardless of `row_dirty`.** Numbers below are for a heavy frame (colored 180×80,
2 of 80 rows dirty → work-ratio 0.025). Time = single-layer cost; allocs/bytes = per
frame, warmed pools.

| Layer | time | allocs/frame | bytes/frame | alloc floor | time floor (~2/80) | at floor? |
|---|---:|---:|---:|---:|---:|---|
| extract_frame | 41µs | 1 | 80 B | 0 | ~1µs | allocs ✅ · time ❌ |
| PaintPlanner::plan | 38µs | 95 | 1.75 KB | 0 | ~1µs | allocs ❌ · time ❌ |
| from_plan (render cmds) | 13µs | 42 | 1.3 KB | 0 | ~0.5µs | allocs ✅ pooled (§5.1 DONE) · time ❌ |
| wgpu prepare (scroll 240×90) | 6.26 → 1.29 ms | — | — | — | — | shaping cached (§5.6 DONE) |
| wgpu prepare (unique text 240×90) | ~22 ms | — | — | — | — | shaping-bound, irreducible |

Raw per-scenario resource numbers (from `pipeline_resources`, warm, 2 dirty rows;
from_plan columns reflect the §5.1 pooling fix):

```
plain_shell   120x40  4800 cells  wr 0.050  extract 1/40B/15µs  plan 1/8B/10µs     from_plan 1/1B/2.8µs
colored_shell 180x80 14400 cells  wr 0.025  extract 1/80B/40µs  plan 95/1.75KB/36µs from_plan 42/1.3KB/13µs
wide_colored  240x90 21600 cells  wr 0.022  extract 1/90B/57µs  plan 107/2KB/48µs   from_plan 47/1.5KB/17µs
```

CPU timing benches (criterion, full-screen frames — none of plan/from_plan/wgpu honor
`row_dirty` yet, so these equal their per-edit cost):
- `paint_plan_dirty_scope/{full,one_row}`: plan ≈ 12 / 80 / 64 / 27 µs (simple/complex/ai/tmux).
- `render_commands_*` (from_plan): 3.6 / 84 / 44 / 9 µs.
- `wgpu_prepare_*` (cached, no change): 0.7 / 15 / 6 / 1.5 µs.
- `wgpu_prepare_dirty_ascii_text_240x90`: **~22.5 ms** (changing text — the cliff).
- `animated_agent_update_extract_plan_render_wgpu_prepare_160x60`: ~768µs (full redraw).

---

## 4. Theoretical-target model (use this to know how far each row is from optimal)

For a localized edit touching `dirty_rows` of `rows`:
- **time floor** ≈ `irreducible_fixed + per_row_cost × dirty_rows` ≈ `work_ratio × full_cost`
  (here ~2.5% of full). `irreducible_fixed` = snapshot + an O(rows) dirty scan + present.
- **allocation floor** = 0 allocations / 0 bytes per frame (warmed, pooled buffers).
- **work-ratio floor** = `dirty_rows / rows` (0.022–0.05 in these scenarios).
- **GPU upload floor** = bytes for the changed rows only, not the whole vertex buffer.

Achieved vs floor today: `extract` is at the allocation floor but not the time floor;
`plan` and `from_plan` are at neither.

---

## 5. Open optimization targets (prioritized; each needs measure → change → re-measure)

### 5.1 `from_plan` allocation churn — DONE
- **Was:** ~500 allocs and **87–97 KB churned every frame** (`pipeline_resources`),
  dominated by `text.to_owned()` per text command plus a fresh `commands` Vec. Floor is 0.
- **Fix:** added `RenderFramePool` in `crates/bootty-render/src/terminal_render.rs`. It
  rebuilds a `TerminalRenderFrame` in place: `drain`s the previous frame's commands,
  reclaiming each text command's `String` into a pool, keeps the `commands` Vec
  capacity, and `push_str`s into recycled buffers instead of `to_owned`. Wired into the
  production hot path via `TerminalRenderCache::rebuild` (`crates/bootty-app/src/renderer.rs`),
  which the repaint path calls on a cache miss; the cache already holds the prior frame,
  so its buffers are the pool's backing store. `store` is now `#[cfg(test)]`.
- **Measured (`pipeline_resources`, warm, 2 dirty rows):** colored 180×80 from_plan
  491 → 42 allocs, 87 KB → 1.3 KB (−98.5%), 30 → 13µs; wide 240×90 551 → 47 allocs,
  97 KB → 1.5 KB, 21 → 17µs; plain 84 → 1 alloc. Guarded by the proptest
  `pooled_rebuild_matches_one_shot_builder` (rebuild twice, assert equal to one-shot
  `from_plan` — catches stale recycled-buffer bugs).
- **Remaining headroom:** ~45 allocs persist because LIFO buffer reuse hands back a
  too-small `String` for a longer run (realloc). A capacity-aware pool (pop the
  best-fit, or reserve to the row's max byte length) could reach the 0 floor.

### 5.2 `plan` allocation churn on styled content — RESOLVED (warm-up artifact, already at floor)
- **Was:** 95–107 allocs/frame on colored scenarios under a 6-iteration warm-up.
- **Finding:** this was pure warm-up churn, not a steady-state miss. `recycle_plan`
  already pools run strings; the residual allocs are LIFO reuse handing a short buffer to
  a long run, which reallocs **once** then stays large. Deepening the resource bench's
  warm loop (6 → 60 mutating extracts) saturates the pool and the count drops to **1
  alloc / 8 B** for `plan` and **1 alloc / 1 B** for `from_plan` — i.e. the 0/frame floor.
- **Conclusion:** no code change. A long-running renderer is always deeply warmed, so it
  already sits at the allocation floor. The §5.1 "remaining headroom" note is likewise a
  warm-up artifact. Fixed the bench to measure representative steady state.

### 5.3 `extract_frame` is allocation-incremental but still time-O(all cells)
- **Evidence:** 1 alloc / 80 B (excellent) but 41–58µs because `assemble_cached_frame`
  rebuilds the *entire* `RenderFrame.cells` + `text` from the row cache every frame.
- **Where:** `crates/bootty-terminal/src/terminal_engine.rs` `assemble_cached_frame`
  (called from the Partial path ~2519). Next headroom: reuse clean rows' `RenderCell`s
  in `self.frame.cells` rather than rebuilding the whole vector. This is delicate —
  downstream consumers expect a complete `frame.cells`.

### 5.4 Cold-cache cliff (the first edit after any full redraw)
- **Evidence:** found via a harness bug — a Partial extract whose `row_cache` isn't warm
  re-extracts everything (~1.5 MB / 130–500µs). The Full path (`terminal_engine.rs:2527`)
  does `self.row_cache.clear()`, and the clean-reuse early return doesn't populate it.
- **Impact:** every full repaint (resize, clear, alt-screen swap, scroll-region change)
  forfeits the incremental win for the next frame.
- **Idea:** keep the row cache valid across a single full redraw, or repopulate it during
  the full path so the next edit is immediately incremental. Measure with a "full frame
  then one edit" bench (does not exist yet).

### 5.5 `PaintPlanner::plan` does a full re-plan every frame (ignores `row_dirty`)
- Now that frames carry real `Partial` dirtiness (post §2 fix), making plan incremental
  is finally worthwhile (it wasn't before — see §6.1).
- **Approach:** use a **per-row plan representation** (the renderer consumes rows), NOT a
  flat-vector splice. See §6.1 for why the splice approach was tried, measured as a
  regression, and reverted.
- **Bench:** `paint_plan_dirty_scope` already contrasts `full` vs `one_row`. Today both
  cost the same; an incremental planner should make `one_row` drop toward the work-ratio
  floor. Consider adding a bench that feeds a genuinely Partial frame.

### 5.6 wgpu changing-text cliff — DIAGNOSED + scroll case DONE
- **Diagnosis:** the ~22ms is **rustybuzz shaping redone for every text command, every
  frame**. `prepare_terminal_frame` (`terminal_wgpu.rs` ~471) has a `prepared_text_cache`
  but it is keyed by the *entire* `TextCommand` (including `rect`), so any change OR move
  misses and falls to `prepare_text_command_into_uncached_with_face` →
  `shape_into_clusters` → `shape_run` (HarfBuzz). 90 rows × 240-char runs reshaped per
  frame. GPU upload (~4 MB vertices) and rasterization (atlas-cached) are minor.
- **Synthetic `wgpu_prepare_dirty_ascii_text_240x90` (~22ms) is irreducible:** its text
  is unique every frame (tick counter), so no cache can help — genuinely new glyphs.
- **Real win — scrolled/repeated text:** when content scrolls, a run moves to a new rect
  but its string is unchanged, so it was shaped on a prior frame. Added a
  **position-independent shaped-run cache** keyed `(text, face, font_size)` in
  `crates/bootty-render/src/terminal_text_atlas.rs` (`ShapedRunCacheKey` /
  `shaped_run_cache`, consulted in `prepare_text_command_into_uncached_with_face`). It
  memoizes only the shape (clusters + total_cells); positioning and atlas-cached
  rasterization still run per command, so output is byte-identical (guarded by
  `shaped_run_cache_reuses_shaping_for_moved_text_without_changing_output`). Bounded at
  `SHAPED_RUN_CACHE_CAP = 1024` entries (clears wholesale on overflow).
- **Measured:** new bench `wgpu_prepare_scroll_ascii_text_240x90`
  (`paint_plan_wgpu.rs`, `ascii_scroll_text_frame`, ring of 128 stable lines windowed +
  scrolled): **6.26 ms → ~1.29 ms median (−87%)**. Worst-case dirty bench unchanged
  (p=0.43); static `wgpu_prepare_*` unaffected.
- **Remaining headroom:** on a cache hit the clusters are cloned into the scratch and the
  lookup clones `command.text` for the key — both allocate. A borrow-keyed lookup or
  storing relative quads (skipping the per-command quad rebuild too) could push further;
  measure against the scroll bench. The unique-text full-redraw (cat) case stays
  shaping-bound — that is fundamental shaping cost, not waste.

---

## 6. Critical gotchas / do-NOT-repeat

### 6.1 The flat-splice incremental planner was tried and REVERTED — do not redo it that way
A prior attempt made `PaintPlanner::plan` incremental by keeping a persistent flat
`TerminalPaintPlan` and `splice`-ing dirty rows' runs in place (per-row count
bookkeeping + prefix sums). It was correctness-gated with a proptest
(`incremental_plan_matches_full_rebuild`, passed) but **measured as a regression**:
`one_row` ≈ or > `full` (e.g. +134% on ai_agent), and the refactor even slowed the `full`
path. Two reasons: (a) at the time frames were always `Dirty::Full` so it never engaged,
and (b) flat-vector splice tail-shift + per-call allocations dominated. It was fully
reverted. **If you make the planner incremental, use a per-row representation so clean
rows are never touched or shifted.**

### 6.2 libghostty dirty is caller-managed (already applied in §2, keep it in mind)
`update` consumes terminal dirty but leaves the render-state per-row/global flags set.
You must `row.set_dirty(false)` + `snapshot.set_dirty(Dirty::Clean)`. Don't remove these.

### 6.3 The incremental extract path must be WARM
`row_cache` only helps after consecutive Partial frames (`cache_matches_frame` true). A
benchmark/measurement must warm with several *mutating* extracts; a no-op extract takes
the clean-reuse path and never warms the cache (this caused a bogus 500µs/1.5MB reading
in the resource bench until the warm loop was fixed to mutate each pass).

### 6.4 Counting allocator pollution
`pipeline_resources.rs` installs a `#[global_allocator]`. It is a **separate bench binary**
on purpose so it does not tax the criterion timing benches. Do not move it into
`paint_plan.rs` or the timing numbers there become invalid.

---

## 7. Measurement framework — what exists, what to build

**Exists:**
- Criterion timing benches (see §8 for the full list and how to run).
- `crates/bootty-app/benches/pipeline_resources.rs` (this session): per-layer
  allocations + bytes + work-ratio for a localized edit, with stated floors.
- App-level RSS/CPU/power via external scripts: `scripts/run-power-thermal-sample.sh`,
  `scripts/run-terminal-public-benchmarks.py --resource-sample`. App-level only.
- `BOOTTY_BENCH_TRACE` JSONL timing events (`crates/bootty-runtime/src/benchmark_trace.rs`).

**Still missing (build these to finish the "resource" axis):**
- **GPU bytes uploaded / frame** — the power lever and the §5.6 diagnosis tool. Either
  derive from `vertex_count × size_of::<vertex>()` in the wgpu bench, or add a counter to
  the renderer (`terminal_wgpu.rs` upload sites) exposing bytes written per
  `prepare_terminal_frame`.
- **RSS / per-window memory** — handterm's headline metric. handterm targets ~1–2 MB per
  *additional* window on a shared-GPU host; bootty's per-process RSS in the public
  benchmark report was 161–426 MiB. A per-window RSS probe would let us track the gap.
- **Work-ratio for plan/from_plan** — confirm they touch all cells (expose a counter of
  cells/runs processed vs total), so the incremental versions can be proven against it.

---

## 8. How to run the benchmarks (exact commands)

All benches are `harness = false` criterion targets in `crates/bootty-app` (and
`crates/bootty-runtime` for pty_drain/flood_response). Run from repo root.

```bash
# Resource report (allocations + work-ratio per layer):
cargo bench -p bootty-app --bench pipeline_resources

# Paint-plan full-vs-one-row + extraction (reduce sampling for speed):
cargo bench -p bootty-app --bench paint_plan -- paint_plan_dirty --measurement-time 3 --warm-up-time 1 --sample-size 50
cargo bench -p bootty-app --bench paint_plan -- extract_frame_one_row_mutate --measurement-time 3 --warm-up-time 1 --sample-size 50
cargo bench -p bootty-app --bench paint_plan -- render_commands --measurement-time 2 --warm-up-time 1 --sample-size 30

# GPU staging / the 22ms cliff:
cargo bench -p bootty-app --bench paint_plan_wgpu -- wgpu_prepare --measurement-time 2 --warm-up-time 1 --sample-size 20
```

**Clean A/B (before/after a change) using git-spice-friendly stash of one file:**
```bash
git stash push crates/bootty-terminal/src/terminal_engine.rs   # revert just the change
cargo bench ... <filter>                                       # baseline → criterion saves it
git stash pop                                                  # restore the change
cargo bench ... <filter>                                       # criterion prints "change: -NN%"
```

**Validation gate before any commit:**
```bash
cargo test -p bootty-terminal -p bootty-render -p bootty-app
cargo clippy -p bootty-terminal -p bootty-render -p bootty-app --tests --benches -- -W clippy::all
cargo fmt
```

---

## 9. Key files & line references (approximate; re-grep to confirm)

- `crates/bootty-terminal/src/terminal_engine.rs`
  - `extract_frame` ~2410; clean-reuse early return ~2470; Partial path ~2485 (`row.set_dirty(false)` ~2517, `snapshot.set_dirty(Dirty::Clean)` before `assemble_cached_frame` ~2519); Full path ~2536 (`row.set_dirty(false)` after `row_dirty.push`, `snapshot.set_dirty(Dirty::Clean)` after the loop); `row_cache.clear()` ~2527.
- `crates/bootty-terminal/tests/dirty_tracking.rs` — characterization tripwire.
- `crates/bootty-render/src/paint_plan.rs` — `PaintPlanner::plan` ~199, `plan_text_runs`, `recycle_plan`, `run_text_pool`.
- `crates/bootty-render/src/terminal_render.rs` — `TerminalRenderFrame::from_plan` (the §5.1 allocation target).
- `crates/bootty-render/src/terminal_wgpu.rs` — `TerminalBackgroundFrameResources::update` ~343, `prepare_terminal_frame` ~471, `write_buffer` 366/969/1014, atlas grow ~510.
- `crates/bootty-app/benches/paint_plan.rs` — `bench_paint_plan_dirty_scope`, `bench_extract_frame_one_row_mutate`, `bench_render_commands`, fixtures via `mod paint_plan_fixtures`.
- `crates/bootty-app/benches/paint_plan_wgpu.rs` — `bench_wgpu_dirty_text_prepare` :381, `ascii_dirty_text_frame` :299.
- `crates/bootty-app/benches/pipeline_resources.rs` — this session's resource report.
- Reference (read-only): handterm clone was at a scratchpad path last session; its custom `metrics.rs` (single-shot MB/s vs memcpy) and per-row dirty model are the inspiration. libghostty checkout: `~/.cargo/git/checkouts/libghostty-rs-*/c1fe97a/`.

---

## 10. Suggested skills for the next session

- `/start` is already done (branch `luan/perf` exists) — do NOT create a new branch.
- `/diagnose` — for the §5.6 wgpu 22ms cliff (reproduce, attribute, instrument).
- `/tdd` — for §5.1/§5.2 allocation-pooling work (write an allocation-budget assertion
  test first, then make it pass).
- `/code-review` or `/crit` — before finalizing the diff.
- `/commit` and `gs:submit` (Git-Spice) — for the closing protocol below.
- Consider the `sym` skill for navigation (`sym callers`, `sym refs`) per repo conventions.

---

## 11. Closing protocol (what the user explicitly asked for)

When the work above is at a good stopping point:

1. **Delete this file:** `rm benchmarks.tmp.md` (it must NOT be pushed).
2. **Stage everything** still in the tree, including the changes this handoff flagged as
   "not from the perf session" (`extensions.rs`, `codexbar.luau`, `sessions.luau`). The
   user said to push **everything we have here, including changes you don't recognize.**
   Do not discard any of them (repo rule: never discard unrelated changes).
   - Suggested grouping (optional): one commit for the perf/measurement work
     (`pipeline_resources.rs` + `Cargo.toml` + any new optimizations), and a separate
     commit for the unrelated extensions/luau changes.
3. **Push the branch** `luan/perf` using Git-Spice (`gs:submit` / `gs submit`). PRs were
   intentionally deferred — confirm with the user whether to open a PR or just push.
4. Run the §8 validation gate before committing; all tests must pass.
