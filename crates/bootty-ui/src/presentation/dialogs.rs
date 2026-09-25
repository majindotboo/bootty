//! Renderer-neutral product dialog ownership and GPUI projections.

mod new_session;
pub use new_session::{NewSessionDialog, NewSessionPickerEvent};

use bootty_mux::RemoteSpaceSummary;
use gpui_kit::component::Colorize as _;

use crate::strings::home_dir;
use std::collections::HashMap;

use crate::{
    gpui::{
        DialogAction, DialogId, DialogIntent, DialogPayload, DialogRole, DialogRow, DialogSpec,
        OptionalSpaceEditorChoice, RemoteSpaceSnapshot, RowId, SpaceEditorChoice,
        SpaceEditorColors, SpaceEditorIcon, SpaceEditorIntent, SpaceEditorSnapshot,
    },
    product_dialogs::{
        keybind_help::KeybindHelpModel,
        searchable::{SearchableEntry, SearchableIntent, SearchableList},
        terminal_find::{
            FindDirection as ProductFindDirection, TerminalFindIntent, TerminalFindModel,
            TerminalFindOutput,
        },
    },
};
use bootty_config::config::{
    AppearanceMode, MultiplexerBackendConfig, RemoteConfig, SshProfileConfig,
};
use bootty_git::{self as project, WorktreeStatus};
use bootty_mux::repository::{RemoteSpaceRef, SpaceMuxOverride, SpaceRemoteOverride};
use bootty_mux::{RepaintHandle, controller::SpaceId};

use crate::{
    action_catalog::Command,
    commands::CommandRegistry,
    remote_catalog::{RemoteCatalogResult, RemoteCatalogTask},
};
use bootty_mux::workspace::{BindingSessionGroup, ScopedSessionTarget};

pub const COMMAND_PALETTE_ID: &str = "command-palette";
pub const DITCH_ID: &str = "ditch-session";
pub const KEYBIND_HELP_ID: &str = "keybind-help";
pub const NEW_SESSION_ID: &str = "new-session";
pub const RENAME_SESSION_ID: &str = "rename-session";
pub const RENAME_TAB_ID: &str = "rename-tab";
pub const SESSION_PICKER_ID: &str = "session-picker";
pub const SPACE_EDITOR_ID: &str = "space-editor";
pub const SPACE_PICKER_ID: &str = "space-picker";
pub const THEME_PICKER_ID: &str = "theme-picker";
pub const TERMINAL_FIND_ID: &str = "terminal-find";

#[derive(Clone, Debug)]
pub enum DialogProjection {
    Dialog(Box<DialogSpec>),
    SpaceEditor(Box<SpaceEditorSnapshot>),
}

pub fn terminal_find_spec(model: &TerminalFindModel) -> DialogSpec {
    let mut spec = DialogSpec::searchable(
        TERMINAL_FIND_ID,
        "Find",
        model.query(),
        vec![
            DialogRow::action("previous", "Previous", DialogAction::new("previous")),
            DialogRow::action("next", "Next", DialogAction::new("next")),
        ],
    );
    spec.role = DialogRole::TerminalFind;
    spec.placement = crate::gpui::DialogPlacement::TopRight;
    spec.icon = Some("search".to_owned());
    spec.footer = Some(model.count_text());
    spec.hint = model.error().map(str::to_owned);
    for (id, label, current) in [
        ("regex", "Regular expression", model.options().regex),
        (
            "case_sensitive",
            "Case sensitive",
            model.options().case_sensitive,
        ),
    ] {
        let mut row = DialogRow::action(id, label, DialogAction::new(id));
        row.current = current;
        spec.rows.push(row);
    }
    spec.text_hint = Some("Find".to_owned());
    spec
}

