use std::{env, hint::black_box};

use anyhow::{Result, ensure};
use bootty_terminal::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::TerminalColorConfig;
use bootty_terminal::terminal_engine::{
    NATIVE_MAX_SCROLLBACK, NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE, TerminalEngine,
};
use criterion::{BatchSize, Criterion};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};
const DEEP_BENCH_ENV: &str = "BOOTTY_DEEP_SCROLLBACK_BENCH";

#[derive(Clone, Copy)]
enum ContentKind {
    Short,
    Wrapped,
    Unicode,
    EmojiCombining,
    AnsiOsc8TabsBox,
    VeryLongLine,
}

impl ContentKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Short => "short_plain",
            Self::Wrapped => "long_wrapped",
            Self::Unicode => "unicode_wide",
            Self::EmojiCombining => "emoji_combining",
            Self::AnsiOsc8TabsBox => "ansi_osc8_tabs_box",
            Self::VeryLongLine => "very_long_line",
        }
    }

    fn append_line(self, out: &mut Vec<u8>, index: usize) {
        match self {
            Self::Short => {
                out.extend_from_slice(format!("line {index:08} target short payload").as_bytes());
            }
            Self::Wrapped => {
                out.extend_from_slice(format!("wrapped {index:08} ").as_bytes());
                out.extend_from_slice("segment ".repeat(40).as_bytes());
            }
            Self::Unicode => {
                out.extend_from_slice(
                    format!(
                        "unicode {index:08} コンニチハ Ελληνικά Кириллица عربى देवनागरी {}",
                        "┃━".repeat(18)
                    )
                    .as_bytes(),
                );
            }
            Self::EmojiCombining => {
                out.extend_from_slice(format!("emoji {index:08} ").as_bytes());
                out.extend_from_slice("🥟 👨‍👩‍👧‍👦 🇺🇳 e\u{301} a\u{0301}\u{0327} ".repeat(8).as_bytes());
            }
            Self::AnsiOsc8TabsBox => {
                out.extend_from_slice(
                    format!(
                        "\x1b[38;5;{}mansi {index:08}\x1b[0m\t┃ box ━ \x1b]8;id=row{index};https://example.invalid/{index}\x1b\\target-link\x1b]8;;\x1b\\",
                        16usize.saturating_add(index.rem_euclid(200))
                    )
                    .as_bytes(),
                );
            }
            Self::VeryLongLine => {
                out.extend_from_slice(format!("very-long {index:08} ").as_bytes());
                out.extend(std::iter::repeat_n(b'x', 16usize.saturating_mul(1024)));
            }
        }
        out.extend_from_slice(b"\r\n");
    }
}

#[derive(Clone, Copy)]
enum ScrollbackBudget {
    BoundedRows(usize),
    NativeBudget,
}

impl ScrollbackBudget {
    fn name(self) -> String {
        match self {
            Self::BoundedRows(rows) => format!("bounded_{rows}"),
            Self::NativeBudget => "native_budget".to_owned(),
        }
    }

    const fn bytes(self) -> usize {
        match self {
            Self::BoundedRows(rows) => {
                rows.saturating_mul(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE)
            }
            Self::NativeBudget => NATIVE_MAX_SCROLLBACK,
        }
    }
}

#[derive(Clone, Copy)]
struct ScrollbackScenario {
    name: &'static str,
    lines: usize,
    content: ContentKind,
    budget: ScrollbackBudget,
}

impl ScrollbackScenario {
    fn bench_name(self, prefix: &str) -> String {
        format!(
            "{prefix}_{}_{}_{}",
            self.name,
            self.content.name(),
            self.budget.name()
        )
    }
}

#[derive(Default)]
struct ScrollbackStats {
    lines: usize,
    input_bytes: usize,
    frame_cells: usize,
    frame_chars: usize,
    matches: usize,
    copied_chars: usize,
    max_scrollback_bytes: usize,
    hash: u64,
}

impl ScrollbackStats {
    fn checksum(&self) -> Result<u64> {
        Ok(self.hash
            ^ u64::try_from(self.lines)?
            ^ u64::try_from(self.input_bytes)?.rotate_left(7)
            ^ u64::try_from(self.frame_cells)?.rotate_left(17)
            ^ u64::try_from(self.frame_chars)?.rotate_left(29)
            ^ u64::try_from(self.matches)?.rotate_left(37)
            ^ u64::try_from(self.copied_chars)?.rotate_left(43)
            ^ u64::try_from(self.max_scrollback_bytes)?.rotate_left(53))
    }
}

fn deep_scrollback_benches_enabled() -> bool {
    matches!(
        env::var(DEEP_BENCH_ENV).as_deref(),
        Ok("1" | "true" | "yes")
    )
}

