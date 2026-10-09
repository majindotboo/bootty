//! App-owned projection and intent application for the GPUI chrome.

use num_traits::ToPrimitive as _;
use std::collections::{HashMap, HashSet};

use crate::chrome_projection::{MuxView, SessionProgressView, SessionView, WindowView};
use crate::gpui::chrome::{
    ChromeIntent, ChromeLayout, ChromePalette, ChromeSnapshot, NativeChromeAction, Rgba,
    SessionContextAction, SessionContextSnapshot, SessionTarget, SidebarAgent, SidebarPosition,
    SidebarProject, SidebarRow, SidebarRowKind, SidebarSnapshot, SidebarTask, SpaceKey,
    SpaceSnapshot, StatusAlignment, StatusBarSnapshot, StatusIntent, StatusItemSnapshot,
    StatusProgress, StatusSegmentSnapshot, TabContextAction, TabContextSnapshot, TitlebarSnapshot,
    UsageMeterSnapshot,
};
use crate::{
    clock::ClockSnapshot,
    metrics::MetricsService,
    usage::{QuotaTone, UsageProvider, UsageService},
};
use bootty_mux::repository::DEFAULT_SPACE_COLOR;

use crate::{
    commands::ExactMuxTarget,
    state::{AppEffect, AppState, ExactMuxAction},
};
use bootty_config::config::OpenBehavior;
use bootty_mux::workspace::{ScopedSessionTarget, TerminalProgressState};

type RemoteGitCache = bootty_git::GitFactsCache<
    bootty_host::remote::RemoteCommandRunner<bootty_host::SystemCommandRunner>,
>;

pub struct NativeChrome {
    pub(crate) local_git: bootty_git::GitFactsCache,
    remote_git: Vec<(
        bootty_config::config::RemoteConfig,
        RemoteGitCache,
        std::time::Instant,
    )>,
    artwork: crate::project_artwork::ProjectArtworkCache,
    pub(crate) metrics: MetricsService,
    pub(crate) usage: UsageService,
    pub(crate) clock: ClockSnapshot,
}

impl Default for NativeChrome {
    fn default() -> Self {
        Self {
            local_git: bootty_git::GitFactsCache::new(),
            remote_git: Vec::new(),
            artwork: crate::project_artwork::ProjectArtworkCache::default(),
            metrics: MetricsService::default(),
            usage: UsageService::default(),
            clock: ClockSnapshot::now(),
        }
    }
}

impl NativeChrome {
    pub(crate) fn refresh(&mut self, now: std::time::Instant, usage_visible: bool) {
        self.local_git.prune(now);
        self.remote_git.retain(|(_, cache, seen)| {
            let current = now.saturating_duration_since(*seen) < std::time::Duration::from_mins(5);
            if current {
                cache.prune(now);
            } else {
                cache.retire();
            }
            current
        });
        self.metrics.refresh(now);
        if usage_visible {
            self.usage.refresh(now);
        }
        self.clock = ClockSnapshot::now();
    }
}

impl Drop for NativeChrome {
    fn drop(&mut self) {
        self.local_git.retire();
        for (_, cache, _) in &self.remote_git {
            cache.retire();
        }
    }
}

macro_rules! ui_color {
    ($color:expr) => {{
        let color = $color;
        Rgba {
            red: color.red,
            green: color.green,
            blue: color.blue,
            alpha: color.alpha,
        }
    }};
}

pub struct ChromeProjection {
    pub(crate) mux: MuxView,
    session_facts: HashMap<String, bootty_git::GitSessionFacts>,
    tab_contexts: HashMap<String, TabContextSnapshot>,
}

pub fn prepare(
    state: &AppState,
    native: &mut NativeChrome,
    sidebar_visible: bool,
    window_focused: bool,
) -> ChromeProjection {
    let selected_session = state.mux().selected_session();
    let sessions = state.mux().sessions();
    let display_names = state.session_display_names(sessions);
    let selected_window = state.mux().selected_window();
    let mut windows = Vec::new();
    let mut tab_contexts = HashMap::new();
    let mut selected_name = None;
    let mut selected_color = None;
    let mut session_views = Vec::with_capacity(sessions.len());
    for (session, display_name) in sessions.iter().zip(display_names) {
        let selected = selected_session.map_or(session.active, |selected| {
            selected == session.id || selected == session.name
        });
        if selected {
            selected_name = Some(if display_name.is_empty() {
                session.name.clone()
            } else {
                display_name.clone()
            });
            (windows, tab_contexts) = selected_windows(state, session, selected_window);
        }
        session_views.push(session_view(state, session, display_name, selected));
    }
    let binding = &state.workspace.active.binding;
    for saved in binding.sessions().sessions() {
        if binding.session_attachment(&saved.identity).is_none() {
            session_views.push(SessionView {
                id: saved.identity.clone(),
                identity: Some(saved.identity.clone()),
                detached: true,
                name: saved.backend_name.clone(),
                display_name: saved.label().to_owned(),
                cwd: (!saved.cwd.is_empty()).then(|| saved.cwd.clone()),
                ..SessionView::default()
            });
        }
    }
    let saved_order = binding
        .sessions()
        .sessions()
        .iter()
        .enumerate()
        .map(|(index, saved)| (saved.identity.as_str(), index))
        .collect::<HashMap<_, _>>();
    session_views.sort_by_key(|session| {
        session
            .identity
            .as_deref()
            .and_then(|id| saved_order.get(id))
            .copied()
            .unwrap_or(usize::MAX)
    });
    // Saved order owns color slots, independent of backend attachment and presentation sorting.
    let colors = session_label_colors(session_views.iter().map(|session| {
        if session.display_name.is_empty() {
            session.name.as_str()
        } else {
            session.display_name.as_str()
        }
    }));
    for (session, (color, dim_color)) in session_views.iter_mut().zip(colors) {
        if session.selected {
            selected_color = Some(color.clone());
        }
        session.color = Some(color);
        session.dim_color = Some(dim_color);
    }
    let scope = state.mux_scope();
    let session_facts = prepare_session_facts(state, native, &session_views);
    ChromeProjection {
        session_facts,
        mux: MuxView {
            windows,
            sessions: session_views,
            scope_key: scope.persistence_value().to_string(),
            session: selected_name,
            sidebar_visible,
            session_color: selected_color,
            keep_awake: state.keep_awake_active(),
            focused: window_focused,
        },
        tab_contexts,
    }
}

fn selected_windows(
    state: &AppState,
    session: &bootty_mux::snapshot::MuxSession,
    selected_window: Option<&str>,
) -> (Vec<WindowView>, HashMap<String, TabContextSnapshot>) {
    let mut windows = Vec::new();
    let mut tab_contexts = HashMap::new();
    let mut ordered = session.windows.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|window| window.index);
    for (index, window) in ordered.iter().enumerate() {
        let active = selected_window == Some(window.id.as_str())
            || (selected_window.is_none() && window.active);
        let progress = (!active).then(|| state.window_progress(window)).flatten();
        windows.push(WindowView {
            id: window.id.clone(),
            index: window.index,
            name: window.name.clone(),
            active,
            progress,
            progress_indeterminate: progress.is_some()
                && state.window_has_indeterminate_progress(window),
        });
        tab_contexts.insert(
            window.id.clone(),
            TabContextSnapshot {
                session_id: session.id.clone(),
                window_id: window.id.clone(),
                pane_actions: pane_menu_commands(state, session, window, selected_window),
                can_activate: !active,
                can_move_left: index > 0,
                can_move_right: index.saturating_add(1) < ordered.len(),
                can_navigate: ordered.len() > 1,
                can_close_pane: state
                    .workspace
                    .active
                    .binding
                    .window_focused_pane(&session.id, &window.id)
                    .is_some(),
            },
        );
    }
    (windows, tab_contexts)
}

