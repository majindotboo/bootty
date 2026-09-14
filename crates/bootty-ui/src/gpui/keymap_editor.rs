//! Dedicated, host-neutral GPUI keymap editor.
//!
//! The host projects the effective keymap and the complete command catalog into a disposable
//! snapshot. This view owns only transient search, filter, selection, and modal state. Durable
//! edits leave through [`KeymapEditorIntent`]; this crate never reads or writes `keymap.json`.

use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};

use gpui_kit::component::{
    Icon, IconName, Selectable as _, Sizable as _, button::Button, label::Label,
};

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
};

use bootty_config::keymap_file::KeymapBindingKind;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::{
    IndexPath, Size, WindowExt as _,
    button::{Button as ComponentButton, ButtonGroup},
    combobox::{Combobox, ComboboxEvent, ComboboxState},
    input::{Editor as ComponentEditor, EditorState, Input, InputEvent, InputState},
    searchable_list::{SearchableListDelegate, SearchableListItem},
};
use gpui_kit::component::{
    alert::{Alert, AlertVariant},
    table::{Column, DataTable, TableDelegate, TableState},
};
use gpui_kit::{
    AnyElement, App, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, IntoElement, IsZero as _, KeyContext, Keystroke, MouseButton, ParentElement,
    Pixels, Point, Render, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, anchored, deferred, div, prelude::*, px, rems,
};

use crate::product_dialogs::searchable::fuzzy_match;

const COLUMN_COUNT: usize = 6;
const MAX_ACTION_COMPLETIONS: usize = 50;
const NO_ACTION_ARGUMENTS_TEXT: &str = "<no arguments>";

#[derive(Clone, Debug)]
struct ActionComboboxDelegate {
    actions: Vec<KeymapActionSnapshot>,
    matches: Vec<KeymapActionSnapshot>,
    preferred_action: Option<String>,
}

impl ActionComboboxDelegate {
    fn new(actions: Vec<KeymapActionSnapshot>, preferred_action: Option<String>) -> Self {
        let mut this = Self {
            actions,
            matches: Vec::new(),
            preferred_action,
        };
        this.update_matches("");
        this
    }

    fn update_matches(&mut self, query: &str) {
        let mut indices = ranked_action_indices(&self.actions, query);
        indices.truncate(MAX_ACTION_COMPLETIONS);

        if normalize_action_query(query).is_empty()
            && let Some(preferred) = self.preferred_action.as_deref()
            && let Some(preferred_index) = self
                .actions
                .iter()
                .position(|action| action.id == preferred)
            && !indices.contains(&preferred_index)
        {
            _ = indices.pop();
            indices.insert(0, preferred_index);
        }

        self.matches = indices
            .into_iter()
            .filter_map(|index| self.actions.get(index))
            .cloned()
            .collect();
    }
}

impl SearchableListDelegate for ActionComboboxDelegate {
    type Item = KeymapActionSnapshot;

    fn items_count(&self, _: usize) -> usize {
        self.matches.len()
    }

    fn item(&self, ix: IndexPath) -> Option<&Self::Item> {
        self.matches.get(ix.row)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.matches
            .iter()
            .position(|item| item.value() == value)
            .map(IndexPath::new)
    }

    fn perform_search(&mut self, query: &str, _: &mut Window, _: &mut App) -> Task<()> {
        self.update_matches(query);
        Task::ready(())
    }
}

/// Where a projected binding came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeymapBindingSource {
    Default,
    User,
}

impl KeymapBindingSource {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::User => "User",
        }
    }
}

/// The small value vocabulary used by Bootty command argument schemas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapArgumentKind {
    String,
    Integer,
    Number,
}

impl KeymapArgumentKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
        }
    }
}

/// One named argument accepted by an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapArgumentSnapshot {
    pub name: String,
    pub kind: KeymapArgumentKind,
    pub required: bool,
    pub choices: Vec<String>,
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
}

/// One action in the complete command catalog, including actions with no binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapActionSnapshot {
    pub id: String,
    pub title: String,
    pub description: String,
    pub arguments: Vec<KeymapArgumentSnapshot>,
}

impl SearchableListItem for KeymapActionSnapshot {
    type Value = String;

    fn title(&self) -> SharedString {
        self.title.clone().into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let selector = format!("keymap-action-option-{}", self.id);
        let debug_selector = selector.clone();
        gpui_kit::div()
            .flex()
            .flex_col()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .w_full()
            .min_w_0()
            .child(Label::new(self.title.clone()).truncate())
            .child(
                Label::new(self.id.clone())
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .truncate(),
            )
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    fn matches(&self, query: &str) -> bool {
        fuzzy_match(&self.title, &normalize_action_query(query)).is_some()
    }
}

/// One context accepted by Bootty's keymap owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapContextSnapshot {
    pub id: String,
    pub label: String,
    pub description: String,
    pub use_builtin_defaults: bool,
}

impl SearchableListItem for KeymapContextSnapshot {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone().into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let selector = format!("keymap-context-option-{}", self.id);
        let debug_selector = selector.clone();
        gpui_kit::div()
            .flex()
            .flex_col()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .w_full()
            .min_w_0()
            .child(Label::new(self.label.clone()).truncate())
            .child(
                Label::new(self.id.clone())
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .truncate(),
            )
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_ascii_lowercase();
        query.is_empty()
            || self.id.to_ascii_lowercase().contains(&query)
            || self.label.to_ascii_lowercase().contains(&query)
            || self.description.to_ascii_lowercase().contains(&query)
    }
}

/// One effective binding projected by the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapBindingSnapshot {
    /// Opaque, stable identity for this snapshot revision.
    pub id: String,
    pub action: String,
    /// JSON input after the action name, when present.
    pub arguments_json: Option<String>,
    /// Chord steps in display and persistence order.
    pub keystrokes: Vec<String>,
    /// Opaque accepted spelling used to target edits without normalizing legacy flag order.
    pub persisted_keystrokes: String,
    pub context: String,
    pub source: KeymapBindingSource,
    /// Whether this row invokes an action or masks the binding with an unbind entry.
    pub kind: KeymapBindingKind,
    pub trigger_options: KeymapTriggerOptions,
    /// Number of other bindings that the authoritative resolver considers conflicting.
    pub conflict_count: usize,
}

impl KeymapBindingSnapshot {
    #[must_use]
    pub fn target(&self) -> KeymapBindingTarget {
        KeymapBindingTarget {
            id: self.id.clone(),
            action: self.action.clone(),
            arguments_json: self.arguments_json.clone(),
            keystrokes: self.keystrokes.clone(),
            persisted_keystrokes: self.persisted_keystrokes.clone(),
            context: self.context.clone(),
            source: self.source,
            kind: self.kind,
            trigger_options: self.trigger_options,
        }
    }
}

/// Trigger capabilities that are not encoded by a GPUI [`Keystroke`] alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Trigger policies are independent options, not exclusive states."
)]
pub struct KeymapTriggerOptions {
    pub performable: bool,
    pub global: bool,
    pub all: bool,
    pub unconsumed: bool,
    pub side_sensitive: bool,
    pub prefixed: bool,
}

/// Complete disposable editor projection for one accepted keymap revision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeymapEditorSnapshot {
    pub path: String,
    pub prefix: Option<String>,
    pub actions: Vec<KeymapActionSnapshot>,
    pub bindings: Vec<KeymapBindingSnapshot>,
    pub contexts: Vec<KeymapContextSnapshot>,
    pub diagnostic: Option<String>,
    pub revision: u64,
}

/// A newly entered binding, independent of persistence format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapBindingDraft {
    pub action: String,
    pub arguments_json: Option<String>,
    pub keystrokes: Vec<String>,
    pub context: String,
    pub trigger_options: KeymapTriggerOptions,
}

/// The exact binding being replaced or removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapBindingTarget {
    pub id: String,
    pub action: String,
    pub arguments_json: Option<String>,
    pub keystrokes: Vec<String>,
    pub persisted_keystrokes: String,
    pub context: String,
    pub source: KeymapBindingSource,
    pub kind: KeymapBindingKind,
    pub trigger_options: KeymapTriggerOptions,
}

/// Typed operations for the application owner to validate and persist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeymapEditorIntent {
    Add {
        binding: KeymapBindingDraft,
    },
    Replace {
        target: KeymapBindingTarget,
        replacement: KeymapBindingDraft,
    },
    Remove {
        target: KeymapBindingTarget,
    },
    /// Include or suppress built-in bindings for one context without changing user bindings.
    SetBuiltInDefaults {
        context: String,
        enabled: bool,
    },
    OpenKeymapFile,
    Close,
}

/// Which column drives the primary search field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeymapSearchMode {
    #[default]
    Action,
    Keystroke,
}

/// Source visibility mirrors the dedicated filters in Zed's keymap editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeymapSourceFilters {
    pub defaults: bool,
    pub user: bool,
    pub unmapped: bool,
}

impl Default for KeymapSourceFilters {
    fn default() -> Self {
        Self {
            defaults: true,
            user: true,
            unmapped: true,
        }
    }
}

impl KeymapSourceFilters {
    const fn contains(self, source: Option<KeymapBindingSource>) -> bool {
        match source {
            Some(KeymapBindingSource::Default) => self.defaults,
            Some(KeymapBindingSource::User) => self.user,
            None => self.unmapped,
        }
    }
}

/// A row after combining effective bindings with commands that are not mapped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeymapEditorRow {
    Binding {
        binding: KeymapBindingSnapshot,
        action: KeymapActionSnapshot,
        conflict_count: usize,
    },
    Unmapped(KeymapActionSnapshot),
}

