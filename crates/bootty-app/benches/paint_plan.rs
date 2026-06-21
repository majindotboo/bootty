use std::hint::black_box;

mod paint_plan_fixtures;

use bootty_app::{
    config::{BoottyConfig, MultiplexerBackendConfig},
    input_binding::BindingAction,
    input_binding_set::BindingSet,
    modifier_remap::ModifierRemapSet,
    mux::{
        sidebar_meta::{
            DiffStat, ProcessStatus, SidebarMetadata, SidebarSessionMetadata,
            sidebar_metadata_sessions, sidebar_metadata_sessions_for_prefix,
        },
        snapshot::{MuxPaneAnchor, MuxSession, MuxWindow},
    },
    paint_plan::PaintPlanner,
    terminal::{
        KeyInput, KeyMods, MacosOptionAsAlt, MouseAction, MouseButton, MouseEncoderSize,
        MouseInput, TerminalKey,
    },
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
    ui::{
        chrome::{self, SidebarModel},
        icons,
        session_picker::SessionPickerDialog,
        sidebar::{build_sidebar_items, build_visible_sidebar_items},
    },
};
use bootty_winit::input::{
    InputSnapshot, WheelScrollState, terminal_input_commands_with_wheel_state,
};
use criterion::{Criterion, criterion_group, criterion_main};
use eframe::egui::{self, Pos2, Rect};
use paint_plan_fixtures::{
    mutate_single_row, prepared_scenarios, scenario_builders, surface_for, terminal_engine,
    write_agent_dashboard_frame,
};

const SIDEBAR_BENCH_SESSION_COUNTS: [usize; 3] = [24, 96, 384];
const SIDEBAR_VISIBLE_METADATA_PREFIX: usize = 42;

fn sidebar_sessions(count: usize) -> Vec<MuxSession> {
    (0..count)
        .map(|index| {
            let group = match index % 6 {
                0 => "agents",
                1 => "infra",
                2 => "app",
                3 => "research",
                4 => "review",
                _ => "ops",
            };
            let id = format!("${}", index + 1);
            let anchor = MuxPaneAnchor {
                session_id: id.clone(),
                pane_id: Some(format!("%{}", index + 10)),
                cwd: Some("/Users/luan/src/bootty".to_owned()),
                process: Some(
                    match index % 4 {
                        0 => "codex",
                        1 => "cargo",
                        2 => "nvim",
                        _ => "zsh",
                    }
                    .to_owned(),
                ),
            };
            MuxSession {
                id: id.clone(),
                name: format!("{group}/session-{index:03}"),
                active: index == 0,
                anchor: anchor.clone(),
                active_window_id: None,
                windows: (0..3)
                    .map(|window| MuxWindow {
                        id: format!("@{}:{window}", index + 1),
                        index: window,
                        name: format!("window-{window}"),
                        active: window == 0,
                        anchor: anchor.clone(),
                    })
                    .collect(),
            }
        })
        .collect()
}

fn native_sidebar_sessions_without_metadata(count: usize) -> Vec<MuxSession> {
    (0..count)
        .map(|index| {
            let id = format!("local-{index}");
            let anchor = MuxPaneAnchor {
                session_id: id.clone(),
                pane_id: None,
                cwd: None,
                process: Some("zsh".to_owned()),
            };
            MuxSession {
                id,
                name: format!("native/session-{index:03}"),
                active: index == 0,
                anchor,
                active_window_id: None,
                windows: Vec::new(),
            }
        })
        .collect()
}

fn sidebar_metadata_for(sessions: &[MuxSession]) -> SidebarMetadata {
    let mut metadata = SidebarMetadata::default();
    for (index, session) in sessions.iter().enumerate() {
        metadata.insert(
            session.name.clone(),
            SidebarSessionMetadata {
                branch: Some(format!("feature/sidebar-{index:03}")),
                diff: Some(DiffStat {
                    added: (index as u32) % 40,
                    removed: (index as u32) % 17,
                }),
                attention: index % 11 == 0,
                status: Some(format!("working batch {}", index % 9)),
                progress: Some(((index * 7) % 101) as u8),
                process_cpu: Some(format!("{:.1}%", (index % 16) as f32 * 1.7)),
                agent_status: (index % 3 == 0).then(|| "codex Working...".to_owned()),
                processes: vec![ProcessStatus {
                    name: session
                        .anchor
                        .process
                        .clone()
                        .unwrap_or_else(|| "shell".to_owned()),
                    cpu_pct: (index % 16) as f32 * 1.7,
                    mem_bytes: 128 * 1024 * 1024 + index as u64 * 1024 * 1024,
                }],
            },
        );
    }
    metadata
}