fn session_view(
    state: &AppState,
    session: &bootty_mux::snapshot::MuxSession,
    display_name: String,
    selected: bool,
) -> SessionView {
    let progress = session
        .windows
        .iter()
        .filter_map(|window| state.window_progress(window))
        .max();
    let progress_indeterminate = progress.is_some()
        && session
            .windows
            .iter()
            .any(|window| state.window_has_indeterminate_progress(window));
    let mut reported_panes = HashSet::new();
    let mut pane_ids = Vec::new();
    let mut progresses = Vec::new();
    for window in &session.windows {
        for pane in std::iter::once(&window.anchor).chain(&window.panes) {
            let Some(pane_id) = pane.pane_id.as_deref() else {
                continue;
            };
            if !reported_panes.insert(pane_id) {
                continue;
            }
            pane_ids.push(pane_id.to_owned());
            if let Some(progress) = state.pane_progress(pane_id) {
                progresses.push(SessionProgressView {
                    process: pane
                        .process
                        .clone()
                        .unwrap_or_else(|| "terminal".to_owned()),
                    value: progress.value.unwrap_or(50),
                    indeterminate: progress.state == TerminalProgressState::Indeterminate,
                });
            }
        }
    }
    SessionView {
        id: session.id.clone(),
        identity: session.tag.identity.clone(),
        detached: false,
        name: session.name.clone(),
        display_name,
        active: session.active,
        selected,
        cwd: session
            .tag
            .identity
            .as_deref()
            .and_then(|identity| state.workspace.active.binding.sessions().get(identity))
            .filter(|saved| !saved.cwd.is_empty())
            .map(|saved| saved.cwd.clone())
            .or_else(|| session.anchor.cwd.clone()),
        pane_id: session.anchor.pane_id.clone(),
        pane_pid: session.anchor.pane_pid,
        process: session.anchor.process.clone(),
        pane_ids,
        color: None,
        dim_color: None,
        progress,
        progress_indeterminate,
        progresses,
        ports: state.session_ports(session),
    }
}

fn prepare_session_facts(
    state: &AppState,
    native: &mut NativeChrome,
    sessions: &[SessionView],
) -> HashMap<String, bootty_git::GitSessionFacts> {
    let scope = state.mux_scope().persistence_value().to_string();
    let now = std::time::Instant::now();
    let Some(remote) = state.active_multiplexer().remote.as_ref() else {
        return session_facts(sessions, &scope, &native.local_git, now);
    };
    if let Some((_, cache, seen)) = native
        .remote_git
        .iter_mut()
        .find(|(config, _, _)| config == remote)
    {
        *seen = now;
        return session_facts(sessions, &scope, cache, now);
    }
    let cache = bootty_git::GitFactsCache::with_remote_runner(
        bootty_host::remote::RemoteCommandRunner::new(
            bootty_host::remote::RemoteHost::new(remote.clone()),
            bootty_host::SystemCommandRunner,
        ),
    );
    let facts = session_facts(sessions, &scope, &cache, now);
    native.remote_git.push((remote.clone(), cache, now));
    facts
}

fn session_facts<R: bootty_git::CommandRunner + Clone + Send + Sync + 'static>(
    sessions: &[SessionView],
    scope: &str,
    cache: &bootty_git::GitFactsCache<R>,
    now: std::time::Instant,
) -> HashMap<String, bootty_git::GitSessionFacts> {
    sessions
        .iter()
        .map(|session| {
            let input = bootty_git::GitSessionFactsInput {
                scope_key: scope.to_owned(),
                session_id: session.id.clone(),
                cwd: session.cwd.clone(),
                pane_pid: session.pane_pid,
                process: session.process.clone(),
                selected: session.selected,
            };
            (session.id.clone(), cache.refresh_session(&input, now))
        })
        .collect()
}

pub fn session_colors(
    sessions: &[bootty_mux::snapshot::MuxSession],
    display_names: &[String],
) -> Vec<(String, String)> {
    session_label_colors(
        sessions
            .iter()
            .zip(display_names)
            .map(|(session, display_name)| {
                if display_name.is_empty() {
                    session.name.as_str()
                } else {
                    display_name.as_str()
                }
            }),
    )
}

fn session_label_colors<'a>(labels: impl Iterator<Item = &'a str>) -> Vec<(String, String)> {
    let groups = labels
        .map(|name| name.split_once('/').map_or(name, |(group, _)| group))
        .collect::<Vec<_>>();
    let mut order = Vec::<&str>::new();
    let mut counts = HashMap::<&str, usize>::new();
    for &group in &groups {
        if !counts.contains_key(group) {
            order.push(group);
        }
        let count = counts.entry(group).or_default();
        *count = count.saturating_add(1);
    }
    let mut positions = HashMap::<&str, usize>::new();
    groups
        .into_iter()
        .map(|group| {
            let group_index = order
                .iter()
                .position(|candidate| *candidate == group)
                .unwrap_or(0);
            let group_count = if group.is_empty() {
                0
            } else {
                counts.get(group).copied().unwrap_or_default()
            };
            let group_position = positions.entry(group).or_default();
            let colors =
                computed_session_color(group_index, order.len(), *group_position, group_count);
            if !group.is_empty() {
                *group_position = group_position.saturating_add(1);
            }
            colors
        })
        .collect()
}

fn computed_session_color(
    position: usize,
    total: usize,
    group_position: usize,
    group_total: usize,
) -> (String, String) {
    let base = if total > 0 {
        60.0 + (position.to_f64().unwrap_or_default() * 300.0) / total.to_f64().unwrap_or_default()
    } else {
        210.0
    };
    let (hue, lightness) = if group_total > 1 {
        let offset = group_position.to_f64().unwrap_or_default()
            / group_total.saturating_sub(1).to_f64().unwrap_or(1.0);
        (
            (base + offset.mul_add(60.0, -30.0) + 360.0) % 360.0,
            (offset - 0.5).mul_add(0.15, 0.55),
        )
    } else {
        (base, 0.6)
    };
    (hsl_hex(hue, 0.55, lightness), hsl_hex(hue, 0.2, 0.45))
}

