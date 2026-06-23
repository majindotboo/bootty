use eframe::egui;

use super::SettingsWindow;
use crate::config::load_or_create_config_document;

/// Which keybind list is being edited: the global list, one of the per-backend lists, or the
/// sidebar navigation list (which has its own action vocabulary).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum KeybindScope {
    Global,
    Native,
    Rmux,
    Tmux,
    Zellij,
    Sidebar,
}

impl KeybindScope {
    const ALL: [(KeybindScope, &'static str); 6] = [
        (Self::Global, "Global"),
        (Self::Native, "Native"),
        (Self::Rmux, "Rmux"),
        (Self::Tmux, "Tmux"),
        (Self::Zellij, "Zellij"),
        (Self::Sidebar, "Sidebar"),
    ];

    fn path(self) -> &'static [&'static str] {
        match self {
            Self::Global => &["input", "keybind"],
            Self::Native => &["input", "backend-keybind", "native"],
            Self::Rmux => &["input", "backend-keybind", "rmux"],
            Self::Tmux => &["input", "backend-keybind", "tmux"],
            Self::Zellij => &["input", "backend-keybind", "zellij"],
            Self::Sidebar => &["input", "sidebar-keybind"],
        }
    }

    /// Action vocabulary offered for this list. The sidebar list maps to a small, distinct set.
    fn actions(self) -> &'static [&'static str] {
        match self {
            Self::Sidebar => SIDEBAR_ACTIONS,
            _ => ACTIONS,
        }
    }

    /// Whether `entry` (`trigger=action`) is a valid binding for this list. The sidebar list uses
    /// its own trigger/action grammar rather than the app-level binding parser.
    fn entry_is_valid(self, trigger: &str, action: &str) -> bool {
        if self == Self::Sidebar {
            trigger
                .parse::<crate::input_binding::BindingTrigger>()
                .is_ok()
                && SIDEBAR_ACTIONS.contains(&action)
        } else {
            crate::input_binding::parse_binding_elements(&format!("{trigger}={action}")).is_ok()
        }
    }
}

/// One editable binding: a trigger (one combo, or a `>`-joined chord) and an action.
#[derive(Default)]
pub(super) struct BindingRow {
    pub trigger: String,
    pub action: String,
}

/// In-progress chord capture: steps accumulate until `deadline` passes with no new key.
pub(super) struct ChordCapture {
    pub row: usize,
    pub steps: Vec<String>,
    pub deadline: Option<f64>,
}

/// Seconds to wait for the next chord step before committing the captured trigger.
const CHORD_TIMEOUT: f64 = 0.8;

/// Action names accepted as app/backend keybinds (the ones `keybind_action` maps). Param actions
/// (e.g. `select_tab`, `move_session`, `text`) take a trailing `:value` the user types in.
const ACTIONS: &[&str] = &[
    "ignore",
    "reload_config",
    "open_settings",
    "new_window",
    "new_mux_session",
    "session_picker",
    "close_window",
    "close_surface",
    "quit",
    "toggle_fullscreen",
    "toggle_sidebar_focus",
    "toggle_sidebar_visibility",
    "new_tab",
    "next_tab",
    "previous_tab",
    "last_tab",
    "select_tab",
    "move_tab",
    "split_right",
    "split_down",
    "select_pane",
    "next_pane",
    "kill_pane",
    "toggle_pane_zoom",
    "next_session",
    "previous_session",
    "last_session",
    "select_session",
    "move_session",
    "ditch_session",
    "scroll_to_top",
    "scroll_to_bottom",
    "scroll_page_up",
    "scroll_page_down",
    "scroll_page_lines",
    "increase_font_size",
    "decrease_font_size",
    "reset_font_size",
    "set_font_size",
    "copy_to_clipboard",
    "paste_from_clipboard",
    "csi",
    "esc",
    "text",
];

/// Actions accepted in the sidebar navigation list (see `sidebar_action` in `app_actions`).
const SIDEBAR_ACTIONS: &[&str] = &[
    "ignore",
    "previous_session",
    "next_session",
    "activate_session",
    "focus_terminal",
];

