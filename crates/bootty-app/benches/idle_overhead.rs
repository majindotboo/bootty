use std::{env, hint::black_box};

use bootty_app::{
    geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface},
    paint_plan::PaintPlanner,
    terminal::TerminalEngine,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use eframe::egui::Vec2;

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};
const DEEP_IDLE_ENV: &str = "BOOTTY_DEEP_IDLE_BENCH";

#[derive(Clone, Copy)]
enum IdleCase {
    EmptyShellBlinkOff,
    EmptyShellBlinkOn,
    LargePrompt,
    FourTabs,
    SixteenTabs,
    SixtyFourTabs,
    FourPanes,
    SixteenPanes,
    LigaturesOn,
    ImePreedit,
    ShellIntegration,
    NotificationQueued,
}

impl IdleCase {
    const fn name(self) -> &'static str {
        match self {
            Self::EmptyShellBlinkOff => "empty_shell_blink_off",
            Self::EmptyShellBlinkOn => "empty_shell_blink_on",
            Self::LargePrompt => "large_prompt",
            Self::FourTabs => "four_tabs",
            Self::SixteenTabs => "sixteen_tabs",
            Self::SixtyFourTabs => "sixty_four_tabs",
            Self::FourPanes => "four_panes",
            Self::SixteenPanes => "sixteen_panes",
            Self::LigaturesOn => "ligatures_on",
            Self::ImePreedit => "ime_preedit",
            Self::ShellIntegration => "shell_integration",
            Self::NotificationQueued => "notification_queued",
        }
    }
}

#[derive(Clone, Copy)]
struct ImportedIdleSample {
    second: u32,
    cpu_milli_pct: u32,
    wakeups: u32,
    rss_bytes: u64,
    gpu_milli_pct: u32,
    power_mw: u32,
}

#[derive(Default)]
struct IdleStats {
    ticks: usize,
    redraws: usize,
    render_commands: usize,
    cells: usize,
    chars: usize,
    modeled_tabs: usize,
    modeled_panes: usize,
    cpu_milli_pct: u64,
    wakeups: u64,
    rss_bytes: u64,
    gpu_milli_pct: u64,
    power_mw: u64,
    hash: u64,
}

impl IdleStats {
    fn checksum(&self) -> u64 {
        self.hash
            ^ self.ticks as u64
            ^ (self.redraws as u64).rotate_left(7)
            ^ (self.render_commands as u64).rotate_left(13)
            ^ (self.cells as u64).rotate_left(19)
            ^ (self.chars as u64).rotate_left(29)
            ^ (self.modeled_tabs as u64).rotate_left(37)
            ^ (self.modeled_panes as u64).rotate_left(43)
            ^ self.cpu_milli_pct.rotate_left(47)
            ^ self.wakeups.rotate_left(53)
            ^ self.rss_bytes.rotate_left(59)
            ^ self.gpu_milli_pct.rotate_left(61)
            ^ self.power_mw.rotate_left(3)
    }
}

fn deep_idle_benches_enabled() -> bool {
    matches!(env::var(DEEP_IDLE_ENV).as_deref(), Ok("1" | "true" | "yes"))
}

fn surface_for(geometry: TerminalGeometry) -> TerminalSurface {
    TerminalSurface::for_size(
        Vec2::new(
            f32::from(geometry.cols) * geometry.cell_width as f32 + 20.0,
            f32::from(geometry.rows) * geometry.cell_height as f32 + 20.0,
        ),
        CellMetrics::new(geometry.cell_width as f32, geometry.cell_height as f32),
        TerminalPadding::uniform(10.0),
    )
}

fn seeded_engine(case: IdleCase) -> TerminalEngine {
    let mut engine = TerminalEngine::new(GEOMETRY).expect("terminal engine");
    match case {
        IdleCase::EmptyShellBlinkOff => engine.write_vt(b"\x1b[?25l$ "),
        IdleCase::EmptyShellBlinkOn => engine.write_vt(b"\x1b[?25h$ "),
        IdleCase::LargePrompt => engine.write_vt(
            format!(
                "\x1b[38;5;81m~/src/bootty\x1b[0m \x1b[38;5;214mfeature/perf\x1b[0m {} % ",
                "git:dirty jobs:7 cpu:12% ".repeat(8)
            )
            .as_bytes(),
        ),
        IdleCase::LigaturesOn => {
            engine.write_vt(b"$ fn main() -> Result<()> { value != other && ready }")
        }
        IdleCase::ImePreedit => engine.write_vt("$ preedit: かな漢字候補".as_bytes()),
        IdleCase::ShellIntegration => {
            engine.write_vt(b"\x1b]133;A\x1b\\$ \x1b]7;file:///Users/luan/src/bootty\x1b\\")
        }
        IdleCase::NotificationQueued => engine.write_vt(b"\x1b]777;notify;build;done\x1b\\$ "),
        IdleCase::FourTabs
        | IdleCase::SixteenTabs
        | IdleCase::SixtyFourTabs
        | IdleCase::FourPanes
        | IdleCase::SixteenPanes => {
            engine.write_vt(b"$ ");
        }
    }
    engine
}

