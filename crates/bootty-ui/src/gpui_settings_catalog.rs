//! Zed-shaped settings taxonomy for the native settings projection.
//!
//! The configuration schema still owns each setting's TOML path and legacy page metadata. This
//! catalog only decides where that setting appears in the native settings window.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use crate::gpui::{ScalarValue, SettingsCategory, SettingsPage, SettingsPageItem, SettingsRow};

const UNSUPPORTED_SCAN_ROOTS: &[&str] = &["extensions", "status", "sidebar", "session"];
// Keep the metadata-only refresh bounded; raise these only with cancellation and streaming UI
// diagnostics so a hostile config tree cannot monopolize the catalog worker.
const UNSUPPORTED_SCAN_MAX_DEPTH: usize = 16;
const UNSUPPORTED_SCAN_MAX_ENTRIES: usize = 4096;

/// One top-level destination in the native settings window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsCatalogPage {
    pub category: SettingsCategory,
    pub id: &'static str,
    pub label: &'static str,
    pub search_terms: &'static str,
}

/// Project the Advanced page's host-owned configuration locations and last write result.
///
/// The settings session owns the write result while the loaded config owns the source path. The
/// native UI only presents those facts; directory paths remain read-only, matching the legacy
/// settings surface without adding a second filesystem action path.
#[must_use]
pub fn advanced_configuration_rows(
    config_path: &Path,
    write_error: Option<&str>,
) -> Vec<SettingsRow> {
    let mut rows = vec![
        SettingsRow::Section("LOCATIONS".to_owned()),
        read_only_path_row("config.path", "Config file", config_path),
    ];
    if let Some(directory) = config_path.parent() {
        rows.extend([
            read_only_path_row("config.directory", "Config directory", directory),
            read_only_path_row(
                "config.themes-directory",
                "Themes directory",
                &directory.join("themes"),
            ),
            read_only_path_row(
                "config.extensions-directory",
                "Extensions directory",
                &directory.join("extensions"),
            ),
        ]);
    }
    rows.extend([
        SettingsRow::Section("RELOAD".to_owned()),
        SettingsRow::Action {
            id: "config:reload".to_owned(),
            label: "Reload configuration".to_owned(),
            help: "Re-read config.toml now and apply settings that can change in the running workspace."
                .to_owned(),
            button: "Reload config.toml".to_owned(),
            enabled: true,
        },
        SettingsRow::Notice {
            text: if write_error.is_some() {
                "Last write failed".to_owned()
            } else {
                "No write errors".to_owned()
            },
            destructive: false,
        },
        SettingsRow::Section("STATE".to_owned()),
        SettingsRow::Notice {
            text: write_error.map_or_else(
                || "No settings write errors recorded.".to_owned(),
                str::to_owned,
            ),
            destructive: write_error.is_some(),
        },
    ]);
    rows
}

fn read_only_path_row(id: &str, label: &str, path: &Path) -> SettingsRow {
    SettingsRow::Value {
        id: id.to_owned(),
        label: label.to_owned(),
        help: "Read-only location.".to_owned(),
        value: ScalarValue::Text(path.display().to_string()),
        control: crate::gpui::SettingsControl::ReadOnly,
        enabled: true,
    }
}

/// One user source retained on disk but unsupported by the native settings host.
///
/// The discovery owner supplies paths without reading or executing their contents. Keeping the
/// path in the diagnostic lets users identify the preserved file while native settings remain
/// source free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedModuleDiagnostic {
    pub path: PathBuf,
    pub detail: String,
}

