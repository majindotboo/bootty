use std::str::FromStr;

use crate::{
    gpui::{InputEvent, Key, Modifiers},
    keymap::{
        AppearanceChoice, BindingAction, BindingKey, BindingTrigger, CopyToClipboard,
        NavigateSearch, PaneDirection, parse_action,
    },
};
use anyhow::Result;
use bootty_config::{
    KeymapBindingSnapshot, KeymapBindingSource, KeymapMatch, KeymapProgram,
    config::{InputConfig, split_keybind_entry},
    keymap_file::{KeymapAction, KeymapBindingKind, KeymapContext},
};
use bootty_control::{Caller, CommandInvocation};
use bootty_mux::command::MuxDirection;
use bootty_terminal::terminal_input::ModifierSideState;
use bootty_terminal::terminal_input_model::{KeyInput, KeyMods, TerminalKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppAction {
    Dock(crate::commands::DockAction),
    ReloadConfig,
    Ignore,
    NewWindow,
    NewMuxSession,
    SessionPicker,
    CommandPalette,
    Close,
    Quit,
    ToggleFullscreen,
    ToggleSidebarFocus,
    FocusTerminal,
    ToggleSidebarVisibility,
    OpenSettings,
    EditTheme,
    ExportTerminal,
    ChangeAppearance(bootty_config::config::AppearanceMode),
    SwitchTheme,
    RenameSession,
    MoveSessionToSpace,
    RenameTab,
    DitchSession,
    CreateSpace,
    CloseSpace,
    EditSpace,
    NextSpace,
    PreviousSpace,
    SelectSpace(u32),
    ShowKeybinds,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalFindAction {
    ToggleRegex,
    ToggleCaseSensitive,
    Prompt,
    Search(String),
    SearchSelection,
    Next,
    Previous,
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub enum KeybindAction {
    OpenSetting(String),
    App(AppAction),
    Mux(MuxKeyAction),
    Scroll(TerminalScrollAction),
    Write(Vec<u8>),
    Font(FontSizeAction),
    Find(TerminalFindAction),
    CopyToClipboard(CopyToClipboard),
    CopyMode,
    PasteFromClipboard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxKeyAction {
    NewTab,
    NextTab,
    PreviousTab,
    LastTab,
    SelectTab(u32),
    MoveTab(i32),
    SplitPane(bootty_mux::pane_layout::SplitDirection),
    SelectPane(MuxDirection),
    NextPane,
    PreviousPane,
    KillPane,
    ClosePane,
    TogglePaneZoom,
    NextSession,
    PreviousSession,
    LastSession,
    SelectSession(u32),
    MoveSession(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalScrollAction {
    Top,
    Bottom,
    PageUp,
    PageDown,
    Lines(i16),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontSizeAction {
    Increase(f32),
    Decrease(f32),
    Reset,
    Set(f32),
}

#[derive(Clone)]
pub struct AppKeyBindings {
    program: KeymapProgram<String, ()>,
}

impl Default for AppKeyBindings {
    fn default() -> Self {
        Self {
            program: KeymapProgram::empty(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarAction {
    Ignore,
    PreviousSession,
    NextSession,
    ActivateSession,
    FocusTerminal,
}

impl SidebarAction {
    pub const ALL: [Self; 5] = [
        Self::Ignore,
        Self::PreviousSession,
        Self::NextSession,
        Self::ActivateSession,
        Self::FocusTerminal,
    ];

    #[must_use]
    pub const fn command_id(self) -> &'static str {
        match self {
            Self::Ignore => "ui.sidebar.ignore",
            Self::PreviousSession => "ui.sidebar.previous_session",
            Self::NextSession => "ui.sidebar.next_session",
            Self::ActivateSession => "ui.sidebar.activate_session",
            Self::FocusTerminal => "ui.sidebar.focus_terminal",
        }
    }
}

#[derive(Clone, Debug)]
struct SidebarKeyBinding {
    trigger: BindingTrigger,
    command: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct SidebarKeyBindings {
    bindings: Vec<SidebarKeyBinding>,
}

impl AppKeyBindings {
    /// Compile the application bindings from input configuration.
    ///
    /// # Errors
    /// Rejects malformed triggers, invalid actions, and keymap compilation diagnostics.
    pub fn from_config(input: &InputConfig) -> Result<Self> {
        Self::from_keybinds(&input.keybind)
    }

    /// Compile binding entries into commands.
    ///
    /// # Errors
    /// Rejects malformed triggers and actions unsupported by this binding context.
    pub fn from_keybinds(keybinds: &[String]) -> Result<Self> {
        let mut bindings = Vec::new();
        for entry in keybinds {
            let (trigger, action) = split_keybind_entry(entry)
                .ok_or_else(|| anyhow::anyhow!("invalid keybind {entry:?}"))?;
            let normalized = trigger.split('>').collect::<Vec<_>>().join(" ");
            let sequence = bootty_config::parse_keymap_sequence(&normalized)
                .map_err(|error| anyhow::anyhow!("invalid keybind {entry:?}: {error:?}"))?;
            let action = parse_action(action)
                .map_err(|error| anyhow::anyhow!("invalid keybind {entry:?}: {error:?}"))?;
            let action_name = action.format_entry();
            keybind_action(action)
                .map_err(|error| anyhow::anyhow!("unsupported keybind {entry:?}: {error}"))?;
            bindings.push(KeymapBindingSnapshot {
                context: KeymapContext::Global,
                keystrokes: sequence.format_entry(),
                action: KeymapAction::command(action_name),
                kind: KeymapBindingKind::Binding,
                source: KeymapBindingSource::User,
            });
        }
        let (program, diagnostics) = KeymapProgram::compile(
            bindings,
            |action| Ok(action.name().map(str::to_owned)),
            |_| Ok(()),
        );
        if let Some(diagnostic) = diagnostics.first() {
            anyhow::bail!("invalid keybind: {diagnostic}");
        }
        Ok(Self { program })
    }

    pub fn invocation_for_key_with_modifier_sides(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        modifier_sides: ModifierSideState,
    ) -> Option<CommandInvocation> {
        self.invocation_for_candidates(&binding_triggers_for_key_with_modifier_sides(
            key,
            modifiers,
            modifier_sides,
        ))
    }

    pub fn invocation_for_scroll_with_modifier_sides(
        &mut self,
        up: bool,
        modifiers: Modifiers,
        modifier_sides: ModifierSideState,
    ) -> Option<CommandInvocation> {
        self.invocation_for_candidates(&binding_triggers_for_scroll_with_modifier_sides(
            up,
            modifiers,
            modifier_sides,
        ))
    }

    pub fn invocation_for_input(&mut self, input: KeyInput) -> Option<CommandInvocation> {
        self.invocation_for_candidates(&binding_triggers_for_key_input(input))
    }

    fn invocation_for_candidates(
        &mut self,
        candidates: &[BindingTrigger],
    ) -> Option<CommandInvocation> {
        let candidates = crate::keymap_runtime::config_keymap_candidates(candidates);
        match self.program.next_candidates(&candidates, |()| true) {
            KeymapMatch::NoMatch => None,
            KeymapMatch::Pending | KeymapMatch::Consumed => {
                Some(CommandInvocation::from_action("ignore", Caller::Keybinding))
            }
            KeymapMatch::Matched { action, .. } => {
                Some(CommandInvocation::from_action(&action, Caller::Keybinding))
            }
        }
    }
}

impl SidebarKeyBindings {
    /// Compile binding entries into commands.
    ///
    /// # Errors
    /// Rejects malformed triggers and actions unsupported by this binding context.
    pub fn from_keybinds(keybinds: &[String]) -> Result<Self> {
        let mut bindings = Vec::new();
        for entry in keybinds {
            let (trigger, action) = split_keybind_entry(entry)
                .ok_or_else(|| anyhow::anyhow!("invalid sidebar keybind {entry:?}"))?;
            let action = sidebar_action(action).map_err(|error| {
                anyhow::anyhow!("unsupported sidebar keybind {entry:?}: {error}")
            })?;
            bindings.push(SidebarKeyBinding {
                trigger: BindingTrigger::from_str(trigger).map_err(|error| {
                    anyhow::anyhow!("invalid sidebar keybind {entry:?}: {error:?}")
                })?,
                command: action.command_id(),
            });
        }
        Ok(Self { bindings })
    }

    #[must_use]
    pub fn invocation_for_key(&self, key: Key, modifiers: Modifiers) -> Option<CommandInvocation> {
        let candidates = binding_triggers_for_key(key, modifiers);
        self.bindings.iter().find_map(|binding| {
            candidates
                .iter()
                .any(|candidate| candidate == &binding.trigger)
                .then(|| CommandInvocation::from_action(binding.command, Caller::Keybinding))
        })
    }
}

fn sidebar_action(input: &str) -> Result<SidebarAction> {
    SidebarAction::ALL
        .into_iter()
        .find(|action| action.command_id().strip_prefix("ui.sidebar.") == Some(input))
        .ok_or_else(|| anyhow::anyhow!("{input} has no Bootty sidebar behavior"))
}

pub fn split_app_actions_for_bindings_with_modifier_sides(
    app_key_bindings: &mut AppKeyBindings,
    events: Vec<InputEvent>,
    modifier_sides: ModifierSideState,
) -> (Vec<InputEvent>, Vec<CommandInvocation>) {
    let mut terminal_events = Vec::with_capacity(events.len());
    let mut actions = Vec::new();
    let mut suppress_next_text = false;
    for event in events {
        if suppress_next_text && matches!(event, InputEvent::ImeCommit(_)) {
            continue;
        }
        if matches!(event, InputEvent::Key { pressed: false, .. }) {
            suppress_next_text = false;
        }

        let invocation = match &event {
            InputEvent::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => app_key_bindings
                .invocation_for_key_with_modifier_sides(*key, *modifiers, modifier_sides)
                .or_else(|| builtin_app_invocation_for_key(*key, *modifiers)),
            InputEvent::MouseWheel {
                delta, modifiers, ..
            } if delta.y != 0.0 => app_key_bindings.invocation_for_scroll_with_modifier_sides(
                delta.y > 0.0,
                *modifiers,
                modifier_sides,
            ),
            _ => None,
        };
        if let Some(invocation) = invocation {
            if matches!(event, InputEvent::Key { .. }) {
                suppress_next_text = true;
            }
            actions.push(invocation);
        } else {
            terminal_events.push(event);
        }
    }
    (terminal_events, actions)
}

// Safety net for new-session even when keybinds are cleared: Cmd+N on macOS, Ctrl+Shift+N
// elsewhere (matching the platform default tables).
#[must_use]
pub fn builtin_app_invocation_for_key(key: Key, modifiers: Modifiers) -> Option<CommandInvocation> {
    builtin_new_session_invocation(
        key == Key::Letter('n'),
        key_mods_for_binding(modifiers, ModifierSideState::default()),
    )
}

#[must_use]
pub fn builtin_app_invocation_for_direct_key(input: KeyInput) -> Option<CommandInvocation> {
    builtin_new_session_invocation(input.key == TerminalKey::N, input.mods)
}

fn builtin_new_session_invocation(is_n: bool, mods: KeyMods) -> Option<CommandInvocation> {
    let platform_modifiers = if cfg!(target_os = "macos") {
        mods.command && !mods.ctrl && !mods.shift
    } else {
        mods.ctrl && mods.shift && !mods.command
    };
    (is_n && platform_modifiers && !mods.alt)
        .then(|| CommandInvocation::from_action("new_mux_session", Caller::BuiltinKeybinding))
}

/// Resolve a `snake_case` binding-action name (e.g. `"rename_session"`) to its
/// runnable [`KeybindAction`], or `None` if it is unknown or has no app behavior.
///
/// [`crate::commands::CommandRegistry`] uses this to resolve core keybinding executors.
#[must_use]
pub fn keybind_action_for_name(name: &str) -> Option<KeybindAction> {
    keybind_action(parse_action(name).ok()?).ok()
}

pub(crate) fn invocation_for_binding_action(action: BindingAction) -> Result<CommandInvocation> {
    let action_name = action.format_entry();
    keybind_action(action)?;
    Ok(CommandInvocation::from_action(
        &action_name,
        Caller::Keybinding,
    ))
}

fn keybind_action(action: BindingAction) -> Result<KeybindAction> {
    use BindingAction as Binding;
    use KeybindAction as Keybind;
    Ok(match action {
        Binding::ReloadConfig => Keybind::App(AppAction::ReloadConfig),
        Binding::Ignore => Keybind::App(AppAction::Ignore),
        Binding::NewWindow => Keybind::App(AppAction::NewWindow),
        Binding::NewMuxSession => Keybind::App(AppAction::NewMuxSession),
        Binding::SessionPicker => Keybind::App(AppAction::SessionPicker),
        Binding::CommandPalette => Keybind::App(AppAction::CommandPalette),
        Binding::CloseWindow => Keybind::App(AppAction::Close),
        Binding::Quit => Keybind::App(AppAction::Quit),
        Binding::ToggleFullscreen => Keybind::App(AppAction::ToggleFullscreen),
        Binding::FocusTerminal => Keybind::App(AppAction::FocusTerminal),
        Binding::ToggleSidebarFocus => Keybind::App(AppAction::ToggleSidebarFocus),
        Binding::ToggleSidebarVisibility => Keybind::App(AppAction::ToggleSidebarVisibility),
        Binding::ShowAgents => Keybind::App(AppAction::Dock(crate::commands::DockAction::Agents)),
        Binding::ShowFiles => Keybind::App(AppAction::Dock(crate::commands::DockAction::Files)),
        Binding::ShowSidebar => Keybind::App(AppAction::Dock(crate::commands::DockAction::Sidebar)),
        Binding::ToggleSessionsPanel => Keybind::App(AppAction::Dock(
            crate::commands::DockAction::TogglePanel(bootty_config::config::PanelKind::Sessions),
        )),
        Binding::ToggleFilesPanel => Keybind::App(AppAction::Dock(
            crate::commands::DockAction::TogglePanel(bootty_config::config::PanelKind::Files),
        )),
        Binding::ToggleChangesPanel => Keybind::App(AppAction::Dock(
            crate::commands::DockAction::TogglePanel(bootty_config::config::PanelKind::Changes),
        )),
        Binding::ToggleDiffPanel => Keybind::App(AppAction::Dock(
            crate::commands::DockAction::TogglePanel(bootty_config::config::PanelKind::Diff),
        )),
        Binding::ToggleAgentsPanel => Keybind::App(AppAction::Dock(
            crate::commands::DockAction::TogglePanel(bootty_config::config::PanelKind::Agents),
        )),
        Binding::ToggleLeftDock => {
            Keybind::App(AppAction::Dock(crate::commands::DockAction::ToggleLeft))
        }
        Binding::ToggleRightDock => {
            Keybind::App(AppAction::Dock(crate::commands::DockAction::ToggleRight))
        }
        Binding::ShowSpaces => Keybind::App(AppAction::Dock(crate::commands::DockAction::Spaces)),
        Binding::ToggleHiddenTabs => Keybind::App(AppAction::Dock(
            crate::commands::DockAction::ToggleHiddenTabs,
        )),
        Binding::ShowCodexBar => {
            Keybind::App(AppAction::Dock(crate::commands::DockAction::CodexBar))
        }
        Binding::ToggleTabBar => {
            Keybind::App(AppAction::Dock(crate::commands::DockAction::ToggleTabBar))
        }
        Binding::EditTheme => Keybind::App(AppAction::EditTheme),
        Binding::ExportTerminal => Keybind::App(AppAction::ExportTerminal),
        Binding::ShowChanges => Keybind::App(AppAction::Dock(crate::commands::DockAction::Changes)),
        Binding::ShowDiff => Keybind::App(AppAction::Dock(crate::commands::DockAction::Diff)),
        Binding::OpenSetting(id) => Keybind::OpenSetting(id),
        Binding::OpenSettings => Keybind::App(AppAction::OpenSettings),
        Binding::ChangeAppearance(choice) => {
            Keybind::App(AppAction::ChangeAppearance(appearance_mode(choice)))
        }
        Binding::SwitchTheme => Keybind::App(AppAction::SwitchTheme),
        Binding::RenameSession => Keybind::App(AppAction::RenameSession),
        Binding::MoveSessionToSpace => Keybind::App(AppAction::MoveSessionToSpace),
        Binding::RenameTab => Keybind::App(AppAction::RenameTab),
        Binding::CreateSpace => Keybind::App(AppAction::CreateSpace),
        Binding::EditSpace => Keybind::App(AppAction::EditSpace),
        Binding::CloseSpace => Keybind::App(AppAction::CloseSpace),
        Binding::NextSpace => Keybind::App(AppAction::NextSpace),
        Binding::PreviousSpace => Keybind::App(AppAction::PreviousSpace),
        Binding::SelectSpace(index) => Keybind::App(AppAction::SelectSpace(index)),
        Binding::DitchSession => Keybind::App(AppAction::DitchSession),
        Binding::ShowKeybinds => Keybind::App(AppAction::ShowKeybinds),
        terminal => return terminal_keybind_action(terminal),
    })
}

fn terminal_keybind_action(action: BindingAction) -> Result<KeybindAction> {
    use BindingAction as Binding;
    use FontSizeAction as Font;
    use KeybindAction as Keybind;
    use MuxKeyAction as Mux;
    use TerminalFindAction as Find;
    use TerminalScrollAction as Scroll;
    Ok(match action {
        Binding::CloseSurface => Keybind::Mux(Mux::ClosePane),
        Binding::NewTab => Keybind::Mux(Mux::NewTab),
        Binding::NextTab => Keybind::Mux(Mux::NextTab),
        Binding::PreviousTab => Keybind::Mux(Mux::PreviousTab),
        Binding::LastTab => Keybind::Mux(Mux::LastTab),
        Binding::SelectTab(index) => Keybind::Mux(Mux::SelectTab(index)),
        Binding::MoveTab(delta) => Keybind::Mux(Mux::MoveTab(delta)),
        Binding::SplitRight => Keybind::Mux(Mux::SplitPane(
            bootty_mux::pane_layout::SplitDirection::Right,
        )),
        Binding::SplitDown => Keybind::Mux(Mux::SplitPane(
            bootty_mux::pane_layout::SplitDirection::Down,
        )),
        Binding::SelectPane(direction) => Keybind::Mux(Mux::SelectPane(mux_direction(direction))),
        Binding::NextPane => Keybind::Mux(Mux::NextPane),
        Binding::PreviousPane => Keybind::Mux(Mux::PreviousPane),
        Binding::KillPane => Keybind::Mux(Mux::KillPane),
        Binding::TogglePaneZoom => Keybind::Mux(Mux::TogglePaneZoom),
        Binding::NextSession => Keybind::Mux(Mux::NextSession),
        Binding::PreviousSession => Keybind::Mux(Mux::PreviousSession),
        Binding::LastSession => Keybind::Mux(Mux::LastSession),
        Binding::SelectSession(index) => Keybind::Mux(Mux::SelectSession(index)),
        Binding::MoveSession(delta) => Keybind::Mux(Mux::MoveSession(delta)),
        Binding::ScrollToTop => Keybind::Scroll(Scroll::Top),
        Binding::ScrollToBottom => Keybind::Scroll(Scroll::Bottom),
        Binding::ScrollPageUp => Keybind::Scroll(Scroll::PageUp),
        Binding::ScrollPageDown => Keybind::Scroll(Scroll::PageDown),
        Binding::ScrollPageLines(lines) => Keybind::Scroll(Scroll::Lines(lines)),
        Binding::StartSearch => Keybind::Find(Find::Prompt),
        Binding::ToggleSearchRegex => Keybind::Find(Find::ToggleRegex),
        Binding::ToggleSearchCaseSensitive => Keybind::Find(Find::ToggleCaseSensitive),
        Binding::EndSearch => Keybind::Find(Find::Close),
        Binding::Search(value) => Keybind::Find(Find::Search(value)),
        Binding::SearchSelection => Keybind::Find(Find::SearchSelection),
        Binding::NavigateSearch(direction) => Keybind::Find(match direction {
            NavigateSearch::Previous => Find::Previous,
            NavigateSearch::Next => Find::Next,
        }),
        Binding::Csi(value) => Keybind::Write(csi_bytes(&value)),
        Binding::Esc(value) => Keybind::Write(esc_bytes(&value)),
        Binding::Text(value) => Keybind::Write(text_action_bytes(&value)),
        Binding::IncreaseFontSize(delta) => Keybind::Font(Font::Increase(delta)),
        Binding::DecreaseFontSize(delta) => Keybind::Font(Font::Decrease(delta)),
        Binding::ResetFontSize => Keybind::Font(Font::Reset),
        Binding::SetFontSize(size) => Keybind::Font(Font::Set(size)),
        Binding::CopyToClipboard(format) => Keybind::CopyToClipboard(format),
        Binding::CopyMode => Keybind::CopyMode,
        Binding::PasteFromClipboard => Keybind::PasteFromClipboard,
        unsupported => anyhow::bail!("{} has no Bootty app behavior", unsupported.format_entry()),
    })
}

const fn appearance_mode(choice: AppearanceChoice) -> bootty_config::config::AppearanceMode {
    match choice {
        AppearanceChoice::System => bootty_config::config::AppearanceMode::System,
        AppearanceChoice::Light => bootty_config::config::AppearanceMode::Light,
        AppearanceChoice::Dark => bootty_config::config::AppearanceMode::Dark,
    }
}

const fn mux_direction(direction: PaneDirection) -> MuxDirection {
    match direction {
        PaneDirection::Left => MuxDirection::Left,
        PaneDirection::Down => MuxDirection::Down,
        PaneDirection::Up => MuxDirection::Up,
        PaneDirection::Right => MuxDirection::Right,
    }
}

fn csi_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len().saturating_add(2));
    bytes.extend_from_slice(b"\x1b[");
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn esc_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len().saturating_add(1));
    bytes.push(0x1b);
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn text_action_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut buf = [0; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.peek().copied() {
            Some('n') => {
                chars.next();
                bytes.push(b'\n');
            }
            Some('r') => {
                chars.next();
                bytes.push(b'\r');
            }
            Some('t') => {
                chars.next();
                bytes.push(b'\t');
            }
            Some('e') => {
                chars.next();
                bytes.push(0x1b);
            }
            Some('\\') => {
                chars.next();
                bytes.push(b'\\');
            }
            Some('x') => {
                chars.next();
                let Some(high) = chars
                    .next()
                    .and_then(|value| value.to_digit(16))
                    .and_then(|digit| u8::try_from(digit).ok())
                else {
                    bytes.extend_from_slice(b"\\x");
                    continue;
                };
                let Some(low) = chars
                    .next()
                    .and_then(|value| value.to_digit(16))
                    .and_then(|digit| u8::try_from(digit).ok())
                else {
                    bytes.extend_from_slice(format!("\\x{high:x}").as_bytes());
                    continue;
                };
                bytes.push((high << 4) | low);
            }
            Some(other) => {
                chars.next();
                bytes.push(b'\\');
                let mut buf = [0; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => bytes.push(b'\\'),
        }
    }
    bytes
}

fn binding_triggers_for_key(key: Key, modifiers: Modifiers) -> Vec<BindingTrigger> {
    binding_triggers_for_key_with_modifier_sides(key, modifiers, ModifierSideState::default())
}

pub(crate) fn binding_triggers_for_key_with_modifier_sides(
    key: Key,
    modifiers: Modifiers,
    modifier_sides: ModifierSideState,
) -> Vec<BindingTrigger> {
    let Some(terminal_key) = terminal_key(key) else {
        return Vec::new();
    };
    let input = KeyInput {
        key: terminal_key,
        mods: key_mods_for_binding(modifiers, modifier_sides),
        repeat: false,
        utf8: None,
        unshifted: binding_char_for_key(key),
    };
    binding_triggers_for_key_input(input)
}
pub(crate) fn binding_triggers_for_scroll_with_modifier_sides(
    up: bool,
    modifiers: Modifiers,
    modifier_sides: ModifierSideState,
) -> Vec<BindingTrigger> {
    let input = KeyInput {
        key: TerminalKey::A,
        mods: key_mods_for_binding(modifiers, modifier_sides),
        repeat: false,
        utf8: None,
        unshifted: None,
    };
    let key = if up {
        BindingKey::ScrollUp
    } else {
        BindingKey::ScrollDown
    };
    BindingTrigger::input_mod_candidates(input)
        .into_iter()
        .map(|mods| BindingTrigger {
            mods,
            key: key.clone(),
        })
        .collect()
}

#[must_use]
pub fn key_mods_for_binding(modifiers: Modifiers, modifier_sides: ModifierSideState) -> KeyMods {
    let mut input = KeyInput {
        key: TerminalKey::A,
        mods: KeyMods {
            shift: modifiers.shift,
            ctrl: modifiers.control,
            alt: modifiers.alt,
            command: cfg!(target_os = "macos") && modifiers.platform,
            ..Default::default()
        },
        repeat: false,
        utf8: None,
        unshifted: None,
    };
    modifier_sides.apply_to_key_input(&mut input);
    input.mods
}

pub(crate) fn binding_triggers_for_key_input(input: KeyInput) -> Vec<BindingTrigger> {
    let mut triggers = Vec::new();
    for mods in BindingTrigger::input_mod_candidates(input) {
        triggers.push(BindingTrigger {
            mods,
            key: BindingKey::Physical(input.key),
        });
        if let Some(ch) = input.unshifted.or_else(|| input.utf8.and_then(single_char)) {
            triggers.push(BindingTrigger {
                mods,
                key: BindingKey::Unicode(ch),
            });
        }
    }
    triggers
}

fn single_char(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

fn binding_char_for_key(key: Key) -> Option<char> {
    Some(match key {
        Key::Letter(ch) => ch,
        Key::Digit(digit) => char::from_digit(u32::from(digit), 10)?,
        Key::Comma => ',',
        Key::Period => '.',
        Key::Slash | Key::QuestionMark => '/',
        Key::Semicolon | Key::Colon => ';',
        Key::Quote => '\'',
        Key::Minus => '-',
        Key::Plus | Key::Equals => '=',
        Key::Backslash | Key::Pipe => '\\',
        Key::Backtick => '`',
        Key::OpenBracket | Key::OpenCurlyBracket => '[',
        Key::CloseBracket | Key::CloseCurlyBracket => ']',
        Key::Space => ' ',
        _ => return None,
    })
}

#[must_use]
pub const fn terminal_key(key: Key) -> Option<TerminalKey> {
    Some(match key {
        Key::Letter('a') => TerminalKey::A,
        Key::Letter('b') => TerminalKey::B,
        Key::Letter('c') => TerminalKey::C,
        Key::Letter('d') => TerminalKey::D,
        Key::Letter('e') => TerminalKey::E,
        Key::Letter('f') => TerminalKey::F,
        Key::Letter('g') => TerminalKey::G,
        Key::Letter('h') => TerminalKey::H,
        Key::Letter('i') => TerminalKey::I,
        Key::Letter('j') => TerminalKey::J,
        Key::Letter('k') => TerminalKey::K,
        Key::Letter('l') => TerminalKey::L,
        Key::Letter('m') => TerminalKey::M,
        Key::Letter('n') => TerminalKey::N,
        Key::Letter('o') => TerminalKey::O,
        Key::Letter('p') => TerminalKey::P,
        Key::Letter('q') => TerminalKey::Q,
        Key::Letter('r') => TerminalKey::R,
        Key::Letter('s') => TerminalKey::S,
        Key::Letter('t') => TerminalKey::T,
        Key::Letter('u') => TerminalKey::U,
        Key::Letter('v') => TerminalKey::V,
        Key::Letter('w') => TerminalKey::W,
        Key::Letter('x') => TerminalKey::X,
        Key::Letter('y') => TerminalKey::Y,
        Key::Letter('z') => TerminalKey::Z,
        Key::Digit(0) => TerminalKey::Digit0,
        Key::Digit(1) | Key::ExclamationMark => TerminalKey::Digit1,
        Key::Digit(2) => TerminalKey::Digit2,
        Key::Digit(3) => TerminalKey::Digit3,
        Key::Digit(4) => TerminalKey::Digit4,
        Key::Digit(5) => TerminalKey::Digit5,
        Key::Digit(6) => TerminalKey::Digit6,
        Key::Digit(7) => TerminalKey::Digit7,
        Key::Digit(8) => TerminalKey::Digit8,
        Key::Digit(9) => TerminalKey::Digit9,
        Key::Space => TerminalKey::Space,
        Key::Backtick => TerminalKey::Backquote,
        Key::Backslash | Key::Pipe => TerminalKey::Backslash,
        Key::OpenBracket | Key::OpenCurlyBracket => TerminalKey::BracketLeft,
        Key::CloseBracket | Key::CloseCurlyBracket => TerminalKey::BracketRight,
        Key::Comma => TerminalKey::Comma,
        Key::Minus => TerminalKey::Minus,
        Key::Period => TerminalKey::Period,
        Key::Plus | Key::Equals => TerminalKey::Equal,
        Key::Semicolon | Key::Colon => TerminalKey::Semicolon,
        Key::Quote => TerminalKey::Quote,
        Key::Slash | Key::QuestionMark => TerminalKey::Slash,
        Key::Enter => TerminalKey::Enter,
        Key::Tab => TerminalKey::Tab,
        Key::Backspace => TerminalKey::Backspace,
        Key::Escape => TerminalKey::Escape,
        Key::Insert => TerminalKey::Insert,
        Key::ArrowUp => TerminalKey::ArrowUp,
        Key::ArrowDown => TerminalKey::ArrowDown,
        Key::ArrowRight => TerminalKey::ArrowRight,
        Key::ArrowLeft => TerminalKey::ArrowLeft,
        Key::Delete => TerminalKey::Delete,
        Key::Home => TerminalKey::Home,
        Key::End => TerminalKey::End,
        Key::PageUp => TerminalKey::PageUp,
        Key::PageDown => TerminalKey::PageDown,
        Key::Function(1) => TerminalKey::F1,
        Key::Function(2) => TerminalKey::F2,
        Key::Function(3) => TerminalKey::F3,
        Key::Function(4) => TerminalKey::F4,
        Key::Function(5) => TerminalKey::F5,
        Key::Function(6) => TerminalKey::F6,
        Key::Function(7) => TerminalKey::F7,
        Key::Function(8) => TerminalKey::F8,
        Key::Function(9) => TerminalKey::F9,
        Key::Function(10) => TerminalKey::F10,
        Key::Function(11) => TerminalKey::F11,
        Key::Function(12) => TerminalKey::F12,
        _ => return None,
    })
}
