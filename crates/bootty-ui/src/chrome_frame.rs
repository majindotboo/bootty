//! App-owned projection and intent application for the GPUI chrome.

use num_traits::ToPrimitive as _;
use std::collections::{HashMap, HashSet};

use crate::chrome_projection::{MuxView, SessionProgressView, SessionView, WindowView};
use crate::gpui::chrome::{
    ChromeIntent, ChromeLayout, ChromePalette, ChromeSnapshot, NativeChromeAction, Rgba,
    SessionContextAction, SessionContextSnapshot, SessionTarget, SidebarPosition, SidebarProject,
    SidebarRow, SidebarRowKind, SidebarSnapshot, SidebarTask, SpaceKey, SpaceSnapshot,
    StatusAlignment, StatusBarSnapshot, StatusIntent, StatusItemSnapshot, StatusProgress,
    StatusSegmentSnapshot, TabContextAction, TabContextSnapshot, TitlebarSnapshot,
};
use crate::{clock::ClockSnapshot, metrics::MetricsService};
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
    pub(crate) clock: ClockSnapshot,
}

impl Default for NativeChrome {
    fn default() -> Self {
        Self {
            local_git: bootty_git::GitFactsCache::new(),
            remote_git: Vec::new(),
            artwork: crate::project_artwork::ProjectArtworkCache::default(),
            metrics: MetricsService::default(),
            clock: ClockSnapshot::now(),
        }
    }
}

impl NativeChrome {
    pub(crate) fn refresh(&mut self, now: std::time::Instant) {
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
        name: session.name.clone(),
        display_name,
        active: session.active,
        selected,
        cwd: if session
            .tag
            .identity
            .as_deref()
            .and_then(|identity| state.workspace.active.binding.sessions().get(identity))
            .is_some_and(|saved| saved.state.projectless)
        {
            None
        } else {
            session
                .tag
                .identity
                .as_deref()
                .and_then(|identity| state.workspace.active.binding.sessions().get(identity))
                .filter(|saved| !saved.cwd.is_empty())
                .map(|saved| saved.cwd.clone())
                .or_else(|| session.anchor.cwd.clone())
        },
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
            let colors = computed_session_color(group_index, *group_position, group_count);
            if !group.is_empty() {
                *group_position = group_position.saturating_add(1);
            }
            colors
        })
        .collect()
}