/// Discover preserved user scripts without opening or executing their contents.
///
/// Missing legacy source roots are normal for a new config. Symlinked directories are not
/// followed, and bounded traversal keeps this diagnostic walk within the configured roots.
///
/// # Errors
/// Returns an error when filesystem metadata or a directory cannot be read.
pub fn scan_unsupported_module_sources(
    config_root: &Path,
) -> Result<Vec<UnsupportedModuleDiagnostic>, String> {
    let mut diagnostics = Vec::new();
    let mut entries_seen = 0;
    for relative_root in UNSUPPORTED_SCAN_ROOTS {
        let root = config_root.join(relative_root);
        let root_type = match fs::symlink_metadata(&root) {
            Ok(metadata) => metadata.file_type(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "scan unsupported module sources: {}: {error}",
                    root.display()
                ));
            }
        };
        if !root_type.is_dir() {
            continue;
        }
        if collect_module_paths(&root, 0, &mut entries_seen, &mut diagnostics)? {
            diagnostics.push(UnsupportedModuleDiagnostic {
                path: root,
                detail: format!(
                    "scan incomplete: depth is capped at {UNSUPPORTED_SCAN_MAX_DEPTH} and entries at {UNSUPPORTED_SCAN_MAX_ENTRIES}"
                ),
            });
        }
    }
    diagnostics.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(diagnostics)
}

fn collect_module_paths(
    root: &Path,
    depth: usize,
    entries_seen: &mut usize,
    diagnostics: &mut Vec<UnsupportedModuleDiagnostic>,
) -> Result<bool, String> {
    let mut limited = false;
    for entry in fs::read_dir(root).map_err(|error| {
        format!(
            "scan unsupported module sources: {}: {error}",
            root.display()
        )
    })? {
        if *entries_seen >= UNSUPPORTED_SCAN_MAX_ENTRIES {
            limited = true;
            break;
        }
        let entry = entry.map_err(|error| format!("scan unsupported module sources: {error}"))?;
        *entries_seen = entries_seen.saturating_add(1);
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            format!(
                "scan unsupported module sources: {}: {error}",
                path.display()
            )
        })?;
        if file_type.is_dir() {
            if depth >= UNSUPPORTED_SCAN_MAX_DEPTH
                || collect_module_paths(&path, depth.saturating_add(1), entries_seen, diagnostics)?
            {
                limited = true;
            }
        } else if file_type.is_file()
            && matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("lua" | "luau")
            )
        {
            diagnostics.push(UnsupportedModuleDiagnostic {
                path,
                detail: "native script execution is retired".to_owned(),
            });
        }
    }
    Ok(limited)
}

/// Project preserved user scripts into visible, non-editable diagnostics.
#[must_use]
pub fn unsupported_module_rows(diagnostics: &[UnsupportedModuleDiagnostic]) -> Vec<SettingsRow> {
    diagnostics
        .iter()
        .map(|diagnostic| SettingsRow::Notice {
            text: if diagnostic.detail.is_empty() {
                format!(
                    "Unsupported custom module source preserved: {}",
                    diagnostic.path.display()
                )
            } else {
                format!(
                    "Unsupported custom module source preserved: {} ({})",
                    diagnostic.path.display(),
                    diagnostic.detail
                )
            },
            destructive: false,
        })
        .collect()
}

/// One parent setting and the ordered rows whose meaning depends on it.
///
/// The catalog supplies this relationship while the application projects the current rows. The
/// host-neutral settings model then keeps the parent and child rows together for the renderer.
/// Child prefixes cover dynamic leaves such as the light and dark color overrides while each
/// projected row retains its concrete config path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsDependency {
    pub parent: &'static str,
    pub children: &'static [&'static str],
    cases: &'static [SettingsDependencyCase],
    child_prefixes: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SettingsDependencyCase {
    discriminant: &'static str,
    children: &'static [&'static str],
    child_prefixes: &'static [&'static str],
}