/// Modifier tokens accepted by the modifier-remap parser, both unsided and per-side.
const MODIFIER_TOKENS: &[&str] = &[
    "ctrl",
    "alt",
    "shift",
    "super",
    "left_ctrl",
    "left_alt",
    "left_shift",
    "left_super",
    "right_ctrl",
    "right_alt",
    "right_shift",
    "right_super",
];

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;

    input_section(win, ui);

    super::section(ui, palette, "KEYBINDINGS");
    ui.label(
        egui::RichText::new(
            "Bindings layer on top of the built-in defaults. Record a trigger (including chords \
             like ctrl+space then c), pick an action, and add a trailing :value for actions that \
             take one (select_tab:1, move_session:-1, text:\\n).",
        )
        .color(palette.muted)
        .size(12.0),
    );
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        ui.label("List");
        let mut scope = win.keybind_scope;
        for (candidate, label) in KeybindScope::ALL {
            let selected = scope == candidate;
            // Selected pill sits on the light `primary` fill, so its text must be dark to read.
            let text = egui::RichText::new(label).color(if selected {
                palette.base
            } else {
                palette.subtext
            });
            if ui.add(egui::Button::selectable(selected, text)).clicked() {
                scope = candidate;
            }
        }
        win.keybind_scope = scope;
    });
    let scope = win.keybind_scope;

    if win.keybind_loaded_scope != Some(scope) {
        let (clear, rows) = read_scope_entries(win, scope);
        win.keybind_clear = clear;
        win.keybind_rows = Some(rows);
        win.keybind_loaded_scope = Some(scope);
        win.keybind_capture = None;
    }

    let mut rows = win.keybind_rows.take().unwrap_or_default();
    let mut clear = win.keybind_clear;
    let mut capture = win.keybind_capture.take();
    let mut changed = false;

    ui.add_space(6.0);
    if ui
        .checkbox(&mut clear, "Drop the built-in defaults for this list")
        .changed()
    {
        changed = true;
    }
    ui.add_space(6.0);

    handle_capture(ui, &mut capture, &mut rows, &mut changed);

    let mut remove: Option<usize> = None;
    let mut toggle_capture: Option<usize> = None;
    for (index, row) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let recording = capture.as_ref().is_some_and(|cap| cap.row == index);
            if recording {
                let steps = capture
                    .as_ref()
                    .map(|cap| cap.steps.join(" > "))
                    .unwrap_or_default();
                let text = if steps.is_empty() {
                    "press keys… (Esc cancels)".to_owned()
                } else {
                    format!("{steps} …")
                };
                ui.add_sized(
                    [180.0, 26.0],
                    egui::Label::new(egui::RichText::new(text).monospace().color(palette.warning)),
                );
            } else {
                let response = ui.add_sized(
                    [180.0, 26.0],
                    egui::TextEdit::singleline(&mut row.trigger)
                        .font(egui::TextStyle::Monospace)
                        .vertical_align(egui::Align::Center)
                        .hint_text("ctrl+a or ctrl+space>c"),
                );
                if response.changed() {
                    changed = true;
                }
            }
            if ui
                .selectable_label(recording, if recording { "Stop" } else { "Rec" })
                .clicked()
            {
                toggle_capture = Some(index);
            }

            ui.label("→");
            // Action is split into a searchable base picker and a small params field, recombined
            // as `base:params` so parameterized actions (select_tab:1, text:\n) stay editable.
            let (base, params) = match row.action.split_once(':') {
                Some((base, params)) => (base.to_owned(), params.to_owned()),
                None => (row.action.clone(), String::new()),
            };
            let base_label = if base.is_empty() {
                "action".to_owned()
            } else {
                base.clone()
            };
            let actions = scope.actions();
            let current_index = actions.iter().position(|name| *name == base);
            if let Some(choice) = super::searchable_combo(
                ui,
                palette,
                &format!("kb_action_{index}"),
                &base_label,
                150.0,
                actions,
                current_index,
            ) {
                let chosen = actions[choice];
                row.action = if params.trim().is_empty() {
                    chosen.to_owned()
                } else {
                    format!("{chosen}:{params}")
                };
                changed = true;
            }
            let mut params_edit = params.clone();
            let response = ui.add_sized(
                [70.0, 26.0],
                egui::TextEdit::singleline(&mut params_edit)
                    .font(egui::TextStyle::Monospace)
                    .vertical_align(egui::Align::Center)
                    .hint_text(":value"),
            );
            if response.changed() {
                row.action = if params_edit.trim().is_empty() {
                    base.clone()
                } else {
                    format!("{base}:{params_edit}")
                };
                changed = true;
            }

            let trigger = row.trigger.trim();
            let action = row.action.trim();
            if trigger.is_empty() || action.is_empty() {
                ui.colored_label(palette.muted, "incomplete");
            } else if scope.entry_is_valid(trigger, action) {
                ui.colored_label(palette.success, "✓");
            } else {
                ui.colored_label(palette.destructive, "invalid");
            }

            if ui.small_button("✕").clicked() {
                remove = Some(index);
            }
        });
    }

    ui.add_space(8.0);
    if ui.button("+ Add binding").clicked() {
        rows.push(BindingRow::default());
        changed = true;
    }

    if let Some(index) = toggle_capture {
        capture = match capture {
            Some(cap) if cap.row == index => None,
            _ => Some(ChordCapture {
                row: index,
                steps: Vec::new(),
                deadline: None,
            }),
        };
    }
    if let Some(index) = remove {
        if index < rows.len() {
            rows.remove(index);
            changed = true;
        }
        capture = match capture {
            Some(cap) if cap.row == index => None,
            Some(cap) if cap.row > index => Some(ChordCapture {
                row: cap.row - 1,
                ..cap
            }),
            other => other,
        };
    }

    win.keybind_clear = clear;
    if changed {
        write_scope(win, scope, clear, &rows);
    }
    win.keybind_rows = Some(rows);
    win.keybind_capture = capture;

    super::section(ui, palette, "EFFECTIVE BINDINGS");
    ui.label(
        egui::RichText::new(
            "Built-in defaults plus your bindings for this list (reopen to refresh).",
        )
        .color(palette.muted)
        .size(12.0),
    );
    egui::CollapsingHeader::new("Show effective bindings").show(ui, |ui| {
        for entry in effective_bindings(win, scope) {
            ui.label(
                egui::RichText::new(entry)
                    .monospace()
                    .color(palette.subtext)
                    .size(12.0),
            );
        }
    });
}