fn hsl_hex(hue: f64, saturation: f64, lightness: f64) -> String {
    let chroma = (1.0 - 2.0f64.mul_add(lightness, -1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let intermediate = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let offset = lightness - chroma / 2.0;
    let (red, green, blue) = match sector.to_u32() {
        Some(0) => (chroma, intermediate, 0.0),
        Some(1) => (intermediate, chroma, 0.0),
        Some(2) => (0.0, chroma, intermediate),
        Some(3) => (0.0, intermediate, chroma),
        Some(4) => (intermediate, 0.0, chroma),
        _ => (chroma, 0.0, intermediate),
    };
    let channel = |value: f64| {
        ((value + offset) * 255.0)
            .clamp(0.0, 255.0)
            .to_u8()
            .unwrap_or_default()
    };
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(red),
        channel(green),
        channel(blue)
    )
}

#[allow(clippy::too_many_lines)]
pub fn snapshot(
    state: &AppState,
    native: &NativeChrome,
    projection: &ChromeProjection,
    width: f32,
    height: f32,
) -> ChromeSnapshot {
    let mut palette = chrome_palette(state.ui_theme().palette);
    if state.config().chrome.tabs_use_session_color {
        palette.tab_accent =
            parse_color(projection.mux.session_color.as_deref()).unwrap_or(palette.tab_accent);
    }
    let config = state.config();
    let chrome = &config.chrome;
    let facts = state.window_chrome_facts();
    let black_notch_chrome = state.black_notch_chrome();
    let space_summaries = state.space_summaries();
    let can_close_space = space_summaries.len() > 1;
    let active_space_appearance = space_summaries
        .iter()
        .find(|space| space.active)
        .map_or((DEFAULT_SPACE_COLOR, false), |space| {
            (space.color, space.tint_sidebar)
        });
    let spaces = space_summaries
        .into_iter()
        .map(|space| SpaceSnapshot {
            key: key(space.id),
            name: space.name,
            icon: space.icon,
            color: Rgba::rgb(space.color[0], space.color[1], space.color[2]),
            active: space.active,
            error: space.error,
            accepts_moves: space.accepts_moves,
            can_close: can_close_space,
        })
        .collect::<Vec<_>>();
    let sidebar = sidebar_snapshot(state, native, projection, palette, active_space_appearance);
    let sidebar_tint = sidebar.tint;
    let status_background = if black_notch_chrome {
        Rgba::rgb(0, 0, 0)
    } else {
        chrome
            .status_background
            .map_or(sidebar_tint, crate::theme::config_rgba)
    };
    let layout_gap = if chrome.sidebar && !facts.fullscreen {
        chrome.gap
    } else {
        0.0
    };
    let (top_status, bottom_status) = status_bars(state, native, projection, status_background);
    let status_height = if top_status.iter().chain(bottom_status.iter()).any(|status| {
        status
            .segments
            .iter()
            .any(|segment| segment.surface == "windows")
    }) {
        chrome.status_height.max(crate::gpui::UI_TAB_BAR_HEIGHT)
    } else {
        chrome.status_height
    };
    ChromeSnapshot {
        palette,
        layout: ChromeLayout {
            left_dock_toggle: chrome.left_dock_toggle,
            right_dock_toggle: chrome.right_dock_toggle,
            panel_tab_style: chrome.panel_tab_style,
            panel_tabs: chrome.panel_tabs,
            tabs: chrome.tabs,

            width,
            height,
            sidebar_position: SidebarPosition::Left,
            sidebar_width: chrome.sidebar_width,
            gap: layout_gap,
            top_inset: facts.top_inset(
                config.window.fullscreen_tabs_in_notch,
                top_status.as_ref().map_or(0.0, |_| status_height),
                config.window.fullscreen_top_offset,
            ),
            notch_span: facts.notch_span,
            wrap_tabs_at_notch: config.window.fullscreen_tabs_wrap_at_notch,
            titlebar_height: 0.0,
            status_height,
            sidebar_visible: chrome.sidebar,
            titlebar_visible: false,
            fullscreen: facts.fullscreen,
        },
        titlebar: TitlebarSnapshot {
            title: "Bootty".to_owned(),
            icon: Some("bootty".to_owned()),
            session_count: projection.mux.sessions.len(),
            reserve_window_controls: config.window.reserves_macos_titlebar_button_area(),
        },
        sidebar: Some(sidebar),
        spaces,
        space_transition: state.space_transition(std::time::Instant::now()).map(
            |(from, to, progress)| crate::gpui::chrome::SpaceTransition {
                from: key(from),
                to: key(to),
                progress,
            },
        ),
        top_status,
        bottom_status,
        window_focused: projection.mux.focused,
    }
}

const fn chrome_palette(theme: crate::gpui::UiPalette) -> ChromePalette {
    ChromePalette {
        mantle: ui_color!(theme.mantle),
        base: ui_color!(theme.base),
        tab_bar: ui_color!(theme.tab_bar),
        pane: ui_color!(theme.pane),
        surface: ui_color!(theme.surface),
        hover: ui_color!(theme.hover),
        border: ui_color!(theme.border),
        border_variant: ui_color!(theme.border_variant),
        text: ui_color!(theme.text),
        subtext: ui_color!(theme.subtext),
        muted: ui_color!(theme.muted),
        accent: ui_color!(theme.accent),
        tab_accent: ui_color!(theme.accent),
    }
}

fn status_bars(
    state: &AppState,
    native: &NativeChrome,
    projection: &ChromeProjection,
    status_background: Rgba,
) -> (Option<StatusBarSnapshot>, Option<StatusBarSnapshot>) {
    let chrome = &state.config().chrome;
    let theme = state.ui_theme().palette;
    let mut top_status = chrome.top_bar.then(|| {
        status_snapshot(
            state,
            "top",
            &chrome.top_segments,
            native,
            theme,
            projection,
            status_background,
        )
    });
    let mut bottom_status = chrome.bottom_bar.then(|| {
        status_snapshot(
            state,
            "bottom",
            &chrome.bottom_segments,
            native,
            theme,
            projection,
            status_background,
        )
    });
    for kind in bootty_config::config::PanelKind::ALL
        .into_iter()
        .filter(|kind| *kind != bootty_config::config::PanelKind::Agents)
    {
        let target = match state.config().panel(kind).button {
            bootty_config::config::PanelButton::None => continue,
            bootty_config::config::PanelButton::Top => &mut top_status,
            bootty_config::config::PanelButton::Bottom => &mut bottom_status,
        };
        let key = if state.config().panel(kind).button == bootty_config::config::PanelButton::Top {
            "top"
        } else {
            "bottom"
        };
        let bar = target.get_or_insert_with(|| {
            status_snapshot(
                state,
                key,
                &[],
                native,
                theme,
                projection,
                status_background,
            )
        });
        let command = crate::commands::DockAction::TogglePanel(kind).command();
        bar.segments.push(StatusSegmentSnapshot {
            align: StatusAlignment::Right,
            source_slot: bar.segments.len(),
            surface: format!("panel:{}", kind.name()),
            items: vec![StatusItemSnapshot {
                key: kind.name().into(),
                text: String::new(),
                icon: Some(command.icon().into()),
                gauge: None,
                pad_left: 0.0,
                pad_right: 0.0,
                progress: None,
                foreground: None,
                background: None,
                active: false,
                action: Some(NativeChromeAction::TogglePanel(kind)),
                reorder_anchor: None,
                tab_context: None,
            }],
        });
    }
    (top_status, bottom_status)
}

fn sidebar_snapshot(
    state: &AppState,
    native: &NativeChrome,
    projection: &ChromeProjection,
    palette: ChromePalette,
    active_space_appearance: ([u8; 3], bool),
) -> SidebarSnapshot {
    let config = state.config();
    let chrome = &config.chrome;
    let theme = state.ui_theme().palette;
    let black_notch_chrome = state.black_notch_chrome();
    let sidebar_config = &config.sidebar;
    let mut rows = if sidebar_config.modules.iter().any(|name| name == "sessions") {
        sidebar_rows(state, native, projection, palette)
    } else {
        Vec::new()
    };
    if sidebar_config.modules.iter().any(|name| name == "sessions") {
        for project in state.workspace.registered_projects(state.mux_scope()) {
            if !rows
                .iter()
                .any(|row| row.project_path.as_deref() == Some(&project.cwd))
            {
                rows.push(empty_project_row(project, palette));
            }
        }
    }
    rows.extend(unclaimed_rows(state, palette));
    apply_project_settings(state, native, &mut rows);
    let sidebar_background = if black_notch_chrome {
        Rgba::rgb(0, 0, 0)
    } else {
        sidebar_config
            .background
            .map_or(palette.base, crate::theme::config_rgba)
    };
    let sidebar_tint = if black_notch_chrome {
        sidebar_background
    } else {
        tint_sidebar(
            sidebar_background,
            active_space_appearance.0,
            active_space_appearance.1,
        )
    };
    SidebarSnapshot {
        projects: state
            .workspace
            .registered_projects(state.mux_scope())
            .cloned()
            .collect(),
        project_target: state.current_command_target_for(
            "project.toggle_collapsed",
            bootty_control::ResourceKind::Binding,
        ),
        rows,
        now_utc: native.clock.epoch,
        footer: if sidebar_config
            .modules
            .iter()
            .any(|module| module == "codexbar")
        {
            sidebar_footer(native, theme)
        } else {
            Vec::new()
        },
        title_visible: config.window.custom_chrome_title_visible(),
        group_by_project: sidebar_config.group_by_project,
        sort_order: sidebar_config.sort_order,
        animate_working: sidebar_config.animate_working,
        focused: state.sidebar_focused(),
        hovered_session: state.sidebar_hovered_session().map(neutral_target),
        dim_when_unfocused: chrome.unfocused_sidebar_dim,
        tint: sidebar_tint,
        foreground: sidebar_config
            .foreground
            .map_or(palette.text, crate::theme::config_rgba),
        hover: sidebar_config
            .hover
            .map_or(palette.hover, crate::theme::config_rgba),
        current: sidebar_config
            .selected
            .map_or(palette.surface, crate::theme::config_rgba),
        border: sidebar_config
            .border
            .map_or(palette.border_variant, crate::theme::config_rgba),
    }
}

fn apply_project_settings(state: &AppState, native: &NativeChrome, rows: &mut [SidebarRow]) {
    let scope = state.mux_scope().persistence_value().to_string();
    for row in rows {
        let Some(project) = state
            .workspace
            .registered_projects(state.mux_scope())
            .find(|project| row.project_path.as_deref() == Some(&project.cwd))
        else {
            continue;
        };
        let settings = &project.settings;
        if matches!(row.kind, SidebarRowKind::Group) {
            if !settings.name.is_empty() {
                row.text.clone_from(&settings.name);
            }
            if !settings.icon.is_empty() {
                row.icon = Some(settings.icon.clone());
                row.artwork = None;
            }
            if let Some(path) = &settings.icon_path {
                row.artwork = native.artwork.request_with_icon(
                    &scope,
                    &project.cwd,
                    None,
                    Some(path),
                    std::time::Instant::now(),
                    &state.repaint,
                );
            }
        }
        if let Some(label) = &mut row.project {
            if !settings.name.is_empty() {
                label.name.clone_from(&settings.name);
            }
            if let Some(path) = &settings.icon_path {
                label.artwork = native.artwork.request_with_icon(
                    &scope,
                    &project.cwd,
                    None,
                    Some(path),
                    std::time::Instant::now(),
                    &state.repaint,
                );
            }
        }
    }
}

fn empty_project_row(
    project: &bootty_mux::repository::RegisteredProject,
    palette: ChromePalette,
) -> SidebarRow {
    SidebarRow {
        key: format!(
            "project:{}:{}",
            project.scope.persistence_value(),
            project.cwd
        ),
        text: project
            .cwd
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&project.cwd)
            .to_owned(),
        secondary: None,
        project: None,
        project_path: Some(project.cwd.clone()),
        branch: None,
        agents: Vec::new(),
        trailing: None,
        trailing_icon: None,
        trailing_color: None,
        working: false,
        needs_attention: false,
        number: None,
        indent: 0,
        tree: None,
        artwork: None,
        icon: Some("folder".to_owned()),
        diff: None,
        color: palette.text,
        dim_color: palette.muted,
        kind: SidebarRowKind::Group,
        active: false,
        current: false,
        selectable: false,
        target: None,
        reorder_anchor: None,
        context: None,
        task: None,
    }
}