impl SettingsDependency {
    /// Return the child rows editable for the parent's current discriminant.
    #[must_use]
    pub fn active_children(&self, value: &ScalarValue) -> &'static [&'static str] {
        let Some(discriminant) = setting_dependency_discriminant(value) else {
            return &[];
        };
        self.cases
            .iter()
            .find(|case| case.discriminant == discriminant)
            .map_or(&[], |case| case.children)
    }

    /// Return whether a projected row stays editable for the parent's value.
    #[must_use]
    pub fn is_child_active(&self, id: &str, value: &ScalarValue) -> bool {
        if !self.is_child(id) {
            return false;
        }
        let Some(discriminant) = setting_dependency_discriminant(value) else {
            return false;
        };
        self.cases
            .iter()
            .find(|case| case.discriminant == discriminant)
            .is_some_and(|case| {
                case.children.contains(&id)
                    || case
                        .child_prefixes
                        .iter()
                        .any(|prefix| id.starts_with(prefix))
            })
    }

    fn is_child(&self, id: &str) -> bool {
        self.children.contains(&id)
            || self
                .child_prefixes
                .iter()
                .any(|prefix| id.starts_with(prefix))
    }
}

fn setting_dependency_discriminant(value: &ScalarValue) -> Option<&str> {
    match value {
        ScalarValue::Bool(true) => Some("true"),
        ScalarValue::Bool(false) => Some("false"),
        ScalarValue::Text(value) | ScalarValue::Token(value) => Some(value),
        ScalarValue::Number(_) => None,
    }
}

const APPEARANCE_MODE_DEPENDENCY: SettingsDependency = SettingsDependency {
    parent: "appearance.mode",
    children: &["appearance.light.theme", "appearance.dark.theme"],
    child_prefixes: &["appearance.light.colors.", "appearance.dark.colors."],
    cases: &[
        SettingsDependencyCase {
            discriminant: "system",
            children: &["appearance.light.theme", "appearance.dark.theme"],
            child_prefixes: &["appearance.light.colors.", "appearance.dark.colors."],
        },
        SettingsDependencyCase {
            discriminant: "light",
            children: &["appearance.light.theme", "appearance.dark.theme"],
            child_prefixes: &["appearance.light.colors.", "appearance.dark.colors."],
        },
        SettingsDependencyCase {
            discriminant: "dark",
            children: &["appearance.light.theme", "appearance.dark.theme"],
            child_prefixes: &["appearance.light.colors.", "appearance.dark.colors."],
        },
    ],
};

const WINDOW_FULLSCREEN_DEPENDENCY: SettingsDependency = SettingsDependency {
    parent: "window.fullscreen-enabled",
    children: &[
        "window.fullscreen",
        "window.fullscreen-tabs-in-notch",
        "window.fullscreen-top-offset",
        "chrome.notched-fullscreen-black-chrome",
    ],
    child_prefixes: &[],
    cases: &[
        SettingsDependencyCase {
            discriminant: "true",
            children: &[
                "window.fullscreen",
                "window.fullscreen-tabs-in-notch",
                "window.fullscreen-top-offset",
                "chrome.notched-fullscreen-black-chrome",
            ],
            child_prefixes: &[],
        },
        SettingsDependencyCase {
            // These configure the next fullscreen launch and remain editable while windowed.
            discriminant: "false",
            children: &[
                "window.fullscreen",
                "window.fullscreen-tabs-in-notch",
                "window.fullscreen-top-offset",
                "chrome.notched-fullscreen-black-chrome",
            ],
            child_prefixes: &[],
        },
    ],
};

const UI_FONT_FAMILY_DEPENDENCY: SettingsDependency = SettingsDependency {
    parent: "font.ui-use-terminal-family",
    children: &["font.ui-family"],
    child_prefixes: &[],
    cases: &[SettingsDependencyCase {
        discriminant: "false",
        children: &["font.ui-family"],
        child_prefixes: &[],
    }],
};

