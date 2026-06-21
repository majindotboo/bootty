use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use super::snapshot::MuxSession;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SidebarMetadata {
    sessions: BTreeMap<String, SidebarSessionMetadata>,
    usage_lines: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SidebarSessionMetadata {
    pub branch: Option<String>,
    pub diff: Option<DiffStat>,
    pub attention: bool,
    pub status: Option<String>,
    pub progress: Option<u8>,
    pub process_cpu: Option<String>,
    pub agent_status: Option<String>,
    pub processes: Vec<ProcessStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarMetadataSession {
    id: String,
    name: String,
    cwd: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessStatus {
    pub name: String,
    pub cpu_pct: f32,
    pub mem_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub added: u32,
    pub removed: u32,
}

impl SidebarMetadataSession {
    fn from_mux_session(session: &MuxSession) -> Self {
        Self {
            id: session.id.clone(),
            name: session.name.clone(),
            cwd: session.anchor.cwd.clone(),
        }
    }
}

impl SidebarMetadata {
    pub fn get(&self, session_name: &str) -> Option<&SidebarSessionMetadata> {
        self.sessions.get(session_name)
    }

    pub fn insert(&mut self, session_name: impl Into<String>, metadata: SidebarSessionMetadata) {
        self.sessions.insert(session_name.into(), metadata);
    }

    pub fn usage_lines(&self) -> &[String] {
        &self.usage_lines
    }

    pub fn set_usage_lines(&mut self, usage_lines: Vec<String>) {
        self.usage_lines = usage_lines;
    }
}

pub fn sidebar_metadata_sessions(sessions: &[MuxSession]) -> Vec<SidebarMetadataSession> {
    sidebar_metadata_sessions_for_prefix(sessions, sessions.len())
}

pub fn sidebar_metadata_sessions_for_prefix(
    sessions: &[MuxSession],
    max_sessions: usize,
) -> Vec<SidebarMetadataSession> {
    let mut metadata_sessions = Vec::with_capacity(sessions.len().min(max_sessions));
    for session in sessions.iter().take(max_sessions) {
        if !needs_sidebar_metadata_request(session) {
            continue;
        }
        metadata_sessions.push(SidebarMetadataSession::from_mux_session(session));
    }
    metadata_sessions
}

fn needs_sidebar_metadata_request(session: &MuxSession) -> bool {
    session.id.starts_with('$') || session.anchor.cwd.is_some()
}

pub fn collect_sidebar_metadata(sessions: &[SidebarMetadataSession]) -> SidebarMetadata {
    let mut metadata = SidebarMetadata {
        usage_lines: collect_usage_lines(32),
        ..SidebarMetadata::default()
    };
    let tmux_metadata = has_tmux_sessions(sessions).then(|| {
        let active_panes = tmux_active_panes();
        TmuxSidebarMetadata {
            process_status: tmux_active_process_status(&active_panes),
            agent_status: tmux_agent_status(&active_panes),
            session_options: tmux_session_options_by_id(),
        }
    });
    let mut repo_metadata = HashMap::<&str, (Option<String>, Option<DiffStat>)>::new();
    for session in sessions {
        let (branch, diff) = session
            .cwd
            .as_deref()
            .map(|cwd| {
                repo_metadata
                    .entry(cwd)
                    .or_insert_with(|| {
                        (
                            git_branch(cwd).filter(|branch| !branch.is_empty()),
                            git_diff_stat(cwd),
                        )
                    })
                    .clone()
            })
            .unwrap_or_default();
        let tmux = tmux_metadata
            .as_ref()
            .and_then(|metadata| metadata.session_options.get(&session.id))
            .cloned()
            .unwrap_or_default();
        let process = tmux_metadata
            .as_ref()
            .and_then(|metadata| metadata.process_status.get(&session.id));
        let session_meta = SidebarSessionMetadata {
            branch,
            diff,
            attention: tmux.attention,
            status: tmux.status,
            progress: tmux.progress,
            process_cpu: process.map(|status| format!("{:.1}%", status.cpu_pct)),
            agent_status: tmux_metadata
                .as_ref()
                .and_then(|metadata| metadata.agent_status.get(&session.id))
                .cloned(),
            processes: process.cloned().into_iter().collect(),
        };
        if !session_meta.is_empty() {
            metadata.insert(session.name.clone(), session_meta);
        }
    }
    metadata
}

struct TmuxSidebarMetadata {
    process_status: BTreeMap<String, ProcessStatus>,
    agent_status: BTreeMap<String, String>,
    session_options: BTreeMap<String, SidebarSessionMetadata>,
}

fn has_tmux_sessions(sessions: &[SidebarMetadataSession]) -> bool {
    sessions.iter().any(|session| session.id.starts_with('$'))
}

impl SidebarSessionMetadata {
    fn is_empty(&self) -> bool {
        self.branch.is_none()
            && self.diff.is_none()
            && !self.attention
            && self.status.is_none()
            && self.progress.is_none()
            && self.process_cpu.is_none()
            && self.agent_status.is_none()
            && self.processes.is_empty()
    }
}

fn tmux_session_options_by_id() -> BTreeMap<String, SidebarSessionMetadata> {
    let output = Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_id}\t#{@attention}\t#{@sidebar_status}\t#{@sidebar_progress}",
        ])
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return BTreeMap::new();
    };
    if !output.status.success() {
        return BTreeMap::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_tmux_session_options_line)
        .collect()
}

