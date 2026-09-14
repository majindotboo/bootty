#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgba {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Rgba {
    #[must_use]
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: u8::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromePalette {
    pub mantle: Rgba,
    pub base: Rgba,
    pub tab_bar: Rgba,
    pub pane: Rgba,
    pub surface: Rgba,
    pub hover: Rgba,
    pub border: Rgba,
    pub border_variant: Rgba,
    pub text: Rgba,
    pub subtext: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceKey(pub i64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTarget {
    pub scope: SpaceKey,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeChromeAction {
    TogglePanel(bootty_config::config::PanelKind),
    ActivateWindow {
        session_id: String,
        window_id: String,
    },
    ToggleKeepAwake,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusProgress {
    pub value: Option<u8>,
    pub color: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarDiffSummary {
    pub added: u64,
    pub removed: u64,
    pub added_color: Rgba,
    pub removed_color: Rgba,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageMeterSnapshot {
    pub meter: crate::usage::QuotaMeter,
    pub provider: crate::usage::UsageProvider,
    pub label: String,
    pub fill: Rgba,
    pub marker: Rgba,
    pub pace: Rgba,
    pub track: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarPosition {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Configured chrome visibility and window presentation flags vary independently"
)]
pub struct ChromeLayout {
    pub left_dock_toggle: bool,
    pub right_dock_toggle: bool,
    pub panel_tab_style: bootty_config::config::PanelTabStyle,
    pub panel_tabs: bootty_config::config::PanelTabs,
    pub dock_tabs: bootty_config::config::TabConfig,
    pub terminal_tabs: bootty_config::config::TabConfig,
    pub width: f32,
    pub height: f32,
    pub sidebar_position: SidebarPosition,
    pub sidebar_width: f32,
    pub gap: f32,
    pub top_inset: f32,
    pub titlebar_height: f32,
    pub status_height: f32,
    pub sidebar_visible: bool,
    pub titlebar_visible: bool,
    pub fullscreen: bool,
}

impl ChromeLayout {
    /// Clamp the configured sidebar to Zed's persisted range while keeping a usable center area in
    /// windows too narrow to satisfy both normal minimums.
    #[must_use]
    pub fn effective_sidebar_width(&self) -> f32 {
        const MIN_SIDEBAR_WIDTH: f32 = 200.0;
        const MAX_SIDEBAR_WIDTH: f32 = 800.0;
        const COMPACT_SIDEBAR_WIDTH: f32 = 120.0;
        const MIN_CENTER_WIDTH: f32 = 200.0;

        let window_limit = (self.width - self.gap).max(0.0);
        let available = (self.width - self.gap - MIN_CENTER_WIDTH)
            .max(COMPACT_SIDEBAR_WIDTH)
            .min(window_limit);
        let minimum = MIN_SIDEBAR_WIDTH.min(available);
        self.sidebar_width
            .clamp(minimum, MAX_SIDEBAR_WIDTH.min(available).max(minimum))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitlebarSnapshot {
    pub title: String,
    pub icon: Option<String>,
    pub session_count: usize,
    pub reserve_window_controls: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChromeSnapshot {
    pub palette: ChromePalette,
    pub layout: ChromeLayout,
    pub titlebar: TitlebarSnapshot,
    pub sidebar: Option<SidebarSnapshot>,
    pub spaces: Vec<SpaceSnapshot>,
    pub space_transition: Option<SpaceTransition>,
    pub top_status: Option<StatusBarSnapshot>,
    pub bottom_status: Option<StatusBarSnapshot>,
    pub window_focused: bool,
}

/// Delays native window movement until pointer motion and lets interactive titlebar children
/// cancel the gesture before that motion occurs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowDragGesture {
    armed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TabDragSource {
    source: String,
    target: TabInsertionTarget,
}

/// Owns tab pointer capture independently of GPUI's titlebar and typed drag recognizers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabDragGesture {
    source: Option<TabDragSource>,
    pointer: Option<(f32, f32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabInsertionTarget {
    Before(String),
    End,
}

#[must_use]
pub fn tab_insertion_target(
    anchors: &[String],
    source: &str,
    target_index: usize,
    right_half: bool,
) -> Option<TabInsertionTarget> {
    let source_index = anchors.iter().position(|anchor| anchor == source)?;
    let boundary = (target_index.saturating_add(usize::from(right_half))).min(anchors.len());
    if boundary == source_index || boundary == source_index.saturating_add(1) {
        return Some(TabInsertionTarget::Before(source.to_owned()));
    }
    Some(
        anchors
            .get(boundary)
            .cloned()
            .map_or(TabInsertionTarget::End, TabInsertionTarget::Before),
    )
}

impl TabDragGesture {
    pub fn begin(&mut self, source: &str) {
        self.source = Some(TabDragSource {
            source: source.to_owned(),
            target: TabInsertionTarget::Before(source.to_owned()),
        });
        self.pointer = None;
    }

    pub fn hover_before(&mut self, before: Option<&str>) {
        if let Some(source) = self.source.as_mut() {
            source.target = before.map_or(TabInsertionTarget::End, |before| {
                TabInsertionTarget::Before(before.to_owned())
            });
        }
    }

    pub fn cancel(&mut self) {
        self.source = None;
        self.pointer = None;
    }

    #[must_use]
    pub fn source(&self) -> Option<&str> {
        self.source.as_ref().map(|source| source.source.as_str())
    }

    #[must_use]
    pub fn insertion_target(&self) -> Option<&TabInsertionTarget> {
        self.source.as_ref().map(|source| &source.target)
    }

    pub const fn move_pointer(&mut self, x: f32, y: f32) {
        if self.source.is_some() {
            self.pointer = Some((x, y));
        }
    }

    #[must_use]
    pub const fn pointer(&self) -> Option<(f32, f32)> {
        self.pointer
    }

    #[must_use]
    pub fn release(&mut self) -> Option<StatusIntent> {
        let source = self.source.take()?;
        self.pointer = None;
        if matches!(&source.target, TabInsertionTarget::Before(before) if before == &source.source)
        {
            return None;
        }
        Some(StatusIntent::Reorder {
            source: source.source,
            before: match source.target {
                TabInsertionTarget::Before(before) => Some(before),
                TabInsertionTarget::End => None,
            },
        })
    }

    #[must_use]
    pub fn release_before(&mut self, before: Option<&str>) -> Option<StatusIntent> {
        self.hover_before(before);
        self.release()
    }
}

impl WindowDragGesture {
    pub const fn arm(&mut self) {
        self.armed = true;
    }

    pub const fn cancel(&mut self) {
        self.armed = false;
    }

    #[must_use]
    pub fn take_on_motion(&mut self) -> bool {
        std::mem::take(&mut self.armed)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarSnapshot {
    pub rows: Vec<SidebarRow>,
    pub footer: Vec<SidebarFooterItem>,
    pub title_visible: bool,
    pub focused: bool,
    pub hovered_session: Option<SessionTarget>,
    pub dim_when_unfocused: f32,
    pub tint: Rgba,
    pub foreground: Rgba,
    pub hover: Rgba,
    pub current: Rgba,
    pub border: Rgba,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct SidebarRow {
    pub key: String,
    pub text: String,
    pub trailing: Option<String>,
    pub trailing_color: Option<Rgba>,
    /// Animate the trailing text with a shimmer while the row reports live work.
    pub trailing_shimmer: bool,
    pub number: Option<usize>,
    pub indent: u16,
    pub tree: Option<String>,
    pub icon: Option<String>,
    pub diff: Option<SidebarDiffSummary>,
    pub color: Rgba,
    pub dim_color: Rgba,
    pub kind: SidebarRowKind,
    pub active: bool,
    pub current: bool,
    pub selectable: bool,
    pub target: Option<SessionTarget>,
    pub reorder_anchor: Option<String>,
    pub context: Option<SessionContextSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarRowKind {
    Group,
    Session,
    Detail,
    Progress {
        value: Option<u8>,
        label: Option<String>,
    },
    Ports(Vec<u16>),
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarFooterItem {
    pub key: String,
    pub text: String,
    pub icon: Option<String>,
    pub meter: Option<UsageMeterSnapshot>,
    pub color: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each action has an independent availability condition"
)]
pub struct SessionContextSnapshot {
    pub can_activate: bool,
    pub can_move_up: bool,
    pub can_move_down: bool,
    pub can_navigate: bool,
    pub can_return_to_last: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionContextAction {
    Activate,
    NewSession,
    SwitchSession,
    PreviousSession,
    NextSession,
    LastSession,
    Rename,
    MoveUp,
    MoveDown,
    Ditch,
    MoveToSpace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceSnapshot {
    pub key: SpaceKey,
    pub name: String,
    pub icon: String,
    pub color: Rgba,
    pub active: bool,
    pub error: Option<String>,
    pub accepts_moves: bool,
    pub can_close: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpaceTransition {
    pub from: SpaceKey,
    pub to: SpaceKey,
    pub progress: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusBarSnapshot {
    pub key: String,
    pub rows: usize,
    pub background: Rgba,
    pub segments: Vec<StatusSegmentSnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusSegmentSnapshot {
    pub align: StatusAlignment,
    pub source_slot: usize,
    pub surface: String,
    pub items: Vec<StatusItemSnapshot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusAlignment {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusItemSnapshot {
    pub key: String,
    pub text: String,
    pub icon: Option<String>,
    pub gauge: Option<f32>,
    pub pad_left: f32,
    pub pad_right: f32,
    pub progress: Option<StatusProgress>,
    pub foreground: Option<Rgba>,
    pub background: Option<Rgba>,
    pub active: bool,
    pub action: Option<NativeChromeAction>,
    pub reorder_anchor: Option<String>,
    pub tab_context: Option<TabContextSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each action has an independent availability condition"
)]
pub struct TabContextSnapshot {
    pub session_id: String,
    pub window_id: String,
    pub can_activate: bool,
    pub can_move_left: bool,
    pub can_move_right: bool,
    pub can_navigate: bool,
    pub can_close_pane: bool,
    pub pane_actions: Vec<(String, bootty_control::CommandInvocation)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabContextAction {
    Activate,
    NewTab,
    PreviousTab,
    NextTab,
    LastTab,
    Rename,
    MoveLeft,
    MoveRight,
    ClosePane,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChromeIntent {
    Command(bootty_control::CommandInvocation),
    StartWindowDrag,
    ActivateSpace(SpaceKey),
    CreateSpace,
    EditSpace(SpaceKey),
    ReconnectSpace(SpaceKey),
    CloseSpace(SpaceKey),
    MoveSessionsToSpace {
        sessions: Vec<SessionTarget>,
        to: SpaceKey,
    },
    ActivateSession(SessionTarget),
    OpenGitChanges(SessionTarget),
    AdoptSession(SessionTarget),
    SessionContext {
        target: SessionTarget,
        action: SessionContextAction,
    },
    ReorderSession {
        source: String,
        before: Option<String>,
    },
    SidebarResizeLive(f32),
    SidebarResizePersist,
    SidebarResizeReset,
    Status(StatusIntent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusIntent {
    Action(NativeChromeAction),
    Context {
        session_id: String,
        window_id: String,
        action: TabContextAction,
    },
    Reorder {
        source: String,
        before: Option<String>,
    },
}