fn append_lines(engine: &mut TerminalEngine, lines: usize, content: ContentKind) -> usize {
    let chunk_lines = match content {
        ContentKind::VeryLongLine => 4,
        _ => 256,
    };
    let mut written = 0_usize;
    let mut next = 0;
    while next < lines {
        let end = next.saturating_add(chunk_lines).min(lines);
        let mut chunk = Vec::with_capacity(end.saturating_sub(next).saturating_mul(128));
        for index in next..end {
            content.append_line(&mut chunk, index);
        }
        written = written.saturating_add(chunk.len());
        engine.write_vt(&chunk);
        next = end;
    }
    written
}

fn build_engine(scenario: ScrollbackScenario) -> Result<(TerminalEngine, usize)> {
    let mut engine = TerminalEngine::new_with_scrollback(
        GEOMETRY,
        TerminalColorConfig::default(),
        scenario.budget.bytes(),
    )?;
    let input_bytes = append_lines(&mut engine, scenario.lines, scenario.content);
    Ok((engine, input_bytes))
}

fn frame_stats(engine: &mut TerminalEngine) -> Result<(usize, usize, u64)> {
    let frame = engine.extract_frame()?;
    let hash = frame
        .text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
            (hash ^ u64::from(u32::from(*ch))).wrapping_mul(0x0000_0100_0000_01b3)
        });
    Ok((frame.cells.len(), frame.text.len(), hash))
}

fn append_and_snapshot(scenario: ScrollbackScenario) -> Result<u64> {
    let (mut engine, input_bytes) = build_engine(scenario)?;
    let (frame_cells, frame_chars, hash) = frame_stats(&mut engine)?;
    ScrollbackStats {
        lines: scenario.lines,
        input_bytes,
        frame_cells,
        frame_chars,
        max_scrollback_bytes: scenario.budget.bytes(),
        hash,
        ..ScrollbackStats::default()
    }
    .checksum()
}

fn contains_needle(text: &[char], needle: &[char]) -> bool {
    !needle.is_empty() && text.windows(needle.len()).any(|window| window == needle)
}

fn scroll_and_search(mut engine: TerminalEngine, pages: usize, needle: &[char]) -> Result<u64> {
    let mut stats = ScrollbackStats::default();
    for _ in 0..pages {
        let page_delta = isize::try_from(GEOMETRY.rows)?
            .checked_neg()
            .ok_or_else(|| anyhow::anyhow!("scrollback page delta overflow"))?;
        engine.scroll_viewport_delta(page_delta);
        let frame = engine.extract_frame()?;
        stats.matches = stats
            .matches
            .saturating_add(usize::from(contains_needle(&frame.text, needle)));
        stats.frame_cells = stats.frame_cells.saturating_add(frame.cells.len());
        stats.frame_chars = stats.frame_chars.saturating_add(frame.text.len());
        stats.hash ^= frame
            .text
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
                (hash ^ u64::from(u32::from(*ch))).wrapping_mul(0x0000_0100_0000_01b3)
            });
    }
    ensure!(
        stats.matches > 0,
        "scrollback search should find replayed OSC8 text"
    );
    engine.scroll_viewport_bottom();
    stats.checksum()
}

fn scroll_and_copy(mut engine: TerminalEngine, pages: usize) -> Result<u64> {
    let mut stats = ScrollbackStats::default();
    let mut copied = String::with_capacity(
        pages
            .saturating_mul(usize::from(GEOMETRY.cols))
            .saturating_mul(usize::from(GEOMETRY.rows)),
    );
    for _ in 0..pages {
        let page_delta = isize::try_from(GEOMETRY.rows)?
            .checked_neg()
            .ok_or_else(|| anyhow::anyhow!("scrollback page delta overflow"))?;
        engine.scroll_viewport_delta(page_delta);
        let frame = engine.extract_frame()?;
        copied.extend(frame.text.iter());
        copied.push('\n');
        stats.frame_cells = stats.frame_cells.saturating_add(frame.cells.len());
        stats.frame_chars = stats.frame_chars.saturating_add(frame.text.len());
    }
    ensure!(
        !copied.is_empty(),
        "scrollback copy should collect visible text"
    );
    stats.copied_chars = copied.chars().count();
    stats.hash = copied.chars().fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
        (hash ^ u64::from(u32::from(ch))).wrapping_mul(0x0000_0100_0000_01b3)
    });
    stats.checksum()
}

fn clear_scrollback(mut engine: TerminalEngine) -> Result<u64> {
    engine.write_vt(b"\x1b[3J");
    ensure!(
        engine.grid_size() == (GEOMETRY.cols, GEOMETRY.rows),
        "clear changed grid size"
    );
    engine.scroll_viewport_bottom();
    let (frame_cells, frame_chars, hash) = frame_stats(&mut engine)?;
    ScrollbackStats {
        frame_cells,
        frame_chars,
        hash,
        ..ScrollbackStats::default()
    }
    .checksum()
}