fn usage_lines_plain() -> Vec<String> {
    vec![
        "terminal 5h 90% +38m".to_owned(),
        "agent 7d 73% +1d06:20 ↺3d20:18".to_owned(),
        "build 50m 42% +12m".to_owned(),
        "overflow 1h 10% +1m".to_owned(),
    ]
}

fn usage_lines_ansi() -> Vec<String> {
    vec![
        "\x1b[38;2;116;199;236m 5h 90% +38m\x1b[0m".to_owned(),
        "\x1b[38;2;249;226;175m████████░░\x1b[0m".to_owned(),
        "\x1b[38;2;166;227;161magent 7d 73% +1d06:20 ↺3d20:18\x1b[0m".to_owned(),
        "\x1b[38;2;137;220;235mbuild 50m 42% +12m\x1b[0m".to_owned(),
        "\x1b[38;2;243;139;168moverflow 1h 10% +1m\x1b[0m".to_owned(),
    ]
}

fn default_native_keybinds() -> Vec<String> {
    BoottyConfig::default()
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native)
}

fn key_input(key: TerminalKey, mods: KeyMods) -> KeyInput {
    KeyInput {
        key,
        mods,
        repeat: false,
        utf8: None,
        unshifted: None,
    }
}

fn binding_lookup_inputs() -> Vec<KeyInput> {
    vec![
        key_input(
            TerminalKey::Space,
            KeyMods {
                ctrl: true,
                ..Default::default()
            },
        ),
        key_input(
            TerminalKey::V,
            KeyMods {
                command: true,
                ..Default::default()
            },
        ),
        key_input(
            TerminalKey::J,
            KeyMods {
                alt: true,
                ..Default::default()
            },
        ),
        key_input(
            TerminalKey::Tab,
            KeyMods {
                ctrl: true,
                ..Default::default()
            },
        ),
        KeyInput {
            key: TerminalKey::A,
            mods: KeyMods::default(),
            repeat: false,
            utf8: Some("a"),
            unshifted: Some('a'),
        },
    ]
}

fn input_burst_events() -> Vec<egui::Event> {
    let keys = [
        egui::Key::C,
        egui::Key::D,
        egui::Key::J,
        egui::Key::K,
        egui::Key::ArrowUp,
        egui::Key::ArrowDown,
    ];
    let mut events = Vec::with_capacity(320);
    for index in 0..64 {
        let pos = Pos2::new(24.0 + (index % 80) as f32, 40.0 + (index % 36) as f32);
        events.push(egui::Event::Text(format!("input-{index:03}")));
        events.push(egui::Event::Key {
            key: keys[index % keys.len()],
            physical_key: None,
            pressed: true,
            repeat: index % 5 == 0,
            modifiers: egui::Modifiers {
                ctrl: index % 2 == 0,
                alt: index % 3 == 0,
                shift: index % 7 == 0,
                ..Default::default()
            },
        });
        events.push(egui::Event::PointerMoved(pos));
        events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: index % 2 == 0,
            modifiers: egui::Modifiers::default(),
        });
        events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 11.0 + (index % 3) as f32),
            modifiers: egui::Modifiers::default(),
            phase: egui::TouchPhase::Move,
        });
    }
    events
}

fn input_snapshot(events: Vec<egui::Event>) -> InputSnapshot {
    InputSnapshot {
        events,
        modifiers: egui::Modifiers::default(),
        modifier_sides: Default::default(),
        hover_pos: Some(Pos2::new(64.0, 64.0)),
        pressed_mouse_button: Some(MouseButton::Left),
        surface: Some(surface_for(120, 40)),
        mouse_exclusion: Some(Rect::from_min_max(
            Pos2::new(2_000.0, 2_000.0),
            Pos2::new(2_100.0, 2_100.0),
        )),
    }
}

fn encode_mouse_input() -> MouseInput {
    MouseInput {
        action: MouseAction::Press,
        button: Some(MouseButton::Left),
        mods: KeyMods {
            ctrl: true,
            ..Default::default()
        },
        x: 42.0,
        y: 84.0,
        size: MouseEncoderSize {
            screen_width: 1080,
            screen_height: 880,
            cell_width: 9,
            cell_height: 22,
            padding_left: 10,
            padding_top: 10,
            padding_right: 10,
            padding_bottom: 10,
        },
    }
}

