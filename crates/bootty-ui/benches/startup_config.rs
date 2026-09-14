use std::{fs, hint::black_box, path::PathBuf};

use anyhow::{Context, Result};
use bootty_config::config::{
    BoottyConfig, builtin_theme_names, load_config_from_path, resolve_theme,
    write_font_size_preference,
};
use bootty_config::{KeymapProgram, legacy_keymap_bindings};
use bootty_mux::repository::WorkspaceRepository;
use bootty_mux::session_membership::{SessionMembership, WorkspaceSession};
use bootty_ui::input::resolve_modifier_remaps;
use criterion::Criterion;

struct BenchDir {
    root: PathBuf,
}

impl BenchDir {
    fn new(name: &str) -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "bootty-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .context("clock")?
                .as_nanos()
        ));
        fs::create_dir_all(&root).context("create benchmark temp dir")?;
        Ok(Self { root })
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

impl Drop for BenchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct ConfigFixture {
    _dir: BenchDir,
    config_path: PathBuf,
    config_dir: PathBuf,
}

fn config_fixture() -> Result<ConfigFixture> {
    let dir = BenchDir::new("startup-config")?;
    let themes_dir = dir.path("themes");
    fs::create_dir_all(&themes_dir).context("create themes dir")?;
    fs::write(
        dir.path("base.toml"),
        r##"
[colors]
background = "#1a1b26"
foreground = "#c0caf5"
cursor = "#c0caf5"
selection-background = "#33467c"
palette = ["#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6", "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"]
palette-generate = true
palette-harmonious = true

[multiplexer]
backend = "native"
"##,
    )
    .context("write base config")?;
    fs::write(
        themes_dir.join("bench-theme.toml"),
        r##"
[metadata]
name = "Bench Theme"
source = "benchmark fixture"
license = "MIT"

[colors]
background = "#101014"
foreground = "#eeeeee"
cursor = "#ffffff"
cursor-text = "#000000"
palette = ["#000000", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#bbbbbb"]
"##,
    )
    .context("write theme")?;
    let config_path = dir.path("config.toml");
    fs::write(
        &config_path,
        r#"
version = 1
theme = "bench-theme"
include = ["base.toml", "?missing.toml"]

[font]
family = ["JetBrains Mono", "Apple Color Emoji"]
size = 15.0
cell-width = 9.0
cell-height = 22.0
baseline-adjustment = 0.0
underline-position = 1.0
underline-thickness = 1.0

[chrome]
sidebar = true
status-bar = true
sidebar-width = 300.0
status-height = 30.0
gap = 1.0
unfocused-sidebar-dim = 0.16
unfocused-terminal-dim = 0.08

[input]
modifier-remap = ["right_alt=alt", "right_shift=shift"]
macos-option-as-alt = "both"
keybind = ["cmd+p=session_picker", "cmd+n=new_mux_session", "performable:cmd+v=paste_from_clipboard"]
sidebar-keybind = ["j=next_session", "k=previous_session", "Enter=activate_session"]

[input.backend-keybind]
native = ["ctrl+space>c=new_tab", "ctrl+space>v=split_right", "alt+j=select_pane:down"]
tmux = ["alt+j=esc:J", "cmd+c=csi:72~"]

[session]
shell = "/bin/zsh"
working-directory = "/tmp"
term = "xterm-bootty"
colorterm = "truecolor"
env = [{ name = "BOOTTY_BENCH", value = "1" }]

[diagnostics]
stability-trace = "/tmp/bootty-stability.csv"

[window]
title = "Bootty Benchmark"
width = 1200.0
height = 900.0
fullscreen = "disabled"
window-decoration = "client"
macos-titlebar-style = "transparent"
"#,
    )
    .context("write config")?;
    Ok(ConfigFixture {
        config_dir: dir.root.clone(),
        _dir: dir,
        config_path,
    })
}

fn session_names(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("group-{}/session-{index:03}", index % 12))
        .collect()
}

fn session_membership(
    config_path: &std::path::Path,
    names: &[String],
) -> Result<SessionMembership> {
    let (_, snapshot) = WorkspaceRepository::open(config_path).context("workspace repository")?;
    let mut sessions = snapshot
        .spaces()
        .first()
        .context("workspace fixture space")?
        .binding()
        .sessions()
        .clone();
    for (index, name) in names.iter().enumerate() {
        sessions.claim(WorkspaceSession {
            identity: format!("id-{index:03}"),
            backend_name: name.clone(),
            display_name: String::new(),
            explicit: false,
            cwd: "/repo".to_owned(),
        });
    }
    Ok(sessions)
}