fn unclaimed_rows(state: &AppState, palette: ChromePalette) -> Vec<SidebarRow> {
    let scope = key(state.mux_scope());
    let mut rows = Vec::new();
    let unclaimed = state.unclaimed_sessions();
    if !unclaimed.is_empty() {
        rows.push(SidebarRow {
            key: "unassigned".to_owned(),
            text: "Unassigned".to_owned(),
            secondary: None,
            project: None,
            project_path: None,
            branch: None,
            agents: Vec::new(),
            trailing: None,
            trailing_icon: None,
            trailing_color: None,
            working: false,
            needs_attention: false,
            number: None,
            indent: 0,
            tree: None,
            artwork: None,
            icon: Some("circle-dashed".to_owned()),
            diff: None,
            color: palette.text,
            dim_color: palette.muted,
            kind: SidebarRowKind::Group,
            active: false,
            current: false,
            selectable: false,
            target: None,
            reorder_anchor: None,
            context: None,
            task: None,
        });
        rows.extend(unclaimed.into_iter().map(|session| SidebarRow {
            key: session.session_id.clone(),
            text: session.name,
            secondary: None,
            project: None,
            project_path: None,
            branch: None,
            agents: Vec::new(),
            trailing: None,
            trailing_icon: None,
            trailing_color: None,
            working: false,
            needs_attention: false,
            number: None,
            indent: 2,
            tree: None,
            artwork: None,
            icon: Some("bootty".to_owned()),
            diff: None,
            color: palette.text,
            dim_color: palette.muted,
            kind: SidebarRowKind::Other("unassigned".to_owned()),
            active: false,
            current: false,
            selectable: true,
            target: Some(SessionTarget {
                scope,
                session_id: session.session_id,
            }),
            reorder_anchor: None,
            context: None,
            task: None,
        }));
    }
    rows
}

fn session_title<'a>(
    name: &'a str,
    project: &str,
    branch: Option<&str>,
    explicit: bool,
) -> &'a str {
    if explicit {
        return name;
    }
    let title = name.strip_prefix(&format!("{project}/")).unwrap_or(name);
    if title == project || branch == Some(title) || std::path::Path::new(title).is_absolute() {
        "Terminal"
    } else {
        title
    }
}

fn live_native_activities(state: &AppState) -> Vec<bootty_agents::NativeSessionActivity> {
    let native_panes = state
        .mux()
        .all_sessions()
        .iter()
        .flat_map(|session| &session.windows)
        .flat_map(|window| &window.panes)
        .filter_map(|pane| pane.native_agent.as_deref())
        .collect::<HashSet<_>>();
    state
        .native_agent_service()
        .map_or_else(Vec::new, |agents| agents.activities())
        .into_iter()
        .filter(|activity| native_panes.contains(activity.id.as_str()))
        .collect()
}

fn sidebar_rows(
    state: &AppState,
    native: &NativeChrome,
    projection: &ChromeProjection,
    palette: ChromePalette,
) -> Vec<SidebarRow> {
    let sessions = &projection.mux.sessions;
    let native_activities = live_native_activities(state);
    let scope = state.mux_scope().persistence_value().to_string();
    let now = std::time::Instant::now();
    let mut rows = Vec::new();
    let grouped = state.config().sidebar.group_by_project;
    let mut groups = Vec::<(Option<&str>, Vec<(usize, &SessionView)>)>::new();
    for (index, session) in sessions.iter().enumerate() {
        let group = session.cwd.as_deref();
        if grouped && let Some((_, indices)) = groups.iter_mut().find(|(key, _)| *key == group) {
            indices.push((index, session));
        } else {
            groups.push((group, vec![(index, session)]));
        }
    }
    let mut last_group = None;
    for (index, session) in groups.into_iter().flat_map(|(_, indices)| indices) {
        let facts = projection
            .session_facts
            .get(&session.id)
            .cloned()
            .unwrap_or_default();
        let name = if session.display_name.is_empty() {
            &session.name
        } else {
            &session.display_name
        };
        let cwd = session.cwd.as_deref();
        let project = cwd
            .and_then(|cwd| cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next())
            .filter(|name| !name.is_empty())
            .unwrap_or("Terminal");
        let group = cwd.unwrap_or("").to_owned();
        let base = sidebar_session_base(state, session, index, sessions.len(), palette);
        let artwork = cwd.and_then(|cwd| {
            native.artwork.request(
                &scope,
                cwd,
                state.active_multiplexer().remote.as_ref(),
                now,
                &state.repaint,
            )
        });
        if grouped && last_group.as_ref() != Some(&group) {
            rows.push(SidebarRow {
                key: format!("project:{scope}:{group}"),
                text: project.to_owned(),
                trailing: None,
                trailing_color: Some(palette.muted),
                color: palette.text,
                dim_color: palette.muted,
                artwork: artwork.clone(),
                icon: Some("folder".to_owned()),
                kind: SidebarRowKind::Group,
                active: false,
                current: false,
                target: None,
                task: None,
                ..base.clone()
            });
        }
        let explicit_title = session
            .identity
            .as_deref()
            .and_then(|identity| state.workspace.active.binding.sessions().get(identity))
            .is_some_and(|saved| saved.explicit);
        let title = session_title(name, project, facts.branch.as_deref(), explicit_title);
        let mut row = SidebarRow {
            text: title.to_owned(),
            project: (!grouped).then(|| SidebarProject {
                name: project.to_owned(),
                artwork,
            }),
            branch: facts.branch,
            icon: Some("terminal".to_owned()),
            kind: if session.detached {
                SidebarRowKind::DetachedSession
            } else {
                SidebarRowKind::Session
            },
            selectable: true,
            ..base
        };
        let terminal_priority = sidebar_session_activity(state, session, &mut row);
        sidebar_native_activity(
            &mut row,
            session,
            &scope,
            &native_activities,
            terminal_priority,
            state.ui_theme().palette,
        );
        rows.push(row);
        last_group = Some(group);
    }
    rows
}