/// Input settings that sit above the keybind lists.
fn input_section(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    super::section(ui, palette, "INPUT");

    egui::Grid::new("settings_input_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Hide mouse pointer while typing");
            let mut hide_pointer = win.config.input.hide_mouse_pointer_while_typing;
            if ui.checkbox(&mut hide_pointer, "").changed() {
                win.config.input.hide_mouse_pointer_while_typing = hide_pointer;
                win.set_bool(&["input", "hide-mouse-pointer-while-typing"], hide_pointer);
            }
            ui.end_row();

            ui.label("Option as Alt (macOS)");
            let tokens = ["none", "left", "right", "both"];
            let current = match win.config.input.macos_option_as_alt {
                crate::config::MacosOptionAsAltConfig::None => 0,
                crate::config::MacosOptionAsAltConfig::Left => 1,
                crate::config::MacosOptionAsAltConfig::Right => 2,
                crate::config::MacosOptionAsAltConfig::Both => 3,
            };
            if let Some(index) = super::searchable_combo(
                ui,
                palette,
                "opt_as_alt",
                tokens[current],
                160.0,
                &tokens,
                Some(current),
            ) {
                win.config.input.macos_option_as_alt = match index {
                    0 => crate::config::MacosOptionAsAltConfig::None,
                    1 => crate::config::MacosOptionAsAltConfig::Left,
                    2 => crate::config::MacosOptionAsAltConfig::Right,
                    _ => crate::config::MacosOptionAsAltConfig::Both,
                };
                win.set_str(&["input", "macos-option-as-alt"], tokens[index]);
            }
            ui.end_row();
        });

    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Rewrite one physical modifier to another (e.g. left_alt → ctrl).")
            .color(palette.muted)
            .size(12.0),
    );
    ui.add_space(6.0);

    if win.modifier_rows.is_none() {
        let rows = win
            .config
            .input
            .modifier_remap
            .iter()
            .map(|entry| match entry.split_once('=') {
                Some((from, to)) => (from.trim().to_owned(), to.trim().to_owned()),
                None => (entry.clone(), String::new()),
            })
            .collect();
        win.modifier_rows = Some(rows);
    }
    let mut rows = win.modifier_rows.take().unwrap_or_default();
    let mut changed = false;
    let mut remove: Option<usize> = None;
    for (index, (from, to)) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let from_index = MODIFIER_TOKENS
                .iter()
                .position(|&token| token == from.as_str());
            let from_label = if from.is_empty() {
                "from"
            } else {
                from.as_str()
            };
            if let Some(choice) = super::searchable_combo(
                ui,
                palette,
                &format!("mod_remap_from_{index}"),
                from_label,
                150.0,
                MODIFIER_TOKENS,
                from_index,
            ) {
                *from = MODIFIER_TOKENS[choice].to_owned();
                changed = true;
            }
            ui.label("→");
            let to_index = MODIFIER_TOKENS
                .iter()
                .position(|&token| token == to.as_str());
            let to_label = if to.is_empty() { "to" } else { to.as_str() };
            if let Some(choice) = super::searchable_combo(
                ui,
                palette,
                &format!("mod_remap_to_{index}"),
                to_label,
                150.0,
                MODIFIER_TOKENS,
                to_index,
            ) {
                *to = MODIFIER_TOKENS[choice].to_owned();
                changed = true;
            }
            if from.is_empty() || to.is_empty() {
                ui.colored_label(palette.muted, "incomplete");
            } else if remap_is_valid(from, to) {
                ui.colored_label(palette.success, "✓");
            } else {
                ui.colored_label(palette.destructive, "invalid");
            }
            if ui.small_button("✕").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        rows.remove(index);
        changed = true;
    }
    ui.add_space(4.0);
    if ui.button("+ Add remap").clicked() {
        rows.push((String::new(), String::new()));
        changed = true;
    }
    if changed {
        // Skip incomplete or invalid rows so a half-edited remap never breaks the reload.
        let entries: Vec<String> = rows
            .iter()
            .filter(|(from, to)| remap_is_valid(from, to))
            .map(|(from, to)| format!("{from}={to}"))
            .collect();
        win.config.input.modifier_remap = entries.clone();
        if entries.is_empty() {
            win.remove(&["input", "modifier-remap"]);
        } else {
            win.set_strings(&["input", "modifier-remap"], &entries);
        }
    }
    win.modifier_rows = Some(rows);
}