fn compile_keybinds(config: &BoottyConfig) -> Result<(KeymapProgram<String, ()>, usize)> {
    let bindings = legacy_keymap_bindings(config);
    let count = bindings.len();
    let (program, diagnostics) = KeymapProgram::compile(
        bindings,
        |action| Ok(action.name().map(str::to_owned)),
        |_| Ok(()),
    );
    anyhow::ensure!(
        diagnostics.is_empty(),
        "keymap diagnostics: {diagnostics:?}"
    );
    Ok((program, count))
}

fn bench_config_load(c: &mut Criterion) -> Result<()> {
    let fixture = config_fixture()?;
    let mut failure = None;
    c.bench_function("startup_config_load_includes_theme_keybinds", |b| {
        b.iter(|| {
            match load_config_from_path(black_box(&fixture.config_path))
                .context("load benchmark config")
            {
                Ok(config) => {
                    black_box(config);
                }
                Err(error) => failure = Some(error),
            }
        });
    });
    failure.map_or(Ok(()), Err)?;

    let names = builtin_theme_names().collect::<Vec<_>>();
    let mut failure = None;
    c.bench_function("startup_config_resolve_builtin_themes", |b| {
        b.iter(|| {
            let result = names.iter().try_fold(0_usize, |count, name| {
                let palette_len = resolve_theme(black_box(name), black_box(&fixture.config_dir))
                    .context("resolve built-in theme")?
                    .colors
                    .palette
                    .len();
                count
                    .checked_add(palette_len)
                    .context("palette count overflow")
            });
            match result {
                Ok(count) => {
                    black_box(count);
                }
                Err(error) => failure = Some(error),
            }
        });
    });
    failure.map_or(Ok(()), Err)?;

    let config = load_config_from_path(&fixture.config_path).context("load benchmark config")?;
    let mut failure = None;
    c.bench_function("startup_config_modifier_and_keybind_build", |b| {
        b.iter(|| {
            let result = (|| -> Result<_> {
                let remaps = resolve_modifier_remaps(&config.input.modifier_remap)
                    .context("modifier remaps")?;
                Ok((remaps, compile_keybinds(black_box(&config))?))
            })();
            match result {
                Ok(value) => {
                    black_box(value);
                }
                Err(error) => failure = Some(error),
            }
        });
    });
    failure.map_or(Ok(()), Err)?;

    let font_fixture = config_fixture()?;
    let mut tick = 0_u32;
    let mut failure = None;
    c.bench_function("startup_config_write_font_size_preference", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            let size = 12.0 + f32::from(u8::try_from(tick % 8).unwrap_or(0));
            match write_font_size_preference(black_box(&font_fixture.config_path), size)
                .context("write font size preference")
            {
                Ok(_) => {
                    black_box(size);
                }
                Err(error) => failure = Some(error),
            }
        });
    });
    failure.map_or(Ok(()), Err)?;
    Ok(())
}

fn bench_session_order(c: &mut Criterion) -> Result<()> {
    let sessions = session_names(384);
    let alive = (0..sessions.len())
        .map(|index| format!("id-{index:03}"))
        .collect::<Vec<_>>();
    let alive_refs = alive
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();

    let steady_dir = BenchDir::new("session-order-steady")?;
    let mut steady = session_membership(&steady_dir.path("config.toml"), &sessions)?;
    c.bench_function("session_order_steady_sync_384", |b| {
        b.iter(|| black_box(steady.retain_alive(black_box(&alive_refs))));
    });

    let move_dir = BenchDir::new("session-order-move")?;
    let mut moving = session_membership(&move_dir.path("config.toml"), &sessions)?;
    c.bench_function("session_order_move_session_persist_384", |b| {
        b.iter(|| {
            let moved_up = moving.move_before(black_box("id-005"), black_box(Some("id-000")));
            let moved_down = moving.move_before(black_box("id-005"), black_box(None));
            black_box((moved_up, moved_down))
        });
    });

    let mut cold_index = 0_u32;
    let mut failure = None;
    c.bench_function("session_order_cold_sync_sqlite_384", |b| {
        b.iter(|| {
            cold_index = cold_index.wrapping_add(1);
            let result = (|| -> Result<_> {
                let dir = BenchDir::new(&format!("session-order-cold-{cold_index}"))?;
                let mut store = session_membership(&dir.path("config.toml"), &sessions)?;
                Ok(store.retain_alive(&alive_refs))
            })();
            match result {
                Ok(value) => {
                    black_box(value);
                }
                Err(error) => failure = Some(error),
            }
        });
    });
    failure.map_or(Ok(()), Err)?;
    Ok(())
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .noise_threshold(0.15)
        .configure_from_args();
    bench_config_load(&mut criterion)?;
    bench_session_order(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