fn parse_tmux_session_options_line(line: &str) -> Option<(String, SidebarSessionMetadata)> {
    let (session_id, options) = line.split_once('\t')?;
    (!session_id.is_empty()).then_some(())?;
    parse_tmux_session_options(options).map(|metadata| (session_id.to_owned(), metadata))
}

fn parse_tmux_session_options(output: &str) -> Option<SidebarSessionMetadata> {
    let line = output.lines().next().unwrap_or_default();
    let mut fields = line.split('\t');
    let attention = fields.next().is_some_and(|field| field == "1");
    let status = fields
        .next()
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_owned);
    let progress = fields
        .next()
        .and_then(|field| field.parse::<u8>().ok())
        .map(|progress| progress.min(100));
    let meta = SidebarSessionMetadata {
        attention,
        status,
        progress,
        ..SidebarSessionMetadata::default()
    };
    (!meta.is_empty()).then_some(meta)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TmuxActivePane {
    session_id: String,
    pane_id: String,
    pid: u32,
    command: String,
}

fn tmux_active_panes() -> Vec<TmuxActivePane> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_id}\t#{pane_active}\t#{pane_id}\t#{pane_pid}\t#{pane_current_command}",
        ])
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_tmux_active_pane)
        .collect()
}

fn tmux_active_process_status(panes: &[TmuxActivePane]) -> BTreeMap<String, ProcessStatus> {
    if panes.is_empty() {
        return BTreeMap::new();
    }

    let samples = build_process_info();
    let children = build_children(&samples);
    panes
        .iter()
        .map(|pane| {
            let mut memo = HashMap::new();
            let (cpu_pct, mem_bytes) = subtree_usage(pane.pid, &children, &samples, &mut memo);
            (
                pane.session_id.clone(),
                ProcessStatus {
                    name: pane.command.clone(),
                    cpu_pct,
                    mem_bytes,
                },
            )
        })
        .collect()
}

#[derive(Clone, Default)]
struct ProcSample {
    ppid: u32,
    cpu_pct: f32,
    rss_bytes: u64,
}