const SETTINGS_CATALOG_PAGES: [SettingsCatalogPage; 8] = [
    SettingsCatalogPage {
        category: SettingsCategory::General,
        id: SettingsCategory::General.id(),
        label: SettingsCategory::General.label(),
        search_terms: "general|startup|restore|reopen|window|workspace|session|open behavior|close|quit|update|multiplexer|backend|terminal provider|native|herdr|rmux|tmux",
    },
    SettingsCatalogPage {
        category: SettingsCategory::Appearance,
        id: SettingsCategory::Appearance.id(),
        label: SettingsCategory::Appearance.label(),
        search_terms: "appearance|theme|mode|colors|palette|font|cursor|mouse pointer",
    },
    SettingsCatalogPage {
        category: SettingsCategory::Keymap,
        id: SettingsCategory::Keymap.id(),
        label: SettingsCategory::Keymap.label(),
        search_terms: "keymap|keybindings|shortcuts|preset|prefix|global|sidebar|backend|modifier",
    },
    SettingsCatalogPage {
        category: SettingsCategory::WindowAndLayout,
        id: SettingsCategory::WindowAndLayout.id(),
        label: SettingsCategory::WindowAndLayout.label(),
        search_terms: "window|layout|fullscreen|titlebar|decoration|split panes|chrome|size",
    },
    SettingsCatalogPage {
        category: SettingsCategory::Panels,
        id: SettingsCategory::Panels.id(),
        label: SettingsCategory::Panels.label(),
        search_terms: "panels|sidebar|status bar|top bar|bottom bar|module|extension region|dock|width|visibility",
    },
    SettingsCatalogPage {
        category: SettingsCategory::Terminal,
        id: SettingsCategory::Terminal.id(),
        label: SettingsCategory::Terminal.label(),
        search_terms: "terminal|session|shell|scrollback|copy on select|option as meta|environment|protocol",
    },
    SettingsCatalogPage {
        category: SettingsCategory::Remotes,
        id: SettingsCategory::Remotes.id(),
        label: SettingsCategory::Remotes.label(),
        search_terms: "remote|ssh|profile|host|port|user|authentication|proxy|connection",
    },
    SettingsCatalogPage {
        category: SettingsCategory::Advanced,
        id: SettingsCategory::Advanced.id(),
        label: SettingsCategory::Advanced.label(),
        search_terms: "advanced|config|diagnostics|unsupported custom module|settings|reload",
    },
];

/// The eight native-settings pages in sidebar order.
#[must_use]
pub const fn settings_catalog_pages() -> &'static [SettingsCatalogPage] {
    &SETTINGS_CATALOG_PAGES
}

/// Return the nested child rows for a parent setting, when the catalog declares one.
#[must_use]
pub fn settings_dependency_for(parent: &str) -> Option<&'static SettingsDependency> {
    match parent {
        "appearance.mode" => Some(&APPEARANCE_MODE_DEPENDENCY),
        "window.fullscreen-enabled" => Some(&WINDOW_FULLSCREEN_DEPENDENCY),
        "font.ui-use-terminal-family" => Some(&UI_FONT_FAMILY_DEPENDENCY),
        _ => None,
    }
}

/// Whether a schema setting belongs on the native Zed-shaped settings surface.
///
/// The legacy TOML keybinding arrays remain supported as an input layer, but the native UI edits
/// bindings through `keymap.json` and the dedicated keymap editor. Projecting both editors would
/// expose two competing sources of truth and reintroduce the old overflowing summary rows.
#[must_use]
pub fn setting_is_visible_in_native_settings(id: &str) -> bool {
    !matches!(
        id,
        "input.keybind"
            | "input.sidebar-keybind"
            | "input.backend-keybind.herdr"
            | "input.backend-keybind.native"
            | "input.backend-keybind.rmux"
            | "input.backend-keybind.tmux"
            | "sidebar.session-modules"
            | "sidebar.modules"
    )
}

