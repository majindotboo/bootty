//! The app-owned command vocabulary.
//!
//! Each command is declared once with its binding action, presentation,
//! palette policy, mutation class, target, and argument shape. Declaration
//! order is the display order in the palette and keybind editor.

use bootty_control::{
    ArgumentSchema, CommandDescriptor, CompactSchema, MutationClass, ResourceKind, ValueType,
};

#[derive(Clone, Copy)]
enum PaletteEntry {
    Shown,
    Override(&'static str),
    Hidden,
}

/// Stable display sections for the command palette. Keep this metadata with the command
/// vocabulary so presentation ordering does not depend on action-name spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandCategory {
    Sessions,
    Layout,
    Appearance,
    Terminal,
    Application,
}

impl CommandCategory {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Layout => "Layout",
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::Application => "Application",
        }
    }

    pub(crate) const fn rank(self) -> usize {
        match self {
            Self::Sessions => 0,
            Self::Layout => 1,
            Self::Appearance => 2,
            Self::Terminal => 3,
            Self::Application => 4,
        }
    }
}

#[derive(Clone, Copy)]
enum ArgumentKind {
    None,
    Index,
    PositionDelta,
    ScrollDelta,
    FontSize,
    PaneDirection,
    Appearance,
    SearchDirection,
    ClipboardFormat,
    Value,
    DockGroup,
}

#[derive(Clone, Copy)]
enum TargetKind {
    None,
    Instance,
    ApplicationWindow,
    Binding,
    Session,
    MuxWindow,
    Pane,
    Terminal,
}

#[derive(Clone, Copy)]
struct CoreCommandSpec {
    title: &'static str,
    description: &'static str,
    descriptor_title: &'static str,
    descriptor_description: &'static str,
    action: &'static str,
    icon: &'static str,
    category: CommandCategory,
    palette: PaletteEntry,
    mutation: MutationClass,
    target: TargetKind,
    argument: ArgumentKind,
}

macro_rules! palette_entry {
    (shown) => {
        PaletteEntry::Shown
    };
    (hidden) => {
        PaletteEntry::Hidden
    };
    ($action:literal) => {
        PaletteEntry::Override($action)
    };
}

macro_rules! descriptor_title {
    ($title:literal) => {
        $title
    };
    ($title:literal, $override:literal) => {
        $override
    };
}

macro_rules! descriptor_description {
    ($description:literal) => {
        $description
    };
    ($description:literal, $override:literal) => {
        $override
    };
}

macro_rules! command_catalog {
    ($(
        $variant:ident:
            $title:literal,
            $description:literal,
            $action:literal,
            $icon:literal,
            $category:ident,
            $palette:tt,
            $mutation:ident,
            $target:ident,
            $argument:ident
            $(=> $descriptor_title:literal, $descriptor_description:literal)?;
    )+) => {
        #[repr(u8)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Command {
            $($variant,)+
        }

        const COMMANDS: &[Command] = &[$(Command::$variant,)+];
        impl Command {
            const fn spec(self) -> &'static CoreCommandSpec {
                match self { $(Self::$variant => &CoreCommandSpec {
                title: $title,
                description: $description,
                descriptor_title: descriptor_title!($title $(, $descriptor_title)?),
                descriptor_description: descriptor_description!(
                    $description $(, $descriptor_description)?
                ),
                action: $action,
                icon: $icon,
                category: CommandCategory::$category,
                palette: palette_entry!($palette),
                mutation: MutationClass::$mutation,
                target: TargetKind::$target,
                argument: ArgumentKind::$argument,
                },)+ }
            }
        }
    };
}