fn build_process_info() -> HashMap<u32, ProcSample> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,pcpu=,rss="])
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let mut samples = HashMap::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(cpu), Some(rss)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(pid), Ok(ppid), Ok(rss)) =
            (pid.parse::<u32>(), ppid.parse::<u32>(), rss.parse::<u64>())
        else {
            continue;
        };
        samples.insert(
            pid,
            ProcSample {
                ppid,
                cpu_pct: cpu.parse::<f32>().unwrap_or(0.0).max(0.0),
                rss_bytes: rss.saturating_mul(1024),
            },
        );
    }
    samples
}

fn build_children(samples: &HashMap<u32, ProcSample>) -> HashMap<u32, Vec<u32>> {
    let mut children = HashMap::new();
    for (&pid, sample) in samples {
        children
            .entry(sample.ppid)
            .or_insert_with(Vec::new)
            .push(pid);
    }
    children
}

fn subtree_usage(
    pid: u32,
    children: &HashMap<u32, Vec<u32>>,
    samples: &HashMap<u32, ProcSample>,
    memo: &mut HashMap<u32, (f32, u64)>,
) -> (f32, u64) {
    if let Some(usage) = memo.get(&pid).copied() {
        return usage;
    }
    let mut cpu = samples
        .get(&pid)
        .map(|sample| sample.cpu_pct)
        .unwrap_or(0.0);
    let mut mem = samples
        .get(&pid)
        .map(|sample| sample.rss_bytes)
        .unwrap_or(0);
    if let Some(kids) = children.get(&pid) {
        for kid in kids {
            let (kid_cpu, kid_mem) = subtree_usage(*kid, children, samples, memo);
            cpu += kid_cpu;
            mem = mem.saturating_add(kid_mem);
        }
    }
    memo.insert(pid, (cpu, mem));
    (cpu, mem)
}

fn parse_tmux_active_pane(line: &str) -> Option<TmuxActivePane> {
    let mut fields = line.split('\t');
    let session_id = fields.next()?;
    let active = fields.next()?;
    let pane_id = fields.next()?;
    let pid = fields.next()?;
    let command = fields.next()?.trim();
    if active != "1" || session_id.is_empty() || pane_id.is_empty() || command.is_empty() {
        return None;
    }
    let pid = pid.parse::<u32>().ok()?;
    Some(TmuxActivePane {
        session_id: session_id.to_owned(),
        pane_id: pane_id.to_owned(),
        pid,
        command: command.rsplit('/').next().unwrap_or(command).to_owned(),
    })
}

fn tmux_agent_status(panes: &[TmuxActivePane]) -> BTreeMap<String, String> {
    panes
        .iter()
        .filter_map(|pane| {
            agent_command(&pane.command).and_then(|agent| {
                capture_agent_status(&pane.pane_id, agent)
                    .map(|status| (pane.session_id.clone(), status))
            })
        })
        .collect()
}

fn agent_command(command: &str) -> Option<&'static str> {
    if command.eq_ignore_ascii_case("claude") {
        Some("claude")
    } else if command.eq_ignore_ascii_case("codex") {
        Some("codex")
    } else if command.eq_ignore_ascii_case("opencode") {
        Some("opencode")
    } else {
        None
    }
}

fn capture_agent_status(pane_id: &str, agent: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", pane_id, "-p", "-S", "-30"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_agent_status(agent, &String::from_utf8_lossy(&output.stdout))
}

fn parse_agent_status(agent: &str, text: &str) -> Option<String> {
    let activity = match agent {
        "claude" => parse_star_activity(text),
        "codex" => parse_codex_activity(text),
        "opencode" => parse_opencode_activity(text),
        _ => None,
    };
    if let Some(activity) = activity {
        return Some(format!("{agent} {activity}"));
    }
    if is_agent_asking(agent, text) {
        return Some(format!("{agent} asking"));
    }
    None
}