fn remap_is_valid(from: &str, to: &str) -> bool {
    if from.is_empty() || to.is_empty() {
        return false;
    }
    let mut set = crate::modifier_remap::ModifierRemapSet::default();
    set.parse(&format!("{from}={to}")).is_ok()
}

fn handle_capture(
    ui: &egui::Ui,
    capture: &mut Option<ChordCapture>,
    rows: &mut [BindingRow],
    changed: &mut bool,
) {
    if capture.is_none() {
        return;
    }
    let now = ui.input(|input| input.time);
    // Keep repainting so the chord-timeout commit fires even without further input.
    ui.ctx().request_repaint();

    if let Some((key, modifiers)) = drain_first_key_press(ui) {
        if key == egui::Key::Escape {
            *capture = None;
            return;
        }
        if let Some(step) = trigger_step(key, modifiers)
            && let Some(cap) = capture.as_mut()
        {
            cap.steps.push(step);
            cap.deadline = Some(now + CHORD_TIMEOUT);
        }
        return;
    }

    let commit = capture.as_ref().and_then(|cap| {
        (cap.deadline.is_some_and(|deadline| now >= deadline) && !cap.steps.is_empty())
            .then(|| (cap.row, cap.steps.join(">")))
    });
    if let Some((row, trigger)) = commit {
        if let Some(entry) = rows.get_mut(row) {
            entry.trigger = trigger;
        }
        *capture = None;
        *changed = true;
    }
}

/// Remove and return the first key-press event this frame so captured keys don't leak into the
/// focused text field.
fn drain_first_key_press(ui: &egui::Ui) -> Option<(egui::Key, egui::Modifiers)> {
    ui.input_mut(|input| {
        let mut first = None;
        input.events.retain(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if first.is_none() {
                    first = Some((*key, *modifiers));
                }
                false
            }
            _ => true,
        });
        first
    })
}