impl KeymapEditorRow {
    #[must_use]
    pub const fn action(&self) -> &KeymapActionSnapshot {
        match self {
            Self::Binding { action, .. } | Self::Unmapped(action) => action,
        }
    }

    #[must_use]
    pub const fn binding(&self) -> Option<&KeymapBindingSnapshot> {
        match self {
            Self::Binding { binding, .. } => Some(binding),
            Self::Unmapped(_) => None,
        }
    }

    #[must_use]
    pub const fn conflict_count(&self) -> usize {
        match self {
            Self::Binding { conflict_count, .. } => *conflict_count,
            Self::Unmapped(_) => 0,
        }
    }
}

#[derive(Clone, Debug)]
struct CachedKeymapEditorRow {
    row: KeymapEditorRow,
    action_search: String,
    normalized_keystrokes: Option<String>,
}

#[derive(Default)]
struct ConflictGroup {
    total: usize,
    global: usize,
    contexts: HashMap<String, usize>,
}

enum ModalOrigin {
    Create {
        combobox: Entity<ComboboxState<ActionComboboxDelegate>>,
        _subscription: Subscription,
    },
    Replace(KeymapBindingSnapshot),
}

#[derive(Clone, Copy)]
enum TriggerOption {
    Performable,
    Global,
    All,
    Unconsumed,
    SideSensitive,
    Prefixed,
}

struct ModalControls {
    context: Entity<InputState>,
    arguments: Entity<EditorState>,
    _context_subscription: Subscription,
    _arguments_subscription: Subscription,
}

struct BindingModal {
    origin: ModalOrigin,
    controls: ModalControls,
    draft: KeymapBindingDraft,
    recording: bool,
    replace_on_next_record: bool,
    error: Option<String>,
    confirmed_conflict: Option<String>,
}

impl BindingModal {
    fn create(
        action: String,
        context: String,
        origin: ModalOrigin,
        controls: ModalControls,
    ) -> Self {
        Self {
            origin,
            controls,
            draft: KeymapBindingDraft {
                action,
                arguments_json: None,
                keystrokes: Vec::new(),
                context,
                trigger_options: KeymapTriggerOptions::default(),
            },
            recording: false,
            replace_on_next_record: false,
            error: None,
            confirmed_conflict: None,
        }
    }

    fn edit(binding: KeymapBindingSnapshot, controls: ModalControls) -> Self {
        Self {
            draft: KeymapBindingDraft {
                action: binding.action.clone(),
                arguments_json: binding.arguments_json.clone(),
                keystrokes: binding.keystrokes.clone(),
                context: binding.context.clone(),
                trigger_options: binding.trigger_options,
            },
            origin: ModalOrigin::Replace(binding),
            controls,
            recording: false,
            replace_on_next_record: false,
            error: None,
            confirmed_conflict: None,
        }
    }

    fn changed(&mut self) {
        self.error = None;
        self.confirmed_conflict = None;
    }
}

/// Rendered dialog content kept separate from the editor's table surface so the Root-owned
/// dialog can mount it as a normal retained view.
struct BindingModalBody {
    editor: Entity<GpuiKeymapEditor>,
    focus: FocusHandle,
    _editor_subscription: Subscription,
}

/// Keymap editor over Bootty-owned keymap facts.
pub struct GpuiKeymapEditor {
    snapshot: KeymapEditorSnapshot,
    all_rows: Vec<CachedKeymapEditorRow>,
    visible_rows: Vec<KeymapEditorRow>,
    focus: FocusHandle,
    search_mode: KeymapSearchMode,
    action_query: String,
    keystroke_query: Vec<String>,
    search_recording: bool,
    exact_keystroke_search: bool,
    source_filters: KeymapSourceFilters,
    conflicts_only: bool,
    selected_row: Option<usize>,
    modal: Option<BindingModal>,
    table: Option<Entity<TableState<KeymapTable>>>,
    context_menu: Option<(Entity<PopupMenu>, Point<Pixels>, Subscription)>,
    search_editor: Option<Entity<InputState>>,
    input_subscriptions: Vec<Subscription>,
}

struct KeymapTable {
    editor: gpui_kit::WeakEntity<GpuiKeymapEditor>,
}

impl TableDelegate for KeymapTable {
    fn columns_count(&self, _: &App) -> usize {
        COLUMN_COUNT
    }
    fn rows_count(&self, cx: &App) -> usize {
        self.editor
            .upgrade()
            .map_or(0, |editor| editor.read(cx).visible_rows.len())
    }
    fn column(&self, col: usize, _: &App) -> Column {
        let (key, name, width) = [
            ("edit", "", 36.0),
            ("action", "Action", 220.0),
            ("arguments", "Arguments", 180.0),
            ("keystrokes", "Keystrokes", 160.0),
            ("context", "Context", 280.0),
            ("source", "Source", 100.0),
        ]
        .get(col)
        .copied()
        .unwrap_or(("unknown", "", 0.0));
        Column::new(key, name).width(px(width)).resizable(col != 0)
    }
    fn render_th(
        &mut self,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col, cx);
        let selector = format!("keymap-column-{}", column.key);
        div()
            .w_full()
            .debug_selector(move || selector)
            .child(Label::new(column.name))
    }
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        self.editor
            .update(cx, |editor, cx| {
                editor
                    .visible_rows
                    .get(row)
                    .cloned()
                    .map_or_else(empty_cell, |data| {
                        GpuiKeymapEditor::render_row_cell(row, col, data, cx)
                    })
            })
            .unwrap_or_else(|_| empty_cell())
    }
    fn render_tr(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let Some(editor) = self.editor.upgrade() else {
            return div().id(index);
        };
        let state = editor.read(cx);
        let Some(data) = state.visible_rows.get(index).cloned() else {
            return div().id(index);
        };
        let selected = state.selected_row == Some(index);
        let conflict = data.conflict_count() > 0;
        let group = row_group_id(&row_identity(&data));
        let click_editor = editor.clone();
        div()
            .id(group.clone())
            .group(group)
            .debug_selector(move || format!("keymap-row-{index}"))
            .border_2()
            .when(conflict, |row| row.bg(cx.theme().danger.opacity(0.15)))
            .when(selected, |row| row.border_color(cx.theme().ring))
            .on_any_mouse_down(move |event, window, cx| {
                if event.button == MouseButton::Right {
                    editor.update(cx, |editor, cx| {
                        editor.select_index(index, cx);
                        editor.create_context_menu(event.position, window, cx);
                    });
                }
            })
            .on_click(move |event, window, cx| {
                click_editor.update(cx, |editor, cx| {
                    editor.select_index(index, cx);
                    if event.click_count() == 2 {
                        editor.open_row(data.clone(), window, cx);
                    }
                });
            })
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        self.editor.upgrade().map_or_else(empty_cell, |editor| {
            editor.read(cx).render_no_matches_hint(cx)
        })
    }
}