fn parse_star_activity(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        let mut chars = trimmed.chars();
        let first = chars.next()?;
        if !matches!(
            first,
            '\u{00B7}'
                | '\u{2022}'
                | '\u{273B}'
                | '\u{22C6}'
                | '\u{2726}'
                | '\u{2727}'
                | '\u{2736}'
                | '\u{2722}'
                | '\u{273D}'
                | '\u{2733}'
        ) || chars.next() != Some(' ')
        {
            return None;
        }
        let rest = chars.collect::<String>();
        rest.contains('\u{2026}').then(|| {
            rest.split_whitespace()
                .next()
                .unwrap_or("working")
                .trim_end_matches('\u{2026}')
                .to_owned()
                + "…"
        })
    })
}

fn parse_codex_activity(text: &str) -> Option<String> {
    text.lines()
        .any(|line| line.trim().starts_with("• Working"))
        .then(|| "Working…".to_owned())
}

fn parse_opencode_activity(text: &str) -> Option<String> {
    let bottom = text.lines().rev().take(10).collect::<Vec<_>>().join(" ");
    (bottom.contains("esc") && bottom.contains("interrupt")).then(|| "Working…".to_owned())
}

fn is_agent_asking(agent: &str, text: &str) -> bool {
    match agent {
        "claude" => text
            .lines()
            .any(|line| line.contains("Enter to select") || line.contains("enter to select")),
        "codex" => text.lines().any(|line| line.contains("to submit answer")),
        "opencode" => {
            let text = text.lines().collect::<String>();
            text.contains("select") && text.contains("submit") && text.contains("dismiss")
        }
        _ => false,
    }
}

const CODEX_USAGE_LOG_FILE: &str = "codex-usage-log.tsv";
const CODEX_PROVIDER_LABEL: &str = "\u{e7cf}";
const CODEX_PROVIDER_COLOR: Rgb = Rgb(0x74, 0xc7, 0xec);
const USAGE_DIM_COLOR: Rgb = Rgb(0x6c, 0x70, 0x86);
const USAGE_ORANGE_COLOR: Rgb = Rgb(0xfa, 0xb3, 0x87);
const USAGE_RED_COLOR: Rgb = Rgb(0xef, 0x44, 0x44);
const USAGE_GREEN_COLOR: Rgb = Rgb(0xa6, 0xe3, 0xa1);

#[derive(Clone, Copy)]
struct Rgb(u8, u8, u8);

#[derive(Clone, Copy)]
struct CodexUsageSample {
    primary_percent: f64,
    primary_reset: i64,
    secondary_percent: f64,
    secondary_reset: i64,
}

#[derive(Clone, Copy)]
struct UsageWindow {
    label: &'static str,
    used_percent: f64,
    window_secs: i64,
    reset_secs: i64,
}

fn collect_usage_lines(width: u16) -> Vec<String> {
    let path = std::env::temp_dir().join(CODEX_USAGE_LOG_FILE);
    let data = fs::read_to_string(path).unwrap_or_default();
    render_codex_usage_lines(&data, usize::from(width), now_secs())
}

fn render_codex_usage_lines(data: &str, width: usize, now_secs: i64) -> Vec<String> {
    let Some(sample) = data
        .lines()
        .filter_map(parse_codex_usage_sample)
        .next_back()
    else {
        return Vec::new();
    };
    let windows = [
        UsageWindow {
            label: "5h",
            used_percent: sample.primary_percent,
            window_secs: 5 * 3600,
            reset_secs: seconds_until_next_reset(sample.primary_reset, now_secs, 5 * 3600),
        },
        UsageWindow {
            label: "7d",
            used_percent: sample.secondary_percent,
            window_secs: 7 * 24 * 3600,
            reset_secs: seconds_until_next_reset(sample.secondary_reset, now_secs, 7 * 24 * 3600),
        },
    ];
    render_sidebar_usage_lines(width, &windows)
}

fn parse_codex_usage_sample(line: &str) -> Option<CodexUsageSample> {
    let mut fields = line.split('\t');
    let _timestamp = fields.next()?;
    Some(CodexUsageSample {
        primary_percent: fields.next()?.parse().ok()?,
        primary_reset: fields.next()?.parse().ok()?,
        secondary_percent: fields.next()?.parse().ok()?,
        secondary_reset: fields.next()?.parse().ok()?,
    })
}