/// Select the native settings page for one schema setting.
///
/// `legacy_page` is the schema page that still supplies the setting's default section and
/// writeback behavior. The native window deliberately does not expose that legacy taxonomy.
#[must_use]
pub fn settings_category_for(id: &str, legacy_page: &str) -> SettingsCategory {
    match id {
        // General owns app-wide defaults. Backend choice controls the default binding for new
        // Spaces; panel and window choices deliberately stay on their dedicated pages.
        "multiplexer.backend" => SettingsCategory::General,

        // Appearance owns the visual language of the application, including interface text.
        "appearance.mode" | "input.hide-mouse-pointer-while-typing" => SettingsCategory::Appearance,
        id if id.starts_with("font.") || id.starts_with("cursor.") => SettingsCategory::Appearance,

        // Terminal owns terminal behavior even though both controls originated in the key page.
        "input.copy-on-select" | "input.macos-option-as-alt" => SettingsCategory::Terminal,

        // Panels own docking and visibility. Fullscreen chrome stays layout.
        "chrome.left-dock-toggle"
        | "chrome.right-dock-toggle"
        | "chrome.panel-tab-style"
        | "chrome.panel-tabs"
        | "chrome.top-bar"
        | "chrome.bottom-bar"
        | "chrome.status-height"
        | "chrome.top-segment"
        | "chrome.bottom-segment"
        | "multiplexer.hide-tmux-status" => SettingsCategory::Panels,
        "chrome.notched-fullscreen-black-chrome" => SettingsCategory::WindowAndLayout,

        _ => match legacy_page {
            "appearance" | "colors" | "text" => SettingsCategory::Appearance,
            "shell" => SettingsCategory::Terminal,
            "keys" => SettingsCategory::Keymap,
            "window" => SettingsCategory::WindowAndLayout,
            "panels" | "sidebar" | "status" => SettingsCategory::Panels,
            "remotes" => SettingsCategory::Remotes,
            "general" => SettingsCategory::General,
            // Extension-declared settings use `extensions` today. Keep an unknown future page
            // contained in Advanced rather than silently dropping its writable schema row.
            _ => SettingsCategory::Advanced,
        },
    }
}

/// The visible section anchor for a schema setting after it is projected into a native page.
#[must_use]
pub fn settings_section<'a>(
    category: SettingsCategory,
    id: &str,
    source_section: &'a str,
) -> &'a str {
    match category {
        SettingsCategory::General => match id {
            "restore_on_startup" => "STARTUP",
            "cli_default_open_behavior" | "default_open_behavior" => "OPENING",
            "when_closing_with_no_tabs" | "on_last_window_closed" => "WINDOWS",
            "multiplexer.backend" => "DEFAULT SPACE",
            _ => source_section,
        },
        SettingsCategory::Appearance => match id {
            "theme" | "appearance.mode" | "appearance.light.theme" | "appearance.dark.theme" => {
                "THEME"
            }
            "colors.*" | "appearance.light.colors.*" | "appearance.dark.colors.*" => {
                "TERMINAL COLORS"
            }
            "chrome.status-background"
            | "chrome.pane-divider-color"
            | "chrome.pane-focus-border-color" => "WINDOW COLORS",
            id if id.starts_with("sidebar.") => "FIXED DOCK COLORS",
            "font.ui-family" | "font.ui-size" | "font.ui-use-terminal-family" => "INTERFACE FONT",
            id if id.starts_with("font.ui-weights.") => "INTERFACE WEIGHTS",
            id if id.starts_with("font.") => "TERMINAL FONT",
            id if id.starts_with("cursor.") => "CURSOR",
            "input.hide-mouse-pointer-while-typing" => "MOUSE POINTER",
            _ => source_section,
        },
        SettingsCategory::Panels => match id {
            "chrome.left-dock-toggle"
            | "chrome.right-dock-toggle"
            | "chrome.panel-tab-style"
            | "chrome.panel-tabs" => "FIXED DOCKS",
            "chrome.top-bar"
            | "chrome.bottom-bar"
            | "chrome.status-height"
            | "multiplexer.hide-tmux-status" => "STATUS BARS",
            "chrome.top-segment" | "chrome.bottom-segment" => "STATUS LAYOUT",
            _ => source_section,
        },
        _ => source_section,
    }
}