fn sidebar_session_activity(state: &AppState, session: &SessionView, row: &mut SidebarRow) -> u8 {
    if session.detached {
        row.reorder_anchor.clone_from(&session.identity);
    }
    let activities = terminal_activities(state, &session.id, None);
    if let Some(activity) = activities
        .iter()
        .max_by_key(|activity| activity_priority(activity.status))
    {
        sidebar_agent_activity(row, activity, state.ui_theme().palette);
    }
    let priority = activities
        .iter()
        .map(|activity| activity_priority(activity.status))
        .max()
        .unwrap_or(0);
    row.agents = activities.iter().map(sidebar_agent).collect();
    if row.task.as_ref().is_some_and(|task| task.state.deleted) {
        row.reorder_anchor = None;
    }
    priority
}

fn sidebar_native_activity(
    row: &mut SidebarRow,
    session: &SessionView,
    binding_id: &str,
    activities: &[bootty_agents::NativeSessionActivity],
    terminal_priority: u8,
    theme: crate::gpui::UiPalette,
) {
    use bootty_agents::NativeSessionStatus as S;
    let matching = activities
        .iter()
        .filter(|activity| {
            activity.binding_id == binding_id
                && activity.task_identity.is_some()
                && activity.task_identity == session.identity
        })
        .collect::<Vec<_>>();
    row.agents
        .extend(matching.iter().map(|activity| SidebarAgent {
            key: activity.id.clone(),
            icon: activity.provider.icon().to_owned(),
            description: format!("{} · {:?}", activity.provider, activity.status),
        }));
    let priority = |activity: &&bootty_agents::NativeSessionActivity| {
        if activity.approval {
            10
        } else if activity.input {
            9
        } else {
            match activity.status {
                S::Error => 8,
                S::Working => 7,
                S::Waiting => 6,
                S::Starting => 5,
                S::Idle | S::Stopped if activity.completed_turn => 3,
                S::Idle | S::Stopped => 0,
            }
        }
    };
    if let Some(activity) = matching
        .into_iter()
        .max_by_key(priority)
        .filter(|activity| priority(activity) >= terminal_priority)
    {
        row.needs_attention = activity.approval || activity.input || activity.status == S::Error;
        row.working = matches!(activity.status, S::Starting | S::Working);
        let badge = if activity.approval {
            Some(("Approval", "shield-question-mark", theme.warning))
        } else if activity.input {
            Some(("Input", "message-circle-question-mark", theme.accent))
        } else {
            match activity.status {
                S::Starting | S::Working => Some(("Working", "circle-dashed", theme.accent)),
                S::Waiting => Some(("Waiting", "clock", theme.muted)),
                S::Error => Some(("Failed", "circle-alert", theme.destructive)),
                S::Idle | S::Stopped if activity.completed_turn => {
                    Some(("Finished", "circle-check", theme.success))
                }
                S::Idle | S::Stopped => None,
            }
        };
        if let Some((text, icon, color)) = badge {
            row.trailing = Some(if text == "Working" {
                activity.working_elapsed.map_or_else(
                    || text.to_owned(),
                    |elapsed| format!("{text} {}", crate::clock::format_working_duration(elapsed)),
                )
            } else {
                text.to_owned()
            });
            row.trailing_icon = Some(icon.to_owned());
            row.trailing_color = Some(color);
        }
    }
}

fn sidebar_agent(activity: &bootty_agents::TerminalAgentActivity) -> SidebarAgent {
    SidebarAgent {
        key: activity.target.handle.clone(),
        icon: activity.provider.icon().to_owned(),
        description: format!(
            "{:?} · {:?}{}",
            activity.provider,
            activity.status,
            activity
                .detail
                .as_ref()
                .map_or_else(String::new, |detail| format!(" · {detail}"))
        ),
    }
}

fn sidebar_agent_activity(
    row: &mut SidebarRow,
    activity: &bootty_agents::TerminalAgentActivity,
    theme: crate::gpui::UiPalette,
) {
    use bootty_agents::TerminalAgentStatus as S;
    row.needs_attention = matches!(activity.status, S::Approval | S::Input | S::Error);
    row.working = matches!(activity.status, S::Starting | S::Working);
    let (status, icon, tone) = match activity.status {
        S::Starting | S::Working => ("Working", "circle-dashed", theme.accent),
        S::Approval => ("Approval", "shield-question-mark", theme.warning),
        S::Input => ("Input", "message-circle-question-mark", theme.accent),
        S::Waiting => ("Waiting", "clock", theme.muted),
        S::Error => ("Failed", "circle-alert", theme.destructive),
        S::Finished => ("Finished", "circle-check", theme.success),
        S::Idle | S::Stopped | S::Unavailable => return,
    };
    row.trailing = Some(activity.working_elapsed.map_or_else(
        || status.to_owned(),
        |elapsed| {
            format!(
                "{status} {}",
                crate::clock::format_working_duration(elapsed)
            )
        },
    ));
    row.trailing_icon = Some(icon.to_owned());
    row.trailing_color = Some(tone);
}

const fn activity_priority(status: bootty_agents::TerminalAgentStatus) -> u8 {
    use bootty_agents::TerminalAgentStatus as S;
    match status {
        S::Approval => 10,
        S::Input => 9,
        S::Error => 8,
        S::Working => 7,
        S::Waiting => 6,
        S::Starting => 5,
        S::Unavailable => 4,
        S::Finished => 3,
        S::Stopped => 2,
        S::Idle => 1,
    }
}

fn terminal_activity(
    state: &AppState,
    session_id: &str,
    window_id: Option<&str>,
) -> Option<bootty_agents::TerminalAgentActivity> {
    terminal_activities(state, session_id, window_id)
        .into_iter()
        .max_by_key(|activity| activity_priority(activity.status))
}

fn terminal_activities(
    state: &AppState,
    session_id: &str,
    window_id: Option<&str>,
) -> Vec<bootty_agents::TerminalAgentActivity> {
    use bootty_control::ResourceKind;
    let Some(agents) = state.terminal_agent_service() else {
        return Vec::new();
    };
    let scope = state.mux_scope();
    let mux = state.mux();
    let Some(session) = mux.backend_session_by_id_or_name(session_id) else {
        return Vec::new();
    };
    let binding = state.binding_target_handle(scope, mux.binding_generation());
    session
        .windows
        .iter()
        .filter(|window| window_id.is_none_or(|id| window.id == id))
        .flat_map(|window| {
            window.panes.iter().filter_map(|pane| {
                let target = ExactMuxTarget::Pane(
                    scope,
                    session.id.clone(),
                    window.id.clone(),
                    pane.pane_id.clone()?,
                )
                .command_target(ResourceKind::Terminal, mux, &binding)?;
                agents.activity(&target)
            })
        })
        .collect()
}