fn seconds_until_next_reset(reset_epoch_secs: i64, now_secs: i64, window_secs: i64) -> i64 {
    let remaining = reset_epoch_secs - now_secs;
    if remaining > 0 || window_secs <= 0 {
        return remaining;
    }
    let rolled = remaining.rem_euclid(window_secs);
    if rolled == 0 { window_secs } else { rolled }
}

fn render_sidebar_usage_lines(width: usize, windows: &[UsageWindow]) -> Vec<String> {
    let safe_width = width.max(1);
    let bar_width = safe_width.saturating_sub(2);
    let mut lines = Vec::with_capacity(windows.len() * 2);
    for window in windows {
        let label = format!("{CODEX_PROVIDER_LABEL} {}", window.label);
        let used = window.used_percent.clamp(0.0, 100.0);
        let remaining_pct = (100.0 - used).clamp(0.0, 100.0);
        let fill_color = quota_color(used, window.reset_secs, window.window_secs);
        let mut right = format!("{}%", remaining_pct.round() as i64);
        if window.reset_secs > 0
            && window.window_secs > 0
            && let Some(balance) = pace_balance_secs(used, window.reset_secs, window.window_secs)
            && balance != 0
        {
            right.push(' ');
            right.push_str(&format_pace(balance));
        }
        if window.reset_secs > 0 {
            right.push(' ');
            right.push('↺');
            right.push_str(&format_reset(window.reset_secs));
        }
        let gap = safe_width.saturating_sub(label.chars().count() + right.chars().count() + 2);
        lines.push(format!(
            " {}{}{} ",
            ansi_fg(CODEX_PROVIDER_COLOR, &label),
            " ".repeat(gap),
            ansi_fg(fill_color, &right)
        ));
        lines.push(format!(
            " {} ",
            render_usage_bar_line(
                bar_width,
                remaining_pct,
                elapsed_percent(window),
                fill_color
            )
        ));
    }
    lines
}

fn render_usage_bar_line(
    width: usize,
    remaining_pct: f64,
    elapsed_pct: f64,
    fill_color: Rgb,
) -> String {
    if width == 0 {
        return String::new();
    }
    let remaining_cells = ((remaining_pct / 100.0) * width as f64)
        .round()
        .clamp(0.0, width as f64) as usize;
    let tick_cell = (elapsed_pct > 0.0).then(|| {
        let expected_remaining_pct = (100.0 - elapsed_pct).clamp(0.0, 100.0);
        ((expected_remaining_pct / 100.0) * width as f64)
            .round()
            .clamp(0.0, width.saturating_sub(1) as f64) as usize
    });
    let tick_color = pace_marker_color(100.0 - remaining_pct, elapsed_pct);
    let mut out = String::new();
    for index in 0..width {
        if Some(index) == tick_cell {
            out.push_str(&ansi_fg(tick_color, "│"));
        } else if index < remaining_cells {
            out.push_str(&ansi_fg(fill_color, "▓"));
        } else {
            out.push_str(&ansi_fg(USAGE_DIM_COLOR, "░"));
        }
    }
    out
}

fn elapsed_percent(window: &UsageWindow) -> f64 {
    if window.window_secs <= 0 || window.reset_secs <= 0 {
        return 0.0;
    }
    let elapsed_secs = (window.window_secs - window.reset_secs).max(0) as f64;
    (elapsed_secs / window.window_secs as f64 * 100.0).clamp(0.0, 100.0)
}

fn pace_balance_secs(used_percent: f64, reset_secs: i64, window_secs: i64) -> Option<i64> {
    let elapsed_secs = window_secs - reset_secs;
    if elapsed_secs < 60 {
        return None;
    }
    let remaining_pct = (100.0 - used_percent).clamp(0.0, 100.0);
    let expected_remaining_pct = (reset_secs as f64 / window_secs as f64) * 100.0;
    Some(((remaining_pct - expected_remaining_pct) * window_secs as f64 / 100.0) as i64)
}