/// Product order inside one native page. Schema declaration order resolves ties.
#[must_use]
pub fn settings_row_order(category: SettingsCategory, id: &str) -> u16 {
    match category {
        SettingsCategory::General => match id {
            "restore_on_startup" => 0,
            "cli_default_open_behavior" => 10,
            "default_open_behavior" => 11,
            "when_closing_with_no_tabs" => 20,
            "on_last_window_closed" => 21,
            "multiplexer.backend" => 30,
            _ => 100,
        },
        SettingsCategory::Appearance => match id {
            "theme" => 0,
            "appearance.mode" => 1,
            "appearance.light.theme" => 2,
            "appearance.dark.theme" => 3,
            "colors.*" => 10,
            "appearance.light.colors.*" => 11,
            "appearance.dark.colors.*" => 12,
            id if id.starts_with("chrome.") => 20,
            id if id.starts_with("sidebar.") => 30,
            "font.family" => 40,
            "font.size" => 41,
            "font.ui-use-terminal-family" => 50,
            "font.ui-family" => 51,
            "font.ui-size" => 52,
            id if id.starts_with("font.ui-weights.") => 53,
            id if id.starts_with("font.") => 42,
            id if id.starts_with("cursor.") => 60,
            "input.hide-mouse-pointer-while-typing" => 70,
            _ => 100,
        },
        SettingsCategory::Keymap => match id {
            "input.preset" => 0,
            "input.prefix" => 1,
            "input.keybind" => 10,
            "input.sidebar-keybind" => 20,
            id if id.starts_with("input.backend-keybind.") => 30,
            "input.modifier-remap" => 40,
            _ => 100,
        },
        SettingsCategory::WindowAndLayout => match id {
            "window.title" => 0,
            "window.macos-titlebar-style" => 1,
            "window.window-decoration" => 2,
            "window.fullscreen-enabled" => 10,
            "window.fullscreen" => 11,
            "window.width" => 20,
            "window.height" => 21,
            "window.fullscreen-tabs-in-notch" => 30,
            "window.fullscreen-top-offset" => 31,
            "chrome.notched-fullscreen-black-chrome" => 32,
            id if id.starts_with("chrome.") => 40,
            _ => 100,
        },
        SettingsCategory::Panels => match id {
            "chrome.left-dock-toggle" => 1,
            "chrome.right-dock-toggle" => 2,
            "chrome.panel-tab-style" => 3,
            "chrome.panel-tabs" => 4,
            id if id.starts_with("chrome.dock-tabs.") => 10,
            id if id.starts_with("chrome.terminal-tabs.") => 11,
            "chrome.top-bar" => 20,
            "chrome.bottom-bar" => 21,
            "chrome.status-height" => 22,
            "multiplexer.hide-tmux-status" => 23,
            "chrome.top-segment" => 40,
            "chrome.bottom-segment" => 41,
            _ => 100,
        },
        SettingsCategory::Terminal => match id {
            "session.shell" => 0,
            "session.working-directory" => 1,
            "session.env" => 2,
            "session.term" => 10,
            "session.colorterm" => 11,
            "session.glyph-protocol" => 12,
            "session.max-scrollback" => 20,
            "session.scrollbar" => 21,
            "input.copy-on-select" => 30,
            "input.macos-option-as-alt" => 31,
            _ => 100,
        },
        _ => 100,
    }
}

/// Adapt Bootty's ordered setting rows to the copied Zed settings page model.
///
/// `SettingsRow::Section` is an adapter-only delimiter. The renderer sees one flat stream of
/// Zed-shaped page items, with each section anchor immediately preceding its setting items.
/// Dependent rows remain one item so the renderer can draw their child container without
/// changing the persistence-neutral row vocabulary.
#[must_use]
pub fn settings_page(source: &SettingsCatalogPage, rows: Vec<SettingsRow>) -> SettingsPage {
    SettingsPage {
        category: source.category,
        title: source.label.to_owned(),
        search_terms: source.search_terms.to_owned(),
        items: settings_page_items(source, rows),
    }
}