fn sidebar_session_base(
    state: &AppState,
    session: &SessionView,
    index: usize,
    session_count: usize,
    palette: ChromePalette,
) -> SidebarRow {
    let scope = key(state.mux_scope());
    let color = parse_color(session.color.as_deref()).unwrap_or(palette.accent);
    let dim_color = parse_color(session.dim_color.as_deref()).unwrap_or(palette.muted);
    SidebarRow {
        key: session
            .identity
            .clone()
            .unwrap_or_else(|| session.id.clone()),
        text: String::new(),
        secondary: None,
        project: None,
        project_path: session.cwd.clone(),
        branch: None,
        agents: Vec::new(),
        trailing: None,
        trailing_icon: None,
        trailing_color: None,
        working: false,
        needs_attention: false,
        number: None,
        indent: 0,
        tree: None,
        artwork: None,
        icon: None,
        diff: None,
        color,
        dim_color,
        kind: SidebarRowKind::Detail,
        active: session.selected,
        current: session.selected,
        selectable: false,
        target: Some(SessionTarget {
            scope,
            session_id: session.id.clone(),
        }),
        reorder_anchor: Some(session.name.clone()),
        task: session.identity.as_deref().and_then(|identity| {
            let binding = &state.workspace.active.binding;
            let saved = binding.sessions().get(identity)?;
            Some(SidebarTask {
                identity: identity.to_owned(),
                native_conversation: session
                    .detached
                    .then(|| {
                        state
                            .saved_native_conversation_invocation(&ScopedSessionTarget::new(
                                binding.scope(),
                                identity,
                            ))
                            .and_then(|invocation| invocation.target)
                    })
                    .flatten(),
                state: saved.state,
                binding: state
                    .saved_session_invocation(binding.scope(), "session.saved", Vec::new())
                    .and_then(|invocation| invocation.target),
                pending: state.saved_session_state_pending(binding.scope(), identity),
            })
        }),
        context: Some(SessionContextSnapshot {
            can_rename: session.identity.as_deref().is_some_and(|identity| {
                state.workspace.active.binding.sessions().contains(identity)
            }),
            can_activate: !session.selected,
            can_move_up: index > 0,
            can_move_down: index.saturating_add(1) < session_count,
            can_navigate: session_count > 1,
            can_return_to_last: state.mux().previous_selected_session().is_some(),
        }),
    }
}

fn sidebar_footer(
    native: &NativeChrome,
    theme: crate::gpui::UiPalette,
) -> Vec<crate::gpui::chrome::SidebarFooterItem> {
    let mut rows = Vec::new();
    let mut error = None;
    for (provider, usage) in UsageProvider::ALL.into_iter().zip(native.usage.current()) {
        error = error.or(usage.error.as_deref());
        let provider_color = match provider {
            UsageProvider::Codex => theme.accent,
            UsageProvider::Claude => theme.warning,
        };
        let tone_color = |tone| match tone {
            QuotaTone::Provider => provider_color,
            QuotaTone::Muted => theme.muted,
            QuotaTone::Success => theme.success,
            QuotaTone::Warning => theme.warning,
            QuotaTone::Critical => theme.destructive,
        };
        for window in &usage.windows {
            let mut meter = window.meter(native.clock.epoch);
            let stale = window
                .resets_at
                .is_some_and(|reset| reset <= native.clock.epoch);
            if stale {
                meter.tone = QuotaTone::Muted;
                meter.pace.clear();
            }
            let reset_at = window
                .resets_at
                .and_then(|epoch| chrono::DateTime::from_timestamp(epoch, 0))
                .map(|date| {
                    date.with_timezone(&chrono::Local)
                        .format("%b %-d %H:%M")
                        .to_string()
                });
            rows.push(crate::gpui::chrome::SidebarFooterItem {
                key: format!("{}:{}", provider.id(), window.label),
                text: format!("{} {}", provider.id(), window.label),
                icon: Some(
                    match provider {
                        UsageProvider::Codex => "openai",
                        UsageProvider::Claude => "claude",
                    }
                    .to_owned(),
                ),
                color: theme.text,
                meter: Some(UsageMeterSnapshot {
                    provider,
                    label: if stale {
                        format!("{} updating", window.label)
                    } else {
                        format!("{} {:.0}%", window.label, meter.remaining_percent)
                    },
                    description: format!(
                        "{} {}: {:.0}% remaining. {} (estimated). Resets {}{}.",
                        provider.id(),
                        window.label,
                        meter.remaining_percent,
                        meter.pace,
                        reset_at.as_deref().unwrap_or("at an unknown time"),
                        if meter.reset.is_empty() {
                            String::new()
                        } else {
                            format!(" (in {})", meter.reset)
                        }
                    ),
                    reset_at,
                    fill: tone_color(meter.tone),
                    marker: tone_color(meter.marker_tone),
                    pace: tone_color(meter.pace_tone),
                    track: theme.border_variant,
                    meter,
                }),
            });
        }
    }
    if rows.is_empty()
        && let Some(error) = error
    {
        rows.push(crate::gpui::chrome::SidebarFooterItem {
            key: "codexbar:error".to_owned(),
            text: format!("codexbar {error}"),
            icon: None,
            color: theme.muted,
            meter: None,
        });
    }
    rows
}

fn status_snapshot(
    state: &AppState,
    key: &str,
    configured: &[bootty_config::config::StatusSegment],
    native: &NativeChrome,
    theme: crate::gpui::UiPalette,
    projection: &ChromeProjection,
    background: Rgba,
) -> StatusBarSnapshot {
    let segments = configured
        .iter()
        .enumerate()
        .filter_map(|(source_slot, configured)| {
            let mut items = status_items(state, &configured.module, native, projection, theme);
            for (index, item) in items.iter_mut().enumerate() {
                item.key = format!("{source_slot}:{index}:{}", item.key);
                item.icon = item.icon.take().or_else(|| configured.icon.clone());
                item.foreground = item
                    .foreground
                    .or_else(|| configured.fg.map(crate::theme::config_rgba));
                item.background = item
                    .background
                    .or_else(|| configured.bg.map(crate::theme::config_rgba));
            }
            (!items.is_empty()).then(|| StatusSegmentSnapshot {
                align: match configured.align {
                    bootty_config::config::SegmentAlign::Left => StatusAlignment::Left,
                    bootty_config::config::SegmentAlign::Center => StatusAlignment::Center,
                    bootty_config::config::SegmentAlign::Right => StatusAlignment::Right,
                },
                source_slot,
                surface: configured.module.clone(),
                items,
            })
        })
        .collect();
    StatusBarSnapshot {
        key: key.to_owned(),
        rows: 1,
        background,
        segments,
    }
}

fn status_items(
    state: &AppState,
    module: &str,
    native: &NativeChrome,
    projection: &ChromeProjection,
    theme: crate::gpui::UiPalette,
) -> Vec<StatusItemSnapshot> {
    match module {
        "clock" => vec![
            status_cell(
                "date",
                native.clock.date.clone(),
                Some("calendar"),
                theme.subtext,
            ),
            status_cell("time", native.clock.time.clone(), Some("clock"), theme.text),
        ],
        "session" if !projection.mux.sidebar_visible => projection
            .mux
            .session
            .as_ref()
            .map(|name| status_cell("session", name.clone(), Some("folder"), theme.accent))
            .into_iter()
            .collect(),
        "windows" => window_status_items(state, projection, theme),
        "sysinfo" => system_status_items(native, projection, theme),
        _ => Vec::new(),
    }
}

fn status_cell(
    key: &str,
    text: String,
    icon: Option<&str>,
    foreground: Rgba,
) -> StatusItemSnapshot {
    StatusItemSnapshot {
        key: key.to_owned(),
        text,
        icon: icon.map(str::to_owned),
        gauge: None,
        pad_left: 0.0,
        pad_right: 0.0,
        progress: None,
        foreground: Some(foreground),
        background: None,
        active: false,
        action: None,
        reorder_anchor: None,
        tab_context: None,
    }
}