fn trigger_step(key: egui::Key, modifiers: egui::Modifiers) -> Option<String> {
    let token = key_token(key)?;
    let mut parts: Vec<&str> = Vec::new();
    // egui aliases `command` to `ctrl` off macOS, so only treat the real Cmd key as cmd.
    if cfg!(target_os = "macos") && (modifiers.mac_cmd || modifiers.command) {
        parts.push("cmd");
    }
    if modifiers.ctrl {
        parts.push("ctrl");
    }
    if modifiers.alt {
        parts.push("alt");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    let mut step = parts.join("+");
    if !step.is_empty() {
        step.push('+');
    }
    step.push_str(&token);
    Some(step)
}

fn read_scope_entries(win: &SettingsWindow, scope: KeybindScope) -> (bool, Vec<BindingRow>) {
    let Ok(document) = load_or_create_config_document(&win.config_path) else {
        return (false, Vec::new());
    };
    let path = scope.path();
    let mut current = document.document().get(path[0]);
    for key in &path[1..] {
        current = current
            .and_then(|item| item.as_table_like())
            .and_then(|table| table.get(key));
    }
    let Some(array) = current.and_then(|item| item.as_array()) else {
        return (false, Vec::new());
    };

    let mut clear = false;
    let mut rows = Vec::new();
    for value in array.iter() {
        let Some(entry) = value.as_str() else {
            continue;
        };
        if entry == "clear" {
            clear = true;
            continue;
        }
        let (trigger, action) = split_entry(entry);
        rows.push(BindingRow { trigger, action });
    }
    (clear, rows)
}

fn write_scope(win: &mut SettingsWindow, scope: KeybindScope, clear: bool, rows: &[BindingRow]) {
    let mut entries: Vec<String> = Vec::new();
    if clear {
        entries.push("clear".to_owned());
    }
    for row in rows {
        let trigger = row.trigger.trim();
        let action = row.action.trim();
        if trigger.is_empty() || action.is_empty() {
            continue;
        }
        // Skip invalid rows so a half-typed binding never makes the whole config fail to reload.
        if scope.entry_is_valid(trigger, action) {
            entries.push(format!("{trigger}={action}"));
        }
    }
    win.set_strings(scope.path(), &entries);
}

/// Split an entry into trigger and action at the action `=`, mirroring the binding parser so
/// triggers that contain `=` (like `cmd+=`) stay intact.
fn split_entry(entry: &str) -> (String, String) {
    let bytes = entry.as_bytes();
    let mut offset = 0;
    while let Some(rel) = entry[offset..].find('=') {
        let index = offset + rel;
        if index + 1 < entry.len() && matches!(bytes[index + 1], b'+' | b'=') {
            offset = index + 1;
            continue;
        }
        return (entry[..index].to_owned(), entry[index + 1..].to_owned());
    }
    (entry.to_owned(), String::new())
}

fn effective_bindings(win: &SettingsWindow, scope: KeybindScope) -> Vec<String> {
    let input = &win.config.input;
    match scope {
        KeybindScope::Global => input.keybind.clone(),
        KeybindScope::Native => input.backend_keybinds.native.clone(),
        KeybindScope::Rmux => input.backend_keybinds.rmux.clone(),
        KeybindScope::Tmux => input.backend_keybinds.tmux.clone(),
        KeybindScope::Zellij => input.backend_keybinds.zellij.clone(),
        KeybindScope::Sidebar => input.sidebar_keybind.clone(),
    }
}

fn key_token(key: egui::Key) -> Option<String> {
    use egui::Key;
    let token = match key {
        Key::A => "a",
        Key::B => "b",
        Key::C => "c",
        Key::D => "d",
        Key::E => "e",
        Key::F => "f",
        Key::G => "g",
        Key::H => "h",
        Key::I => "i",
        Key::J => "j",
        Key::K => "k",
        Key::L => "l",
        Key::M => "m",
        Key::N => "n",
        Key::O => "o",
        Key::P => "p",
        Key::Q => "q",
        Key::R => "r",
        Key::S => "s",
        Key::T => "t",
        Key::U => "u",
        Key::V => "v",
        Key::W => "w",
        Key::X => "x",
        Key::Y => "y",
        Key::Z => "z",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        Key::Num9 => "9",
        Key::Comma => ",",
        Key::Period => ".",
        Key::Slash => "/",
        Key::Semicolon => ";",
        Key::Quote => "'",
        Key::Minus => "-",
        Key::Plus | Key::Equals => "=",
        Key::Backslash => "\\",
        Key::Backtick => "`",
        Key::Space => "space",
        Key::Enter => "Enter",
        Key::Tab => "Tab",
        Key::Backspace => "Backspace",
        Key::Delete => "Delete",
        Key::ArrowUp => "ArrowUp",
        Key::ArrowDown => "ArrowDown",
        Key::ArrowLeft => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PageUp",
        Key::PageDown => "PageDown",
        Key::Insert => "Insert",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F11 => "F11",
        Key::F12 => "F12",
        _ => return None,
    };
    Some(token.to_owned())
}