impl GpuiKeymapEditor {
    pub fn new(snapshot: KeymapEditorSnapshot, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            snapshot,
            all_rows: Vec::new(),
            visible_rows: Vec::new(),
            focus: cx.focus_handle(),
            search_mode: KeymapSearchMode::Action,
            action_query: String::new(),
            keystroke_query: Vec::new(),
            search_recording: false,
            exact_keystroke_search: false,
            source_filters: KeymapSourceFilters::default(),
            conflicts_only: false,
            selected_row: None,
            modal: None,
            table: None,
            context_menu: None,
            search_editor: None,
            input_subscriptions: Vec::with_capacity(1),
        };
        this.rebuild_all_rows();
        this.reselect_first_visible();
        this
    }

    /// Construct an editor with its search field ready before the host attaches it to a window.
    /// Stateful GPUI controls belong to the window-owning construction path, not `render`.
    pub fn new_with_window(
        snapshot: KeymapEditorSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(snapshot, cx);
        this.ensure_search_editor(window, cx);
        this.ensure_table(window, cx);
        this
    }

    #[must_use]
    pub const fn snapshot(&self) -> &KeymapEditorSnapshot {
        &self.snapshot
    }

    pub fn set_snapshot(&mut self, snapshot: KeymapEditorSnapshot, cx: &mut Context<Self>) {
        let selected_id = self
            .selected_row()
            .and_then(|row| row.binding().map(|binding| binding.id.clone()));
        self.snapshot = snapshot;
        self.rebuild_all_rows();
        self.refresh_visible_rows();
        self.selected_row = selected_id
            .and_then(|id| {
                self.visible_rows
                    .iter()
                    .position(|row| row.binding().is_some_and(|binding| binding.id == id))
            })
            .or_else(|| (!self.visible_rows.is_empty()).then_some(0));
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if self.selected_row.is_none()
            && let Some(search_editor) = &self.search_editor
        {
            search_editor.focus_handle(cx).focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
    }

    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Focus the editor on one action. If the action is currently unmapped, open a prefilled
    /// create modal, matching Zed's `ChangeKeybinding` behavior.
    pub fn focus_action(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search_mode = KeymapSearchMode::Action;
        action.clone_into(&mut self.action_query);
        self.keystroke_query.clear();
        if let Some(search_editor) = &self.search_editor {
            search_editor.update(cx, |editor, cx| {
                editor.set_value(action.to_owned(), window, cx);
            });
        }
        self.reselect_first_visible();
        let mapped = self
            .snapshot
            .bindings
            .iter()
            .any(|binding| binding.action == action);
        if !mapped && self.snapshot.actions.iter().any(|entry| entry.id == action) {
            self.open_create_modal(Some(action.to_owned()), window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    #[must_use]
    pub const fn search_mode(&self) -> KeymapSearchMode {
        self.search_mode
    }

    pub fn set_search_mode(&mut self, mode: KeymapSearchMode, cx: &mut Context<Self>) {
        if self.search_mode != mode {
            self.search_mode = mode;
            self.action_query.clear();
            self.keystroke_query.clear();
            self.search_recording = false;
            self.reselect_first_visible();
            cx.notify();
        }
    }

    pub fn set_search(&mut self, query: impl Into<String>, cx: &mut Context<Self>) {
        let query = query.into();
        match self.search_mode {
            KeymapSearchMode::Action => {
                self.action_query.clone_from(&query);
                self.keystroke_query.clear();
            }
            KeymapSearchMode::Keystroke => {
                self.action_query.clear();
                self.keystroke_query = query.split_whitespace().map(str::to_owned).collect();
            }
        }
        self.reselect_first_visible();
        cx.notify();
    }

    /// The keymap filter shares gpui-component's real input engine with the file editors.
    fn ensure_search_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let editor = self.search_editor.get_or_insert_with(|| {
            let initial_text = self.action_query.clone();
            let editor = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Filter action names…")
                    .default_value(initial_text)
            });
            self.input_subscriptions.push(cx.subscribe(
                &editor,
                |this, editor, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change)
                        || this.search_mode != KeymapSearchMode::Action
                    {
                        return;
                    }
                    this.action_query = editor.read(cx).value().to_string();
                    this.reselect_first_visible();
                    cx.notify();
                },
            ));
            editor
        });
        let desired_text = if self.search_mode == KeymapSearchMode::Action {
            self.action_query.as_str()
        } else {
            ""
        };
        let placeholder = search_placeholder(self.search_mode);
        editor.update(cx, |editor, cx| {
            editor.set_placeholder(placeholder, window, cx);
            if editor.value().as_ref() != desired_text {
                editor.set_value(desired_text.to_owned(), window, cx);
            }
        });
        editor.clone()
    }

    pub fn set_exact_keystroke_search(&mut self, exact: bool, cx: &mut Context<Self>) {
        self.exact_keystroke_search = exact;
        self.reselect_first_visible();
        cx.notify();
    }

    #[must_use]
    pub const fn source_filters(&self) -> KeymapSourceFilters {
        self.source_filters
    }

    pub fn set_source_filters(&mut self, filters: KeymapSourceFilters, cx: &mut Context<Self>) {
        self.source_filters = filters;
        self.reselect_first_visible();
        cx.notify();
    }

    pub fn set_conflicts_only(&mut self, conflicts_only: bool, cx: &mut Context<Self>) {
        self.conflicts_only = conflicts_only;
        self.reselect_first_visible();
        cx.notify();
    }

    /// The cached visible rows in display order.
    ///
    /// Snapshot projection, conflict derivation, filtering, and sorting happen only when their
    /// inputs change. Paint and virtual-table reads borrow this stable slice.
    #[must_use]
    pub fn visible_rows(&self) -> &[KeymapEditorRow] {
        &self.visible_rows
    }

    fn rebuild_all_rows(&mut self) {
        let actions = self
            .snapshot
            .actions
            .iter()
            .map(|action| (action.id.as_str(), action))
            .collect::<HashMap<_, _>>();

        let mut conflict_groups = HashMap::<String, ConflictGroup>::new();
        for binding in &self.snapshot.bindings {
            let group = conflict_groups
                .entry(normalize_keystrokes(&binding.keystrokes))
                .or_default();
            let context = normalize_context(&binding.context);
            group.total = group.total.saturating_add(1);
            if context == "global" {
                group.global = group.global.saturating_add(1);
            }
            let count = group.contexts.entry(context).or_default();
            *count = count.saturating_add(1);
        }

        let mut rows = self
            .snapshot
            .bindings
            .iter()
            .filter_map(|binding| {
                let action = actions.get(binding.action.as_str()).copied()?;
                let normalized_keystrokes = normalize_keystrokes(&binding.keystrokes);
                let context = normalize_context(&binding.context);
                let group = conflict_groups.get(&normalized_keystrokes)?;
                let derived_conflicts = if context == "global" {
                    group.total.saturating_sub(1)
                } else {
                    group.global.saturating_add(
                        group
                            .contexts
                            .get(&context)
                            .copied()
                            .unwrap_or_default()
                            .saturating_sub(1),
                    )
                };
                let row = KeymapEditorRow::Binding {
                    binding: binding.clone(),
                    action: action.clone(),
                    conflict_count: binding.conflict_count.max(derived_conflicts),
                };
                Some(CachedKeymapEditorRow {
                    action_search: action_search(action),
                    normalized_keystrokes: Some(normalized_keystrokes),
                    row,
                })
            })
            .collect::<Vec<_>>();

        let mapped = self
            .snapshot
            .bindings
            .iter()
            .map(|binding| binding.action.as_str())
            .collect::<HashSet<_>>();
        rows.extend(
            self.snapshot
                .actions
                .iter()
                .filter(|action| !mapped.contains(action.id.as_str()))
                .map(|action| CachedKeymapEditorRow {
                    action_search: action_search(action),
                    normalized_keystrokes: None,
                    row: KeymapEditorRow::Unmapped(action.clone()),
                }),
        );
        rows.sort_by(|left, right| {
            left.row
                .action()
                .title
                .to_ascii_lowercase()
                .cmp(&right.row.action().title.to_ascii_lowercase())
                .then_with(|| left.row.action().id.cmp(&right.row.action().id))
                .then_with(|| {
                    left.row
                        .binding()
                        .map(|binding| binding.id.as_str())
                        .cmp(&right.row.binding().map(|binding| binding.id.as_str()))
                })
        });
        self.all_rows = rows;
    }

    #[must_use]
    pub fn selected_row(&self) -> Option<KeymapEditorRow> {
        self.selected_row
            .and_then(|index| self.visible_rows.get(index).cloned())
    }

    #[must_use]
    pub fn modal_draft(&self) -> Option<&KeymapBindingDraft> {
        self.modal.as_ref().map(|modal| &modal.draft)
    }

    #[must_use]
    pub fn modal_error(&self) -> Option<&str> {
        self.modal.as_ref().and_then(|modal| modal.error.as_deref())
    }

    fn refresh_visible_rows(&mut self) {
        let action_query = normalize_action_query(&self.action_query);
        let keystroke_query = normalize_keystrokes(&self.keystroke_query);
        let mut visible_rows = self
            .all_rows
            .iter()
            .enumerate()
            .filter_map(|(source_index, cached)| {
                let row = &cached.row;
                if !self
                    .source_filters
                    .contains(row.binding().map(|binding| binding.source))
                    || (self.conflicts_only && row.conflict_count() == 0)
                {
                    return None;
                }
                let action_score = if action_query.is_empty() {
                    0
                } else {
                    fuzzy_match(&cached.action_search, &action_query)?.score
                };
                if self.search_mode == KeymapSearchMode::Keystroke
                    && !keystroke_query.is_empty()
                    && !cached
                        .normalized_keystrokes
                        .as_ref()
                        .is_some_and(|binding| {
                            if self.exact_keystroke_search {
                                binding == &keystroke_query
                            } else {
                                binding.contains(&keystroke_query)
                            }
                        })
                {
                    return None;
                }

                Some((action_score, source_index, cached.row.clone()))
            })
            .collect::<Vec<_>>();
        if !action_query.is_empty() {
            visible_rows
                .sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        }
        self.visible_rows = visible_rows.into_iter().map(|(_, _, row)| row).collect();
    }

    fn draft_conflict_count(&self, draft: &KeymapBindingDraft, ignored_id: Option<&str>) -> usize {
        self.snapshot
            .bindings
            .iter()
            .filter(|binding| ignored_id != Some(binding.id.as_str()))
            .filter(|binding| {
                super::keybinding::parse_keybinding(&binding.keystrokes.join(" > "))
                    .zip(super::keybinding::parse_keybinding(
                        &draft.keystrokes.join(" > "),
                    ))
                    .is_some_and(|(binding, draft)| binding == draft)
                    && super::keybinding::keybinding_contexts_overlap(
                        &binding.context,
                        &draft.context,
                    )
            })
            .count()
    }

    fn reselect_first_visible(&mut self) {
        self.refresh_visible_rows();
        let count = self.visible_rows.len();
        self.selected_row = match (self.selected_row, count) {
            (_, 0) => None,
            (Some(index), count) if index < count => Some(index),
            _ => Some(0),
        };
    }

    fn select_relative(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.visible_rows.len();
        if count == 0 {
            self.selected_row = None;
            return;
        }
        let current = self.selected_row.unwrap_or(0);
        let selected = if forward {
            let next = current.saturating_add(1);
            if next < count { next } else { 0 }
        } else {
            current
                .checked_sub(1)
                .unwrap_or_else(|| count.saturating_sub(1))
        };
        self.select_index(selected, cx);
    }

    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let count = self.visible_rows.len();
        if count == 0 {
            self.selected_row = None;
            return;
        }
        let index = index.min(count.saturating_sub(1));
        self.selected_row = Some(index);
        cx.notify();
    }

    fn open_row(&mut self, row: KeymapEditorRow, window: &mut Window, cx: &mut Context<Self>) {
        match row {
            KeymapEditorRow::Binding { binding, .. } => {
                self.open_edit_modal(binding, window, cx);
            }
            KeymapEditorRow::Unmapped(action) => {
                self.open_create_modal(Some(action.id), window, cx);
            }
        }
    }

    fn dismiss_context_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu.take();
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn create_context_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let editor = cx.entity();
        let edit_editor = editor.clone();
        let edit_row = row.clone();
        let remove = row.binding().cloned();
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            let mut menu = menu.item(
                PopupMenuItem::new(if edit_row.binding().is_some() {
                    "Edit"
                } else {
                    "Create"
                })
                .on_click(move |_, window, cx| {
                    let row = edit_row.clone();
                    edit_editor.update(cx, |editor, cx| {
                        editor.open_row(row, window, cx);
                    });
                }),
            );
            if let Some(binding) = remove {
                let remove_editor = editor.clone();
                menu = menu.item(PopupMenuItem::new("Delete").on_click(move |_, _, cx| {
                    let binding = binding.clone();
                    remove_editor.update(cx, |_, cx| {
                        cx.emit(KeymapEditorIntent::Remove {
                            target: binding.target(),
                        });
                    });
                }));
            }
            menu.separator().item(
                PopupMenuItem::new("Edit in JSON").on_click(move |_, _, cx| {
                    editor.update(cx, |_, cx| {
                        cx.emit(KeymapEditorIntent::OpenKeymapFile);
                    });
                }),
            )
        });
        let subscription =
            cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, window, cx| {
                this.dismiss_context_menu(window, cx);
            });
        self.context_menu = Some((menu, position, subscription));
        cx.notify();
    }

    fn open_create_modal(
        &mut self,
        action: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One editor owns one dialog. External focus requests can arrive while the modal is
        // already open; keep its draft and retained controls authoritative until dismissal.
        if self.modal.is_some() {
            return;
        }
        let action_delegate =
            ActionComboboxDelegate::new(self.snapshot.actions.clone(), action.clone());
        let selected_index = action
            .as_ref()
            .and_then(|action| action_delegate.position(action));
        let action_combobox = cx.new(|cx| {
            ComboboxState::new(
                action_delegate,
                selected_index.into_iter().collect(),
                window,
                cx,
            )
            .searchable(true)
        });
        let action_combobox_subscription = cx.subscribe_in(
            &action_combobox,
            window,
            |this, _, event: &ComboboxEvent<ActionComboboxDelegate>, _, cx| match event {
                ComboboxEvent::Change(values) => {
                    let Some(action) = values.first() else {
                        return;
                    };
                    if let Some(modal) = &mut this.modal {
                        modal.draft.action.clone_from(action);
                        modal.changed();
                        cx.notify();
                    }
                }
                ComboboxEvent::Confirm(_) => {}
            },
        );
        let context = self
            .snapshot
            .contexts
            .first()
            .map_or_else(|| "Global".to_owned(), |context| context.id.clone());
        let controls = Self::modal_controls(context.clone(), window, cx);
        self.modal = Some(BindingModal::create(
            action.unwrap_or_default(),
            context,
            ModalOrigin::Create {
                combobox: action_combobox,
                _subscription: action_combobox_subscription,
            },
            controls,
        ));
        self.configure_modal_controls(window, cx);
        Self::open_binding_dialog(window, cx);
        cx.notify();
    }

    fn open_edit_modal(
        &mut self,
        binding: KeymapBindingSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal.is_some() {
            return;
        }
        let controls = Self::modal_controls(binding.context.clone(), window, cx);
        self.modal = Some(BindingModal::edit(binding, controls));
        self.configure_modal_controls(window, cx);
        Self::open_binding_dialog(window, cx);
        cx.notify();
    }

    fn open_binding_dialog(window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.entity();
        let body = cx.new(|cx| BindingModalBody {
            _editor_subscription: cx.observe(&editor, |_, _, cx| cx.notify()),
            editor: editor.clone(),
            focus: cx.focus_handle(),
        });
        window.open_dialog(cx, move |dialog, _, cx| {
            let (title, description) = editor.read(cx).modal_title();
            let editor_for_cancel = editor.clone();
            let editor_for_save = editor.clone();
            let editor_for_confirm = editor.clone();
            let editor_for_footer_cancel = editor.clone();
            dialog
                .title(
                    gpui_kit::div()
                        .flex()
                        .flex_col()
                        .debug_selector(|| "keymap-modal-dialog-title".to_owned())
                        .gap_0p5()
                        .child(Label::new(title))
                        .when(!description.is_empty(), |header| {
                            header.child(
                                Label::new(description)
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground),
                            )
                        }),
                )
                .w(px(544.0))
                .max_w(px(720.0))
                .close_button(false)
                .on_ok(move |_, window, cx| {
                    editor_for_confirm.update(cx, |editor, cx| {
                        editor.submit_modal_from_ui(window, cx);
                    });
                    // Validation and successful submission own the close decision.
                    false
                })
                .on_cancel(move |_, _, cx| {
                    editor_for_cancel.update(cx, |editor, cx| {
                        editor.dismiss_modal_state(cx);
                    });
                    true
                })
                .content({
                    let body = body.clone();
                    move |content, _, _| {
                        content.child(
                            div()
                                .debug_selector(|| "keymap-modal-dialog-content".to_owned())
                                .w_full()
                                .child(body.clone()),
                        )
                    }
                })
                .footer(
                    gpui_kit::div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(public_selector(
                            "keymap-modal-cancel",
                            Button::new("cancel")
                                .label("Cancel")
                                .on_click(move |_, window, cx| {
                                    editor_for_footer_cancel.update(cx, |editor, cx| {
                                        editor.cancel_modal(window, cx);
                                    });
                                }),
                        ))
                        .child(public_selector(
                            "keymap-modal-save",
                            Button::new("save-btn")
                                .label("Save")
                                .on_click(move |_, window, cx| {
                                    editor_for_save.update(cx, |editor, cx| {
                                        editor.submit_modal_from_ui(window, cx);
                                    });
                                }),
                        )),
                )
        });
    }

    fn modal_title(&self) -> (String, String) {
        let Some(modal) = self.modal.as_ref() else {
            return ("Keybinding".to_owned(), String::new());
        };
        if matches!(modal.origin, ModalOrigin::Create { .. }) {
            return ("Create Keybinding".to_owned(), String::new());
        }
        self.snapshot
            .actions
            .iter()
            .find(|action| action.id == modal.draft.action)
            .map_or_else(
                || ("Edit Keybinding".to_owned(), String::new()),
                |action| (action.title.clone(), action.description.clone()),
            )
    }

    fn modal_controls(
        initial_context: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ModalControls {
        let arguments_editor = cx.new(|cx| EditorState::new(window, cx));
        let arguments_editor_subscription =
            cx.subscribe(&arguments_editor, |this, editor, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = editor.read(cx).value();
                if let Some(modal) = &mut this.modal {
                    modal.draft.arguments_json =
                        (!value.trim().is_empty()).then(|| value.to_string());
                    modal.changed();
                    cx.notify();
                }
            });
        let context_editor = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("e.g. Terminal && backend == rmux")
                .default_value(initial_context)
        });
        let context_editor_subscription =
            cx.subscribe(&context_editor, |this, editor, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                if let Some(modal) = &mut this.modal {
                    modal.draft.context = editor.read(cx).value().to_string();
                    modal.changed();
                    cx.notify();
                }
            });
        ModalControls {
            context: context_editor,
            arguments: arguments_editor,
            _context_subscription: context_editor_subscription,
            _arguments_subscription: arguments_editor_subscription,
        }
    }

    fn configure_modal_controls(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.as_ref() else {
            return;
        };
        {
            let arguments_editor = &modal.controls.arguments;
            let value = modal.draft.arguments_json.clone().unwrap_or_default();
            arguments_editor.update(cx, |editor, cx| editor.set_value(value, window, cx));
        }
        {
            let context_editor = &modal.controls.context;
            let context = modal.draft.context.clone();
            context_editor.update(cx, |editor, cx| {
                editor.set_value(context, window, cx);
            });
        }
    }

    pub fn record_modal_keystroke(&mut self, keystroke: &str, cx: &mut Context<Self>) {
        let prefix = self.snapshot.prefix.clone();
        let Some(modal) = &mut self.modal else {
            return;
        };
        if modal.replace_on_next_record {
            modal.draft.keystrokes.clear();
            modal.replace_on_next_record = false;
        }
        let keystroke =
            set_step_side_sensitivity(keystroke, modal.draft.trigger_options.side_sensitive);
        if modal.draft.trigger_options.prefixed {
            modal.draft.keystrokes.clear();
            modal.draft.keystrokes.extend(prefix);
            modal.draft.keystrokes.push(keystroke);
            modal.recording = false;
        } else {
            modal.draft.keystrokes.push(keystroke);
        }
        modal.changed();
        cx.notify();
    }

    pub fn set_modal_trigger_options(
        &mut self,
        options: KeymapTriggerOptions,
        cx: &mut Context<Self>,
    ) {
        let prefix = self.snapshot.prefix.as_deref();
        let Some(modal) = &mut self.modal else {
            return;
        };
        if options.prefixed != modal.draft.trigger_options.prefixed {
            if options.prefixed {
                if let Some(prefix) = prefix
                    && modal.draft.keystrokes.first().map(String::as_str) != Some(prefix)
                {
                    modal.draft.keystrokes.insert(0, prefix.to_owned());
                }
            } else if prefix.is_some_and(|prefix| {
                modal.draft.keystrokes.first().map(String::as_str) == Some(prefix)
            }) {
                modal.draft.keystrokes.remove(0);
            }
        }
        if options.side_sensitive != modal.draft.trigger_options.side_sensitive {
            for step in &mut modal.draft.keystrokes {
                *step = set_step_side_sensitivity(step, options.side_sensitive);
            }
        }
        modal.draft.trigger_options = options;
        modal.changed();
        cx.notify();
    }

    fn toggle_modal_trigger_option(&mut self, option: TriggerOption, cx: &mut Context<Self>) {
        let Some(modal) = &self.modal else {
            return;
        };
        let mut options = modal.draft.trigger_options;
        let value = match option {
            TriggerOption::Performable => &mut options.performable,
            TriggerOption::Global => &mut options.global,
            TriggerOption::All => &mut options.all,
            TriggerOption::Unconsumed => &mut options.unconsumed,
            TriggerOption::SideSensitive => &mut options.side_sensitive,
            TriggerOption::Prefixed => &mut options.prefixed,
        };
        *value = !*value;
        self.set_modal_trigger_options(options, cx);
    }

    fn clear_modal_keystrokes(&mut self, cx: &mut Context<Self>) {
        if let Some(modal) = &mut self.modal {
            modal.draft.keystrokes.clear();
            modal.changed();
            cx.notify();
        }
    }

    fn cancel_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_modal_state(cx);
        window.close_dialog(cx);
    }

    fn dismiss_modal_state(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        cx.notify();
    }

    fn submit_modal_from_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.submit_modal(cx);
        if self.modal.is_none() {
            window.close_dialog(cx);
        }
    }

    /// Validate and submit the modal. A conflicting chord requires the same save operation twice.
    pub fn submit_modal(&mut self, cx: &mut Context<Self>) {
        let Some(mut modal) = self.modal.take() else {
            return;
        };
        let Some(action) = self
            .snapshot
            .actions
            .iter()
            .find(|action| action.id == modal.draft.action.trim())
        else {
            modal.error = Some("Choose an action from the command catalog.".to_owned());
            self.modal = Some(modal);
            cx.notify();
            return;
        };
        modal.draft.action.clone_from(&action.id);
        let context = modal.draft.context.trim().to_owned();
        modal.draft.context = context;
        modal.draft.keystrokes = modal
            .draft
            .keystrokes
            .iter()
            .map(|step| step.trim().to_owned())
            .filter(|step| !step.is_empty())
            .collect();
        if modal.draft.keystrokes.is_empty() {
            modal.error = Some("Record at least one keystroke.".to_owned());
            self.modal = Some(modal);
            cx.notify();
            return;
        }
        if let Err(error) = validate_modal_keystrokes(&modal.draft.keystrokes) {
            modal.error = Some(error.to_owned());
            self.modal = Some(modal);
            cx.notify();
            return;
        }
        if modal.draft.keystrokes.len() > 1
            && (modal.draft.trigger_options.global || modal.draft.trigger_options.all)
        {
            modal.error = Some("Global and all-surfaces triggers cannot be chords.".to_owned());
            self.modal = Some(modal);
            cx.notify();
            return;
        }
        if modal.draft.context.is_empty() {
            "Global".clone_into(&mut modal.draft.context);
        } else if !modal.draft.context.eq_ignore_ascii_case("global")
            && let Err(error) = gpui_kit::KeyBindingContextPredicate::parse(&modal.draft.context)
        {
            modal.error = Some(format!("Invalid keybinding context: {error}"));
            self.modal = Some(modal);
            cx.notify();
            return;
        }

        let ignored_id = match &modal.origin {
            ModalOrigin::Create { .. } => None,
            ModalOrigin::Replace(binding) => Some(binding.id.as_str()),
        };
        let conflict_count = self.draft_conflict_count(&modal.draft, ignored_id);
        let conflict_signature = format!(
            "{}|{}|{}",
            normalize_keystrokes(&modal.draft.keystrokes),
            normalize_context(&modal.draft.context),
            modal.draft.action
        );
        if conflict_count > 0 && modal.confirmed_conflict.as_deref() != Some(&conflict_signature) {
            modal.error = Some(format!(
                "This keybinding conflicts with {conflict_count} existing {}. Save again to confirm.",
                if conflict_count == 1 {
                    "binding"
                } else {
                    "bindings"
                }
            ));
            modal.confirmed_conflict = Some(conflict_signature);
            self.modal = Some(modal);
            cx.notify();
            return;
        }

        let intent = match modal.origin {
            ModalOrigin::Create { .. } => KeymapEditorIntent::Add {
                binding: modal.draft,
            },
            ModalOrigin::Replace(binding) => KeymapEditorIntent::Replace {
                target: binding.target(),
                replacement: modal.draft,
            },
        };
        cx.emit(intent);
        cx.notify();
    }

    fn on_key_down(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_recording {
            if event.keystroke.key == "escape" {
                self.search_recording = false;
            } else {
                if self.keystroke_query.len() == 3 {
                    self.keystroke_query.clear();
                }
                self.keystroke_query
                    .push(persisted_keystroke(&event.keystroke));
                self.reselect_first_visible();
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }

        if self.modal.as_ref().is_some_and(|modal| modal.recording) {
            if event.keystroke.key == "escape" {
                if let Some(modal) = &mut self.modal {
                    modal.recording = false;
                }
            } else {
                self.record_modal_keystroke(&persisted_keystroke(&event.keystroke), cx);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }

        let combobox_focused = self
            .modal
            .as_ref()
            .is_some_and(|modal| match &modal.origin {
                ModalOrigin::Create { combobox, .. } => {
                    combobox.read(cx).focus_handle(cx).is_focused(window)
                }
                ModalOrigin::Replace(_) => false,
            });
        if combobox_focused && event.keystroke.key != "escape" {
            return;
        }

        match event.keystroke.key.as_str() {
            "escape" if self.modal.is_none() => cx.emit(KeymapEditorIntent::Close),
            "up" if self.modal.is_none() => self.select_relative(false, cx),
            "down" if self.modal.is_none() => self.select_relative(true, cx),
            "enter" if self.modal.is_some() => self.submit_modal_from_ui(window, cx),
            "enter" => {
                if let Some(row) = self.selected_row() {
                    self.open_row(row, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn render_filter_dropdown(cx: &Context<Self>) -> impl IntoElement {
        let editor = cx.entity();
        Button::new("KeymapEditorFilterMenuButton")
            .icon(Icon::new(IconName::Settings2))
            .accessibility_label("Filter keybindings")
            .tooltip("Filters")
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, cx| {
                let state = editor.read(cx);
                let filters = [
                    ("Conflicts", state.conflicts_only),
                    ("No Action", state.source_filters.unmapped),
                    ("User", state.source_filters.user),
                    ("Default", state.source_filters.defaults),
                ];
                let mut menu = menu.label("Filters");
                for (label, checked) in filters {
                    let editor = editor.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _, cx| {
                            editor.update(cx, |editor, cx| {
                                if label == "Conflicts" {
                                    editor.set_conflicts_only(!editor.conflicts_only, cx);
                                } else {
                                    let mut filters = editor.source_filters;
                                    match label {
                                        "No Action" => filters.unmapped = !filters.unmapped,
                                        "User" => filters.user = !filters.user,
                                        _ => filters.defaults = !filters.defaults,
                                    }
                                    editor.set_source_filters(filters, cx);
                                }
                            });
                        },
                    ));
                }
                menu
            })
    }

    fn render_context_defaults_dropdown(&self, cx: &Context<Self>) -> impl IntoElement {
        let editor = cx.entity();
        let contexts = self.snapshot.contexts.clone();
        Button::new("KeymapEditorDefaultsButton")
            .label("Use built-in defaults")
            .outline()
            .xsmall()
            .tooltip("Choose which keymap contexts include built-in bindings")
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                let mut menu = menu.label("Use built-in defaults");
                for context in &contexts {
                    let editor = editor.clone();
                    let context_id = context.id.clone();
                    let enabled = context.use_builtin_defaults;
                    menu = menu.item(
                        PopupMenuItem::new(context.label.clone())
                            .checked(enabled)
                            .on_click(move |_, _, cx| {
                                editor.update(cx, |_, cx| {
                                    cx.emit(KeymapEditorIntent::SetBuiltInDefaults {
                                        context: context_id.clone(),
                                        enabled: !enabled,
                                    });
                                });
                            }),
                    );
                }
                menu
            })
    }

    fn render_keystroke_search(&self, cx: &Context<Self>) -> AnyElement {
        let recording = self.search_recording;
        let actions = gpui_kit::div()
            .flex()
            .items_center()
            .child(public_selector(
                "keymap-keystroke-record",
                Button::new("kit-keymap-keystroke-record")
                    .icon(if recording {
                        Icon::default().path("icons/square.svg")
                    } else {
                        Icon::new(IconName::Search)
                    })
                    .accessibility_label(if recording {
                        "Stop recording search keystrokes"
                    } else {
                        "Record search keystrokes"
                    })
                    .text_color(if recording {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.search_recording = !this.search_recording;
                        this.focus.focus(window, cx);
                        cx.notify();
                    })),
            ))
            .child(public_selector(
                "keymap-search-exact",
                Button::new("kit-keymap-search-exact")
                    .icon(Icon::new(IconName::CaseSensitive))
                    .accessibility_label("Toggle exact keystroke matching")
                    .selected(self.exact_keystroke_search)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_exact_keystroke_search(!this.exact_keystroke_search, cx);
                    })),
            ))
            .when(recording, |actions| {
                actions.child(public_selector(
                    "keymap-keystroke-clear",
                    Button::new("kit-keymap-keystroke-clear")
                        .icon(Icon::new(IconName::Delete))
                        .accessibility_label("Clear search keystrokes")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.keystroke_query.clear();
                            this.reselect_first_visible();
                            cx.notify();
                        })),
                ))
            });
        keystroke_input(
            "keymap-keystroke-input",
            recording,
            ("SEARCH", cx.theme().primary),
            "Record keystrokes to search",
            &self.keystroke_query,
            actions,
            cx,
        )
    }

    // Direct port of Zed's KeymapEditor toolbar from keymap_editor.rs:2015-2115.
    fn render_toolbar(&self, search_editor: &Entity<InputState>, cx: &Context<Self>) -> AnyElement {
        let keystroke_selected = self.search_mode == KeymapSearchMode::Keystroke;
        gpui_kit::div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                gpui_kit::div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        gpui_kit::div()
                            .flex()
                            .items_center()
                            .id("keymap-search-action")
                            .debug_selector(|| "keymap-search-action".to_owned())
                            .key_context({
                                let mut context = KeyContext::new_with_defaults();
                                context.add("BufferSearchBar");
                                context
                            })
                            .flex_1()
                            .min_w_0()
                            .h_8()
                            .px_2()
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_md()
                            .child(crate::gpui::focus_input(
                                search_editor,
                                Input::new(search_editor).appearance(false),
                            )),
                    )
                    .child(
                        gpui_kit::div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .flex_none()
                            .min_w_80()
                            .child(public_selector(
                                "keymap-search-keystroke",
                                Button::new("KeymapEditorKeystrokeSearchButton")
                                    .icon(Icon::default().path("icons/keyboard.svg"))
                                    .accessibility_label("Search by keystrokes")
                                    .selected(keystroke_selected)
                                    .tooltip("Search by Keystrokes")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_search_mode(
                                            if this.search_mode == KeymapSearchMode::Keystroke {
                                                KeymapSearchMode::Action
                                            } else {
                                                KeymapSearchMode::Keystroke
                                            },
                                            cx,
                                        );
                                    })),
                            ))
                            .child(public_selector(
                                "keymap-filter-menu",
                                Self::render_filter_dropdown(cx),
                            ))
                            .child(public_selector(
                                "keymap-use-built-in-defaults",
                                self.render_context_defaults_dropdown(cx),
                            ))
                            .child(public_selector(
                                "keymap-edit-json",
                                Button::new("edit-in-json").label("Edit in JSON").on_click(
                                    cx.listener(|_, _, _, cx| {
                                        cx.emit(KeymapEditorIntent::OpenKeymapFile);
                                    }),
                                ),
                            ))
                            .child(public_selector(
                                "keymap-create",
                                Button::new("create")
                                    .label("Create Keybinding")
                                    .outline()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_create_modal(None, window, cx);
                                    })),
                            )),
                    ),
            )
            .when(keystroke_selected, |toolbar| {
                toolbar.child(
                    gpui_kit::div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(self.render_keystroke_search(cx))
                        .child(div().min_w_80()),
                )
            })
            .into_any_element()
    }

    // Direct port of Zed's row action column from keymap_editor.rs:1144-1233.
    fn render_row_button(index: usize, row: KeymapEditorRow, cx: &Context<Self>) -> AnyElement {
        let identity = row_identity(&row);
        let conflict_count = row.conflict_count();
        let icon = if conflict_count > 0 {
            Icon::default().path("icons/triangle-alert.svg")
        } else {
            Icon::default().path("icons/pencil.svg")
        };
        let selector = format!("keymap-row-action-{index}");
        let stable_id = format!("keymap-row-action-{identity}");
        let debug_selector = selector;
        let action_label = if conflict_count > 0 {
            format!("View conflicts for {}", row.action().title)
        } else {
            format!("Edit keybinding for {}", row.action().title)
        };
        div()
            .id(SharedString::from(stable_id))
            .debug_selector(move || debug_selector)
            .flex()
            .child(
                Button::new(format!("keymap-icon-{identity}"))
                    .icon(icon)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_index(index, cx);
                        this.focus.focus(window, cx);
                        this.open_row(row.clone(), window, cx);
                        cx.stop_propagation();
                    }))
                    .accessibility_label(action_label)
                    .xsmall()
                    .when(conflict_count > 0, |button| {
                        button.text_color(cx.theme().warning).tooltip(format!(
                            "This keybinding conflicts with {conflict_count} other {}",
                            if conflict_count == 1 {
                                "binding"
                            } else {
                                "bindings"
                            }
                        ))
                    }),
            )
            .into_any_element()
    }

    fn render_row_cell(
        index: usize,
        col: usize,
        row: KeymapEditorRow,
        cx: &Context<Self>,
    ) -> AnyElement {
        if col == 0 {
            return Self::render_row_button(index, row, cx);
        }
        let action = row.action();
        if col == 1 {
            let title = action.title.clone();
            let description = format!("{}\n{}", action.id, action.description);
            return div()
                .id(format!("keymap-action-{}", row_identity(&row)))
                .child(Label::new(title).truncate())
                .tooltip(move |_, cx| {
                    cx.new(|_| gpui_kit::component::tooltip::Tooltip::new(description.clone()))
                        .into()
                })
                .into_any_element();
        }
        let accepts_arguments = !action.arguments.is_empty();
        match (col, &row) {
            (2, _) => row
                .binding()
                .and_then(|binding| binding.arguments_json.clone())
                .map_or_else(
                    || {
                        if accepts_arguments {
                            muted_text(NO_ACTION_ARGUMENTS_TEXT, cx)
                        } else {
                            empty_cell()
                        }
                    },
                    text_cell,
                ),
            (3, KeymapEditorRow::Binding { binding, .. }) => {
                table_keystroke_cell(&row_identity(&row), &binding.keystrokes)
            }
            (4, KeymapEditorRow::Binding { binding, .. }) => {
                if binding.context.eq_ignore_ascii_case("global") {
                    muted_text("<global>", cx)
                } else {
                    text_cell(binding.context.clone())
                }
            }
            (5, KeymapEditorRow::Binding { binding, .. }) => {
                text_cell(binding_source_label(binding))
            }
            _ => empty_cell(),
        }
    }

    fn render_no_matches_hint(&self, cx: &App) -> AnyElement {
        let hint = if self.conflicts_only {
            "No conflicting keybinds found"
        } else if self.search_mode == KeymapSearchMode::Keystroke {
            "No keybinds found matching the entered keystrokes"
        } else {
            "No matches found for the provided query"
        };
        Label::new(hint)
            .text_color(cx.theme().muted_foreground)
            .into_any_element()
    }

    fn ensure_table(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TableState<KeymapTable>> {
        let table = self.table.get_or_insert_with(|| {
            let editor = cx.weak_entity();
            cx.new(|cx| {
                TableState::new(KeymapTable { editor }, window, cx)
                    .row_selectable(false)
                    .col_selectable(false)
                    .row_header(false)
            })
        });
        // The editor owns selection; project it into Kit for scrolling and accessibility.
        if table.read(cx).selected_row() != self.selected_row {
            table.update(cx, |table, cx| match self.selected_row {
                Some(row) => table.set_selected_row(row, cx),
                None => table.clear_selection(cx),
            });
        }
        table.clone()
    }

    fn render_table(&self, table: &Entity<TableState<KeymapTable>>) -> AnyElement {
        let table = DataTable::new(table).stripe(true);
        div()
            .id("keymap-table")
            .debug_selector(|| "keymap-table".to_owned())
            .min_h_0()
            .flex_1()
            .child(table)
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(gpui_kit::Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
            .into_any_element()
    }

    fn show_matching_modal_bindings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        window.close_dialog(cx);
        self.search_mode = KeymapSearchMode::Keystroke;
        self.exact_keystroke_search = true;
        self.action_query.clear();
        self.keystroke_query = modal.draft.keystrokes;
        self.search_recording = false;
        self.reselect_first_visible();
        self.focus.focus(window, cx);
        cx.notify();
    }

    // Direct port of Zed's modal KeystrokeInput composition from
    // ui_components/keystroke_input.rs:520-662.
    fn render_modal_keystroke_input(
        modal: &BindingModal,
        modal_focus: FocusHandle,
        cx: &Context<Self>,
    ) -> AnyElement {
        let recording = modal.recording;
        let actions = gpui_kit::div()
            .flex()
            .items_center()
            .child(public_selector(
                "keymap-modal-record",
                Button::new("kit-keymap-modal-record")
                    .icon(if recording {
                        Icon::default().path("icons/square.svg")
                    } else {
                        Icon::new(IconName::Play)
                    })
                    .accessibility_label(if recording {
                        "Stop recording keybinding"
                    } else {
                        "Record a keybinding"
                    })
                    .text_color(if recording {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(modal) = &mut this.modal {
                            modal.recording = !modal.recording;
                            modal.replace_on_next_record =
                                modal.recording && matches!(&modal.origin, ModalOrigin::Replace(_));
                            modal_focus.focus(window, cx);
                            cx.notify();
                        }
                    })),
            ))
            .when(recording, |actions| {
                actions.child(public_selector(
                    "keymap-modal-clear",
                    Button::new("kit-keymap-modal-clear")
                        .icon(Icon::new(IconName::Delete))
                        .accessibility_label("Clear keybinding")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.clear_modal_keystrokes(cx);
                        })),
                ))
            });
        keystroke_input(
            "keymap-modal-keystrokes",
            recording,
            ("REC", cx.theme().danger),
            "Record a keybinding",
            &modal.draft.keystrokes,
            actions,
            cx,
        )
    }

    fn render_modal_trigger_options(&self, modal: &BindingModal, cx: &Context<Self>) -> AnyElement {
        gpui_kit::div()
            .flex()
            .items_center()
            .id("keymap-modal-trigger-options")
            .debug_selector(|| "keymap-modal-trigger-options".to_owned())
            .gap_1()
            .child(
                ButtonGroup::new("keymap-trigger-options")
                    .multiple(true)
                    .outline()
                    .compact()
                    .with_size(Size::Small)
                    .children(
                        [
                            (
                                "keymap-trigger-option-performable",
                                "Performable",
                                modal.draft.trigger_options.performable,
                                TriggerOption::Performable,
                            ),
                            (
                                "keymap-trigger-option-global",
                                "Global",
                                modal.draft.trigger_options.global,
                                TriggerOption::Global,
                            ),
                            (
                                "keymap-trigger-option-all",
                                "All surfaces",
                                modal.draft.trigger_options.all,
                                TriggerOption::All,
                            ),
                            (
                                "keymap-trigger-option-unconsumed",
                                "Pass-through",
                                modal.draft.trigger_options.unconsumed,
                                TriggerOption::Unconsumed,
                            ),
                            (
                                "keymap-trigger-option-side-sensitive",
                                "Modifier side",
                                modal.draft.trigger_options.side_sensitive,
                                TriggerOption::SideSensitive,
                            ),
                        ]
                        .into_iter()
                        .map(|(id, label, selected, option)| {
                            ComponentButton::new(id)
                                .label(label)
                                .selected(selected)
                                .toggled(true)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_modal_trigger_option(option, cx);
                                }))
                        }),
                    ),
            )
            .when_some(self.snapshot.prefix.clone(), |options, prefix| {
                options.child(
                    ComponentButton::new("keymap-trigger-option-prefix")
                        .label(format!("Prefix {prefix}"))
                        .outline()
                        .compact()
                        .with_size(Size::Small)
                        .selected(modal.draft.trigger_options.prefixed)
                        .toggled(true)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_modal_trigger_option(TriggerOption::Prefixed, cx);
                        })),
                )
            })
            .into_any_element()
    }

    fn render_modal_keystroke_section(
        &self,
        modal: &BindingModal,
        modal_focus: &FocusHandle,
        matching_bindings_count: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        gpui_kit::div()
            .flex()
            .flex_col()
            .gap_1()
            .child(Label::new("Edit Keystroke"))
            .child(Self::render_modal_keystroke_input(
                modal,
                modal_focus.clone(),
                cx,
            ))
            .child(self.render_modal_trigger_options(modal, cx))
            .child(gpui_kit::div().flex().items_center().gap_px().when(
                matching_bindings_count > 0,
                |matching| {
                    matching
                        .child(
                            Label::new(format!(
                                "There {} {matching_bindings_count} {} with the same keystrokes.",
                                if matching_bindings_count == 1 {
                                    "is"
                                } else {
                                    "are"
                                },
                                if matching_bindings_count == 1 {
                                    "binding"
                                } else {
                                    "bindings"
                                }
                            ))
                            .text_sm()
                            .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            Button::new("show_matching")
                                .label("View")
                                .text_sm()
                                .icon(
                                    Icon::new(IconName::ExternalLink)
                                        .small()
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show_matching_modal_bindings(window, cx);
                                })),
                        )
                },
            ))
            .into_any_element()
    }

    fn render_modal_arguments(
        arguments_editor: &Entity<EditorState>,
        action: Option<&KeymapActionSnapshot>,
        cx: &App,
    ) -> AnyElement {
        gpui_kit::div()
            .flex()
            .flex_col()
            .gap_1()
            .child(Label::new("Edit Arguments"))
            .child(
                div()
                    .id("keymap-modal-arguments")
                    .debug_selector(|| "keymap-modal-arguments".to_owned())
                    .w_full()
                    .child(crate::gpui::focus_input(
                        arguments_editor,
                        ComponentEditor::new(arguments_editor)
                            .h(px(96.0))
                            .aria_label("JSON arguments"),
                    )),
            )
            .when_some(action, |arguments, action| {
                arguments.child(gpui_kit::div().flex().flex_col().gap_0p5().children(
                    action.arguments.iter().map(|argument| {
                        Label::new(argument_description(argument))
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                    }),
                ))
            })
            .into_any_element()
    }

    fn render_modal_action(
        action_combobox: &Entity<ComboboxState<ActionComboboxDelegate>>,
    ) -> AnyElement {
        gpui_kit::div()
            .flex()
            .flex_col()
            .gap_1()
            .child(Label::new("Action"))
            .child(
                div()
                    .id("keymap-modal-action")
                    .debug_selector(|| "keymap-modal-action".to_owned())
                    .w_full()
                    // The shared Combobox propagates Confirm after opening/selecting.
                    // That action belongs to this field, not the parent dialog's Save.
                    .on_action(|_: &gpui_kit::component::dialog::Confirm, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        Combobox::new(action_combobox)
                            .placeholder("Choose an action…")
                            .search_placeholder("Search actions…")
                            .with_size(Size::Small)
                            .w_full(),
                    ),
            )
            .into_any_element()
    }

    // Direct port of Zed's KeybindingEditorModal from keymap_editor.rs:3055-3196. The action
    // catalog uses gpui-component's retained Combobox so filtering, selection, and dismissal keep
    // the same focus contract as the rest of Bootty's Zed-derived controls.
    fn render_modal_body(&self, modal_focus: &FocusHandle, cx: &Context<Self>) -> AnyElement {
        let Some(modal) = self.modal.as_ref() else {
            return div().into_any_element();
        };
        let action = self
            .snapshot
            .actions
            .iter()
            .find(|action| action.id == modal.draft.action)
            .cloned();
        let ignored_id = match &modal.origin {
            ModalOrigin::Create { .. } => None,
            ModalOrigin::Replace(binding) => Some(binding.id.as_str()),
        };
        let matching_bindings_count = self.draft_conflict_count(&modal.draft, ignored_id);
        let mut body = gpui_kit::div().flex().flex_col().w_full().gap_2p5();
        if let ModalOrigin::Create {
            combobox: action_combobox,
            ..
        } = &modal.origin
        {
            body = body.child(Self::render_modal_action(action_combobox));
        }

        body = body.child(self.render_modal_keystroke_section(
            modal,
            modal_focus,
            matching_bindings_count,
            cx,
        ));

        if action
            .as_ref()
            .is_some_and(|action| !action.arguments.is_empty())
            || modal.draft.arguments_json.is_some()
        {
            body = body.child(Self::render_modal_arguments(
                &modal.controls.arguments,
                action.as_ref(),
                cx,
            ));
        }

        let context_editor = &modal.controls.context;
        body = body.child(
            gpui_kit::div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Label::new("Edit Context"))
                .child(
                    div()
                        .id("keymap-modal-context")
                        .debug_selector(|| "keymap-modal-context".to_owned())
                        .w_full()
                        .aria_label("Keybinding context")
                        .child(crate::gpui::focus_input(
                            context_editor,
                            Input::new(context_editor).with_size(Size::Small).w_full(),
                        )),
                ),
        );

        if let Some(error) = &modal.error {
            body = body.child(Alert::new("keymap-validation", error.clone()).with_variant(
                if error.contains("Save again") {
                    AlertVariant::Warning
                } else {
                    AlertVariant::Error
                },
            ));
        }

        body.id("keymap-modal")
            .debug_selector(|| "keymap-modal".to_owned())
            .track_focus(modal_focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(
                cx.listener(|this, event: &gpui_kit::ScrollWheelEvent, _, cx| {
                    if this.modal.as_ref().is_some_and(|modal| modal.recording)
                        && !event.delta.pixel_delta(px(1.0)).y.is_zero()
                    {
                        let up = event.delta.pixel_delta(px(1.0)).y > px(0.0);
                        let step = wheel_keystroke(up, event.modifiers);
                        this.record_modal_keystroke(&step, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .into_any_element()
    }
}

impl Render for BindingModalBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.editor
            .update(cx, |editor, cx| editor.render_modal_body(&self.focus, cx))
    }
}

impl EventEmitter<KeymapEditorIntent> for GpuiKeymapEditor {}

impl Focusable for GpuiKeymapEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for GpuiKeymapEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search_editor = self.ensure_search_editor(window, cx);
        let table = self.ensure_table(window, cx);
        gpui_kit::div()
            .flex()
            .flex_col()
            .id("keymap-editor")
            .track_focus(&self.focus)
            .key_context("KeymapEditor")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(
                cx.listener(|this, event: &gpui_kit::ScrollWheelEvent, _, cx| {
                    if this.modal.as_ref().is_some_and(|modal| modal.recording)
                        && !event.delta.pixel_delta(px(1.0)).y.is_zero()
                    {
                        let up = event.delta.pixel_delta(px(1.0)).y > px(0.0);
                        let step = wheel_keystroke(up, event.modifiers);
                        this.record_modal_keystroke(&step, cx);
                        cx.stop_propagation();
                        return;
                    }
                    if !event.delta.pixel_delta(px(1.0)).y.is_zero()
                        && this.context_menu.take().is_some()
                    {
                        cx.notify();
                    }
                }),
            )
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .p_2()
            .gap_1()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_toolbar(&search_editor, cx))
            .when_some(self.snapshot.diagnostic.clone(), |editor, diagnostic| {
                editor.child(Alert::error("keymap-diagnostic", diagnostic))
            })
            .child(self.render_table(&table))
    }
}

