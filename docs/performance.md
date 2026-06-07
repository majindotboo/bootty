# Runtime latency guardrails

Bootty exposes lightweight terminal metrics in the top status bar and keeps
the extraction/planning path benchmarkable outside the live app.

## Status bar metrics

- `cols x rows`: current terminal grid size.
- `drain`: worker time spent draining pending PTY bytes into
  `libghostty-vt`, plus drained bytes reported since the previous UI sample.
- `update`: time spent in `RenderState::update(&terminal)` while producing the
  published render frame.
- `extract`: render-frame extraction time after the render-state update starts,
  including row/cell traversal, style/color reads, and reusable text-buffer
  population.
- `paint`: UI-thread time spent planning terminal paint, building
  `TerminalRenderFrame`, and enqueuing the WGPU callback. It is not GPU render
  pass time.
- `runs`: number of grouped terminal text runs in the latest paint plan.

`RendererMetrics` also tracks internal counters such as extracted cells, chars,
dirty rows, and cursor-blink state. Those counters are diagnostic data, not a
stable status bar contract.

## Guardrails

- PTY drain should stay bounded by the worker drain budgets. Large shell bursts
  should backlog and catch up instead of monopolizing a UI frame.
- Render-state update plus frame extraction should remain comfortably below an
  interactive frame budget for ordinary prompt usage.
- Text run count should be much lower than cell count for typical shell output;
  when it approaches cell count, grouping is ineffective.
- Idle frames should not do expensive terminal work without terminal state
  changes, cursor blink, or chrome repaint needs.
- Terminal performance changes should preserve the unit, property, app-path,
  and benchmark seams that describe the affected runtime behavior.

## Allocation-sensitive paths

- The PTY reader sends fresh `Vec<u8>` chunks through the channel.
- `TerminalWorker` stores pending PTY chunks in a `VecDeque` and drains them in
  bounded slices.
- `RenderFrame` reuses vectors across extractions, including the shared
  character buffer referenced by cells.
- Frame extraction still traverses the visible grid.
- `PaintPlanner` still builds a `String` per text run.
- Dirty-row state is available but is not yet used to skip row cache updates.
- The WGPU callback path still has CPU-side command, glyph, and vertex-buffer
  preparation costs before GPU submission.

## Benchmark seams

- `extract_frame_*` measures Ghostty render-state update plus reusable frame
  extraction through `TerminalEngine`.
- `paint_plan_*` measures renderer-independent grouping and primitive planning
  through `PaintPlanner`.
- `render_commands_*` measures conversion from a paint plan into renderer-owned
  terminal commands, including text shaping and sprite routing.
- `wgpu_prepare_*` measures steady-state WGPU CPU staging after one untimed
  renderer warm-up prepare, including text atlas, vertex buffer, image upload,
  and layer preparation work.
- `*_simple_shell_120x40` covers ordinary prompt output.
- `*_complex_shell_180x80` covers truecolor, indexed colors, decorations, wide
  glyphs, emoji, combining marks, block drawing, box drawing, and powerline
  glyphs.
- `*_ai_agent_dashboard_220x70` covers noisy AI-agent-style tool streams,
  status gutters, diff-like rows, dense trace text, truecolor spans, emoji, and
  sprite glyph routing.
- `*_tmux_images_truecolor_240x90` covers tmux-style panes, status lines,
  htop/log-like animation surfaces, truecolor regions, box drawing, powerline
  glyphs, and Kitty image protocol payloads.
- `animated_agent_update_extract_plan_render_160x60` measures a mutating
  end-to-end CPU pipeline: terminal writes, frame extraction, paint planning,
  text contract construction, and renderer command preparation.

## Benchmark usage

Property tests cover geometry minimums, host input coordinate conversion,
renderer text-run command accounting, terminal feature extraction consistency,
and PTY drain scheduling invariants. Drain scheduling properties assert that
each worker slice stays within the available data, per-slice budget, and
per-frame budget, and that frame publication honors fast-path input, backlog,
settle, and heartbeat rules.

Run the benchmark harness with `cargo bench -p bootty` when changing
extraction, planning, text grouping, sprite routing, or terminal WGPU command
generation.

## Benchmark checkpoints

Current local baseline on the M4 Pro workstation, collected with:
`cargo bench -p bootty --bench paint_plan -- --sample-size 10 --measurement-time 0.2 --warm-up-time 0.1`

- `paint_plan_simple_shell_120x40`: about 17 µs
- `paint_plan_complex_shell_180x80`: about 81 µs
- `paint_plan_ai_agent_dashboard_220x70`: about 64 µs
- `paint_plan_tmux_images_truecolor_240x90`: about 43 µs
- `extract_frame_clean_steady_state_simple_shell_120x40`: about 67 µs
- `extract_frame_clean_steady_state_complex_shell_180x80`: about 266 µs
- `extract_frame_clean_steady_state_ai_agent_dashboard_220x70`: about 247 µs
- `extract_frame_clean_steady_state_tmux_images_truecolor_240x90`: about 295 µs
- `extract_frame_one_row_mutate_simple_shell_120x40`: about 72 µs
- `extract_frame_one_row_mutate_complex_shell_180x80`: about 275 µs
- `extract_frame_one_row_mutate_ai_agent_dashboard_220x70`: about 253 µs
- `extract_frame_one_row_mutate_tmux_images_truecolor_240x90`: about 343 µs
- `render_commands_simple_shell_120x40`: about 374 µs
- `render_commands_complex_shell_180x80`: about 2.53 ms
- `render_commands_ai_agent_dashboard_220x70`: about 1.93 ms
- `render_commands_tmux_images_truecolor_240x90`: about 63 µs
- `wgpu_prepare_simple_shell_120x40`: about 817 µs
- `wgpu_prepare_complex_shell_180x80`: about 5.33 ms
- `wgpu_prepare_ai_agent_dashboard_220x70`: about 3.86 ms
- `wgpu_prepare_tmux_images_truecolor_240x90`: about 198 µs
- `animated_agent_update_extract_plan_render_160x60`: about 2.05 ms
- `animated_agent_update_extract_plan_render_wgpu_prepare_160x60`: about 5.46 ms

Optimization targets for the next slices:

- Dirty-row extraction should keep the clean steady-state benches within 10% of these baselines and drive the one-row-mutate benches materially below the current full-scan path, with the large scenarios (`complex_shell`, `ai_agent_dashboard`, `tmux_images_truecolor`) as the decision makers.
- Paint-plan text run work should keep `paint_plan_*` and `render_commands_*` within 10% of baseline at every intermediate step, then improve the Unicode-heavy scenarios (`complex_shell`, `ai_agent_dashboard`) once string materialization is reduced.
- WGPU staging work should keep visual parity tests green and avoid regressing `wgpu_prepare_*` by more than 10% at any intermediate step; success means steady-frame prepare gets measurably cheaper while animated prepare stays bounded.