/// A session's color and dim color from its group's place in saved order. Each group turns the
/// golden angle from the last, so neighbors stay far apart and a new session changes no other
/// session's color.
fn computed_session_color(
    position: usize,
    group_position: usize,
    group_total: usize,
) -> (String, String) {
    const GOLDEN_ANGLE: f64 = 137.507_764;
    let base = position
        .to_f64()
        .unwrap_or_default()
        .mul_add(GOLDEN_ANGLE, 60.0)
        % 360.0;
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
        sidebar_tint
    };
    let layout_gap = if chrome.sidebar && !facts.fullscreen {
        chrome.gap
    } else {
        0.0
    };
    let (top_status, bottom_status) = status_bars(state, native, projection, status_background);
    // The status bar holds the tab strip, so it is a tab bar tall.
    let status_height = crate::gpui::UI_TAB_BAR_HEIGHT;
    ChromeSnapshot {
        palette,
        layout: ChromeLayout {
            left_dock_toggle: chrome.left_dock_toggle,
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
            status_height,
            sidebar_visible: chrome.sidebar,
            fullscreen: facts.fullscreen,
        },
        titlebar: TitlebarSnapshot {
            reserve_window_controls: config.window.reserves_macos_titlebar_button_area(),
            back: state.can_navigate(crate::state::NavigationStep::Back),
            forward: state.can_navigate(crate::state::NavigationStep::Forward),
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

/// A pull request in words and color: merged in the accent, closed as a failure, a draft muted,
/// and an open one by its checks.
/// The lines a session's checkout adds and removes against its last commit; none when clean.
fn sidebar_diff(
    added: Option<u64>,
    removed: Option<u64>,
    theme: crate::gpui::UiPalette,
) -> Option<crate::gpui::chrome::SidebarDiffSummary> {
    let (added, removed) = (added.unwrap_or(0), removed.unwrap_or(0));
    (added > 0 || removed > 0).then(|| crate::gpui::chrome::SidebarDiffSummary {
        added,
        removed,
        added_color: ui_color!(theme.success),
        removed_color: ui_color!(theme.destructive),
    })
}

fn sidebar_pull_request(
    status: Option<&bootty_git::github::PullRequestStatus>,
    theme: crate::gpui::UiPalette,
) -> Option<crate::gpui::chrome::SidebarPullRequest> {
    use bootty_git::github::{ChecksState, PullRequestState};
    let status = status?;
    let (state, color) = match (status.state, status.checks) {
        (PullRequestState::Merged, _) => ("Merged", theme.accent),
        (PullRequestState::Closed, _) => ("Closed", theme.destructive),
        (PullRequestState::Draft, _) => ("Draft", theme.muted),
        (PullRequestState::Open, ChecksState::Failing) => ("Open", theme.destructive),
        (PullRequestState::Open, ChecksState::Pending) => ("Open", theme.warning),
        (PullRequestState::Open, _) => ("Open", theme.success),
    };
    let checks = match status.checks {
        ChecksState::None => "",
        ChecksState::Pending => " · checks running",
        ChecksState::Passing => " · checks passing",
        ChecksState::Failing => " · checks failing",
    };
    Some(crate::gpui::chrome::SidebarPullRequest {
        number: status.number,
        title: status.title.clone(),
        url: status.url.clone(),
        status: format!("{state}{checks}"),
        color: ui_color!(color),
    })
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
    let theme = state.ui_theme().palette;
    (
        Some(status_snapshot(
            state,
            "top",
            &TOP_BAR,
            native,
            theme,
            projection,
            status_background,
        )),
        None,
    )
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
    if sidebar_config.modules.iter().any(|name| name == "sessions") {
        append_creation_drafts(state, &mut rows, palette);
    }
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
        title_visible: config.window.custom_chrome_title_visible(),
        group_by_project: sidebar_config.group_by_project,
        sort_order: sidebar_config.sort_order,
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

fn append_creation_drafts(state: &AppState, rows: &mut Vec<SidebarRow>, palette: ChromePalette) {
    let selected = state.selected_creation_draft();
    if selected.is_some() {
        for row in rows.iter_mut().filter(|row| row.kind.is_session()) {
            row.current = false;
            row.active = false;
        }
    }
    let scope = state.mux_scope();
    let Some(binding) = state
        .saved_session_invocation(scope, "session.saved", Vec::new())
        .and_then(|invocation| invocation.target)
    else {
        return;
    };
    for draft in state
        .creation_draft_summaries()
        .filter(|draft| draft.scope == scope)
    {
        if rows.iter().any(|row| {
            row.task
                .as_ref()
                .is_some_and(|task| task.identity == draft.identity)
        }) {
            continue;
        }
        let project = project_name(draft.project.as_deref()).to_owned();
        let row = SidebarRow {
            key: format!("draft:{}", draft.identity),
            text: draft.title,
            secondary: None,
            project: Some(SidebarProject {
                name: project.clone(),
                artwork: None,
            }),
            project_path: draft.project.clone(),
            cwd: Some(draft.cwd.clone()),
            host: None,
            branch: draft.branch,
            trailing: Some("Draft".into()),
            trailing_icon: None,
            trailing_color: None,
            number: None,
            indent: 0,
            tree: None,
            artwork: None,
            icon: Some("pencil".into()),
            diff: None,
            pull_request: None,
            worktree: false,
            scripts: Vec::new(),
            color: palette.text,
            dim_color: palette.muted,
            kind: SidebarRowKind::Draft {
                identity: draft.identity.clone(),
                binding: binding.clone(),
            },
            active: selected.as_deref() == Some(&draft.identity),
            current: selected.as_deref() == Some(&draft.identity),
            selectable: true,
            target: None,
            reorder_anchor: None,
            context: None,
            task: None,
        };
        if state.config().sidebar.group_by_project {
            if let Some(group) = rows.iter().position(|row| {
                row.kind == SidebarRowKind::Group && row.project_path == draft.project
            }) {
                rows.insert(group.saturating_add(1), row);
            } else {
                let key = format!(
                    "project:{}:{}",
                    scope.persistence_value(),
                    draft.project.as_deref().unwrap_or("")
                );
                rows.push(project_row(&row, key, &project, None, palette));
                rows.push(row);
            }
        } else {
            rows.push(row);
        }
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
        cwd: Some(project.cwd.clone()),
        host: None,
        branch: None,
        trailing: None,
        trailing_icon: None,
        trailing_color: None,
        number: None,
        indent: 0,
        tree: None,
        artwork: None,
        icon: Some("folder".to_owned()),
        diff: None,
        pull_request: None,
        worktree: false,
        scripts: Vec::new(),
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
            cwd: None,
            host: state
                .active_multiplexer()
                .remote
                .as_ref()
                .map(bootty_mux::RemoteTarget::label),
            branch: None,
            trailing: None,
            trailing_icon: None,
            trailing_color: None,
            number: None,
            indent: 0,
            tree: None,
            artwork: None,
            icon: Some("circle-dashed".to_owned()),
            diff: None,
            pull_request: None,
            worktree: false,
            scripts: Vec::new(),
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
        rows.extend(unclaimed.into_iter().map(|session| {
            SidebarRow {
                key: session.session_id.clone(),
                text: session.name,
                secondary: None,
                project: None,
                project_path: None,
                cwd: None,
                host: state
                    .active_multiplexer()
                    .remote
                    .as_ref()
                    .map(bootty_mux::RemoteTarget::label),
                branch: None,
                trailing: None,
                trailing_icon: None,
                trailing_color: None,
                number: None,
                indent: 2,
                tree: None,
                artwork: None,
                icon: Some("bootty".to_owned()),
                diff: None,
                pull_request: None,
                worktree: false,
                scripts: Vec::new(),
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
            }
        }));
    }
    rows
}

fn session_title<'a>(name: &'a str, project: &str, explicit: bool) -> &'a str {
    if explicit {
        return name;
    }
    let title = name.strip_prefix(&format!("{project}/")).unwrap_or(name);
    // A session named after its project or a path has no name of its own; one named after its
    // branch keeps the branch, which tells worktree sessions apart.
    if title == project || std::path::Path::new(title).is_absolute() {
        "Terminal"
    } else {
        title
    }
}

/// The sessions the sidebar lists, in order: each session once, by project when grouped, without
/// the tasks a conversation delegated, which show under their parent instead.
fn listed_sessions(projection: &ChromeProjection, grouped: bool) -> Vec<(usize, &SessionView)> {
    let mut groups = Vec::<(Option<&str>, Vec<(usize, &SessionView)>)>::new();
    for (index, session) in projection.mux.sessions.iter().enumerate() {
        let group = session_project_path(projection, session);
        if grouped && let Some((_, indices)) = groups.iter_mut().find(|(key, _)| *key == group) {
            indices.push((index, session));
        } else {
            groups.push((group, vec![(index, session)]));
        }
    }
    groups
        .into_iter()
        .flat_map(|(_, indices)| indices)
        .collect()
}

fn sidebar_rows(
    state: &AppState,
    native: &NativeChrome,
    projection: &ChromeProjection,
    palette: ChromePalette,
) -> Vec<SidebarRow> {
    let sessions = &projection.mux.sessions;
    let scope = state.mux_scope().persistence_value().to_string();
    let now = std::time::Instant::now();
    let mut rows = Vec::new();
    let grouped = state.config().sidebar.group_by_project;
    let names = state.saved_session_names();
    let mut last_group = None;
    for (index, session) in listed_sessions(projection, grouped) {
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
        let project_path = session_project_path(projection, session);
        let project = project_name(project_path);
        let group = project_path.unwrap_or("").to_owned();
        let mut base = sidebar_session_base(state, session, index, sessions.len(), palette);
        if let Some(task) = base.task.as_mut() {
            task.name = names.get(&task.identity).cloned().unwrap_or_default();
        }
        set_project(&mut base, state, project_path);
        let artwork = project_path.and_then(|path| {
            native.artwork.request(
                &scope,
                path,
                state.active_multiplexer().remote.as_ref(),
                now,
                &state.repaint,
            )
        });
        if grouped && last_group.as_ref() != Some(&group) {
            rows.push(project_row(
                &base,
                format!("project:{scope}:{group}"),
                project,
                artwork.clone(),
                palette,
            ));
        }
        let explicit_title = session
            .identity
            .as_deref()
            .and_then(|identity| state.workspace.active.binding.sessions().get(identity))
            .is_some_and(|saved| saved.explicit);
        let title = session_title(name, project, explicit_title);
        let row = session_row(
            state,
            facts,
            SidebarProject {
                name: project.to_owned(),
                artwork,
            },
            title,
            base,
        );
        rows.push(row);
        last_group = Some(group);
    }
    rows
}

/// A session's own row: its title, project, branch, pull request and diff.
fn session_row(
    state: &AppState,
    facts: bootty_git::GitSessionFacts,
    project: SidebarProject,
    title: &str,
    base: SidebarRow,
) -> SidebarRow {
    SidebarRow {
        text: title.to_owned(),
        project: Some(project),
        branch: facts.branch,
        pull_request: sidebar_pull_request(facts.pull_request.as_ref(), state.ui_theme().palette),
        diff: sidebar_diff(
            facts.diff_added,
            facts.diff_removed,
            state.ui_theme().palette,
        ),
        worktree: facts.project.is_some(),
        icon: Some("terminal".to_owned()),
        kind: SidebarRowKind::Session,
        selectable: true,
        ..base
    }
}

/// A session row's project: its path, and the names of the scripts it can run.
fn set_project(row: &mut SidebarRow, state: &AppState, path: Option<&str>) {
    row.project_path = path.map(str::to_owned);
    row.scripts = path
        .and_then(|path| {
            state
                .workspace
                .registered_projects(state.mux_scope())
                .find(|project| project.cwd == path)
        })
        .map_or_else(Vec::new, |project| {
            project
                .settings
                .scripts
                .iter()
                .map(|script| script.name.clone())
                .collect()
        });
}

/// The header a project's sessions group under, from one of its sessions' rows.
fn project_row(
    session: &SidebarRow,
    key: String,
    project: &str,
    artwork: Option<std::sync::Arc<gpui_kit::RenderImage>>,
    palette: ChromePalette,
) -> SidebarRow {
    SidebarRow {
        key,
        text: project.to_owned(),
        trailing: None,
        trailing_color: Some(palette.muted),
        color: palette.text,
        dim_color: palette.muted,
        artwork,
        icon: Some("folder".to_owned()),
        kind: SidebarRowKind::Group,
        active: false,
        current: false,
        target: None,
        task: None,
        ..session.clone()
    }
}

/// A linked worktree belongs to its main checkout's project; other sessions to their cwd.
fn session_project_path<'a>(
    projection: &'a ChromeProjection,
    session: &'a SessionView,
) -> Option<&'a str> {
    projection
        .session_facts
        .get(&session.id)
        .and_then(|facts| facts.project.as_deref())
        .or(session.cwd.as_deref())
}