fn window_status_items(
    state: &AppState,
    projection: &ChromeProjection,
    theme: crate::gpui::UiPalette,
) -> Vec<StatusItemSnapshot> {
    projection
        .mux
        .windows
        .iter()
        .flat_map(|window| {
            let make_cell = |part: &str, text: String| {
                let mut item = status_cell(
                    &format!("{}:{part}", window.id),
                    text,
                    None,
                    if window.active {
                        theme.text
                    } else {
                        theme.subtext
                    },
                );
                item.active = window.active;
                item.reorder_anchor = Some(window.id.clone());
                item.tab_context = projection.tab_contexts.get(&window.id).cloned();
                if let Some(context) = &item.tab_context {
                    item.action = Some(NativeChromeAction::ActivateWindow {
                        session_id: context.session_id.clone(),
                        window_id: window.id.clone(),
                    });
                }
                item
            };
            let index = make_cell("index", window.index.to_string());
            let mut name = make_cell("name", window.name.clone());
            name.icon = Some(
                projection
                    .tab_contexts
                    .get(&window.id)
                    .and_then(|context| {
                        terminal_activity(state, &context.session_id, Some(&window.id))
                    })
                    .map_or("terminal", |activity| activity.provider.icon())
                    .to_owned(),
            );
            if let Some(native) = state
                .mux()
                .selected_session_windows()
                .iter()
                .find(|candidate| candidate.id == window.id)
                .and_then(|candidate| {
                    candidate
                        .panes
                        .iter()
                        .find_map(|pane| pane.native_agent.as_deref())
                })
                .and_then(|id| {
                    state
                        .native_agent_service()?
                        .activities()
                        .into_iter()
                        .find(|record| {
                            record.id == id
                                && record.binding_id
                                    == state.mux_scope().persistence_value().to_string()
                                && projection
                                    .tab_contexts
                                    .get(&window.id)
                                    .is_some_and(|context| {
                                        record.task_identity
                                            == state.workspace.session_identity(
                                                state.mux_scope(),
                                                &context.session_id,
                                            )
                                    })
                        })
                })
            {
                name.text = native.title;
                name.icon = Some(native.provider.icon().to_owned());
                name.progress = matches!(
                    native.status,
                    bootty_agents::NativeSessionStatus::Starting
                        | bootty_agents::NativeSessionStatus::Working
                )
                .then_some(StatusProgress {
                    value: None,
                    color: theme.accent,
                });
                return [index, name];
            }
            name.progress = window.progress.map(|value| StatusProgress {
                value: (!window.progress_indeterminate).then_some(value),
                color: parse_color(projection.mux.session_color.as_deref()).unwrap_or(theme.accent),
            });
            [index, name]
        })
        .collect()
}

fn system_status_items(
    native: &NativeChrome,
    projection: &ChromeProjection,
    theme: crate::gpui::UiPalette,
) -> Vec<StatusItemSnapshot> {
    let metrics = native.metrics.current();
    let mut awake = status_cell(
        "awake",
        String::new(),
        Some(if projection.mux.keep_awake {
            "coffee-cup-filled"
        } else {
            "coffee-cup"
        }),
        if projection.mux.keep_awake {
            theme.base
        } else {
            theme.subtext
        },
    );
    awake.action = Some(NativeChromeAction::ToggleKeepAwake);
    awake.background = projection.mux.keep_awake.then_some(theme.success);
    let cpu = if metrics.load1 > 0.0 {
        status_cell(
            "cpu",
            format!("{:.2}", metrics.load1),
            Some("cpu"),
            if metrics.load1 >= 4.0 {
                theme.warning
            } else {
                theme.subtext
            },
        )
    } else {
        status_cell(
            "cpu",
            format!("{:.0}%", metrics.cpu),
            Some("cpu"),
            if metrics.cpu >= 80.0 {
                theme.warning
            } else {
                theme.subtext
            },
        )
    };
    let memory = status_cell(
        "memory",
        format!("{:.0}%", metrics.mem_used_pct),
        Some("memory-stick"),
        theme.subtext,
    );
    let power = metrics.battery_percent.map_or_else(
        || status_cell("power", String::new(), Some("plug-zap"), theme.success),
        |percent| {
            let remaining = metrics
                .battery_time_to_full_secs
                .or(metrics.battery_time_to_empty_secs)
                .and_then(|seconds| (seconds / 60.0).round().to_u64())
                .map_or_else(Default::default, |minutes| {
                    format!(" {}:{:02}", minutes / 60, minutes % 60)
                });
            let mut item = status_cell(
                "power",
                format!("{percent:.0}%{remaining}"),
                metrics.on_ac.then_some(if percent >= 99.5 {
                    "battery-full"
                } else {
                    "plug"
                }),
                if !metrics.on_ac && percent <= 20.0 {
                    theme.warning
                } else if metrics.on_ac {
                    theme.success
                } else {
                    theme.text
                },
            );
            item.gauge = Some(percent / 100.0);
            item
        },
    );
    let mut items = vec![awake, cpu, memory, power];
    for (index, item) in items.iter_mut().enumerate() {
        item.background.get_or_insert(if index % 2 == 0 {
            theme.surface
        } else {
            theme.hover
        });
    }
    items
}

fn parse_color(value: Option<&str>) -> Option<Rgba> {
    value
        .and_then(|value| bootty_config::color::Color::from_hex(value).ok())
        .map(crate::theme::config_rgba)
}

fn tint_sidebar(background: Rgba, space_color: [u8; 3], enabled: bool) -> Rgba {
    if !enabled {
        return background;
    }
    let [red, green, blue] = space_color;
    let blend = |background: u8, tint: u8| {
        let mixed = u16::from(background)
            .saturating_mul(7)
            .saturating_add(u16::from(tint))
            / 8;
        u8::try_from(mixed).unwrap_or(u8::MAX)
    };
    Rgba {
        red: blend(background.red, red),
        green: blend(background.green, green),
        blue: blend(background.blue, blue),
        alpha: background.alpha,
    }
}

pub fn apply(state: &mut AppState, intent: ChromeIntent) -> Vec<AppEffect> {
    let mut effects = Vec::new();
    let focus_terminal = matches!(
        intent,
        ChromeIntent::ActivateSession(_)
            | ChromeIntent::AdoptSession(_)
            | ChromeIntent::ReopenSession(_)
    ) || (matches!(intent, ChromeIntent::ActivateSpace(_))
        && state.config().default_open_behavior != OpenBehavior::NewWindow);
    let handled = match intent {
        ChromeIntent::Command(invocation) => {
            state.dispatch_command(
                invocation,
                crate::state::ViewportSnapshot::default(),
                &mut effects,
            );
            true
        }
        ChromeIntent::StartWindowDrag => false,
        ChromeIntent::ActivateSpace(space) => {
            let space = id(space);
            if state.config().default_open_behavior == OpenBehavior::NewWindow {
                effects.push(AppEffect::OpenSpaceWindow(space));
                true
            } else {
                state.activate_space_from_ui(space)
            }
        }
        ChromeIntent::CreateSpace => state.open_create_space_dialog_from_ui(),
        ChromeIntent::EditSpace(space) => state.open_edit_space_dialog_from_ui(id(space)),
        ChromeIntent::ReconnectSpace(space) => state.reconnect_space_from_ui(id(space)),
        ChromeIntent::CloseSpace(space) => state.close_space_from_ui(id(space)),
        ChromeIntent::MoveSessionsToSpace { sessions, to } => sessions
            .iter()
            .all(|session| state.move_scoped_session_to_space(&scoped_target(session), id(to))),
        ChromeIntent::OpenGitChanges(target) => {
            state.open_session_git_changes(id(target.scope), &target.session_id);
            true
        }
        ChromeIntent::ActivateSession(target) => {
            apply_session_activation(state, &scoped_target(&target), &mut effects)
        }
        ChromeIntent::ReopenSession(target) => {
            let scope = id(target.scope);
            state
                .saved_session_invocation(scope, "session.reopen", vec![target.session_id])
                .is_some_and(|invocation| {
                    matches!(
                        state.dispatch_command(
                            invocation,
                            crate::state::ViewportSnapshot::default(),
                            &mut effects
                        ),
                        bootty_control::CommandOutcome::Success { .. }
                    )
                })
        }
        ChromeIntent::RenameSavedSession(target) => {
            state.open_rename_session_dialog_for(&target.session_id)
        }
        ChromeIntent::AdoptSession(target) => {
            state.adopt_and_activate_scoped_session(&scoped_target(&target))
        }
        ChromeIntent::SessionContext { target, action } => {
            apply_session_context(state, &scoped_target(&target), action)
        }
        ChromeIntent::ReorderSession { source, before } => {
            state.reorder_session_before(&source, before.as_deref())
        }
        ChromeIntent::SidebarResizeLive(width) => {
            state.set_sidebar_width_live(width);
            true
        }
        ChromeIntent::SidebarResizePersist => {
            state.persist_sidebar_width(state.config().chrome.sidebar_width, &mut effects);
            true
        }
        ChromeIntent::SidebarResizeReset => {
            let width = bootty_config::config::ChromeConfig::default().sidebar_width;
            state.set_sidebar_width_live(width);
            state.persist_sidebar_width(width, &mut effects);
            true
        }
        ChromeIntent::Status(intent) => apply_status(state, intent),
    };
    effects.push(AppEffect::RequestRepaint);
    if handled
        && focus_terminal
        && !effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::NativeConversation(_)))
    {
        effects.push(AppEffect::FocusTerminal);
    }
    effects
}

