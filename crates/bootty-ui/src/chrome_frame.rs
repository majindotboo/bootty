//! App-owned projection and intent application for the GPUI chrome.

use num_traits::ToPrimitive as _;
use std::collections::{HashMap, HashSet};

use crate::chrome_projection::{MuxView, SessionProgressView, SessionView, WindowView};
use crate::gpui::chrome::{
    ChromeIntent, ChromeLayout, ChromePalette, ChromeSnapshot, NativeChromeAction, Rgba,
    SessionContextAction, SessionContextSnapshot, SessionTarget, SidebarPosition, SidebarRow,
    SidebarRowKind, SidebarSnapshot, SpaceKey, SpaceSnapshot, StatusAlignment, StatusBarSnapshot,
    StatusIntent, StatusItemSnapshot, StatusProgress, StatusSegmentSnapshot, TabContextAction,
    TabContextSnapshot, TitlebarSnapshot, UsageMeterSnapshot,
};
use crate::{
    clock::ClockSnapshot,
    metrics::MetricsService,
    usage::{QuotaTone, UsageProvider, UsageService},
};
use bootty_config::config::SidebarPosition as ConfigSidebarPosition;
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
    home: Option<std::path::PathBuf>,
    pub(crate) local_git: bootty_git::GitFactsCache,
    remote_git: Vec<(
        bootty_config::config::RemoteConfig,
        RemoteGitCache,
        std::time::Instant,
    )>,
    pub(crate) metrics: MetricsService,
    pub(crate) usage: UsageService,
    pub(crate) clock: ClockSnapshot,
}