fn reflow_scrollback(mut engine: TerminalEngine) -> Result<u64> {
    let narrow = TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 9,
        cell_height: 22,
    };
    let wide = TerminalGeometry {
        cols: 160,
        rows: 48,
        cell_width: 9,
        cell_height: 22,
    };
    engine.resize(narrow)?;
    ensure!(
        engine.grid_size() == (narrow.cols, narrow.rows),
        "narrow reflow changed grid size"
    );
    let narrow_stats = frame_stats(&mut engine)?;
    engine.resize(wide)?;
    ensure!(
        engine.grid_size() == (wide.cols, wide.rows),
        "wide reflow changed grid size"
    );
    let wide_stats = frame_stats(&mut engine)?;
    ScrollbackStats {
        frame_cells: narrow_stats.0.saturating_add(wide_stats.0),
        frame_chars: narrow_stats.1.saturating_add(wide_stats.1),
        hash: narrow_stats.2 ^ wide_stats.2,
        ..ScrollbackStats::default()
    }
    .checksum()
}

fn default_append_scenarios() -> Vec<ScrollbackScenario> {
    vec![
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::Short,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::Wrapped,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::Unicode,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::EmojiCombining,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::AnsiOsc8TabsBox,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "100k",
            lines: 100_000,
            content: ContentKind::Short,
            budget: ScrollbackBudget::NativeBudget,
        },
        ScrollbackScenario {
            name: "100k",
            lines: 100_000,
            content: ContentKind::AnsiOsc8TabsBox,
            budget: ScrollbackBudget::NativeBudget,
        },
        ScrollbackScenario {
            name: "1mb_line",
            lines: 64,
            content: ContentKind::VeryLongLine,
            budget: ScrollbackBudget::NativeBudget,
        },
    ]
}

fn deep_append_scenarios() -> Vec<ScrollbackScenario> {
    if !deep_scrollback_benches_enabled() {
        return Vec::new();
    }
    vec![
        ScrollbackScenario {
            name: "1m",
            lines: 1_000_000,
            content: ContentKind::Short,
            budget: ScrollbackBudget::NativeBudget,
        },
        ScrollbackScenario {
            name: "10m",
            lines: 10_000_000,
            content: ContentKind::Short,
            budget: ScrollbackBudget::NativeBudget,
        },
        ScrollbackScenario {
            name: "16mb_line",
            lines: 1_024,
            content: ContentKind::VeryLongLine,
            budget: ScrollbackBudget::NativeBudget,
        },
    ]
}

const fn operation_scenario() -> ScrollbackScenario {
    ScrollbackScenario {
        name: "100k",
        lines: 100_000,
        content: ContentKind::AnsiOsc8TabsBox,
        budget: ScrollbackBudget::NativeBudget,
    }
}

fn bench_append_and_memory(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let scenarios = default_append_scenarios()
        .into_iter()
        .chain(deep_append_scenarios())
        .collect::<Vec<_>>();
    for scenario in scenarios {
        c.bench_function(&scenario.bench_name("scrollback_append_snapshot"), |b| {
            b.iter(|| {
                black_box(append_and_snapshot(scenario).map_err(|error| failure = Some(error)))
            });
        });
    }
    failure.map_or(Ok(()), Err)
}

fn bench_search_copy_clear_reflow(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let scenario = operation_scenario();
    let needle = "target-link".chars().collect::<Vec<_>>();
    c.bench_function("scrollback_search_100k_ansi_osc8_pages", |b| {
        b.iter_batched(
            || build_engine(scenario),
            |result| {
                black_box(
                    result
                        .and_then(|(engine, _)| scroll_and_search(engine, 64, &needle))
                        .map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::LargeInput,
        );
    });
    c.bench_function("scrollback_copy_100k_ansi_osc8_pages", |b| {
        b.iter_batched(
            || build_engine(scenario),
            |result| {
                black_box(
                    result
                        .and_then(|(engine, _)| scroll_and_copy(engine, 64))
                        .map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::LargeInput,
        );
    });
    c.bench_function("scrollback_clear_reclaim_100k_ansi_osc8", |b| {
        b.iter_batched(
            || build_engine(scenario),
            |result| {
                black_box(
                    result
                        .and_then(|(engine, _)| clear_scrollback(engine))
                        .map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::LargeInput,
        );
    });
    c.bench_function("scrollback_reflow_100k_ansi_osc8", |b| {
        b.iter_batched(
            || build_engine(scenario),
            |result| {
                black_box(
                    result
                        .and_then(|(engine, _)| reflow_scrollback(engine))
                        .map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::LargeInput,
        );
    });
    failure.map_or(Ok(()), Err)
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .sample_size(10)
        .noise_threshold(0.20)
        .configure_from_args();
    bench_append_and_memory(&mut criterion)?;
    bench_search_copy_clear_reflow(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