fn bench_paint_plan(c: &mut Criterion) {
    for scenario in prepared_scenarios() {
        let mut planner = PaintPlanner::default();
        c.bench_function(&format!("paint_plan_{}", scenario.name), |b| {
            b.iter(|| {
                let plan = planner.plan(scenario.surface, &scenario.frame, 16.0);
                black_box(
                    plan.text_runs
                        .iter()
                        .map(|run| run.text.len())
                        .sum::<usize>(),
                )
            })
        });
    }
}

fn bench_extract_frame(c: &mut Criterion) {
    for (name, builder) in scenario_builders() {
        let mut engine = builder();
        c.bench_function(&format!("extract_frame_clean_steady_state_{name}"), |b| {
            b.iter(|| black_box(engine.extract_frame().expect("frame").stats.cells))
        });
    }
}

fn bench_extract_frame_one_row_mutate(c: &mut Criterion) {
    for (name, builder) in scenario_builders() {
        let mut engine = builder();
        let mut tick = 0_u32;
        c.bench_function(&format!("extract_frame_one_row_mutate_{name}"), |b| {
            b.iter(|| {
                tick = tick.wrapping_add(1);
                mutate_single_row(&mut engine, tick);
                black_box(engine.extract_frame().expect("frame").stats.dirty_rows)
            })
        });
    }
}

fn bench_terminal_write_vt(c: &mut Criterion) {
    let mut plain = terminal_engine(120, 40);
    c.bench_function("terminal_write_vt_plain_carriage_return", |b| {
        b.iter(|| {
            plain.write_vt(black_box(b"plain terminal output\r"));
            black_box(plain.grid_size())
        })
    });

    let mut ansi = terminal_engine(120, 40);
    c.bench_function("terminal_write_vt_ansi_csi_color", |b| {
        b.iter(|| {
            ansi.write_vt(black_box(
                b"\x1b[1;1H\x1b[38;2;1;2;3mcolored terminal output\x1b[0m",
            ));
            black_box(ansi.grid_size())
        })
    });
}

fn bench_terminal_input_pipeline(c: &mut Criterion) {
    let events = input_burst_events();
    let remaps = ModifierRemapSet::default();
    let mut wheel_state = WheelScrollState::default();

    c.bench_function("terminal_input_events_to_commands_mixed_burst", |b| {
        b.iter(|| {
            let snapshot = input_snapshot(black_box(events.clone()));
            black_box(
                terminal_input_commands_with_wheel_state(
                    snapshot,
                    black_box(&remaps),
                    black_box(MacosOptionAsAlt::Both),
                    black_box(&mut wheel_state),
                )
                .len(),
            )
        })
    });

    let keybinds = default_native_keybinds();
    c.bench_function("keybinding_parse_default_native_config", |b| {
        b.iter(|| {
            let mut set = BindingSet::default();
            for entry in &keybinds {
                set.parse_and_put(black_box(entry.as_str()))
                    .expect("default keybind parses");
            }
            black_box(set.get_trigger(black_box(&BindingAction::NewTab)).is_some())
        })
    });

    let mut binding_set = BindingSet::default();
    for entry in default_native_keybinds() {
        binding_set
            .parse_and_put(&entry)
            .expect("default keybind parses");
    }
    let inputs = binding_lookup_inputs();
    c.bench_function("keybinding_lookup_default_native_config", |b| {
        b.iter(|| {
            let mut hits = 0_u8;
            for input in &inputs {
                if binding_set.get_event(black_box(*input)).is_some() {
                    hits = hits.saturating_add(1);
                }
            }
            black_box(hits)
        })
    });

    let mut engine = terminal_engine(120, 40);
    let keys = binding_lookup_inputs();
    let mouse = encode_mouse_input();
    let mut out = Vec::with_capacity(4096);
    c.bench_function("terminal_encode_keys_mouse_paste_burst", |b| {
        b.iter(|| {
            for key in &keys {
                out.clear();
                engine
                    .encode_key_to_vec(black_box(*key), black_box(&mut out))
                    .expect("encode key");
                black_box(out.len());
            }
            out.clear();
            engine
                .encode_mouse_to_vec(black_box(mouse), black_box(&mut out))
                .expect("encode mouse");
            black_box(out.len());
            out.clear();
            engine
                .encode_paste_to_vec(
                    black_box("paste burst with unicode 🥟"),
                    black_box(&mut out),
                )
                .expect("encode paste");
            black_box(out.len())
        })
    });
}