fn quota_color(used_percent: f64, reset_secs: i64, window_secs: i64) -> Rgb {
    if window_secs <= 0 || reset_secs <= 0 {
        let urgency = (used_percent / 100.0).clamp(0.0, 1.0) as f32;
        return urgency_tint(urgency);
    }
    let elapsed_pct = ((window_secs - reset_secs) as f64 / window_secs as f64) * 100.0;
    let ratio = if elapsed_pct > 0.0 {
        used_percent / elapsed_pct
    } else if used_percent > 0.0 {
        f64::INFINITY
    } else {
        1.0
    };
    let urgency = ((ratio - 1.0) / 0.5).clamp(0.0, 1.0) as f32;
    urgency_tint(urgency)
}

fn pace_marker_color(used_percent: f64, elapsed_pct: f64) -> Rgb {
    let deficit = used_percent - elapsed_pct;
    if deficit <= 0.0 {
        USAGE_GREEN_COLOR
    } else if deficit < 3.0 {
        USAGE_ORANGE_COLOR
    } else {
        USAGE_RED_COLOR
    }
}

fn urgency_tint(urgency: f32) -> Rgb {
    let urgency = urgency.clamp(0.0, 1.0);
    let red_end = blend(USAGE_RED_COLOR, CODEX_PROVIDER_COLOR, 0.25);
    if urgency < 0.5 {
        blend(
            CODEX_PROVIDER_COLOR,
            USAGE_ORANGE_COLOR,
            urgency * 2.0 * 0.75,
        )
    } else {
        let mid = blend(CODEX_PROVIDER_COLOR, USAGE_ORANGE_COLOR, 0.75);
        blend(mid, red_end, (urgency - 0.5) * 2.0)
    }
}

fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let mix = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn format_reset(secs: i64) -> String {
    let secs = secs.max(0);
    if secs >= 86400 {
        format!(
            "{}d{:02}:{:02}",
            secs / 86400,
            (secs % 86400) / 3600,
            (secs % 3600) / 60
        )
    } else if secs >= 3600 {
        format!("{}h{:02}", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}m", secs / 60)
    }
}

fn format_pace(secs: i64) -> String {
    let abs_secs = secs.unsigned_abs();
    let sign = if secs >= 0 { '+' } else { '-' };
    let text = if abs_secs >= 86400 {
        format!("{}d{}h", abs_secs / 86400, (abs_secs % 86400) / 3600)
    } else if abs_secs >= 3600 {
        format!("{}h{:02}", abs_secs / 3600, (abs_secs % 3600) / 60)
    } else {
        format!("{}m", abs_secs / 60)
    };
    format!("{sign}{text}")
}