const fn search_placeholder(mode: KeymapSearchMode) -> &'static str {
    match mode {
        KeymapSearchMode::Action => "Search actions",
        KeymapSearchMode::Keystroke => "Search keystrokes",
    }
}

fn empty_cell() -> AnyElement {
    Label::new("").into_any_element()
}

fn public_selector(id: impl Into<String>, child: impl IntoElement) -> AnyElement {
    let id = id.into();
    let debug_selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || debug_selector)
        .flex()
        .child(child)
        .into_any_element()
}

fn text_cell(text: impl Into<SharedString>) -> AnyElement {
    Label::new(text).truncate().into_any_element()
}

fn muted_text(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    Label::new(text)
        .truncate()
        .text_color(cx.theme().muted_foreground)
        .into_any_element()
}

fn row_identity(row: &KeymapEditorRow) -> String {
    match row {
        KeymapEditorRow::Binding { binding, .. } => format!("binding-{}", binding.id),
        KeymapEditorRow::Unmapped(action) => format!("unmapped-{}", action.id),
    }
}

fn row_group_id(identity: &str) -> SharedString {
    SharedString::from(format!("keymap-table-row-{identity}"))
}

// Shared geometry for keybinding capture and keystroke search.
fn keystroke_input(
    id: &'static str,
    recording: bool,
    indicator: (&'static str, gpui_kit::Hsla),
    placeholder: &'static str,
    steps: &[String],
    actions: Div,
    cx: &App,
) -> AnyElement {
    let colors = cx.theme().colors;
    let width = rems(4.0);
    let indicator = gpui_kit::div()
        .flex()
        .items_center()
        .h_4()
        .pr_1()
        .gap_0p5()
        .border_1()
        .border_color(colors.border)
        .bg(colors.background.blend(colors.primary.opacity(0.1)))
        .rounded_sm()
        .child(
            Icon::default()
                .path("icons/circle.svg")
                .small()
                .text_color(indicator.1),
        )
        .child(
            Label::new(indicator.0)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(indicator.1),
        );
    gpui_kit::div()
        .flex()
        .items_center()
        .id(id)
        .debug_selector(move || id.to_owned())
        .py_2()
        .px_3()
        .gap_2()
        .min_h_10()
        .w_full()
        .flex_1()
        .justify_between()
        .rounded_md()
        .overflow_hidden()
        .bg(if recording {
            colors.background.blend(colors.primary.opacity(0.1))
        } else {
            colors.background
        })
        .border_1()
        .border_color(if recording { colors.ring } else { colors.input })
        .child(
            gpui_kit::div()
                .flex()
                .items_center()
                .w(width)
                .gap_0p5()
                .justify_start()
                .flex_none()
                .when(recording, |this| this.child(indicator)),
        )
        .child(
            gpui_kit::div()
                .flex()
                .items_center()
                .size_full()
                .min_w_0()
                .justify_center()
                .flex_wrap()
                .gap_1()
                .child(if steps.is_empty() {
                    Label::new(placeholder)
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .into_any_element()
                } else {
                    keystroke_cell(steps)
                }),
        )
        .child(actions.w(width).gap_0p5().justify_end().flex_none())
        .into_any_element()
}

fn keystroke_cell(steps: &[String]) -> AnyElement {
    crate::gpui::keybinding_element_from_text(&steps.join(" > "))
}

fn table_keystroke_cell(identity: &str, steps: &[String]) -> AnyElement {
    let selector = format!("keymap-keystrokes-{identity}");
    let debug_selector = selector.clone();
    div()
        .id(SharedString::from(selector))
        .debug_selector(move || debug_selector)
        .h_full()
        .flex()
        .items_center()
        .child(keystroke_cell(steps))
        .into_any_element()
}

fn binding_source_label(binding: &KeymapBindingSnapshot) -> String {
    match binding.kind {
        KeymapBindingKind::Unbind => format!("{} · Unbound", binding.source.label()),
        KeymapBindingKind::Binding if binding.action == "ignore" => {
            format!("{} · Consume", binding.source.label())
        }
        KeymapBindingKind::Binding => binding.source.label().to_owned(),
    }
}

fn validate_modal_keystrokes(steps: &[String]) -> Result<(), &'static str> {
    let source = steps.join(" > ");
    let Some(keystrokes) = crate::gpui::keybinding::parse_keybinding(&source) else {
        return Err(
            "Invalid keybinding. Use modifiers and keys separated by '+'; use '>' for chords.",
        );
    };
    for keystroke in keystrokes {
        let key = &keystroke.inner().key;
        if keystroke.inner().modifiers.function {
            return Err(
                "The Fn modifier is not supported by Bootty keybindings. Record a shortcut without Fn.",
            );
        }
        if key
            .strip_prefix('f')
            .and_then(|number| number.parse::<u8>().ok())
            .is_some_and(|number| !(1..=12).contains(&number))
        {
            return Err(
                "Function keys above F12 are not supported by Bootty keybindings. Record F1–F12 or another key.",
            );
        }
    }
    Ok(())
}