fn settings_page_items(
    source: &SettingsCatalogPage,
    rows: Vec<SettingsRow>,
) -> Vec<SettingsPageItem> {
    let mut items = Vec::new();
    let mut pending_section = None;
    let mut has_active_section = false;
    let mut ids = HashSet::new();
    let mut rows = rows.into_iter().map(Some).collect::<Vec<_>>();

    for index in 0..rows.len() {
        let Some(row) = rows.get_mut(index).and_then(Option::take) else {
            continue;
        };
        match row {
            SettingsRow::Section(title) => {
                pending_section = Some(title);
                has_active_section = false;
            }
            row => {
                if let Some(title) = pending_section.take() {
                    items.push(settings_page_section_header(source, title, &mut ids));
                    has_active_section = true;
                } else if !has_active_section {
                    items.push(settings_page_section_header(
                        source,
                        source.label.to_owned(),
                        &mut ids,
                    ));
                    has_active_section = true;
                }
                items.push(dependent_settings_page_item(row, &mut rows));
            }
        }
    }
    items
}

fn dependent_settings_page_item(
    parent: SettingsRow,
    rows: &mut [Option<SettingsRow>],
) -> SettingsPageItem {
    let Some(parent_id) = settings_row_id(&parent) else {
        return SettingsPageItem::Setting(parent);
    };
    let Some(dependency) = settings_dependency_for(parent_id) else {
        return SettingsPageItem::Setting(parent);
    };
    let active_value = settings_row_value(&parent);
    let children = rows
        .iter_mut()
        .filter_map(|row| {
            let id = row.as_ref().and_then(settings_row_id)?;
            if !dependency.is_child(id) {
                return None;
            }
            let active = active_value.is_some_and(|value| dependency.is_child_active(id, value));
            let child = row.take()?;
            active.then_some(child)
        })
        .collect::<Vec<_>>();
    if children.is_empty() {
        SettingsPageItem::Setting(parent)
    } else {
        SettingsPageItem::Dependent { parent, children }
    }
}

const fn settings_row_value(row: &SettingsRow) -> Option<&ScalarValue> {
    match row {
        SettingsRow::Value { value, .. } => Some(value),
        _ => None,
    }
}

fn settings_row_id(row: &SettingsRow) -> Option<&str> {
    match row {
        SettingsRow::Value { id, .. }
        | SettingsRow::Action { id, .. }
        | SettingsRow::StringList { id, .. }
        | SettingsRow::ModifierRemaps { id, .. }
        | SettingsRow::Environment { id, .. }
        | SettingsRow::FontFeatures { id, .. }
        | SettingsRow::AnsiPalette { id, .. } => Some(id),
        SettingsRow::StatusSegments(snapshot) => Some(&snapshot.id),
        SettingsRow::Section(_)
        | SettingsRow::Notice { .. }
        | SettingsRow::ModuleIntegrations(_)
        | SettingsRow::Remote(_) => None,
    }
}

fn settings_page_section_header(
    source: &SettingsCatalogPage,
    title: String,
    ids: &mut HashSet<String>,
) -> SettingsPageItem {
    let base_id = format!("{}:{}", source.id, settings_section_slug(&title));
    let mut id = base_id.clone();
    let mut duplicate = 2_usize;
    while !ids.insert(id.clone()) {
        id = format!("{base_id}-{duplicate}");
        duplicate = duplicate.saturating_add(1);
    }
    SettingsPageItem::SectionHeader {
        id,
        search_terms: format!("{}|{}", source.search_terms, title.to_ascii_lowercase()),
        title,
    }
}

fn settings_section_slug(label: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in label.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.is_empty() {
        "section".to_owned()
    } else {
        slug
    }
}