// title, description, binding action, icon, category, palette, mutation, target, argument
command_catalog! {
    OpenSetting:
        "Open setting", "Open a setting by its configuration path",
        "open_setting", "settings", Application, hidden, Write, ApplicationWindow, Value;
    ToggleSessionsPanel:
        "Toggle Sessions panel", "Show or hide the sessions panel",
        "toggle_sessions_panel", "panel-left", Layout, shown, Write, ApplicationWindow, DockGroup;
    ToggleFilesPanel:
        "Toggle Files panel", "Show or hide the files panel",
        "toggle_files_panel", "panel-left", Layout, shown, Write, ApplicationWindow, DockGroup;
    ToggleChangesPanel:
        "Toggle Changes panel", "Show or hide the changes panel",
        "toggle_changes_panel", "panel-left", Layout, shown, Write, ApplicationWindow, DockGroup;
    ToggleDiffPanel:
        "Toggle Diff panel", "Show or hide the diff panel",
        "toggle_diff_panel", "panel-left", Layout, shown, Write, ApplicationWindow, DockGroup;
    ToggleAgentsPanel:
        "Toggle Agents panel", "Show or hide the agents panel",
        "toggle_agents_panel", "panel-left", Layout, shown, Write, ApplicationWindow, DockGroup;
    ToggleLeftDock:
        "Toggle left dock", "Show or hide the left dock",
        "toggle_left_dock", "panel-left", Layout, shown, Write, ApplicationWindow, None;
    ToggleRightDock:
        "Toggle right dock", "Show or hide the right dock",
        "toggle_right_dock", "panel-right", Layout, shown, Write, ApplicationWindow, None;
    ShowSidebar:
        "Show Sessions", "Open the Sessions panel",
        "show_sidebar", "panel-left", Layout, shown, Write, ApplicationWindow, DockGroup;
    ShowCodexBar:
        "Show agent usage", "Open usage and quota meters in Agents",
        "show_codexbar", "chart-no-axes-column", Layout, shown, Write, ApplicationWindow, DockGroup;
    ShowSpaces:
        "Show Spaces", "Open the Space switcher in Sessions",
        "show_spaces", "layout-grid", Layout, shown, Write, ApplicationWindow, DockGroup;
    ToggleHiddenTabs:
        "Toggle always hide tabs", "Toggle command-only panel switching for the focused group",
        "toggle_hidden_tabs", "panel-top", Layout, shown, Write, ApplicationWindow, DockGroup;
    ToggleTabBar:
        "Toggle always show tabs", "Toggle single-panel tabs for the focused tab group",
        "toggle_tab_bar", "panel-top", Layout, shown, Write, ApplicationWindow, DockGroup;
    EditTheme:
        "Edit Theme", "Import, duplicate, preview and save terminal colors",
        "edit_theme", "paintbrush", Application, shown, Write, ApplicationWindow, None;
    ExportTerminal:
        "Export Terminal", "Save retained terminal content as plain text, ANSI or HTML",
        "export_terminal", "download", Terminal, shown, Write, ApplicationWindow, None;
    ShowAgents:
        "Show Agents", "Open agent attention, session and navigation controls",
        "show_agents", "bot", Application, shown, Write, ApplicationWindow, DockGroup;
    ShowFiles:
        "Browse Files", "Open the Files panel for the terminal directory",
        "show_files", "folder", Application, shown, Write, ApplicationWindow, DockGroup;
    ShowChanges:
        "Show Git Changes", "Open the Changes and Diff tool panels",
        "show_changes", "git-branch", Application, shown, Write, ApplicationWindow, DockGroup;
    ShowDiff:
        "Show Git Diff", "Open the Git Diff panel",
        "show_diff", "git-compare", Application, shown, Write, ApplicationWindow, DockGroup;
    NewSession:
        "New Session",
        "Pick a directory or worktree and start a session",
        "new_mux_session", "square-plus", Sessions, shown, Write, Binding, None;
    SwitchSession:
        "Switch Session", "Fuzzy-find and jump to an open session",
        "session_picker", "terminal", Sessions, shown, Write, Binding, None;
    RenameSession:
        "Rename Session", "Rename the current session",
        "rename_session", "pencil", Sessions, shown, Write, Session, None;
    DitchSession:
        "Ditch Session", "Close the session and optionally remove its worktree",
        "ditch_session", "trash-2", Sessions, shown, Destructive, Session, None;
    MoveSessionToSpace:
        "Move Session to Space", "Hand the current session to another space, or to no space",
        "move_session_to_space", "shapes", Sessions, shown, Write, Session, None;
    CreateSpace:
        "New Space", "Create and activate a new space",
        "create_space", "plus", Application, shown, Write, ApplicationWindow, None;
    EditSpace:
        "Edit Space", "Edit the active space",
        "edit_space", "pencil", Application, shown, Write, Binding, None;
    CloseSpace:
        "Close Space", "Close the active space",
        "close_space", "x", Application, shown, Destructive, Binding, None;
    NextSpace:
        "Next Space", "Activate the next space",
        "next_space", "chevron-right", Application, shown, Write, ApplicationWindow, None;
    PreviousSpace:
        "Previous Space", "Activate the previous space",
        "previous_space", "chevron-left", Application, shown, Write, ApplicationWindow, None;
    NextSession:
        "Next Session", "Activate the next session",
        "next_session", "chevron-down", Sessions, shown, Write, Binding, None;
    PreviousSession:
        "Previous Session", "Activate the previous session",
        "previous_session", "chevron-up", Sessions, shown, Write, Binding, None;
    LastSession:
        "Last Session", "Toggle back to the most recent session",
        "last_session", "history", Sessions, shown, Write, Binding, None;
    NewTab:
        "New Tab", "Open a new tab in the current session",
        "new_tab", "plus", Layout, shown, Write, Session, None;
    NextTab:
        "Next Tab", "Activate the next tab",
        "next_tab", "chevron-right", Layout, shown, Write, Session, None;
    PreviousTab:
        "Previous Tab", "Activate the previous tab",
        "previous_tab", "chevron-left", Layout, shown, Write, Session, None;
    LastTab:
        "Last Tab", "Toggle back to the most recently used tab",
        "last_tab", "arrow-right-to-line", Layout, shown, Write, Session, None;
    MoveTabLeft:
        "Move Tab Left", "Reorder the current tab one position left",
        "move_tab:-1", "move-horizontal", Layout, shown, Write, MuxWindow, PositionDelta =>
        "Move Tab", "Move the selected tab by the signed position delta.";
    MoveTabRight:
        "Move Tab Right", "Reorder the current tab one position right",
        "move_tab:1", "move-horizontal", Layout, shown, Write, MuxWindow, PositionDelta =>
        "Move Tab", "Move the selected tab by the signed position delta.";
    RenameTab:
        "Rename Tab", "Rename the current tab",
        "rename_tab", "pencil", Layout, shown, Write, MuxWindow, None;
    SplitRight:
        "Split Right", "Split the current pane horizontally",
        "split_right", "columns-2", Layout, shown, Write, Pane, None;
    SplitDown:
        "Split Down", "Split the current pane vertically",
        "split_down", "rows-2", Layout, shown, Write, Pane, None;
    NextPane:
        "Next Pane", "Move focus to the next pane",
        "next_pane", "layout-grid", Layout, shown, Write, MuxWindow, None;
    PreviousPane:
        "Previous Pane", "Move focus to the previous pane",
        "previous_pane", "layout-grid", Layout, shown, Write, MuxWindow, None;
    TogglePaneZoom:
        "Toggle Pane Zoom", "Zoom the focused pane to fill the window, or restore it",
        "toggle_pane_zoom", "maximize-2", Layout, shown, Write, Pane, None;
    KillPane:
        "Kill Pane", "Close the focused pane",
        "kill_pane", "x", Layout, shown, Destructive, Pane, None;
    ClosePane:
        "Close Pane", "Close the active pane, cascading to the tab",
        "close_surface", "square-x", Layout, shown, Destructive, Pane, None;
    NewWindow:
        "New Window", "Open a new top-level window",
        "new_window", "app-window", Application, shown, Write, Binding, None;
    ToggleSidebar:
        "Toggle Sessions Panel", "Show or hide the Sessions panel",
        "toggle_sidebar_visibility", "panel-left", Application, hidden, Write, ApplicationWindow, None;
    FocusTerminal:
        "Focus Terminal", "Move keyboard focus to the active terminal",
        "focus_terminal", "terminal", Application, shown, Write, ApplicationWindow, None;
    FocusSidebar:
        "Focus Sessions Panel", "Move keyboard focus to the Sessions panel",
        "toggle_sidebar_focus", "panel-left-open", Application, shown, Write, ApplicationWindow, None;
    ToggleFullscreen:
        "Toggle Fullscreen", "Enter or leave fullscreen",
        "toggle_fullscreen", "maximize", Application, shown, Write, ApplicationWindow, None;
    ScrollToTop:
        "Scroll to Top", "Jump to the top of the scrollback",
        "scroll_to_top", "arrow-up-to-line", Terminal, shown, Write, Terminal, None;
    ScrollToBottom:
        "Scroll to Bottom", "Jump to the latest output",
        "scroll_to_bottom", "arrow-down-to-line", Terminal, shown, Write, Terminal, None;
    CopyMode:
        "Copy Mode", "Enter tmux-style scrollback navigation and text selection",
        "copy_mode", "copy", Terminal, shown, Write, Terminal, None;
    IncreaseFontSize:
        "Increase Font Size", "Make the terminal text larger",
        "increase_font_size", "zoom-in", Terminal, "increase_font_size:1", Write,
        ApplicationWindow, FontSize;
    DecreaseFontSize:
        "Decrease Font Size", "Make the terminal text smaller",
        "decrease_font_size", "zoom-out", Terminal, "decrease_font_size:1", Write,
        ApplicationWindow, FontSize;
    ResetFontSize:
        "Reset Font Size", "Restore the configured font size",
        "reset_font_size", "type", Terminal, shown, Write, ApplicationWindow, None;
    Find:
        "Find in Terminal", "Search the terminal scrollback",
        "start_search", "search", Terminal, shown, Write, Terminal, None;
    KeyboardShortcuts:
        "Keyboard Shortcuts", "Browse the active keybindings",
        "show_keybinds", "keyboard", Application, shown, Read, ApplicationWindow, None;
    UseSystemAppearance:
        "Use System Appearance", "Follow the operating system light/dark appearance",
        "change_appearance:system", "sun-moon", Appearance, shown, Write, ApplicationWindow, Appearance =>
        "Change Appearance", "Use the system, light, or dark appearance.";
    UseLightAppearance:
        "Use Light Appearance", "Switch Bootty to the configured light appearance branch",
        "change_appearance:light", "sun-moon", Appearance, shown, Write, ApplicationWindow, Appearance =>
        "Change Appearance", "Use the system, light, or dark appearance.";
    UseDarkAppearance:
        "Use Dark Appearance", "Switch Bootty to the configured dark appearance branch",
        "change_appearance:dark", "sun-moon", Appearance, shown, Write, ApplicationWindow, Appearance =>
        "Change Appearance", "Use the system, light, or dark appearance.";
    SwitchTheme:
        "Switch Theme", "Pick a theme for the active light or dark appearance branch",
        "switch_theme", "palette", Appearance, shown, Write, ApplicationWindow, None;
    OpenSettings:
        "Settings", "Open the settings surface",
        "open_settings", "settings", Application, shown, Write, ApplicationWindow, None;
    ReloadConfig:
        "Reload Config", "Re-read the config file from disk",
        "reload_config", "refresh-cw", Application, shown, Write, ApplicationWindow, None;
    Quit:
        "Quit Bootty", "Close the application",
        "quit", "power", Application, shown, Destructive, Instance, None;

    // Editor-only below: parameterized or low-level actions the palette omits.
    CommandPalette:
        "Command Palette", "Search and run any command",
        "command_palette", "search", Application, hidden, Write, ApplicationWindow, None;
    CloseWindow:
        "Close Window", "Close the current window",
        "close_window", "x", Application, hidden, Destructive, ApplicationWindow, None;
    Ignore:
        "Ignore", "Do nothing — mask a default binding so the keys pass through",
        "ignore", "ban", Application, hidden, Write, None, None;
    SelectTab:
        "Select Tab", "Jump to tab N (value 1–9)",
        "select_tab", "hash", Layout, hidden, Write, Session, Index;
    MoveTab:
        "Move Tab", "Reorder the current tab by N",
        "move_tab", "move-horizontal", Layout, hidden, Write, MuxWindow, PositionDelta =>
        "Move Tab", "Move the selected tab by the signed position delta.";
    SelectPane:
        "Select Pane", "Focus the pane in a direction (left/right/up/down)",
        "select_pane", "layout", Layout, hidden, Write, MuxWindow, PaneDirection;
    SelectSession:
        "Select Session", "Jump to session N (value 1–9)",
        "select_session", "hash", Sessions, hidden, Write, Binding, Index;
    SelectSpace:
        "Select Space", "Jump to space N (value 1–9)",
        "select_space", "hash", Application, hidden, Write, ApplicationWindow, Index;
    MoveSession:
        "Move Session", "Reorder the current session by N",
        "move_session", "move-vertical", Sessions, hidden, Write, Session, PositionDelta;
    ScrollPageUp:
        "Scroll Page Up", "Scroll up one page",
        "scroll_page_up", "chevrons-up", Terminal, hidden, Write, Terminal, None;
    ScrollPageDown:
        "Scroll Page Down", "Scroll down one page",
        "scroll_page_down", "chevrons-down", Terminal, hidden, Write, Terminal, None;
    ScrollPageLines:
        "Scroll Lines", "Scroll by N lines (negative scrolls up)",
        "scroll_page_lines", "list", Terminal, hidden, Write, Terminal, ScrollDelta;
    SetFontSize:
        "Set Font Size", "Set the font size to N points",
        "set_font_size", "type", Terminal, hidden, Write, ApplicationWindow, FontSize;
    Search:
        "Search Terminal", "Search the terminal scrollback for text",
        "search", "search", Terminal, hidden, Write, Terminal, Value;
    SearchSelection:
        "Search Selection", "Search the terminal scrollback for the selected text",
        "search_selection", "search", Terminal, hidden, Write, Terminal, None;
    NavigateSearchNext:
        "Next Search Match", "Move to the next terminal search match",
        "navigate_search:next", "search", Terminal, hidden, Write, Terminal, SearchDirection =>
        "Navigate Search", "Move to the next or previous terminal search match.";
    NavigateSearchPrevious:
        "Previous Search Match", "Move to the previous terminal search match",
        "navigate_search:previous", "search", Terminal, hidden, Write, Terminal, SearchDirection =>
        "Navigate Search", "Move to the next or previous terminal search match.";
    ToggleSearchRegex:
        "Toggle Regex Search", "Use regular expressions in terminal search",
        "toggle_search_regex", "search", Terminal, shown, Write, Terminal, None;
    ToggleSearchCaseSensitive:
        "Toggle Case Sensitive Search", "Match letter case in terminal search",
        "toggle_search_case_sensitive", "search", Terminal, shown, Write, Terminal, None;
    EndSearch:
        "Close Terminal Search", "Close the terminal search surface",
        "end_search", "search", Terminal, hidden, Write, Terminal, None;
    Copy:
        "Copy", "Copy the selection to the clipboard",
        "copy_to_clipboard", "copy", Terminal, hidden, Write, Terminal, ClipboardFormat;
    Paste:
        "Paste", "Paste from the clipboard",
        "paste_from_clipboard", "clipboard", Terminal, hidden, Write, Terminal, None;
    SendCsi:
        "Send CSI", "Write a CSI escape sequence to the terminal",
        "csi", "terminal", Terminal, hidden, Write, Terminal, Value;
    SendEsc:
        "Send ESC", "Write an ESC sequence to the terminal",
        "esc", "terminal", Terminal, hidden, Write, Terminal, Value;
    SendText:
        "Send Text", "Write literal text to the terminal",
        "text", "terminal", Terminal, hidden, Write, Terminal, Value;
}