fn wheel_keystroke(up: bool, modifiers: gpui_kit::Modifiers) -> String {
    let mut parts = Vec::new();
    if modifiers.platform {
        parts.push("cmd");
    }
    if modifiers.control {
        parts.push("ctrl");
    }
    if modifiers.alt {
        parts.push("alt");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    parts.push(if up { "scroll_up" } else { "scroll_down" });
    parts.join("+")
}

fn set_step_side_sensitivity(step: &str, side_sensitive: bool) -> String {
    const MODIFIERS: [&str; 12] = [
        "cmd",
        "ctrl",
        "alt",
        "shift",
        "left_cmd",
        "left_ctrl",
        "left_alt",
        "left_shift",
        "right_cmd",
        "right_ctrl",
        "right_alt",
        "right_shift",
    ];
    let (mut modifiers, key) = if step.contains('+') {
        let mut parts = step.split('+').collect::<Vec<_>>();
        let Some(key) = parts.pop() else {
            return step.to_owned();
        };
        (parts, key.to_owned())
    } else {
        let mut rest = step;
        let mut parts = Vec::new();
        while let Some((candidate, tail)) = rest.split_once('-') {
            if !MODIFIERS.contains(&candidate) {
                break;
            }
            parts.push(candidate);
            rest = tail;
        }
        (parts, rest.to_owned())
    };
    if key.is_empty() {
        return step.to_owned();
    }
    for modifier in &mut modifiers {
        *modifier = if side_sensitive {
            match *modifier {
                "cmd" => "left_cmd",
                "ctrl" => "left_ctrl",
                "alt" => "left_alt",
                "shift" => "left_shift",
                value => value,
            }
        } else {
            match *modifier {
                "left_cmd" | "right_cmd" => "cmd",
                "left_ctrl" | "right_ctrl" => "ctrl",
                "left_alt" | "right_alt" => "alt",
                "left_shift" | "right_shift" => "shift",
                value => value,
            }
        };
    }
    modifiers.push(&key);
    modifiers.join("+")
}

/// Convert GPUI's platform-pretty display keystroke into Bootty's durable trigger grammar.
/// `Keystroke::to_string()` intentionally yields strings such as `⌘K` on macOS, which are for
/// presentation and cannot round-trip through `config.toml`.
fn persisted_keystroke(keystroke: &Keystroke) -> String {
    let mut parts = Vec::with_capacity(6);
    if keystroke.modifiers.platform {
        parts.push("cmd".to_owned());
    }
    if keystroke.modifiers.control {
        parts.push("ctrl".to_owned());
    }
    if keystroke.modifiers.alt {
        parts.push("alt".to_owned());
    }
    if keystroke.modifiers.shift {
        parts.push("shift".to_owned());
    }
    if keystroke.modifiers.function {
        parts.push("fn".to_owned());
    }
    parts.push(persisted_key_name(&keystroke.key));
    parts.join("+")
}

fn persisted_key_name(key: &str) -> String {
    match key {
        "up" => "ArrowUp".to_owned(),
        "down" => "ArrowDown".to_owned(),
        "left" => "ArrowLeft".to_owned(),
        "right" => "ArrowRight".to_owned(),
        "pageup" => "PageUp".to_owned(),
        "pagedown" => "PageDown".to_owned(),
        "backspace" => "Backspace".to_owned(),
        "delete" => "Delete".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "space" => "Space".to_owned(),
        "insert" => "Insert".to_owned(),
        "enter" => "Enter".to_owned(),
        "tab" => "Tab".to_owned(),
        "escape" => "Escape".to_owned(),
        key if key.len() == 1 => key.to_ascii_lowercase(),
        key => key.to_owned(),
    }
}

fn argument_description(argument: &KeymapArgumentSnapshot) -> String {
    let mut description = format!("{}: {}", argument.name, argument.kind.label());
    if argument.required {
        description.push_str(" (required)");
    }
    if !argument.choices.is_empty() {
        let _ = write!(description, " — {}", argument.choices.join(", "));
    } else if argument.minimum.is_some() || argument.maximum.is_some() {
        let _ = write!(
            description,
            " — {}..{}",
            argument
                .minimum
                .map_or_else(|| "−∞".to_owned(), |value| value.to_string()),
            argument
                .maximum
                .map_or_else(|| "∞".to_owned(), |value| value.to_string())
        );
    }
    description
}

fn action_search(action: &KeymapActionSnapshot) -> String {
    action.title.clone()
}

fn ranked_action_indices(actions: &[KeymapActionSnapshot], query: &str) -> Vec<usize> {
    let query = normalize_action_query(query);
    if query.is_empty() {
        return (0..actions.len()).collect();
    }

    let mut matches = actions
        .iter()
        .enumerate()
        .filter_map(|(index, action)| {
            fuzzy_match(&action.title, &query).map(|matched| (index, matched.score))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    matches.into_iter().map(|(index, _)| index).collect()
}

/// Direct port of Zed's action-query normalization. Action IDs such as `new_tab` then match the
/// humanized action titles used by both the keymap table and completion list.
fn normalize_action_query(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut last_char = None;

    for character in input.trim().chars() {
        let character = if character == '_' { ' ' } else { character };
        match (last_char, character) {
            (Some(':'), ':') => continue,
            (Some(previous), current) if previous.is_whitespace() && current.is_whitespace() => {
                continue;
            }
            _ => last_char = Some(character),
        }
        result.push(character);
    }

    result
}

fn normalize_context(context: &str) -> String {
    context.trim().to_ascii_lowercase()
}

fn normalize_keystrokes(steps: &[String]) -> String {
    steps
        .iter()
        .map(|step| normalize_keystroke_query(step))
        .filter(|step| !step.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_keystroke_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}