fn ansi_fg(rgb: Rgb, text: &str) -> String {
    format!("\x1b[38;2;{};{};{}m{text}\x1b[39m", rgb.0, rgb.1, rgb.2)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn git_branch(dir: &str) -> Option<String> {
    git_output(dir, ["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|output| output.trim().to_owned())
        .filter(|branch| !branch.is_empty())
}

fn git_diff_stat(dir: &str) -> Option<DiffStat> {
    let output = git_output(dir, ["diff", "HEAD", "--numstat"])?;
    parse_git_numstat(&output)
}

fn git_output<const N: usize>(dir: &str, args: [&str; N]) -> Option<String> {
    if !Path::new(dir).exists() {
        return None;
    }
    let output = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-C", dir])
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_git_numstat(output: &str) -> Option<DiffStat> {
    let mut diff = DiffStat::default();
    for line in output.lines() {
        let mut fields = line.split('\t');
        let added = fields.next().and_then(|field| field.parse::<u32>().ok());
        let removed = fields.next().and_then(|field| field.parse::<u32>().ok());
        if let (Some(added), Some(removed)) = (added, removed) {
            diff.added = diff.added.saturating_add(added);
            diff.removed = diff.removed.saturating_add(removed);
        }
    }
    (diff.added > 0 || diff.removed > 0).then_some(diff)
}

#[cfg(test)]
mod tests {
    use super::super::snapshot::{MuxPaneAnchor, MuxSession, MuxWindow};
    use super::*;

    #[test]
    fn parses_git_numstat_into_added_and_removed_totals() {
        assert_eq!(
            parse_git_numstat("7\t4\tsrc/lib.rs\n-\t-\timage.png\n3\t2\tREADME.md\n"),
            Some(DiffStat {
                added: 10,
                removed: 6
            })
        );
    }

    #[test]
    fn empty_git_numstat_has_no_diff() {
        assert_eq!(parse_git_numstat(""), None);
    }

    #[test]
    fn sidebar_metadata_sessions_keep_only_worker_inputs() {
        let tmux_anchor = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%1".to_owned()),
            cwd: Some("/tmp/project".to_owned()),
            process: Some("zsh".to_owned()),
        };
        let native_repo_anchor = MuxPaneAnchor {
            session_id: "local-repo".to_owned(),
            pane_id: None,
            cwd: Some("/tmp/native".to_owned()),
            process: Some("zsh".to_owned()),
        };
        let native_empty_anchor = MuxPaneAnchor {
            session_id: "local-empty".to_owned(),
            pane_id: None,
            cwd: None,
            process: Some("zsh".to_owned()),
        };
        let sessions = vec![
            MuxSession {
                id: "$1".to_owned(),
                name: "work/api".to_owned(),
                active: true,
                anchor: tmux_anchor.clone(),
                active_window_id: Some("@1".to_owned()),
                windows: vec![MuxWindow {
                    id: "@1".to_owned(),
                    index: 0,
                    name: "editor".to_owned(),
                    active: true,
                    anchor: tmux_anchor,
                }],
            },
            MuxSession {
                id: "local-repo".to_owned(),
                name: "native/repo".to_owned(),
                active: false,
                anchor: native_repo_anchor,
                active_window_id: None,
                windows: Vec::new(),
            },
            MuxSession {
                id: "local-empty".to_owned(),
                name: "native/empty".to_owned(),
                active: false,
                anchor: native_empty_anchor,
                active_window_id: None,
                windows: Vec::new(),
            },
        ];

        let metadata_sessions = sidebar_metadata_sessions(&sessions);

        assert_eq!(
            metadata_sessions,
            vec![
                SidebarMetadataSession {
                    id: "$1".to_owned(),
                    name: "work/api".to_owned(),
                    cwd: Some("/tmp/project".to_owned()),
                },
                SidebarMetadataSession {
                    id: "local-repo".to_owned(),
                    name: "native/repo".to_owned(),
                    cwd: Some("/tmp/native".to_owned()),
                }
            ]
        );
    }

    #[test]
    fn sidebar_metadata_sessions_for_prefix_skips_offscreen_sessions() {
        let sessions = vec![
            MuxSession {
                id: "$1".to_owned(),
                name: "work/one".to_owned(),
                active: true,
                anchor: MuxPaneAnchor {
                    session_id: "$1".to_owned(),
                    pane_id: Some("%1".to_owned()),
                    cwd: Some("/tmp/one".to_owned()),
                    process: Some("zsh".to_owned()),
                },
                active_window_id: None,
                windows: Vec::new(),
            },
            MuxSession {
                id: "$2".to_owned(),
                name: "work/two".to_owned(),
                active: false,
                anchor: MuxPaneAnchor {
                    session_id: "$2".to_owned(),
                    pane_id: Some("%2".to_owned()),
                    cwd: Some("/tmp/two".to_owned()),
                    process: Some("zsh".to_owned()),
                },
                active_window_id: None,
                windows: Vec::new(),
            },
        ];

        let metadata_sessions = sidebar_metadata_sessions_for_prefix(&sessions, 1);

        assert_eq!(
            metadata_sessions,
            vec![SidebarMetadataSession {
                id: "$1".to_owned(),
                name: "work/one".to_owned(),
                cwd: Some("/tmp/one".to_owned()),
            }]
        );
    }

    #[test]
    fn native_sidebar_metadata_sessions_do_not_need_tmux_polling() {
        let sessions = vec![SidebarMetadataSession {
            id: "local".to_owned(),
            name: "local".to_owned(),
            cwd: None,
        }];

        assert!(!has_tmux_sessions(&sessions));
    }

    #[test]
    fn tmux_sidebar_metadata_sessions_need_tmux_polling() {
        let sessions = vec![SidebarMetadataSession {
            id: "$1".to_owned(),
            name: "work".to_owned(),
            cwd: None,
        }];

        assert!(has_tmux_sessions(&sessions));
    }

    #[test]
    fn parses_tmux_status_and_progress_options() {
        let meta = parse_tmux_session_options("1\treview needed\t142\n").unwrap();

        assert!(meta.attention);
        assert_eq!(meta.status.as_deref(), Some("review needed"));
        assert_eq!(meta.progress, Some(100));
    }

    #[test]
    fn parses_tmux_session_options_line_with_session_id() {
        let (session_id, meta) = parse_tmux_session_options_line("$2\t0\tbuilding\t64").unwrap();

        assert_eq!(session_id, "$2");
        assert!(!meta.attention);
        assert_eq!(meta.status.as_deref(), Some("building"));
        assert_eq!(meta.progress, Some(64));
    }

    #[test]
    fn empty_tmux_session_options_line_is_ignored() {
        assert_eq!(parse_tmux_session_options_line("$2\t\t\t"), None);
    }

    #[test]
    fn parses_active_tmux_pane_once_for_process_and_agent_metadata() {
        assert_eq!(
            parse_tmux_active_pane("$2\t1\t%7\t1234\t/opt/homebrew/bin/codex"),
            Some(TmuxActivePane {
                session_id: "$2".to_owned(),
                pane_id: "%7".to_owned(),
                pid: 1234,
                command: "codex".to_owned(),
            })
        );
        assert_eq!(parse_tmux_active_pane("$2\t0\t%7\t1234\tcodex"), None);
        assert_eq!(parse_tmux_active_pane("$2\t1\t%7\tbad\tcodex"), None);
    }

    #[test]
    fn agent_command_matches_known_agents_case_insensitively() {
        assert_eq!(agent_command("Codex"), Some("codex"));
        assert_eq!(agent_command("claude"), Some("claude"));
        assert_eq!(agent_command("opencode"), Some("opencode"));
        assert_eq!(agent_command("zsh"), None);
    }

    #[test]
    fn codex_usage_lines_keep_expired_primary_and_active_pace_marker() {
        let now = 1_782_082_196;
        let data = "1782064741\t7\t1782073613\t44\t1782351227\n";

        let lines = render_codex_usage_lines(data, 32, now);

        assert_eq!(
            lines.len(),
            4,
            "usage output should be label/bar pairs: {lines:?}"
        );
        assert!(
            lines[0].contains("5h"),
            "expired 5h window should remain visible: {lines:?}"
        );
        assert!(
            lines[2].contains("7d"),
            "7d window should remain visible: {lines:?}"
        );
        assert!(
            lines[0].contains('+'),
            "5h label should include pace text: {lines:?}"
        );
        assert!(
            lines[0].contains('↺'),
            "5h label should include reset text: {lines:?}"
        );
        assert!(
            lines[1].contains('│'),
            "5h usage bar should include a pace marker: {lines:?}"
        );
        assert!(
            lines[3].contains('│'),
            "active usage bar should include a pace marker: {lines:?}"
        );
    }
}