impl Command {
    /// Iterate every command in display order.
    pub fn all() -> impl Iterator<Item = Self> {
        COMMANDS.iter().copied()
    }

    /// Human title (e.g. "Rename Session").
    #[must_use]
    pub const fn title(self) -> &'static str {
        self.spec().title
    }

    /// One-line description for the palette/editor.
    #[must_use]
    pub const fn description(self) -> &'static str {
        self.spec().description
    }

    /// The binding action string used by the keybind editor and dispatch.
    #[must_use]
    pub const fn action(self) -> &'static str {
        self.spec().action
    }

    /// The canonical command id without any baked-in argument.
    pub(crate) fn id(self) -> &'static str {
        self.action()
            .split_once(':')
            .map_or_else(|| self.action(), |(id, _)| id)
    }

    /// Lucide icon slug shown in the palette.
    #[must_use]
    pub const fn icon(self) -> &'static str {
        self.spec().icon
    }

    #[must_use]
    pub const fn category(self) -> CommandCategory {
        self.spec().category
    }

    /// The exact action string the command palette dispatches.
    #[must_use]
    pub const fn palette_action(self) -> Option<&'static str> {
        match self.spec().palette {
            PaletteEntry::Shown => Some(self.action()),
            PaletteEntry::Override(action) => Some(action),
            PaletteEntry::Hidden => None,
        }
    }

    /// The command whose action string is `name`.
    #[must_use]
    pub fn from_action(name: &str) -> Option<Self> {
        Self::all().find(|command| command.action() == name)
    }

    pub(crate) fn descriptor(self) -> CommandDescriptor {
        let spec = self.spec();
        CommandDescriptor {
            id: self.id().to_owned(),
            title: spec.descriptor_title.to_owned(),
            description: spec.descriptor_description.to_owned(),
            mutation: spec.mutation,
            arguments: spec.argument.schema(),
            target: spec.target.resource_kind(),
            palette: self.palette_action().is_some(),
        }
    }
}