impl Default for NativeChrome {
    fn default() -> Self {
        Self {
            home: bootty_git::home_dir(),
            local_git: bootty_git::GitFactsCache::new(),
            remote_git: Vec::new(),
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
    let session_colors = session_colors(sessions, &display_names);
    let selected_window = state.mux().selected_window();
    let mut windows = Vec::new();
    let mut tab_contexts = HashMap::new();
    let mut selected_name = None;
    let mut selected_color = None;
    let mut session_views = Vec::with_capacity(sessions.len());
    for ((session, display_name), (color, dim_color)) in
        sessions.iter().zip(display_names).zip(session_colors)
    {
        let selected = selected_session.map_or(session.active, |selected| {
            selected == session.id || selected == session.name
        });
        if selected {
            selected_name = Some(if display_name.is_empty() {
                session.name.clone()
            } else {
                display_name.clone()
            });
            selected_color = Some(color.clone());
            (windows, tab_contexts) = selected_windows(state, session, selected_window);
        }
        session_views.push(session_view(
            state,
            session,
            display_name,
            (color, dim_color),
            selected,
        ));
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
    colors: (String, String),
    selected: bool,
) -> SessionView {
    let (color, dim_color) = colors;
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
        name: session.name.clone(),
        display_name,
        active: session.active,
        selected,
        cwd: session.anchor.cwd.clone(),
        pane_id: session.anchor.pane_id.clone(),
        pane_pid: session.anchor.pane_pid,
        process: session.anchor.process.clone(),
        pane_ids,
        color: Some(color),
        dim_color: Some(dim_color),
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
    let groups = sessions
        .iter()
        .zip(display_names)
        .map(|(session, display_name)| {
            let name = if display_name.is_empty() {
                &session.name
            } else {
                display_name
            };
            name.split_once('/')
                .map_or(name.as_str(), |(group, _)| group)
        })
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

pub fn snapshot(
    state: &AppState,
    native: &NativeChrome,
    projection: &ChromeProjection,
    width: f32,
    height: f32,
) -> ChromeSnapshot {
    let palette = chrome_palette(state.ui_theme().palette);
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
            dock_tabs: chrome.dock_tabs,
            terminal_tabs: chrome.terminal_tabs,

            width,
            height,
            sidebar_position: match config.sidebar.position {
                ConfigSidebarPosition::Left => SidebarPosition::Left,
                ConfigSidebarPosition::Right => SidebarPosition::Right,
            },
            sidebar_width: chrome.sidebar_width,
            gap: layout_gap,
            top_inset: facts.top_inset(
                config.window.fullscreen_tabs_in_notch,
                top_status.as_ref().map_or(0.0, |_| status_height),
                config.window.fullscreen_top_offset,
            ),
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
            "bottom",
            &chrome.bottom_segments,
            native,
            theme,
            projection,
            status_background,
        )
    });
    for kind in bootty_config::config::PanelKind::ALL {
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
            status_snapshot(key, &[], native, theme, projection, status_background)
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
    rows.extend(unclaimed_rows(state, palette));
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
        rows,
        footer: sidebar_footer(native, theme),
        title_visible: config.window.custom_chrome_title_visible(),
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

fn unclaimed_rows(state: &AppState, palette: ChromePalette) -> Vec<SidebarRow> {
    let scope = key(state.mux_scope());
    let mut rows = Vec::new();
    let unclaimed = state.unclaimed_sessions();
    if !unclaimed.is_empty() {
        rows.push(SidebarRow {
            key: "unassigned".to_owned(),
            text: "Unassigned".to_owned(),
            trailing: None,
            trailing_color: None,
            trailing_shimmer: false,
            number: None,
            indent: 0,
            tree: None,
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
        });
        rows.extend(unclaimed.into_iter().map(|session| SidebarRow {
            key: session.session_id.clone(),
            text: session.name,
            trailing: None,
            trailing_color: None,
            trailing_shimmer: false,
            number: None,
            indent: 2,
            tree: None,
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
        }));
    }
    rows
}

fn sidebar_rows(
    state: &AppState,
    native: &NativeChrome,
    projection: &ChromeProjection,
    palette: ChromePalette,
) -> Vec<SidebarRow> {
    let sessions = &projection.mux.sessions;
    let display_name = |session: &SessionView| {
        if session.display_name.is_empty() {
            session.name.clone()
        } else {
            session.display_name.clone()
        }
    };
    let names = sessions.iter().map(display_name).collect::<Vec<_>>();
    let mut groups = session_groups(&names);
    let mut modules = state.config().sidebar.session_modules.clone();
    if !state.config().sidebar.session_modules_configured {
        modules.extend(bootty_agents::AgentKind::ALL.map(|provider| provider.module().to_owned()));
    }
    let mut rows = Vec::new();
    let mut last_group = None;
    for (index, (session, name)) in sessions.iter().zip(&names).enumerate() {
        let (group, suffix) = name.split_once('/').unwrap_or((name, ""));
        let (count, emitted) = groups.entry(group).or_default();
        *emitted = emitted.saturating_add(1);
        let grouped = !group.is_empty() && *count > 1;
        let last = *emitted == *count;
        let base = sidebar_session_base(state, session, index, sessions.len(), palette);
        if grouped && last_group != Some(group) {
            rows.push(SidebarRow {
                key: format!("group:{group}:{}", session.id),
                text: group.to_owned(),
                kind: SidebarRowKind::Group,
                active: false,
                current: false,
                target: None,
                ..base.clone()
            });
        }
        let facts = projection
            .session_facts
            .get(&session.id)
            .cloned()
            .unwrap_or_default();
        let has_diff = facts.diff_added.is_some() && facts.diff_removed.is_some();
        let diff = sidebar_diff(&facts, &modules, state.ui_theme().palette);
        let process = facts
            .display_process
            .as_ref()
            .filter(|process| !process.is_empty());
        let trailing = (!has_diff && modules.iter().any(|module| module == "process"))
            .then(|| process.cloned())
            .flatten();
        let trailing_color = trailing.as_ref().map(|_| state.ui_theme().palette.subtext);
        rows.push(SidebarRow {
            diff,
            trailing,
            trailing_color,
            text: if grouped && !suffix.is_empty() {
                suffix
            } else {
                group
            }
            .to_owned(),
            kind: SidebarRowKind::Session,
            number: Some(index.saturating_add(1)),
            indent: if grouped { 2 } else { 0 },
            tree: Some(
                if !grouped {
                    "none"
                } else if last {
                    "last"
                } else {
                    "middle"
                }
                .to_owned(),
            ),
            selectable: true,
            ..base.clone()
        });
        let detail = |id: &str, icon: &str, text: String| {
            sidebar_detail(
                &base,
                grouped,
                last,
                id,
                icon,
                text,
                state.ui_theme().palette.subtext,
            )
        };
        rows.extend(sidebar_session_details(
            state, native, session, &facts, &modules, base.color, &detail,
        ));
        last_group = Some(group);
    }
    rows
}

fn sidebar_diff(
    facts: &bootty_git::GitSessionFacts,
    modules: &[String],
    theme: crate::gpui::UiPalette,
) -> Option<crate::gpui::chrome::SidebarDiffSummary> {
    if modules.iter().any(|module| module == "diffs") {
        facts
            .diff_added
            .zip(facts.diff_removed)
            .map(|(added, removed)| crate::gpui::chrome::SidebarDiffSummary {
                added,
                removed,
                added_color: theme.success,
                removed_color: theme.destructive,
            })
    } else {
        None
    }
}

fn session_groups(names: &[String]) -> HashMap<&str, (usize, usize)> {
    let mut groups = HashMap::<&str, (usize, usize)>::new();
    for name in names {
        let count = &mut groups
            .entry(name.split('/').next().unwrap_or(name))
            .or_default()
            .0;
        *count = count.saturating_add(1);
    }
    groups
}

fn sidebar_detail(
    base: &SidebarRow,
    grouped: bool,
    last: bool,
    id: &str,
    icon: &str,
    text: String,
    subtext: Rgba,
) -> SidebarRow {
    SidebarRow {
        key: format!("{}:{id}", base.key),
        text,
        icon: Some(icon.to_owned()),
        color: subtext,
        indent: if grouped { 4 } else { 2 },
        tree: Some(
            if !grouped {
                "none"
            } else if last {
                "blank"
            } else {
                "pipe"
            }
            .to_owned(),
        ),
        active: false,
        current: base.current,
        selectable: true,
        ..base.clone()
    }
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
        key: session.id.clone(),
        text: String::new(),
        trailing: None,
        trailing_color: None,
        trailing_shimmer: false,
        number: None,
        indent: 0,
        tree: None,
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
        context: Some(SessionContextSnapshot {
            can_activate: !session.selected,
            can_move_up: index > 0,
            can_move_down: index.saturating_add(1) < session_count,
            can_navigate: session_count > 1,
            can_return_to_last: state.mux().previous_selected_session().is_some(),
        }),
    }
}

fn sidebar_session_details(
    state: &AppState,
    native: &NativeChrome,
    session: &SessionView,
    facts: &bootty_git::GitSessionFacts,
    modules: &[String],
    color: Rgba,
    detail: &impl Fn(&str, &str, String) -> SidebarRow,
) -> Vec<SidebarRow> {
    let has_diff = facts.diff_added.is_some() && facts.diff_removed.is_some();
    let process = facts
        .display_process
        .as_ref()
        .filter(|process| !process.is_empty());
    let mut rows = Vec::new();
    for module in modules {
        match module.as_str() {
            "diffs" => {}
            "process" => {
                if has_diff && let Some(process) = process {
                    rows.push(detail("process", "terminal", process.clone()));
                }
            }
            "directory" => {
                let cwd = session.cwd.as_deref().unwrap_or("unknown");
                let home = state
                    .active_multiplexer()
                    .remote
                    .is_none()
                    .then_some(native.home.as_deref())
                    .flatten();
                rows.push(detail(
                    "cwd",
                    "folder",
                    bootty_git::project::display_path(cwd, home),
                ));
            }
            "branch" => {
                let mut row = detail(
                    "branch",
                    "git-branch",
                    facts.branch.clone().unwrap_or_else(|| "unknown".to_owned()),
                );
                row.trailing = match facts.branch_status {
                    bootty_git::BranchStatus::Current => None,
                    bootty_git::BranchStatus::Stale => Some("stale".to_owned()),
                    bootty_git::BranchStatus::Unknown => Some("unknown".to_owned()),
                };
                rows.push(row);
            }
            "ports" if !session.ports.is_empty() => rows.push(detail(
                "ports",
                "plug",
                session
                    .ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
            "progress" => {
                for (index, progress) in session.progresses.iter().enumerate() {
                    let mut row =
                        detail(&format!("progress:{index}"), "", progress.process.clone());
                    row.icon = None;
                    row.color = color;
                    row.dim_color = state.ui_theme().palette.border;
                    row.kind = SidebarRowKind::Progress {
                        value: (!progress.indeterminate).then_some(progress.value),
                        label: Some(progress.process.clone()),
                    };
                    rows.push(row);
                }
            }
            name => rows.extend(sidebar_agent_rows(state, session, name, detail)),
        }
    }
    rows
}

fn sidebar_agent_rows(
    state: &AppState,
    session: &SessionView,
    name: &str,
    detail: &impl Fn(&str, &str, String) -> SidebarRow,
) -> Vec<SidebarRow> {
    let agent_scope = state.mux_scope().persistence_value().to_string();
    let mut rows = Vec::new();
    let Some(provider) = bootty_agents::AgentKind::ALL
        .into_iter()
        .find(|provider| provider.module() == name)
    else {
        return Vec::new();
    };
    let Some(agents) = state.agent_service() else {
        return Vec::new();
    };
    // One row per pane running this agent, whichever window is active.
    for pane in &session.pane_ids {
        let agent = agents.snapshot_scoped(provider, Some(&agent_scope), Some(pane));
        if agent.source == bootty_agents::AgentSource::None {
            continue;
        }
        let theme = state.ui_theme().palette;
        let color = match provider {
            bootty_agents::AgentKind::Pi => theme.primary,
            bootty_agents::AgentKind::Codex => theme.accent,
            bootty_agents::AgentKind::Claude => theme.warning,
        };
        let mut row = detail(
            &format!("{name}:{pane}"),
            provider.icon(),
            provider.to_string(),
        );
        row.color = color;
        row.trailing = Some(if agent.unread() {
            format!("● {}", agent.display_status())
        } else {
            agent.display_status()
        });
        row.trailing_color = Some(if agent.status == bootty_agents::AgentStatus::Idle {
            theme.muted
        } else {
            color
        });
        row.trailing_shimmer = agent.status.is_working();
        rows.push(row);
    }
    rows
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
            let meter = window.meter(native.clock.epoch);
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
                    label: format!("{} {:.0}% left", window.label, meter.remaining_percent),
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
            let mut items = status_items(&configured.module, native, projection, theme);
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
        "windows" => window_status_items(projection, theme),
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
                .map(|minutes| format!(" {}:{:02}", minutes / 60, minutes % 60))
                .unwrap_or_default();
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
        ChromeIntent::ActivateSession(_) | ChromeIntent::AdoptSession(_)
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
            state.activate_scoped_session_from_ui(&scoped_target(&target))
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
    if handled && focus_terminal {
        effects.push(AppEffect::FocusTerminal);
    }
    effects
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
        _ if !state.activate_scoped_session_from_ui(target) => false,
        SessionContextAction::NewSession => state.open_new_session_dialog_from_ui(),
        SessionContextAction::SwitchSession => state.open_session_picker_dialog_from_ui(),
        SessionContextAction::LastSession => state.activate_last_session_from_ui(),
        SessionContextAction::Rename => state.open_rename_session_dialog_for(&target.session_id),
        SessionContextAction::MoveUp => state.move_session_from_ui(&target.session_id, -1),
        SessionContextAction::MoveDown => state.move_session_from_ui(&target.session_id, 1),
        SessionContextAction::MoveToSpace => state.open_space_picker_for(target),
        SessionContextAction::Ditch => state.open_ditch_session_dialog_for(&target.session_id),
    }
}

fn apply_status(state: &mut AppState, intent: StatusIntent) -> bool {
    match intent {
        StatusIntent::Action(NativeChromeAction::TogglePanel(_)) => false, // Dispatched as a Command by the chrome button.
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