fn bench_render_commands(c: &mut Criterion) {
    for scenario in prepared_scenarios() {
        let mut planner = PaintPlanner::default();
        let plan = planner
            .plan(scenario.surface, &scenario.frame, 16.0)
            .clone();
        let text_contract =
            TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());

        c.bench_function(&format!("render_commands_{}", scenario.name), |b| {
            b.iter(|| {
                black_box(
                    TerminalRenderFrame::from_plan(&plan, &text_contract)
                        .commands
                        .len(),
                )
            })
        });
    }
}

fn bench_sidebar_items(c: &mut Criterion) {
    for count in SIDEBAR_BENCH_SESSION_COUNTS {
        let sessions = sidebar_sessions(count);
        let metadata = sidebar_metadata_for(&sessions);
        let selected = sessions
            .get(count / 2)
            .map(|session| session.id.as_str())
            .unwrap_or("$1");
        c.bench_function(&format!("sidebar_items_{count}_rich_sessions"), |b| {
            b.iter(|| {
                black_box(build_sidebar_items(
                    black_box(&sessions),
                    black_box(Some(selected)),
                    black_box(&metadata),
                ))
                .len()
            })
        });
    }
}

fn bench_visible_sidebar_items(c: &mut Criterion) {
    const VISIBLE_ROWS: usize = 42;

    for count in SIDEBAR_BENCH_SESSION_COUNTS {
        let sessions = sidebar_sessions(count);
        let metadata = sidebar_metadata_for(&sessions);
        let selected = sessions
            .get(count / 2)
            .map(|session| session.id.as_str())
            .unwrap_or("$1");
        c.bench_function(
            &format!("visible_sidebar_items_{count}_rich_sessions"),
            |b| {
                b.iter(|| {
                    black_box(build_visible_sidebar_items(
                        black_box(&sessions),
                        black_box(Some(selected)),
                        black_box(&metadata),
                        black_box(VISIBLE_ROWS),
                    ))
                    .len()
                })
            },
        );
    }
}

fn bench_sidebar_metadata_request(c: &mut Criterion) {
    for count in SIDEBAR_BENCH_SESSION_COUNTS {
        let sessions = sidebar_sessions(count);
        c.bench_function(
            &format!("sidebar_metadata_request_{count}_rich_sessions"),
            |b| b.iter(|| black_box(sidebar_metadata_sessions(black_box(&sessions))).len()),
        );
        c.bench_function(
            &format!("sidebar_metadata_request_{count}_rich_sessions_visible_prefix"),
            |b| {
                b.iter(|| {
                    black_box(sidebar_metadata_sessions_for_prefix(
                        black_box(&sessions),
                        black_box(SIDEBAR_VISIBLE_METADATA_PREFIX),
                    ))
                    .len()
                })
            },
        );

        let native_sessions = native_sidebar_sessions_without_metadata(count);
        c.bench_function(
            &format!("sidebar_metadata_request_{count}_native_no_metadata"),
            |b| b.iter(|| black_box(sidebar_metadata_sessions(black_box(&native_sessions))).len()),
        );
    }
}

fn bench_sidebar_ui(c: &mut Criterion) {
    for count in SIDEBAR_BENCH_SESSION_COUNTS {
        let sessions = sidebar_sessions(count);
        let metadata = sidebar_metadata_for(&sessions);
        let selected = sessions
            .get(count / 2)
            .map(|session| session.id.as_str())
            .unwrap_or("$1");
        let context = egui::Context::default();
        icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(280.0, 900.0));

        c.bench_function(&format!("sidebar_ui_{count}_rich_sessions"), |b| {
            b.iter(|| {
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen_rect),
                        events: vec![egui::Event::PointerMoved(Pos2::new(32.0, 180.0))],
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show_inside(ui, |ui| {
                            black_box(chrome::show_sidebar(
                                ui,
                                bootty_ui::ThemePalette::default(),
                                900.0,
                                SidebarModel {
                                    sessions: black_box(&sessions),
                                    selected_session: black_box(Some(selected)),
                                    metadata: black_box(&metadata),
                                    title_visible: true,
                                    reserve_titlebar_buttons: true,
                                    title_icon: None,
                                    top_inset: 0.0,
                                    border_visible: true,
                                    separator_visible: true,
                                    focused: false,
                                    hovered_session: None,
                                    unfocused_dim: 0.0,
                                    hover_override: None,
                                    current_override: None,
                                    border_override: None,
                                },
                            ));
                        });
                    },
                );
                black_box(output.shapes.len())
            })
        });
    }
}