fn model_count(case: IdleCase) -> (usize, usize) {
    match case {
        IdleCase::FourTabs => (4, 1),
        IdleCase::SixteenTabs => (16, 1),
        IdleCase::SixtyFourTabs => (64, 1),
        IdleCase::FourPanes => (1, 4),
        IdleCase::SixteenPanes => (1, 16),
        _ => (1, 1),
    }
}

fn hash_text(text: &[char]) -> u64 {
    text.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
        (hash ^ u64::from(*ch as u32)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn run_idle_case(case: IdleCase, seconds: u32) -> u64 {
    let mut engines = Vec::new();
    let (tabs, panes) = model_count(case);
    for _ in 0..tabs.saturating_mul(panes) {
        engines.push(seeded_engine(case));
    }
    let mut planner = PaintPlanner::default();
    let surface = surface_for(GEOMETRY);
    let mut stats = IdleStats {
        modeled_tabs: tabs,
        modeled_panes: panes,
        ..IdleStats::default()
    };

    for tick in 0..seconds.saturating_mul(4) {
        let blink_redraw = matches!(case, IdleCase::EmptyShellBlinkOn) && tick % 2 == 0;
        let side_effect_redraw = matches!(
            case,
            IdleCase::NotificationQueued | IdleCase::ShellIntegration
        ) && tick == 0;
        stats.ticks += 1;
        if !(blink_redraw || side_effect_redraw || tick == 0) {
            continue;
        }
        stats.redraws += 1;
        for engine in &mut engines {
            let frame = engine.extract_frame().expect("idle frame");
            let plan = planner.plan(surface, frame, 16.0).clone();
            let text_contract = TerminalTextContract::for_terminal_paint_plan(
                &plan,
                &TerminalTextConfig::default(),
            );
            let render_frame = TerminalRenderFrame::from_plan(&plan, &text_contract);
            stats.render_commands += render_frame.commands.len();
            stats.cells += frame.stats.cells;
            stats.chars += frame.stats.chars;
            stats.hash ^= hash_text(&frame.text);
        }
    }

    assert!(stats.ticks > 0);
    stats.checksum()
}

fn imported_idle_samples(seconds: u32) -> Vec<ImportedIdleSample> {
    (0..seconds)
        .map(|second| ImportedIdleSample {
            second,
            cpu_milli_pct: 50 + second % 7,
            wakeups: 2 + second % 3,
            rss_bytes: 96 * 1024 * 1024 + u64::from(second % 11) * 64 * 1024,
            gpu_milli_pct: second % 2,
            power_mw: 90 + second % 9,
        })
        .collect()
}

fn summarize_imported_idle(samples: &[ImportedIdleSample]) -> u64 {
    let mut stats = IdleStats::default();
    for sample in samples {
        stats.ticks += 1;
        stats.cpu_milli_pct += u64::from(sample.cpu_milli_pct);
        stats.wakeups += u64::from(sample.wakeups);
        stats.rss_bytes = stats.rss_bytes.max(sample.rss_bytes);
        stats.gpu_milli_pct += u64::from(sample.gpu_milli_pct);
        stats.power_mw += u64::from(sample.power_mw);
        stats.hash ^= u64::from(sample.second).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    stats.checksum()
}

fn default_idle_cases() -> [IdleCase; 12] {
    [
        IdleCase::EmptyShellBlinkOff,
        IdleCase::EmptyShellBlinkOn,
        IdleCase::LargePrompt,
        IdleCase::FourTabs,
        IdleCase::SixteenTabs,
        IdleCase::SixtyFourTabs,
        IdleCase::FourPanes,
        IdleCase::SixteenPanes,
        IdleCase::LigaturesOn,
        IdleCase::ImePreedit,
        IdleCase::ShellIntegration,
        IdleCase::NotificationQueued,
    ]
}

fn bench_idle_cases(c: &mut Criterion) {
    for case in default_idle_cases() {
        c.bench_function(&format!("idle_overhead_{}_60s_model", case.name()), |b| {
            b.iter_batched(
                || case,
                |case| black_box(run_idle_case(case, 60)),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_long_duration_models(c: &mut Criterion) {
    let durations = if deep_idle_benches_enabled() {
        vec![60, 5 * 60, 30 * 60]
    } else {
        vec![60, 5 * 60]
    };
    for seconds in durations {
        c.bench_function(
            &format!("idle_overhead_empty_shell_{seconds}s_model"),
            |b| b.iter(|| black_box(run_idle_case(IdleCase::EmptyShellBlinkOff, seconds))),
        );
    }
}

fn bench_imported_system_counters(c: &mut Criterion) {
    c.bench_function("idle_overhead_import_60s_system_counters", |b| {
        b.iter(|| black_box(summarize_imported_idle(&imported_idle_samples(60))))
    });
    c.bench_function("idle_overhead_import_5m_system_counters", |b| {
        b.iter(|| black_box(summarize_imported_idle(&imported_idle_samples(5 * 60))))
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).noise_threshold(0.20);
    targets = bench_idle_cases, bench_long_duration_models, bench_imported_system_counters,
}
criterion_main!(benches);
