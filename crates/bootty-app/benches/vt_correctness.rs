use std::hint::black_box;

use bootty_app::{geometry::TerminalGeometry, terminal::TerminalEngine, terminal_frame::CellStyle};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use libghostty_vt::style::RgbColor;

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};

struct CorrectnessCase {
    name: &'static str,
    feature_class: &'static str,
    input: &'static [u8],
    expected_text: &'static str,
}

#[derive(Default)]
struct CorrectnessStats {
    cells: usize,
    chars: usize,
    side_effects: usize,
    image_placements: usize,
    dirty_rows: usize,
    hash: u64,
}

impl CorrectnessStats {
    fn checksum(&self) -> u64 {
        self.hash
            ^ self.cells as u64
            ^ (self.chars as u64).rotate_left(9)
            ^ (self.side_effects as u64).rotate_left(17)
            ^ (self.image_placements as u64).rotate_left(29)
            ^ (self.dirty_rows as u64).rotate_left(41)
    }
}

const CASES: &[CorrectnessCase] = &[
    CorrectnessCase {
        name: "vt100_basic_wrap_tab_erase",
        feature_class: "vt100_basic",
        input: b"alpha\tomega\r\nsecond line\x1b[2Krewritten",
        expected_text: "rewritten",
    },
    CorrectnessCase {
        name: "sgr_truecolor_attributes",
        feature_class: "sgr",
        input: b"\x1b[1;3;4;9;38;2;116;199;236;48;5;236mstyled\x1b[0m plain",
        expected_text: "styled",
    },
    CorrectnessCase {
        name: "cursor_address_insert_delete",
        feature_class: "cursor_addressing",
        input: b"abcdef\x1b[1;3H\x1b[2@XY\x1b[1;6H\x1b[PZ",
        expected_text: "XY",
    },
    CorrectnessCase {
        name: "scroll_region_origin_mode",
        feature_class: "scroll_region",
        input: b"\x1b[2;6r\x1b[?6h\x1b[Hone\r\ntwo\r\nthree\r\nfour\r\nfive\x1b[?6l\x1b[r",
        expected_text: "five",
    },
    CorrectnessCase {
        name: "alternate_screen_roundtrip",
        feature_class: "alternate_screen",
        input: b"main\x1b[?1049halt\x1b[2Jalt-screen\x1b[?1049lmain-restored",
        expected_text: "main",
    },
    CorrectnessCase {
        name: "bracketed_paste_focus_mouse_modes",
        feature_class: "input_modes",
        input: b"\x1b[?2004h\x1b[?1004h\x1b[?1000h\x1b[?1006hinput modes\x1b[?1006l\x1b[?1000l\x1b[?1004l\x1b[?2004l",
        expected_text: "input modes",
    },
    CorrectnessCase {
        name: "device_status_queries",
        feature_class: "reports",
        input: b"reports\x1b[c\x1b[5n\x1b[6n\x1bP$qm\x1b\\",
        expected_text: "reports",
    },
    CorrectnessCase {
        name: "osc_title_hyperlink_clipboard",
        feature_class: "osc",
        input: b"\x1b]0;Bootty Bench\x07\x1b]8;id=bench;https://example.invalid\x1b\\link\x1b]8;;\x1b\\\x1b]52;c;Ym9vdHR5\x1b\\",
        expected_text: "link",
    },
    CorrectnessCase {
        name: "synchronized_update_section",
        feature_class: "sync_updates",
        input: b"\x1b[?2026hbatched one\r\nbatched two\x1b[?2026l",
        expected_text: "batched",
    },
];

fn terminal_engine() -> TerminalEngine {
    TerminalEngine::new(GEOMETRY).expect("terminal engine")
}

fn hash_bytes(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn hash_color(mut hash: u64, color: Option<RgbColor>) -> u64 {
    if let Some(color) = color {
        hash = hash_bytes(hash, &[color.r, color.g, color.b]);
    }
    hash
}

fn style_bits(style: &CellStyle) -> u16 {
    u16::from(style.bold)
        | (u16::from(style.italic) << 1)
        | (u16::from(style.faint) << 2)
        | (u16::from(style.blink) << 3)
        | (u16::from(style.inverse) << 4)
        | (u16::from(style.invisible) << 5)
        | (u16::from(style.strikethrough) << 6)
        | (u16::from(style.overline) << 7)
}

fn run_case(mut engine: TerminalEngine, case: &CorrectnessCase) -> u64 {
    engine.write_vt(case.input);
    let side_effects = engine.drain_side_effects();
    let frame = engine.extract_frame().expect("correctness frame");
    let mut stats = CorrectnessStats {
        cells: frame.cells.len(),
        chars: frame.text.len(),
        side_effects: side_effects.len(),
        image_placements: frame.images.placements.len(),
        dirty_rows: frame.row_dirty.iter().filter(|dirty| **dirty).count(),
        hash: hash_bytes(0xcbf2_9ce4_8422_2325, case.feature_class.as_bytes()),
    };

    for cell in &frame.cells {
        stats.hash = hash_bytes(stats.hash, &cell.x.to_le_bytes());
        stats.hash = hash_bytes(stats.hash, &cell.y.to_le_bytes());
        stats.hash = hash_color(stats.hash, cell.fg);
        stats.hash = hash_color(stats.hash, cell.bg);
        stats.hash = hash_bytes(stats.hash, &style_bits(&cell.style).to_le_bytes());
        stats.hash = hash_bytes(stats.hash, format!("{:?}", cell.style.underline).as_bytes());
        for character in frame.cell_text(cell) {
            stats.hash = hash_bytes(stats.hash, &(*character as u32).to_le_bytes());
        }
    }

    assert!(
        frame
            .text
            .iter()
            .collect::<String>()
            .contains(case.expected_text),
        "{} missing expected text {:?}",
        case.name,
        case.expected_text
    );
    stats.checksum()
}

fn bench_vt_correctness_cases(c: &mut Criterion) {
    for case in CASES {
        c.bench_function(&format!("vt_correctness_gate_{}", case.name), |b| {
            b.iter_batched(
                terminal_engine,
                |engine| black_box(run_case(engine, case)),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_vt_correctness_matrix(c: &mut Criterion) {
    c.bench_function("vt_correctness_gate_matrix_all", |b| {
        b.iter(|| {
            let mut checksum = 0_u64;
            for case in CASES {
                checksum ^= run_case(terminal_engine(), case);
            }
            black_box(checksum)
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).noise_threshold(0.20);
    targets = bench_vt_correctness_cases, bench_vt_correctness_matrix,
}
criterion_main!(benches);
