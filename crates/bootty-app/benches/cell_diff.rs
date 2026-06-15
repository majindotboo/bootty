use std::hint::black_box;

use bootty_app::{
    geometry::TerminalGeometry,
    terminal::{RenderCell, RenderFrame, TerminalEngine},
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use libghostty_vt::style::{RgbColor, Underline};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 80,
    rows: 24,
    cell_width: 9,
    cell_height: 22,
};

struct ExpectedCell {
    x: u16,
    y: u16,
    text: &'static str,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    bold: bool,
    underline: bool,
    hyperlink: Option<&'static str>,
}

struct CellDiffCase {
    name: &'static str,
    input: &'static [u8],
    expected_cells: &'static [ExpectedCell],
    expected_cursor: Option<(u16, u16)>,
    expected_scrollbar: bool,
}

#[derive(Default)]
struct DiffStats {
    checked_cells: usize,
    mismatched_cells: usize,
    mismatched_modes: usize,
    unsupported_features: usize,
    crash: bool,
    timeout: bool,
    hash: u64,
}

impl DiffStats {
    fn checksum(&self) -> u64 {
        self.hash
            ^ self.checked_cells as u64
            ^ (self.mismatched_cells as u64).rotate_left(7)
            ^ (self.mismatched_modes as u64).rotate_left(13)
            ^ (self.unsupported_features as u64).rotate_left(19)
            ^ u64::from(self.crash).rotate_left(29)
            ^ u64::from(self.timeout).rotate_left(31)
    }
}

const RGB_BLUE: RgbColor = RgbColor {
    r: 116,
    g: 199,
    b: 236,
};
const RGB_BG: RgbColor = RgbColor {
    r: 30,
    g: 41,
    b: 59,
};

const CASES: &[CellDiffCase] = &[
    CellDiffCase {
        name: "plain_grid_and_cursor",
        input: b"alpha\r\n\x1b[10;20Hcursor-here",
        expected_cells: &[
            ExpectedCell {
                x: 0,
                y: 0,
                text: "a",
                fg: None,
                bg: None,
                bold: false,
                underline: false,
                hyperlink: None,
            },
            ExpectedCell {
                x: 19,
                y: 9,
                text: "c",
                fg: None,
                bg: None,
                bold: false,
                underline: false,
                hyperlink: None,
            },
        ],
        expected_cursor: Some((30, 9)),
        expected_scrollbar: true,
    },
    CellDiffCase {
        name: "style_color_attributes",
        input: b"\x1b[1;4;38;2;116;199;236;48;2;30;41;59mstyled\x1b[0m",
        expected_cells: &[
            ExpectedCell {
                x: 0,
                y: 0,
                text: "s",
                fg: Some(RGB_BLUE),
                bg: Some(RGB_BG),
                bold: true,
                underline: true,
                hyperlink: None,
            },
            ExpectedCell {
                x: 5,
                y: 0,
                text: "d",
                fg: Some(RGB_BLUE),
                bg: Some(RGB_BG),
                bold: true,
                underline: true,
                hyperlink: None,
            },
        ],
        expected_cursor: Some((6, 0)),
        expected_scrollbar: true,
    },
    CellDiffCase {
        name: "hyperlink_span",
        input: b"\x1b]8;id=doc;https://example.invalid/doc\x1b\\docs\x1b]8;;\x1b\\ plain",
        expected_cells: &[
            ExpectedCell {
                x: 0,
                y: 0,
                text: "d",
                fg: None,
                bg: None,
                bold: false,
                underline: false,
                hyperlink: Some("https://example.invalid/doc"),
            },
            ExpectedCell {
                x: 3,
                y: 0,
                text: "s",
                fg: None,
                bg: None,
                bold: false,
                underline: false,
                hyperlink: Some("https://example.invalid/doc"),
            },
            ExpectedCell {
                x: 5,
                y: 0,
                text: "p",
                fg: None,
                bg: None,
                bold: false,
                underline: false,
                hyperlink: None,
            },
        ],
        expected_cursor: Some((10, 0)),
        expected_scrollbar: true,
    },
    CellDiffCase {
        name: "alternate_screen_restores_main",
        input: b"main\x1b[?1049h\x1b[2Jalternate\x1b[?1049lrestored",
        expected_cells: &[
            ExpectedCell {
                x: 0,
                y: 0,
                text: "m",
                fg: None,
                bg: None,
                bold: false,
                underline: false,
                hyperlink: None,
            },
            ExpectedCell {
                x: 4,
                y: 0,
                text: "r",
                fg: None,
                bg: None,
                bold: false,
                underline: false,
                hyperlink: None,
            },
        ],
        expected_cursor: Some((12, 0)),
        expected_scrollbar: true,
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

fn cell_at<'a>(frame: &'a RenderFrame, expected: &ExpectedCell) -> Option<&'a RenderCell> {
    frame
        .cells
        .iter()
        .find(|cell| cell.x == expected.x && cell.y == expected.y)
}

fn cell_matches(frame: &RenderFrame, cell: &RenderCell, expected: &ExpectedCell) -> bool {
    let text = frame.cell_text(cell).iter().collect::<String>();
    text == expected.text
        && cell.fg == expected.fg
        && cell.bg == expected.bg
        && cell.style.bold == expected.bold
        && (cell.style.underline != Underline::None) == expected.underline
        && cell.hyperlink.as_deref() == expected.hyperlink
}

fn run_case(mut engine: TerminalEngine, case: &CellDiffCase) -> u64 {
    let mut stats = DiffStats::default();
    engine.write_vt(case.input);
    let frame = match engine.extract_frame() {
        Ok(frame) => frame,
        Err(_) => {
            stats.crash = true;
            return stats.checksum();
        }
    };

    stats.hash = hash_bytes(0xcbf2_9ce4_8422_2325, case.name.as_bytes());
    for expected in case.expected_cells {
        stats.checked_cells += 1;
        match cell_at(frame, expected) {
            Some(cell) if cell_matches(frame, cell, expected) => {
                stats.hash = hash_bytes(stats.hash, expected.text.as_bytes());
            }
            _ => stats.mismatched_cells += 1,
        }
    }

    if let Some((x, y)) = case.expected_cursor {
        match frame.cursor {
            Some(cursor) if cursor.x == x && cursor.y == y => {}
            _ => stats.mismatched_modes += 1,
        }
    }
    if frame.scrollbar.is_some() != case.expected_scrollbar {
        stats.mismatched_modes += 1;
    }

    assert_eq!(stats.mismatched_cells, 0, "{} cell mismatches", case.name);
    assert_eq!(
        stats.mismatched_modes, 0,
        "{} mode mismatches cursor={:?} scrollbar={:?}",
        case.name, frame.cursor, frame.scrollbar
    );
    stats.checksum()
}
fn bench_cell_diff_cases(c: &mut Criterion) {
    for case in CASES {
        c.bench_function(&format!("cell_diff_gate_{}", case.name), |b| {
            b.iter_batched(
                terminal_engine,
                |engine| black_box(run_case(engine, case)),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_cell_diff_matrix(c: &mut Criterion) {
    c.bench_function("cell_diff_gate_matrix_all", |b| {
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
    targets = bench_cell_diff_cases, bench_cell_diff_matrix,
}
criterion_main!(benches);