fn apply_session_activation(
    state: &mut AppState,
    target: &ScopedSessionTarget,
    effects: &mut Vec<AppEffect>,
) -> bool {
    if state
        .workspace
        .binding(target.scope)
        .is_some_and(|binding| {
            binding.sessions().contains(&target.session_id)
                && binding.session_attachment(&target.session_id).is_none()
        })
        && let Some(invocation) = state
            .saved_native_conversation_invocation(target)
            .or_else(|| {
                state.saved_session_invocation(
                    target.scope,
                    "session.reopen",
                    vec![target.session_id.clone()],
                )
            })
    {
        matches!(
            state.dispatch_command(
                invocation,
                crate::state::ViewportSnapshot::default(),
                effects
            ),
            bootty_control::CommandOutcome::Success { .. }
        )
    } else {
        state.activate_scoped_session_from_ui(target)
    }
}

fn apply_session_context(
    state: &mut AppState,
    target: &ScopedSessionTarget,
    action: SessionContextAction,
) -> bool {
    match action {
        SessionContextAction::Activate => state.activate_scoped_session_from_ui(target),
        SessionContextAction::PreviousSession => {
            state.activate_relative_scoped_session_from_ui(target, -1)
        }
        SessionContextAction::NextSession => {
            state.activate_relative_scoped_session_from_ui(target, 1)
        }
        SessionContextAction::MoveToSpace => state.open_space_picker_for(target),
        _ if !state.activate_scoped_session_from_ui(target) => false,
        SessionContextAction::NewSession => state.open_new_session_dialog_from_ui(),
        SessionContextAction::SwitchSession => state.open_session_picker_dialog_from_ui(),
        SessionContextAction::LastSession => state.activate_last_session_from_ui(),
        SessionContextAction::Rename => state.open_rename_session_dialog_for(&target.session_id),
        SessionContextAction::MoveUp => state.move_session_from_ui(&target.session_id, -1),
        SessionContextAction::MoveDown => state.move_session_from_ui(&target.session_id, 1),
    }
}

fn apply_status(state: &mut AppState, intent: StatusIntent) -> bool {
    match intent {
        StatusIntent::Action(NativeChromeAction::FocusConversation(target)) => {
            let mut invocation = bootty_control::CommandInvocation::from_action(
                "agents.native.focus",
                bootty_control::Caller::Internal,
            );
            invocation.target = Some(target);
            state.commands.queue(invocation);
            true
        }
        // Conversation closure is host-owned; panel toggles arrive through shared commands.
        StatusIntent::Action(
            NativeChromeAction::CloseConversation(_)
            | NativeChromeAction::TogglePanel(_)
            | NativeChromeAction::SurfaceChooser(_)
            | NativeChromeAction::CancelSurfaceChooser(_),
        ) => false,
        StatusIntent::Action(NativeChromeAction::ToggleKeepAwake) => {
            state.toggle_keep_awake();
            true
        }
        StatusIntent::Action(NativeChromeAction::ActivateWindow {
            session_id,
            window_id,
        }) => state.apply_exact_mux_action(
            ExactMuxAction::Activate,
            ExactMuxTarget::window(state.mux_scope(), &session_id, &window_id),
        ),
        StatusIntent::Context {
            session_id,
            window_id,
            action,
        } => state.apply_exact_mux_action(
            match action {
                TabContextAction::Activate => ExactMuxAction::Activate,
                TabContextAction::NewTab => ExactMuxAction::NewTab,
                TabContextAction::PreviousTab => ExactMuxAction::RelativeWindow(-1),
                TabContextAction::NextTab => ExactMuxAction::RelativeWindow(1),
                TabContextAction::LastTab => ExactMuxAction::LastWindow,
                TabContextAction::MoveLeft => ExactMuxAction::MoveWindow(-1),
                TabContextAction::MoveRight => ExactMuxAction::MoveWindow(1),
                TabContextAction::ClosePane => ExactMuxAction::CloseWindowPane,
                TabContextAction::Rename => {
                    return state.open_rename_tab_dialog_for(&session_id, &window_id);
                }
            },
            ExactMuxTarget::window(state.mux_scope(), &session_id, &window_id),
        ),
        StatusIntent::Reorder { source, before } => {
            state.reorder_window_before_from_ui(&source, before.as_deref())
        }
    }
}

fn neutral_target(target: &ScopedSessionTarget) -> SessionTarget {
    SessionTarget {
        scope: key(target.scope),
        session_id: target.session_id.clone(),
    }
}

fn scoped_target(target: &SessionTarget) -> ScopedSessionTarget {
    ScopedSessionTarget::new(id(target.scope), target.session_id.clone())
}

const fn key(id: bootty_mux::controller::SpaceId) -> SpaceKey {
    SpaceKey(id.persistence_value())
}

const fn id(key: SpaceKey) -> bootty_mux::controller::SpaceId {
    bootty_mux::controller::SpaceId::from_persistence(key.0)
}

fn pane_menu_commands(
    state: &AppState,
    session: &bootty_mux::snapshot::MuxSession,
    window: &bootty_mux::snapshot::MuxWindow,
    selected_window: Option<&str>,
) -> Vec<(String, bootty_control::CommandInvocation)> {
    use bootty_control::{Caller, CommandInvocation, ResourceKind};
    use bootty_mux::capability::BindingOperation;
    let target =
        state.mux_resource_target(state.mux_scope(), ResourceKind::Session, &session.id, None);
    let mut commands = Vec::new();
    let mut add = |label: String, command: &str, args: Vec<String>| {
        let mut invocation = CommandInvocation::new(command, args, Caller::Keybinding);
        invocation.target.clone_from(&target);
        commands.push((label, invocation));
    };
    if state.supports_pane_operation(BindingOperation::ExtractPane)
        && window.panes.len() > 1
        && let Some(pane) = state
            .workspace
            .active
            .binding
            .window_focused_pane(&session.id, &window.id)
    {
        add(
            "Extract Active Pane into a Tab".to_owned(),
            "pane.extract",
            vec![pane.to_owned()],
        );
    }
    if state.supports_pane_operation(BindingOperation::MergeWindows)
        && let Some(selected) = selected_window.filter(|selected| *selected != window.id)
    {
        add(
            "Merge into Active Tab".to_owned(),
            "pane.merge",
            vec![window.id.clone(), selected.to_owned()],
        );
    }
    if state.supports_pane_operation(BindingOperation::MovePane)
        && let Some(pane) = state
            .workspace
            .active
            .binding
            .window_focused_pane(&session.id, &window.id)
    {
        for destination in &session.windows {
            if destination.id == window.id {
                continue;
            }
            if let Some(destination_pane) = state
                .workspace
                .active
                .binding
                .window_focused_pane(&session.id, &destination.id)
            {
                add(
                    format!(
                        "Move Active Pane to {}: {}",
                        destination.index, destination.name
                    ),
                    "pane.move",
                    vec![
                        pane.to_owned(),
                        destination_pane.to_owned(),
                        "right".to_owned(),
                    ],
                );
            }
        }
    }
    commands
}