fn bench_sidebar_ui_usage_footer(c: &mut Criterion) {
    let count = 96;
    let sessions = sidebar_sessions(count);
    let selected = sessions
        .get(count / 2)
        .map(|session| session.id.as_str())
        .unwrap_or("$1");
    let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(280.0, 900.0));

    for (name, usage_lines) in [
        ("plain_usage_footer", usage_lines_plain()),
        ("ansi_usage_footer", usage_lines_ansi()),
    ] {
        let mut metadata = sidebar_metadata_for(&sessions);
        metadata.set_usage_lines(usage_lines);
        let context = egui::Context::default();
        icons::install_icon_fonts(&context);
        c.bench_function(&format!("sidebar_ui_96_rich_sessions_{name}"), |b| {
            b.iter(|| {
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen_rect),
                        events: vec![egui::Event::PointerMoved(Pos2::new(32.0, 180.0))],
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show_inside(ui, |ui| {
                            black_box(chrome::show_sidebar(
                                ui,
                                bootty_ui::ThemePalette::default(),
                                900.0,
                                SidebarModel {
                                    sessions: black_box(&sessions),
                                    selected_session: black_box(Some(selected)),
                                    metadata: black_box(&metadata),
                                    title_visible: true,
                                    reserve_titlebar_buttons: true,
                                    title_icon: None,
                                    top_inset: 0.0,
                                    border_visible: true,
                                    separator_visible: true,
                                    focused: false,
                                    hovered_session: None,
                                    unfocused_dim: 0.0,
                                    hover_override: None,
                                    current_override: None,
                                    border_override: None,
                                },
                            ));
                        });
                    },
                );
                black_box(output.shapes.len())
            })
        });
    }
}

fn bench_session_picker_ui(c: &mut Criterion) {
    let sessions = sidebar_sessions(384);
    let selected = sessions
        .get(sessions.len() / 2)
        .map(|session| session.id.as_str())
        .unwrap_or("$1");
    let context = egui::Context::default();
    icons::install_icon_fonts(&context);
    let theme = bootty_ui::Theme::new(bootty_ui::ThemePalette::default());
    let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 900.0));

    c.bench_function("session_picker_ui_384_sessions_unfiltered", |b| {
        b.iter(|| {
            let mut dialog = SessionPickerDialog::open();
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events: vec![egui::Event::PointerMoved(Pos2::new(600.0, 450.0))],
                    ..Default::default()
                },
                |ui| {
                    black_box(dialog.show(
                        ui.ctx(),
                        black_box(theme),
                        black_box(&sessions),
                        black_box(Some(selected)),
                    ));
                },
            );
            black_box(output.shapes.len())
        })
    });
}

fn bench_animated_agent_pipeline(c: &mut Criterion) {
    let mut engine = terminal_engine(160, 60);
    let surface = surface_for(160, 60);
    let mut planner = PaintPlanner::default();
    let mut tick = 0_u32;

    c.bench_function("animated_agent_update_extract_plan_render_160x60", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            write_agent_dashboard_frame(&mut engine, tick, 160, 60);
            let frame = engine.extract_frame().expect("frame");
            let plan = planner.plan(surface, frame, 16.0).clone();
            let text_contract = TerminalTextContract::for_terminal_paint_plan(
                &plan,
                &TerminalTextConfig::default(),
            );
            black_box(
                TerminalRenderFrame::from_plan(&plan, &text_contract)
                    .commands
                    .len(),
            )
        })
    });
}

criterion_group!(
name = benches;
// These benches cover CPU planning, terminal extraction, input routing,
// render command building, and egui chrome. WGPU preparation lives in paint_plan_wgpu.
config = Criterion::default().noise_threshold(0.15);
targets =
    bench_paint_plan,
    bench_extract_frame,
    bench_extract_frame_one_row_mutate,
    bench_terminal_write_vt,
    bench_terminal_input_pipeline,
    bench_render_commands,
    bench_sidebar_items,
    bench_visible_sidebar_items,
    bench_sidebar_metadata_request,
    bench_sidebar_ui,
    bench_sidebar_ui_usage_footer,
    bench_session_picker_ui,
    bench_animated_agent_pipeline,
);
criterion_main!(benches);