pub fn apply_terminal_find_intent(
    model: &mut TerminalFindModel,
    intent: &DialogIntent,
) -> Option<TerminalFindOutput> {
    match intent {
        DialogIntent::Dismiss { dialog } if dialog.0 == TERMINAL_FIND_ID => {
            model.apply(TerminalFindIntent::Close)
        }
        DialogIntent::TextChanged { dialog, value } if dialog.0 == TERMINAL_FIND_ID => {
            model.apply(TerminalFindIntent::SetQuery(value.clone()))
        }
        DialogIntent::Find {
            dialog,
            query,
            direction,
        } if dialog.0 == TERMINAL_FIND_ID => {
            let _ = model.apply(TerminalFindIntent::SetQuery(query.clone()));
            let direction = match direction {
                crate::gpui::FindDirection::Current => ProductFindDirection::Current,
                crate::gpui::FindDirection::Previous => ProductFindDirection::Previous,
                crate::gpui::FindDirection::Next => ProductFindDirection::Next,
            };
            match direction {
                ProductFindDirection::Current => {
                    model.apply(TerminalFindIntent::SetQuery(query.clone()))
                }
                ProductFindDirection::Previous => model.apply(TerminalFindIntent::Previous),
                ProductFindDirection::Next => model.apply(TerminalFindIntent::Next),
            }
        }
        DialogIntent::Activate { dialog, action, .. } if dialog.0 == TERMINAL_FIND_ID => {
            match action.0.as_str() {
                "regex" => model.apply(TerminalFindIntent::ToggleRegex),
                "case_sensitive" => model.apply(TerminalFindIntent::ToggleCaseSensitive),
                "previous" => model.apply(TerminalFindIntent::Previous),
                "next" => model.apply(TerminalFindIntent::Next),
                _ => None,
            }
        }
        DialogIntent::FocusTerminal { dialog } if dialog.0 == TERMINAL_FIND_ID => {
            model.apply(TerminalFindIntent::FocusTerminal)
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandPaletteEvent {
    Close,
    Run(Command),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionPickerEvent {
    Close,
    ActivateSession(ScopedSessionTarget),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpacePickerEvent {
    Close,
    Move {
        session: ScopedSessionTarget,
        space: Option<SpaceId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameSessionEvent {
    Close,
    Rename { session_id: String, name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameTabEvent {
    Close,
    Rename {
        session_id: String,
        window_id: String,
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemePickerEvent {
    Close,
    RestorePreview,
    Preview(String),
    Select(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DitchAction {
    DetachWorktree,
    KillOnly,
    RemoveWorktree {
        force: bool,
    },
    RemoveWorktreeAndBranch {
        force: bool,
        branch: String,
        repo: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DitchSessionEvent {
    Close,
    Ditch {
        session_id: String,
        cwd: Option<String>,
        action: DitchAction,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceMoveTarget {
    pub id: SpaceId,
    pub name: String,
    pub icon: String,
    pub reachable: bool,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceDraft {
    pub space_id: Option<SpaceId>,
    pub name: String,
    pub icon: String,
    pub color: [u8; 3],
    pub tint_sidebar: bool,
    pub backend: Option<MultiplexerBackendConfig>,
    pub remote_source: SpaceRemoteOverride,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpaceEditorEvent {
    Close,
    Save(SpaceDraft),
}

#[derive(Default)]
enum RemoteCatalogState {
    #[default]
    Idle,
    Running(RemoteCatalogTask),
    Ready(Vec<RemoteSpaceSummary>, Option<String>),
    Failed(String),
}

pub struct SpaceEditorDialog {
    distributions: Vec<bootty_config::config::WslDistribution>,
    wsl_discovery: Option<crate::remote_catalog::WslDiscoveryTask>,
    wsl_error: Option<String>,
    draft: SpaceDraft,
    profiles: Vec<(String, SshProfileConfig)>,
    catalog: RemoteCatalogState,
    new_remote_space_name: String,
    new_remote_space_backend: MultiplexerBackendConfig,
    icon_search: String,
}

impl SpaceEditorDialog {
    #[must_use]
    pub fn new_space(icon: String, mux: SpaceMuxOverride) -> Self {
        Self::open(
            None,
            String::new(),
            icon,
            bootty_mux::repository::DEFAULT_SPACE_COLOR,
            false,
            mux,
        )
    }

    #[must_use]
    pub fn edit_space(
        space_id: SpaceId,
        name: String,
        icon: String,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> Self {
        Self::open(Some(space_id), name, icon, color, tint_sidebar, mux)
    }

    fn open(
        space_id: Option<SpaceId>,
        name: String,
        icon: String,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> Self {
        Self {
            draft: SpaceDraft {
                space_id,
                name,
                icon,
                color,
                tint_sidebar,
                backend: mux.backend,
                remote_source: mux.remote,
            },
            profiles: Vec::new(),
            distributions: Vec::new(),
            wsl_discovery: None,
            wsl_error: None,
            catalog: RemoteCatalogState::Idle,
            new_remote_space_name: String::new(),
            new_remote_space_backend: MultiplexerBackendConfig::Tmux,
            icon_search: String::new(),
        }
    }

    #[must_use]
    pub fn with_profiles(
        mut self,
        profiles: impl Iterator<Item = (String, SshProfileConfig)>,
    ) -> Self {
        self.profiles = profiles.collect();
        if let Some(profile) = self.selected_profile_id() {
            self.start_catalog(&profile, None);
        }
        self
    }

    #[must_use]
    pub fn with_distributions(
        mut self,
        distributions: Vec<bootty_config::config::WslDistribution>,
    ) -> Self {
        self.distributions = distributions;
        self
    }

    pub(crate) fn discover_wsl(mut self, repaint: RepaintHandle) -> Self {
        if cfg!(windows) {
            self.wsl_discovery = Some(crate::remote_catalog::WslDiscoveryTask::start(repaint));
        }
        self
    }

    const fn is_wsl(&self) -> bool {
        matches!(
            self.draft.remote_source,
            SpaceRemoteOverride::Inline(RemoteConfig::Wsl(_))
        )
    }

    pub fn poll(&mut self) {
        if let Some(result) = self
            .wsl_discovery
            .as_ref()
            .and_then(crate::remote_catalog::WslDiscoveryTask::try_recv)
        {
            match result {
                Ok(distributions) => self.distributions = distributions,
                Err(error) => self.wsl_error = Some(error),
            }
            self.wsl_discovery = None;
        }

        let RemoteCatalogState::Running(task) = &self.catalog else {
            return;
        };
        let Some(result) = task.try_recv() else {
            return;
        };
        let profile_id = task.profile_id.clone();
        match result {
            Ok(result) => self.accept_catalog_result(&profile_id, result),
            Err(error) => self.catalog = RemoteCatalogState::Failed(error),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> SpaceEditorSnapshot {
        let normalized_name = normalized_name(&self.draft.name);
        SpaceEditorSnapshot {
            title: if self.draft.space_id.is_some() {
                "Edit Space"
            } else {
                "New Space"
            }
            .to_owned(),
            name: self.draft.name.clone(),
            name_error: normalized_name
                .is_none()
                .then(|| "name cannot be empty".to_owned()),
            icon_search: self.icon_search.clone(),
            icons: matching_space_icons(&self.icon_search)
                .into_iter()
                .map(|icon| SpaceEditorIcon {
                    id: icon.clone(),
                    glyph: icon.clone(),
                    label: icon.clone(),
                    selected: self.draft.icon == icon,
                })
                .collect(),
            color: self.draft.color,
            tint_sidebar: self.draft.tint_sidebar,
            backends: backend_choices(self.draft.backend)
                .into_iter()
                .filter(|choice| {
                    !self.is_wsl() || matches!(choice.id.as_deref(), Some("rmux" | "tmux"))
                })
                .collect(),
            backend_enabled: !matches!(self.draft.remote_source, SpaceRemoteOverride::Profile(_)),
            locations: self.location_choices(),
            location_notice: if self.is_wsl() {
                Some("Linux files and processes stay in the selected WSL distribution. Choose rmux or tmux.".to_owned())
            } else if matches!(self.draft.remote_source, SpaceRemoteOverride::Inline(_)) {
                Some(
                    "Legacy inline SSH settings are preserved. Select an SSH profile to migrate."
                        .to_owned(),
                )
            } else {
                self.wsl_error.clone()
            },
            remote: self.remote_snapshot(),
            can_save: normalized_name.is_some() && self.remote_ready(),
            colors: SpaceEditorColors::default(),
        }
    }

    pub fn apply(&mut self, intent: SpaceEditorIntent) -> Option<SpaceEditorEvent> {
        match intent {
            SpaceEditorIntent::SetName(name) => self.draft.name = name,
            SpaceEditorIntent::SetIconSearch(search) => self.icon_search = search,
            SpaceEditorIntent::SelectIcon(icon) => self.draft.icon = icon,
            SpaceEditorIntent::SetColor(color) => self.draft.color = color,
            SpaceEditorIntent::SetTintSidebar(tint) => self.draft.tint_sidebar = tint,
            SpaceEditorIntent::SelectBackend(backend) => {
                let backend = backend.as_deref().and_then(parse_backend);
                if !self.is_wsl()
                    || matches!(
                        backend,
                        Some(MultiplexerBackendConfig::Rmux | MultiplexerBackendConfig::Tmux)
                    )
                {
                    self.draft.backend = backend;
                }
            }
            SpaceEditorIntent::SelectLocation(location) => self.select_location(&location),
            SpaceEditorIntent::SelectRemoteSpace(id) => self.select_remote_space(&id),
            SpaceEditorIntent::RetryRemoteSpaces => {
                if let Some(profile) = self.selected_profile_id() {
                    self.start_catalog(&profile, None);
                }
            }
            SpaceEditorIntent::SetNewRemoteSpaceName(name) => {
                self.new_remote_space_name = name;
            }
            SpaceEditorIntent::SelectNewRemoteSpaceBackend(backend) => {
                if let Some(backend) = parse_backend(&backend) {
                    self.new_remote_space_backend = backend;
                }
            }
            SpaceEditorIntent::CreateRemoteSpace { name, backend } => {
                if let (Some(profile), Some(backend)) =
                    (self.selected_profile_id(), parse_backend(&backend))
                {
                    self.start_catalog(&profile, Some((name, backend)));
                }
            }
            SpaceEditorIntent::Save if self.remote_ready() => {
                let name = normalized_name(&self.draft.name)?;
                self.draft.name = name;
                return Some(SpaceEditorEvent::Save(self.draft.clone()));
            }
            SpaceEditorIntent::Close => return Some(SpaceEditorEvent::Close),
            SpaceEditorIntent::Save => {}
        }
        None
    }

    fn selected_profile_id(&self) -> Option<String> {
        match &self.draft.remote_source {
            SpaceRemoteOverride::Profile(remote) => Some(remote.profile_id.clone()),
            _ => None,
        }
    }

    fn location_choices(&self) -> Vec<SpaceEditorChoice> {
        let mut choices = vec![
            SpaceEditorChoice {
                id: "inherit".to_owned(),
                label: "Inherit".to_owned(),
                detail: None,
                selected: matches!(self.draft.remote_source, SpaceRemoteOverride::Inherit),
                enabled: true,
            },
            SpaceEditorChoice {
                id: "local".to_owned(),
                label: "This computer".to_owned(),
                detail: None,
                selected: matches!(self.draft.remote_source, SpaceRemoteOverride::Local),
                enabled: true,
            },
        ];
        choices.extend(self.profiles.iter().map(|(id, profile)| SpaceEditorChoice {
            id: format!("profile:{id}"),
            label: profile.name.clone(),
            detail: None,
            selected: matches!(
                &self.draft.remote_source,
                SpaceRemoteOverride::Profile(remote) if remote.profile_id == *id
            ),
            enabled: true,
        }));
        let mut distributions = self.distributions.clone();
        if let SpaceRemoteOverride::Inline(RemoteConfig::Wsl(remote)) = &self.draft.remote_source
            && !distributions.contains(&remote.distribution)
        {
            distributions.push(remote.distribution.clone());
        }
        choices.extend(distributions.into_iter().map(|distribution| SpaceEditorChoice {
            id: format!("wsl:{}", distribution.as_str()),
            label: format!("WSL: {}", distribution.as_str()),
            detail: Some("Linux distribution".to_owned()),
            selected: matches!(&self.draft.remote_source, SpaceRemoteOverride::Inline(RemoteConfig::Wsl(remote)) if remote.distribution == distribution),
            enabled: true,
        }));
        choices
    }

    fn select_location(&mut self, location: &str) {
        self.draft.remote_source = match location {
            "inherit" => SpaceRemoteOverride::Inherit,
            "local" => SpaceRemoteOverride::Local,
            location if let Some(distribution) = location.strip_prefix("wsl:") => {
                let Ok(distribution) = bootty_config::config::WslDistribution::new(distribution)
                else {
                    return;
                };
                if !matches!(
                    self.draft.backend,
                    Some(MultiplexerBackendConfig::Rmux | MultiplexerBackendConfig::Tmux)
                ) {
                    self.draft.backend = Some(MultiplexerBackendConfig::Rmux);
                }
                SpaceRemoteOverride::Inline(RemoteConfig::Wsl(
                    bootty_config::config::WslRemoteConfig { distribution },
                ))
            }
            location => {
                let Some(profile_id) = location.strip_prefix("profile:") else {
                    return;
                };
                SpaceRemoteOverride::Profile(RemoteSpaceRef {
                    profile_id: profile_id.to_owned(),
                    remote_space_id: String::new(),
                    remote_space_name: String::new(),
                    backend: MultiplexerBackendConfig::Tmux,
                })
            }
        };
        self.catalog = RemoteCatalogState::Idle;
        if let Some(profile) = self.selected_profile_id() {
            self.start_catalog(&profile, None);
        }
    }

    fn select_remote_space(&mut self, id: &str) {
        let RemoteCatalogState::Ready(spaces, _) = &self.catalog else {
            return;
        };
        let Some(space) = spaces.iter().find(|space| space.id == id) else {
            return;
        };
        let Some(profile_id) = self.selected_profile_id() else {
            return;
        };
        self.draft.backend = Some(space.backend);
        self.draft.remote_source = SpaceRemoteOverride::Profile(RemoteSpaceRef {
            profile_id,
            remote_space_id: space.id.clone(),
            remote_space_name: space.name.clone(),
            backend: space.backend,
        });
    }

    fn remote_snapshot(&self) -> RemoteSpaceSnapshot {
        if self.selected_profile_id().is_none() {
            return RemoteSpaceSnapshot::Hidden;
        }
        match &self.catalog {
            RemoteCatalogState::Idle | RemoteCatalogState::Running(_) => {
                RemoteSpaceSnapshot::Loading
            }
            RemoteCatalogState::Failed(message) => RemoteSpaceSnapshot::Failed {
                message: message.clone(),
            },
            RemoteCatalogState::Ready(spaces, warning) => RemoteSpaceSnapshot::Ready {
                spaces: spaces
                    .iter()
                    .map(|space| SpaceEditorChoice {
                        id: space.id.clone(),
                        label: space.name.clone(),
                        detail: Some(backend_label(Some(space.backend)).to_owned()),
                        selected: matches!(
                            &self.draft.remote_source,
                            SpaceRemoteOverride::Profile(remote)
                                if remote.remote_space_id == space.id
                        ),
                        enabled: true,
                    })
                    .collect(),
                warning: warning.clone(),
                new_name: self.new_remote_space_name.clone(),
                create_backends: [
                    MultiplexerBackendConfig::Rmux,
                    MultiplexerBackendConfig::Tmux,
                ]
                .into_iter()
                .map(|backend| SpaceEditorChoice {
                    id: backend_id(backend).to_owned(),
                    label: backend_label(Some(backend)).to_owned(),
                    detail: None,
                    selected: backend == self.new_remote_space_backend,
                    enabled: true,
                })
                .collect(),
                can_create: !self.new_remote_space_name.trim().is_empty(),
            },
        }
    }

    const fn remote_ready(&self) -> bool {
        if self.is_wsl() {
            return matches!(
                self.draft.backend,
                Some(MultiplexerBackendConfig::Rmux | MultiplexerBackendConfig::Tmux)
            );
        }

        !matches!(
            &self.draft.remote_source,
            SpaceRemoteOverride::Profile(remote) if remote.remote_space_id.is_empty()
        )
    }

    fn start_catalog(
        &mut self,
        profile_id: &str,
        create: Option<(String, MultiplexerBackendConfig)>,
    ) {
        let Some(profile) = self
            .profiles
            .iter()
            .find(|(id, _)| id == profile_id)
            .map(|(_, profile)| profile.clone())
        else {
            self.catalog =
                RemoteCatalogState::Failed(format!("SSH profile '{profile_id}' is unavailable"));
            return;
        };
        self.catalog = match RemoteCatalogTask::start(profile_id.to_owned(), profile, create) {
            Ok(task) => RemoteCatalogState::Running(task),
            Err(error) => RemoteCatalogState::Failed(error),
        };
    }

    fn accept_catalog_result(&mut self, profile_id: &str, result: RemoteCatalogResult) {
        if self.selected_profile_id().as_deref() != Some(profile_id) {
            self.catalog = RemoteCatalogState::Idle;
            return;
        }
        let (spaces, warning) = match result {
            RemoteCatalogResult::Listed(spaces) => (spaces, None),
            RemoteCatalogResult::Created {
                selected,
                refreshed,
            } => {
                self.draft.backend = Some(selected.backend);
                self.draft.remote_source = SpaceRemoteOverride::Profile(RemoteSpaceRef {
                    profile_id: profile_id.to_owned(),
                    remote_space_id: selected.id.clone(),
                    remote_space_name: selected.name.clone(),
                    backend: selected.backend,
                });
                self.new_remote_space_name.clear();
                match refreshed {
                    Ok(spaces) => (spaces, None),
                    Err(error) => (
                        vec![selected],
                        Some(format!(
                            "Remote Space was created, but refresh failed: {error}"
                        )),
                    ),
                }
            }
        };
        self.catalog = RemoteCatalogState::Ready(spaces, warning);
    }
}

pub struct CommandPaletteDialog {
    localizer: crate::i18n::Localizer,
    list: SearchableList<usize>,
    commands: Vec<Command>,
    current: CommandPaletteState,
}

/// Synchronous application facts that the command palette can show as checked.
///
/// This is intentionally limited to values owned by the app state and available without querying
/// a native window or mux backend. A toggle whose current value is not authoritative here stays
/// unchecked rather than displaying a guess.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandPaletteState {
    pub appearance_mode: AppearanceMode,
    pub sidebar_visible: bool,
}

impl CommandPaletteDialog {
    /// Open the palette with bundled English labels.
    ///
    /// # Errors
    /// Returns an error if the bundled translation catalog cannot be loaded.
    pub fn open(keybinds: &[String], current: CommandPaletteState) -> anyhow::Result<Self> {
        Ok(Self::open_localized(
            keybinds,
            current,
            &crate::i18n::Localizer::new("en")?,
        ))
    }

    #[must_use]
    pub fn open_localized(
        keybinds: &[String],
        current: CommandPaletteState,
        localizer: &crate::i18n::Localizer,
    ) -> Self {
        let bindings = keybind_map(keybinds);
        let mut commands = CommandRegistry::core()
            .palette_commands()
            .collect::<Vec<_>>();
        // Keep the source list in the same fixed category order as the projected groups. This
        // lets the shared Command index paths continue to map directly back to `self.commands`.
        commands.sort_by_key(|command| command.category().rank());
        let entries = commands
            .iter()
            .enumerate()
            .map(|(index, command)| {
                let mut entry = SearchableEntry::new(
                    index,
                    localizer.text(
                        &crate::i18n::presentation_key("command", command.action(), "title"),
                        command.title(),
                    ),
                );
                entry.secondary = Some(localizer.text(
                    &crate::i18n::presentation_key("command", command.action(), "description"),
                    command.description(),
                ));
                entry.keywords.push(command.title().to_owned());
                entry.trailing = command
                    .palette_action()
                    .and_then(|action| bindings.get(action).cloned());
                entry.keywords.push(command.action().to_owned());
                entry
            })
            .collect();
        Self {
            list: SearchableList::new(entries),
            commands,
            current,
            localizer: localizer.clone(),
        }
    }

    #[must_use]
    pub fn current_action(&self) -> Option<&'static str> {
        self.list
            .selected_value()
            .and_then(|index| self.commands.get(*index))
            .map(|command| command.action())
    }

    pub fn spec(&self) -> DialogSpec {
        let mut rows = Vec::new();
        let mut category = None;
        let mut shown = 0_usize;
        for (visible, row) in self.list.rows().into_iter().enumerate() {
            let Some(&command) = self.commands.get(row.source_index) else {
                continue;
            };
            let next_category = command.category().label();
            if category != Some(next_category) {
                rows.push(DialogRow::section(
                    format!("section-{next_category}"),
                    self.localizer.text(
                        &crate::i18n::presentation_key("command-category", next_category, "title"),
                        next_category,
                    ),
                ));
                category = Some(next_category);
            }
            rows.push(DialogRow {
                id: RowId::new(visible.to_string()),
                icon: Some(command.icon().to_owned()),
                label: row.primary.to_owned(),
                color: None,
                detail: row.secondary.map(str::to_owned),
                trailing: None,
                keybinding: row.trailing.map(str::to_owned),
                current: self.current.is_current(command),
                enabled: true,
                destructive: false,
                action: Some(DialogAction::new("run").with_payload(row.source_index.to_string())),
                preview: None,
            });
            shown = shown.saturating_add(1);
        }
        let mut spec = DialogSpec::searchable(
            COMMAND_PALETTE_ID,
            self.localizer.message("palette-title", None),
            self.list.filter(),
            rows,
        );
        spec.icon = Some("search".to_owned());
        spec.hint = Some(self.localizer.message("palette-hint", None));
        spec.footer = Some({
            let mut args = fluent_bundle::FluentArgs::new();
            args.set("shown", shown);
            args.set("total", self.list.total());
            self.localizer.message("palette-count", Some(&args))
        });
        spec.text_hint = Some(self.localizer.message("palette-search", None));
        spec.empty_text = self.localizer.message("palette-empty", None);
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<CommandPaletteEvent> {
        if intent.dialog_id().0 != COMMAND_PALETTE_ID {
            return None;
        }
        match intent {
            DialogIntent::Dismiss { .. } => Some(CommandPaletteEvent::Close),
            DialogIntent::TextChanged { value, .. } => {
                self.list.apply(SearchableIntent::SetFilter(value.clone()));
                None
            }
            DialogIntent::SelectionChanged { row, .. } => {
                if let Some(index) = parse_index(row) {
                    self.list.apply(SearchableIntent::Select(index));
                }
                None
            }
            DialogIntent::Activate { payload, .. } => payload_index(payload)
                .and_then(|index| self.commands.get(index).copied())
                .map(CommandPaletteEvent::Run),
            _ => None,
        }
    }
}

impl CommandPaletteState {
    fn is_current(self, command: Command) -> bool {
        match command {
            Command::UseSystemAppearance => self.appearance_mode == AppearanceMode::System,
            Command::UseLightAppearance => self.appearance_mode == AppearanceMode::Light,
            Command::UseDarkAppearance => self.appearance_mode == AppearanceMode::Dark,
            Command::ToggleSidebar => self.sidebar_visible,
            _ => false,
        }
    }
}

pub struct SessionPickerDialog {
    list: SearchableList<ScopedSessionTarget>,
}

impl SessionPickerDialog {
    #[must_use]
    pub fn open() -> Self {
        Self {
            list: SearchableList::new(Vec::new()),
        }
    }

    pub fn update_groups(&mut self, groups: &[BindingSessionGroup]) {
        let entries = groups
            .iter()
            .flat_map(|group| {
                group.sessions.iter().map(|session| {
                    let mut entry =
                        SearchableEntry::new(group.target(session), group.display_name(session));
                    entry.secondary = Some(group.label.clone());
                    entry.trailing.clone_from(&session.anchor.process);
                    entry.keywords.push(session.name.clone());
                    entry
                })
            })
            .collect();
        self.list.replace_entries(entries);
    }

    #[must_use]
    pub fn spec(&self, groups: &[BindingSessionGroup]) -> DialogSpec {
        let colors = groups
            .iter()
            .flat_map(|group| {
                let names = group
                    .sessions
                    .iter()
                    .map(|session| group.display_name(session).to_owned())
                    .collect::<Vec<_>>();
                group
                    .sessions
                    .iter()
                    .zip(crate::chrome_frame::session_colors(&group.sessions, &names))
                    .map(|(session, (color, _))| (group.target(session), color))
            })
            .collect::<HashMap<_, _>>();
        let rows = self
            .list
            .rows()
            .into_iter()
            .enumerate()
            .map(|(visible, row)| {
                let target = row.value;
                let current = groups.iter().any(|group| {
                    group.scope == target.scope
                        && group.sessions.iter().any(|session| {
                            session.id == target.session_id && group.session_is_current(session)
                        })
                });
                DialogRow {
                    id: RowId::new(visible.to_string()),
                    icon: Some("terminal".to_owned()),
                    label: row.primary.to_owned(),
                    color: colors
                        .get(target)
                        .and_then(|color| gpui_kit::Hsla::parse_hex(color).ok()),
                    detail: row.secondary.map(str::to_owned),
                    trailing: row.trailing.map(str::to_owned),
                    keybinding: None,
                    current,
                    enabled: true,
                    destructive: false,
                    action: Some(DialogAction {
                        id: crate::gpui::ActionId::new("activate"),
                        payload: DialogPayload::Session(target.clone()),
                    }),
                    preview: None,
                }
            })
            .collect::<Vec<_>>();
        let shown = rows.len();
        let total = groups
            .iter()
            .map(|group| group.sessions.len())
            .sum::<usize>();
        let mut spec = DialogSpec::searchable(
            SESSION_PICKER_ID,
            "Session Finder",
            self.list.filter(),
            rows,
        );
        spec.icon = Some("terminal".to_owned());
        spec.footer = Some(format!("{shown} / {total} sessions"));
        spec.text_hint = Some("filter sessions or bindings…".to_owned());
        "no matching sessions".clone_into(&mut spec.empty_text);
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<SessionPickerEvent> {
        if intent.dialog_id().0 != SESSION_PICKER_ID {
            return None;
        }
        match intent {
            DialogIntent::Dismiss { .. } => Some(SessionPickerEvent::Close),
            DialogIntent::TextChanged { value, .. } => {
                self.list.apply(SearchableIntent::SetFilter(value.clone()));
                None
            }
            DialogIntent::Activate {
                payload: DialogPayload::Session(target),
                ..
            } => Some(SessionPickerEvent::ActivateSession(target.clone())),
            _ => None,
        }
    }
}

pub struct SpacePickerDialog {
    session: ScopedSessionTarget,
    session_name: String,
    list: SearchableList<Option<SpaceId>>,
}

impl SpacePickerDialog {
    #[must_use]
    pub fn open(
        session: ScopedSessionTarget,
        session_name: String,
        spaces: Vec<SpaceMoveTarget>,
    ) -> Self {
        let mut entries = spaces
            .into_iter()
            .filter(|space| !space.current)
            .map(|space| {
                let mut entry = SearchableEntry::new(Some(space.id), space.name);
                entry.secondary =
                    (!space.reachable).then(|| "runs on another multiplexer".to_owned());
                entry.keywords.push(space.icon);
                entry.enabled = space.reachable;
                entry
            })
            .collect::<Vec<_>>();
        let mut unassign = SearchableEntry::new(None, "Nothing");
        unassign.secondary = Some("leave it running, claimed by no Space".to_owned());
        unassign.keywords.push("unassign".to_owned());
        entries.push(unassign);
        Self {
            session,
            session_name,
            list: SearchableList::new(entries),
        }
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let rows = self
            .list
            .rows()
            .into_iter()
            .enumerate()
            .map(|(visible, row)| DialogRow {
                id: RowId::new(visible.to_string()),
                icon: Some(if row.value.is_some() {
                    "shapes".to_owned()
                } else {
                    "circle-dashed".to_owned()
                }),
                label: row.primary.to_owned(),
                detail: row.secondary.map(str::to_owned),
                trailing: None,
                keybinding: None,
                color: None,
                current: false,
                enabled: row.enabled,
                destructive: false,
                action: row.enabled.then(|| DialogAction::new("move")),
                preview: None,
            })
            .collect();
        let mut spec = DialogSpec::searchable(
            SPACE_PICKER_ID,
            "Move Session to Space",
            self.list.filter(),
            rows,
        );
        spec.icon = Some("shapes".to_owned());
        spec.footer = Some(self.session_name.clone());
        spec.text_hint = Some("filter spaces…".to_owned());
        "no matching spaces".clone_into(&mut spec.empty_text);
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<SpacePickerEvent> {
        if intent.dialog_id().0 != SPACE_PICKER_ID {
            return None;
        }
        match intent {
            DialogIntent::Dismiss { .. } => Some(SpacePickerEvent::Close),
            DialogIntent::TextChanged { value, .. } => {
                self.list.apply(SearchableIntent::SetFilter(value.clone()));
                None
            }
            DialogIntent::SelectionChanged { row, .. } => {
                if let Some(index) = parse_index(row) {
                    self.list.apply(SearchableIntent::Select(index));
                }
                None
            }
            DialogIntent::Activate { row, .. } => parse_index(row).and_then(|index| {
                self.list.apply(SearchableIntent::Select(index));
                self.list
                    .selected_value()
                    .copied()
                    .map(|space| SpacePickerEvent::Move {
                        session: self.session.clone(),
                        space,
                    })
            }),
            _ => None,
        }
    }
}

pub struct DitchSessionDialog {
    session_id: String,
    cwd: Option<String>,
    status: WorktreeStatus,
    list: SearchableList<DitchAction>,
    inspection: Option<std::sync::mpsc::Receiver<Self>>,
    remote: bool,
}

impl DitchSessionDialog {
    pub fn open(session_id: String, cwd: Option<String>) -> Self {
        let status = cwd.as_deref().map(project::status).unwrap_or_default();
        let main = cwd.as_deref().and_then(project::main_worktree);
        let trunk = cwd.as_deref().and_then(project::trunk_branch);
        let multi_worktree = cwd.as_deref().map_or(0, project::worktree_count) > 1;
        let actions = ditch_actions(&status, main.as_deref(), trunk.as_deref(), multi_worktree);
        let entries = actions
            .into_iter()
            .map(|action| {
                let (label, detail) = ditch_action_text(&action);
                let mut entry = SearchableEntry::new(action, label);
                entry.secondary = Some(detail);
                entry
            })
            .collect();
        Self {
            session_id,
            cwd,
            status,
            list: SearchableList::new(entries),
            inspection: None,
            remote: false,
        }
    }

    pub fn open_with_repaint(
        session_id: String,
        cwd: Option<String>,
        repaint: RepaintHandle,
    ) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut dialog = Self::open(session_id.clone(), None);
        dialog.cwd.clone_from(&cwd);
        dialog.inspection = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(Self::open(session_id, cwd));
            repaint();
        });
        dialog
    }

    #[must_use]
    pub fn open_remote(session_id: String, cwd: Option<String>) -> Self {
        // Remote Git cleanup needs a remote execution contract; never inspect a remote path locally.
        let mut dialog = Self::open(session_id, None);
        dialog.cwd = cwd;
        dialog.remote = true;
        dialog
    }

    pub fn poll(&mut self) {
        if let Some(inspection) = &self.inspection {
            match inspection.try_recv() {
                Ok(dialog) => *self = dialog,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.inspection = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let path = self.cwd.as_deref().map_or_else(
            || "(unknown)".to_owned(),
            |cwd| bootty_git::project::display_path(cwd, home_dir().as_deref()),
        );
        let mut rows = vec![DialogRow::section("path", format!("path: {path}"))];
        rows.push(DialogRow::section(
            "git",
            if self.remote {
                "Remote session: worktree and branch are kept".to_owned()
            } else if self.inspection.is_some() {
                "Inspecting worktree…".to_owned()
            } else if !self.status.in_repo {
                "git: not a git repository".to_owned()
            } else {
                format!(
                    "branch: {} · {} · {}",
                    self.status.branch.as_deref().unwrap_or("detached"),
                    if self.status.is_linked_worktree {
                        "linked worktree"
                    } else {
                        "main worktree"
                    },
                    if self.status.dirty { "dirty" } else { "clean" },
                )
            },
        ));
        rows.extend(self.list.rows().into_iter().map(|row| DialogRow {
            id: RowId::new(row.source_index.to_string()),
            icon: Some(if matches!(row.value, DitchAction::DetachWorktree) {
                "unlink".to_owned()
            } else if matches!(row.value, DitchAction::KillOnly) {
                "x".to_owned()
            } else {
                "trash-2".to_owned()
            }),
            label: row.primary.to_owned(),
            color: None,
            detail: row.secondary.map(str::to_owned),
            trailing: None,
            keybinding: None,
            current: false,
            enabled: self.inspection.is_none(),
            destructive: !matches!(
                row.value,
                DitchAction::DetachWorktree | DitchAction::KillOnly
            ),
            action: Some(DialogAction::new("ditch").with_payload(row.source_index.to_string())),
            preview: None,
        }));
        DialogSpec {
            id: DialogId::new(DITCH_ID),
            role: DialogRole::SearchableList,
            title: format!("Ditch session {}?", self.session_id),
            icon: Some("trash-2".to_owned()),
            hint: Some("↑ ↓ navigate   Enter confirm   Esc cancel".to_owned()),
            footer: None,
            text: None,
            text_label: None,
            fields: Vec::new(),
            busy: false,
            text_hint: None,
            rows,
            empty_text: String::new(),
            placement: crate::gpui::DialogPlacement::Center,
        }
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<DitchSessionEvent> {
        if intent.dialog_id().0 != DITCH_ID {
            return None;
        }
        match intent {
            DialogIntent::Dismiss { .. } => Some(DitchSessionEvent::Close),
            DialogIntent::Activate { payload, .. } if self.inspection.is_none() => {
                payload_index(payload).and_then(|index| {
                    self.list
                        .rows()
                        .get(index)
                        .map(|row| DitchSessionEvent::Ditch {
                            session_id: self.session_id.clone(),
                            cwd: self.cwd.clone(),
                            action: row.value.clone(),
                        })
                })
            }
            _ => None,
        }
    }
}

pub struct KeybindHelpDialog {
    model: KeybindHelpModel,
}

impl KeybindHelpDialog {
    #[must_use]
    pub fn open(bindings: &[String]) -> Self {
        Self {
            model: KeybindHelpModel::from_raw(bindings),
        }
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let rows = self
            .model
            .rows()
            .into_iter()
            .enumerate()
            .map(|(index, row)| DialogRow {
                id: RowId::new(index.to_string()),
                icon: None,
                label: row.action,
                color: None,
                detail: None,
                trailing: None,
                keybinding: Some(row.chord),
                current: false,
                enabled: true,
                destructive: false,
                action: None,
                preview: None,
            })
            .collect::<Vec<_>>();
        let shown = rows.len();
        let mut spec =
            DialogSpec::searchable(KEYBIND_HELP_ID, "Keybindings", self.model.filter(), rows);
        spec.icon = Some("keyboard".to_owned());
        spec.hint = Some("Esc close".to_owned());
        spec.footer = Some(format!("{shown} / {} bindings", self.model.total()));
        spec.text_hint = Some("filter keybindings…".to_owned());
        "no matching keybindings".clone_into(&mut spec.empty_text);
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> bool {
        if intent.dialog_id().0 != KEYBIND_HELP_ID {
            return false;
        }
        match intent {
            DialogIntent::Dismiss { .. } => true,
            DialogIntent::TextChanged { value, .. } => {
                self.model.apply(SearchableIntent::SetFilter(value.clone()));
                false
            }
            DialogIntent::SelectionChanged { row, .. } => {
                if let Some(index) = parse_index(row) {
                    self.model.apply(SearchableIntent::Select(index));
                }
                false
            }
            _ => false,
        }
    }
}

pub struct RenameSessionDialog {
    session_id: String,
    name: String,
}

impl RenameSessionDialog {
    #[must_use]
    pub const fn open(session_id: String, name: String) -> Self {
        Self { session_id, name }
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let mut spec = DialogSpec::prompt(
            RENAME_SESSION_ID,
            "Rename Session",
            &self.name,
            "new session name…",
            DialogAction::new("submit"),
        );
        spec.icon = Some("square-pen".to_owned());
        if self.name.trim().is_empty()
            && let Some(row) = spec.rows.first_mut()
        {
            row.enabled = false;
            row.detail = Some("name cannot be empty".to_owned());
        }
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<RenameSessionEvent> {
        if intent.dialog_id().0 != RENAME_SESSION_ID {
            return None;
        }
        match intent {
            DialogIntent::Dismiss { .. } => Some(RenameSessionEvent::Close),
            DialogIntent::TextChanged { value, .. } => {
                self.name.clone_from(value);
                None
            }
            DialogIntent::Activate { .. } if !self.name.trim().is_empty() => {
                Some(RenameSessionEvent::Rename {
                    session_id: self.session_id.clone(),
                    name: self.name.trim().to_owned(),
                })
            }
            _ => None,
        }
    }
}

pub struct RenameTabDialog {
    session_id: String,
    window_id: String,
    name: String,
}

impl RenameTabDialog {
    #[must_use]
    pub const fn open(session_id: String, window_id: String, name: String) -> Self {
        Self {
            session_id,
            window_id,
            name,
        }
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let mut spec = DialogSpec::prompt(
            RENAME_TAB_ID,
            "Rename Tab",
            &self.name,
            "new tab name…",
            DialogAction::new("submit"),
        );
        spec.icon = Some("square-pen".to_owned());
        spec.footer = Some("Clear the field to follow terminal title codes again".to_owned());
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<RenameTabEvent> {
        if intent.dialog_id().0 != RENAME_TAB_ID {
            return None;
        }
        match intent {
            DialogIntent::Dismiss { .. } => Some(RenameTabEvent::Close),
            DialogIntent::TextChanged { value, .. } => {
                self.name.clone_from(value);
                None
            }
            DialogIntent::Activate { .. } => Some(RenameTabEvent::Rename {
                session_id: self.session_id.clone(),
                window_id: self.window_id.clone(),
                name: self.name.trim().to_owned(),
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeKind {
    Light,
    Dark,
}

impl ThemeKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

pub struct ThemePickerDialog {
    entries: Vec<(String, ThemeKind)>,
    filter: String,
    scope: Option<ThemeKind>,
    current: Option<String>,
    branch_label: String,
    last_preview: Option<String>,
}

impl ThemePickerDialog {
    #[must_use]
    pub fn open(names: Vec<String>, current: Option<String>, branch_label: String) -> Self {
        Self {
            entries: names
                .into_iter()
                .map(|name| {
                    let kind = classify_theme(&name);
                    (name, kind)
                })
                .collect(),
            filter: String::new(),
            scope: None,
            current,
            branch_label,
            last_preview: None,
        }
    }

    pub fn spec(&self) -> DialogSpec {
        let mut rows = Vec::new();
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            if self.scope.is_some_and(|scope| scope != kind) {
                continue;
            }
            let mut matches = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, (_, entry_kind))| *entry_kind == kind)
                .filter_map(|(index, (name, _))| {
                    crate::product_dialogs::searchable::fuzzy_match(name, &self.filter)
                        .map(|matched| (index, matched.score, name))
                })
                .collect::<Vec<_>>();
            matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            if matches.is_empty() {
                continue;
            }
            rows.push(DialogRow::section(
                rows.len().to_string(),
                format!("{} themes", kind.label()),
            ));
            for (_, _, name) in matches {
                let mut row = DialogRow::action(
                    rows.len().to_string(),
                    name,
                    DialogAction::new("select").with_payload(name),
                );
                row.icon = Some("palette".to_owned());
                row.detail = Some(format!("{} · {}", self.branch_label, kind.label()));
                row.current = self.current.as_deref() == Some(name.as_str());
                row.preview = Some(DialogAction::new("preview").with_payload(name));
                rows.push(row);
            }
        }
        let mut spec = DialogSpec::searchable(THEME_PICKER_ID, "Switch Theme", &self.filter, rows);
        spec.role = DialogRole::ThemePicker;
        spec.icon = Some("palette".to_owned());
        spec.hint = Some("Tab all/light/dark   Enter select   Esc close".to_owned());
        spec.footer = Some(format!(
            "{} themes · {}",
            self.entries.len(),
            self.scope.map_or("All", ThemeKind::label)
        ));
        spec.text_hint = Some("filter themes…".to_owned());
        "no matching themes".clone_into(&mut spec.empty_text);
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<ThemePickerEvent> {
        if intent.dialog_id().0 != THEME_PICKER_ID {
            return None;
        }
        match intent {
            DialogIntent::Dismiss { .. } => Some(ThemePickerEvent::Close),
            DialogIntent::TextChanged { value, .. } => {
                self.filter.clone_from(value);
                None
            }
            DialogIntent::CycleScope { .. } => {
                self.scope = match self.scope {
                    None => Some(ThemeKind::Light),
                    Some(ThemeKind::Light) => Some(ThemeKind::Dark),
                    Some(ThemeKind::Dark) => None,
                };
                self.last_preview = None;
                None
            }
            DialogIntent::Preview {
                payload: DialogPayload::Text(name),
                ..
            } if self.entries.iter().any(|(entry, _)| entry == name) => {
                if self.current.as_ref() == Some(name) {
                    self.last_preview
                        .take()
                        .map(|_| ThemePickerEvent::RestorePreview)
                } else if self.last_preview.as_ref() == Some(name) {
                    None
                } else {
                    self.last_preview = Some(name.clone());
                    Some(ThemePickerEvent::Preview(name.clone()))
                }
            }
            DialogIntent::Activate {
                payload: DialogPayload::Text(name),
                ..
            } if self.entries.iter().any(|(entry, _)| entry == name) => {
                Some(ThemePickerEvent::Select(name.clone()))
            }
            _ => None,
        }
    }
}

fn keybind_map(bindings: &[String]) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for (chord, action) in bindings
        .iter()
        .filter_map(|raw| crate::product_dialogs::keybind_help::parse_keybind(raw))
    {
        // Config order is precedence order in the legacy resolver: the first configured binding
        // is the visible/default accelerator for a command.
        result.entry(action).or_insert(chord);
    }
    result
}

fn parse_index(row: &RowId) -> Option<usize> {
    row.0.parse().ok()
}

fn payload_index(payload: &DialogPayload) -> Option<usize> {
    match payload {
        DialogPayload::Text(value) => value.parse().ok(),
        DialogPayload::None | DialogPayload::Session(_) => None,
    }
}

fn classify_theme(name: &str) -> ThemeKind {
    let name = name.to_ascii_lowercase();
    if ["light", "latte", "day", "dawn", "lotus", "solarized light"]
        .iter()
        .any(|needle| name.contains(needle))
    {
        ThemeKind::Light
    } else {
        ThemeKind::Dark
    }
}

fn ditch_actions(
    status: &WorktreeStatus,
    main: Option<&str>,
    trunk: Option<&str>,
    multi_worktree: bool,
) -> Vec<DitchAction> {
    let mut actions = Vec::new();
    if multi_worktree && status.branch.is_some() {
        actions.push(DitchAction::DetachWorktree);
    }
    actions.push(DitchAction::KillOnly);
    if status.is_linked_worktree {
        actions.push(DitchAction::RemoveWorktree {
            force: status.dirty,
        });
        if let (Some(branch), Some(repo)) = (&status.branch, main)
            && trunk != Some(branch.as_str())
        {
            actions.push(DitchAction::RemoveWorktreeAndBranch {
                force: true,
                branch: branch.clone(),
                repo: repo.to_owned(),
            });
        }
    }
    actions
}

fn ditch_action_text(action: &DitchAction) -> (&'static str, String) {
    match action {
        DitchAction::DetachWorktree => (
            "Detach worktree",
            "Detach HEAD to free the branch; keep the worktree, branch, and commits".to_owned(),
        ),
        DitchAction::KillOnly => (
            "Kill session",
            "Close the session; keep the worktree and branch".to_owned(),
        ),
        DitchAction::RemoveWorktree { force } => (
            "Kill + remove worktree",
            if *force {
                "Discard uncommitted changes and remove the linked worktree".to_owned()
            } else {
                "Remove the linked worktree".to_owned()
            },
        ),
        DitchAction::RemoveWorktreeAndBranch { branch, .. } => (
            "Kill + remove worktree + delete branch",
            format!(
                "Remove the worktree and delete branch '{branch}' (uncommitted changes and unmerged commits are lost)"
            ),
        ),
    }
}

fn backend_choices(selected: Option<MultiplexerBackendConfig>) -> Vec<OptionalSpaceEditorChoice> {
    std::iter::once((None, "Inherit"))
        .chain(
            [
                MultiplexerBackendConfig::Herdr,
                MultiplexerBackendConfig::Native,
                MultiplexerBackendConfig::Rmux,
                MultiplexerBackendConfig::Tmux,
            ]
            .into_iter()
            .map(|backend| (Some(backend), backend_label(Some(backend)))),
        )
        .map(|(backend, label)| OptionalSpaceEditorChoice {
            id: backend.map(|backend| backend_id(backend).to_owned()),
            label: label.to_owned(),
            selected: selected == backend,
            enabled: true,
        })
        .collect()
}

const fn backend_id(backend: MultiplexerBackendConfig) -> &'static str {
    match backend {
        MultiplexerBackendConfig::Herdr => "herdr",
        MultiplexerBackendConfig::Native => "native",
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
    }
}

fn parse_backend(backend: &str) -> Option<MultiplexerBackendConfig> {
    match backend {
        "herdr" => Some(MultiplexerBackendConfig::Herdr),
        "native" => Some(MultiplexerBackendConfig::Native),
        "rmux" => Some(MultiplexerBackendConfig::Rmux),
        "tmux" => Some(MultiplexerBackendConfig::Tmux),
        _ => None,
    }
}

const fn backend_label(backend: Option<MultiplexerBackendConfig>) -> &'static str {
    match backend {
        None => "Inherit",
        Some(MultiplexerBackendConfig::Herdr) => "Herdr",
        Some(MultiplexerBackendConfig::Native) => "Native",
        Some(MultiplexerBackendConfig::Rmux) => "Rmux",
        Some(MultiplexerBackendConfig::Tmux) => "Tmux",
    }
}

fn normalized_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn space_icon_inventory() -> &'static [String] {
    use std::sync::LazyLock;

    static ICONS: LazyLock<Vec<String>> = LazyLock::new(|| {
        use iconflow::{Pack, Size, Style, list, try_icon};

        list(Pack::Phosphor)
            .iter()
            .filter(|icon| try_icon(Pack::Phosphor, icon, Style::Regular, Size::Regular).is_ok())
            .map(|icon| format!("phosphor:{icon}"))
            .chain(list(Pack::Lucide).iter().map(|icon| (*icon).to_owned()))
            .filter(|icon| crate::gpui::has_icon(icon))
            .collect()
    });
    &ICONS
}

#[must_use]
pub fn default_space_icon(existing: &[String]) -> String {
    space_icon_inventory()
        .iter()
        .find(|icon| !existing.iter().any(|used| used == *icon))
        .or_else(|| space_icon_inventory().first())
        .cloned()
        .unwrap_or_else(|| "folder".to_owned())
}

fn matching_space_icons(query: &str) -> Vec<String> {
    let query = query.to_ascii_lowercase();
    space_icon_inventory()
        .iter()
        .filter(|icon| icon.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect()
}