impl ArgumentKind {
    fn schema(self) -> CompactSchema {
        let argument = match self {
            Self::None => None,
            Self::DockGroup => Some(ArgumentSchema {
                required: false,
                ..bounded_integer("group", 1, i64::MAX)
            }),
            Self::Index => Some(bounded_integer("index", 1, i64::from(u32::MAX))),
            Self::PositionDelta => Some(bounded_integer(
                "delta",
                i64::from(i32::MIN),
                i64::from(i32::MAX),
            )),
            Self::ScrollDelta => Some(bounded_integer(
                "delta",
                i64::from(i16::MIN),
                i64::from(i16::MAX),
            )),
            Self::FontSize => Some(argument("size", ValueType::Number)),
            Self::PaneDirection => {
                Some(choice("direction", true, &["left", "right", "up", "down"]))
            }
            Self::Appearance => Some(choice("appearance", true, &["system", "light", "dark"])),
            Self::SearchDirection => Some(choice("direction", true, &["next", "previous"])),
            Self::ClipboardFormat => {
                Some(choice("format", false, &["plain", "vt", "html", "mixed"]))
            }
            Self::Value => Some(argument("value", ValueType::String)),
        };
        CompactSchema {
            arguments: argument.into_iter().collect(),
        }
    }
}

impl TargetKind {
    const fn resource_kind(self) -> Option<ResourceKind> {
        match self {
            Self::None => None,
            Self::Instance => Some(ResourceKind::Instance),
            Self::ApplicationWindow => Some(ResourceKind::ApplicationWindow),
            Self::Binding => Some(ResourceKind::Binding),
            Self::Session => Some(ResourceKind::Session),
            Self::MuxWindow => Some(ResourceKind::MuxWindow),
            Self::Pane => Some(ResourceKind::Pane),
            Self::Terminal => Some(ResourceKind::Terminal),
        }
    }
}

fn bounded_integer(name: &str, minimum: i64, maximum: i64) -> ArgumentSchema {
    ArgumentSchema {
        minimum: Some(minimum),
        maximum: Some(maximum),
        ..argument(name, ValueType::Integer)
    }
}

fn choice(name: &str, required: bool, choices: &[&str]) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type: ValueType::String,
        required,
        choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
        minimum: None,
        maximum: None,
    }
}

fn argument(name: &str, value_type: ValueType) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type,
        required: true,
        choices: Vec::new(),
        minimum: None,
        maximum: None,
    }
}