fn project_name(path: Option<&str>) -> &str {
    path.and_then(|path| {
        path.trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
    })
    .filter(|name| !name.is_empty())
    .unwrap_or("No project")
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
        cwd: session.cwd.clone(),
        host: state
            .active_multiplexer()
            .remote
            .as_ref()
            .map(bootty_mux::RemoteTarget::label),
        branch: None,
        trailing: None,
        trailing_icon: None,
        trailing_color: None,
        number: None,
        indent: 0,
        tree: None,
        artwork: None,
        icon: None,
        diff: None,
        pull_request: None,
        worktree: false,
        scripts: Vec::new(),
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
                name: String::new(),
                title: String::new(),
                state: saved.state,
                binding: state
                    .saved_session_invocation(binding.scope(), "session.saved", Vec::new())
                    .and_then(|invocation| invocation.target),
                session: state.mux_resource_target(
                    binding.scope(),
                    bootty_control::ResourceKind::Session,
                    &session.id,
                    None,
                ),
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

/// The status bar's one layout: the session, when the sidebar doesn't show it, and the session's
/// tabs at the left; the machine's state and the time at the right.
const TOP_BAR: [(StatusAlignment, &str); 4] = [
    (StatusAlignment::Left, "session"),
    (StatusAlignment::Left, "windows"),
    (StatusAlignment::Right, "sysinfo"),
    (StatusAlignment::Right, "clock"),
];

fn status_snapshot(
    state: &AppState,
    key: &str,
    modules: &[(StatusAlignment, &str)],
    native: &NativeChrome,
    theme: crate::gpui::UiPalette,
    projection: &ChromeProjection,
    background: Rgba,
) -> StatusBarSnapshot {
    let segments = modules
        .iter()
        .enumerate()
        .filter_map(|(source_slot, &(align, module))| {
            let mut items = status_items(state, module, native, projection, theme);
            for (index, item) in items.iter_mut().enumerate() {
                item.key = format!("{source_slot}:{index}:{}", item.key);
            }
            (!items.is_empty()).then(|| StatusSegmentSnapshot {
                align,
                source_slot,
                surface: module.to_owned(),
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
    _state: &AppState,
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
            name.icon = Some("terminal".to_owned());
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
        ChromeIntent::Command(invocation) => dispatch_all(state, vec![invocation], &mut effects),
        ChromeIntent::Commands(invocations) => dispatch_all(state, invocations, &mut effects),
        // The GPUI host owns window moves.
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
        ChromeIntent::CloseSpace(space) => state.close_space_from_ui(id(space)),
        ChromeIntent::MoveSessionsToSpace { sessions, to } => sessions
            .iter()
            .all(|session| state.move_scoped_session_to_space(&scoped_target(session), id(to))),
        ChromeIntent::ActivateSession(target) => {
            apply_session_activation(state, &scoped_target(&target), &mut effects)
        }
        ChromeIntent::ReopenSession(target) => dispatch_saved(
            state,
            id(target.scope),
            "session.reopen",
            vec![target.session_id],
            &mut effects,
        ),
        ChromeIntent::RenameSavedSession(target) => {
            state.open_rename_session_dialog_for(&target.session_id)
        }
        ChromeIntent::AdoptSession(target) => {
            state.adopt_and_activate_scoped_session(&scoped_target(&target))
        }
        ChromeIntent::SessionContext { target, action } => {
            apply_session_context(state, &scoped_target(&target), action)
        }
        ChromeIntent::RunProjectScript {
            session,
            project,
            name,
        } => state.run_project_script(id(session.scope), &session.session_id, project, name),
        intent @ (ChromeIntent::CopyText(_)
        | ChromeIntent::RevealPath { .. }
        | ChromeIntent::OpenInEditor { .. }
        | ChromeIntent::NewSessionOnBranch { .. }
        | ChromeIntent::NewSessionInProject { .. }) => apply_place(state, intent, &mut effects),
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
        && let Some(invocation) = state.saved_session_invocation(
            target.scope,
            "session.reopen",
            vec![target.session_id.clone()],
        )
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

/// Runs `command` with one argument against a Space's binding; whether it succeeded.
fn dispatch_all(
    state: &mut AppState,
    invocations: Vec<bootty_control::CommandInvocation>,
    effects: &mut Vec<AppEffect>,
) -> bool {
    for invocation in invocations {
        state.dispatch_command(
            invocation,
            crate::state::ViewportSnapshot::default(),
            effects,
        );
    }
    true
}

fn dispatch_saved(
    state: &mut AppState,
    scope: bootty_mux::controller::SpaceId,
    command: &str,
    arguments: Vec<String>,
    effects: &mut Vec<AppEffect>,
) -> bool {
    state
        .saved_session_invocation(scope, command, arguments)
        .is_some_and(|invocation| {
            matches!(
                state.dispatch_command(
                    invocation,
                    crate::state::ViewportSnapshot::default(),
                    effects
                ),
                bootty_control::CommandOutcome::Success { .. }
            )
        })
}

/// A session's place: copy its path or branch, show its directory, or start a session there.
fn apply_place(state: &mut AppState, intent: ChromeIntent, effects: &mut Vec<AppEffect>) -> bool {
    match intent {
        ChromeIntent::CopyText(text) => {
            if let Err(error) = crate::platform::write_clipboard_text(&text) {
                state.record_error("Could not copy", &error);
            }
            true
        }
        ChromeIntent::RevealPath { scope, path } => {
            dispatch_saved(state, id(scope), "files.reveal", vec![path], effects)
        }
        ChromeIntent::OpenInEditor {
            scope,
            path,
            editor,
        } => dispatch_saved(
            state,
            id(scope),
            "files.open_in",
            vec![path, editor.to_owned()],
            effects,
        ),
        ChromeIntent::NewSessionInProject { scope, path } => {
            state.open_new_session_dialog_in(id(scope), path)
        }
        ChromeIntent::NewSessionOnBranch { scope, path } => {
            state.open_new_session_on_branch(id(scope), path)
        }
        _ => false,
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
        // The sessions toggle arrives through the shared dock command.
        StatusIntent::Action(NativeChromeAction::ToggleSessions) => false,
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
    let target =
        state.mux_resource_target(state.mux_scope(), ResourceKind::Session, &session.id, None);
    let mut commands = Vec::new();
    let mut add = |label: String, command: &str, args: Vec<String>| {
        let mut invocation = CommandInvocation::new(command, args, Caller::Keybinding);
        invocation.target.clone_from(&target);
        commands.push((label, invocation));
    };
    if window.panes.len() > 1
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
    if let Some(selected) = selected_window.filter(|selected| *selected != window.id) {
        add(
            "Merge into Active Tab".to_owned(),
            "pane.merge",
            vec![window.id.clone(), selected.to_owned()],
        );
    }
    if let Some(pane) = state
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
