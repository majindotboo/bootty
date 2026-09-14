//! Native settings view, draft ownership, and retained controls.
//!
//! The list/page/root render path is ported from Zed's `settings_ui` at commit
//! `1662f5f3f6` (`settings_ui.rs:3433-3646,3747-4078,4510-4597`).

use gpui_kit::component::{button::Button, label::Label};

use std::collections::{HashMap, HashSet};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{
    InputEvent as ComponentInputEvent, InputState as ComponentInputState,
};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable as _, IntoElement,
    KeyDownEvent, ListAlignment, ListOffset, ListState, ParentElement, Render, SharedString,
    Styled, UniformListScrollHandle, Window, div, list, prelude::*, px,
};

use crate::gpui::setup_ui_font;

use super::{
    components::{NavBarEntry, settings_row_identity},
    font_features::{FontFeatureEditorEvent, FontFeatureEditorSnapshot, GpuiFontFeatureEditor},
    model::{
        SettingsCategory, SettingsContent, SettingsControl, SettingsIntent, SettingsPage,
        SettingsPageItem, SettingsRow,
    },
    search::{SearchIndex, SearchMatches},
};

gpui_kit::actions!(
    settings_editor,
    [
        /// Toggles focus between the settings navigation and main content.
        #[derive(Eq)]
        ToggleFocusNav
    ]
);

pub(super) const NAVBAR_CONTAINER_TAB_INDEX: isize = 0;
pub(super) const NAVBAR_GROUP_TAB_INDEX: isize = 1;
pub(super) const CONTENT_CONTAINER_TAB_INDEX: isize = 4;
pub(super) const CONTENT_GROUP_TAB_INDEX: isize = 5;

/// Settings owns its draft, navigation, and retained controls.
pub struct GpuiSettings {
    pub(crate) draft: crate::settings_session::SettingsSession,
    pub(super) content: SettingsContent,
    pub(super) category: SettingsCategory,
    pub(super) search: String,
    pub(super) active_editor: Option<String>,
    pub(super) focus: FocusHandle,
    pub(super) navbar_focus: FocusHandle,
    pub(super) content_focus: FocusHandle,
    pub(super) content_focus_handles: HashMap<SettingsCategory, HashMap<String, FocusHandle>>,
    pub(super) search_input: Option<Entity<ComponentInputState>>,
    pub(super) font_feature_editor: Option<FontFeatureEditor>,
    pub(super) search_index: SearchIndex,
    pub(super) filter_table: Vec<Vec<bool>>,
    pub(super) has_query: bool,
    pub(super) navbar_entries: Vec<NavBarEntry>,
    pub(super) navbar_scroll_handle: UniformListScrollHandle,
    pub(super) list_state: ListState,
    pub(super) last_layout_font: Option<(gpui_kit::Font, gpui_kit::Pixels)>,
    pub(super) active_section: Option<String>,
    pub(super) pending_section: Option<String>,
    pub(super) pending_content_focus: Option<(SettingsCategory, Option<String>)>,
    pub(super) choice_focus_handles: HashMap<String, FocusHandle>,
}

pub(super) struct FontFeatureEditor {
    pub(super) editor: Entity<GpuiFontFeatureEditor>,
    _subscription: gpui_kit::Subscription,
}

impl GpuiSettings {
    pub fn new(
        mut snapshot: SettingsContent,
        draft: crate::settings_session::SettingsSession,
        cx: &mut Context<Self>,
    ) -> Self {
        normalize_navigation_titles(&mut snapshot);
        let search_index = SearchIndex::build(&snapshot.pages);
        let SearchMatches {
            table, has_query, ..
        } = search_index.matches(&snapshot.pages, "");
        let list_state = ListState::new(0, ListAlignment::Top, px(0.0)).measure_all();
        list_state.set_scroll_handler(cx.listener(
            |this, event: &gpui_kit::ListScrollEvent, _, cx| {
                this.update_active_section(event.visible_range.start);
                cx.notify();
            },
        ));
        let mut settings = Self {
            draft,
            content: snapshot,
            category: SettingsCategory::default(),
            search: String::new(),
            active_editor: None,
            focus: cx.focus_handle(),
            navbar_focus: cx
                .focus_handle()
                .tab_index(NAVBAR_CONTAINER_TAB_INDEX)
                .tab_stop(false),
            content_focus: cx
                .focus_handle()
                .tab_index(CONTENT_CONTAINER_TAB_INDEX)
                .tab_stop(false),
            content_focus_handles: HashMap::new(),
            search_input: None,
            font_feature_editor: None,
            search_index,
            filter_table: table,
            has_query,
            navbar_entries: Vec::new(),
            navbar_scroll_handle: UniformListScrollHandle::default(),
            list_state,
            last_layout_font: None,
            active_section: None,
            pending_section: None,
            pending_content_focus: None,
            choice_focus_handles: HashMap::new(),
        };
        settings.sync_choice_focus_handles(cx);
        settings.sync_content_focus_handles(cx);
        settings.sync_font_feature_editor(cx);
        settings.rebuild_navbar(cx);
        settings.reset_list_state();
        settings
    }

    /// Construct settings controls that require a window before the first frame is rendered.
    pub fn new_with_window(
        snapshot: SettingsContent,
        draft: crate::settings_session::SettingsSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(snapshot, draft, cx);
        this.ensure_search_input(window, cx);
        if let Some(editor) = &this.font_feature_editor {
            editor.editor.update(cx, |editor, cx| {
                editor.ensure_inputs(window, cx);
            });
        }
        this
    }

    pub(super) fn focus_editor(&mut self, editor: Option<String>, cx: &mut Context<Self>) {
        self.active_editor = editor;
        cx.notify();
    }

    pub fn select_category(&mut self, category: SettingsCategory, cx: &mut Context<Self>) {
        self.category = category;
        self.active_editor = None;
        if let Some(entry) = self
            .navbar_entries
            .iter_mut()
            .find(|entry| entry.is_root && entry.category == category)
        {
            entry.expanded = true;
        }
        self.reset_list_state();
        self.active_section = self.pending_section.clone();
        if let Some(section) = self.pending_section.take() {
            self.scroll_to_section(&section, cx);
        } else {
            self.list_state.scroll_to_reveal_item(0);
        }
        cx.notify();
    }

    /// Publish changed content without rebuilding controls or disturbing scroll for equal input.
    pub fn set_content(&mut self, mut snapshot: SettingsContent, cx: &mut Context<Self>) -> bool {
        normalize_navigation_titles(&mut snapshot);
        if self.content == snapshot {
            return false;
        }
        let old_structure =
            visible_structure_signature(&self.content, self.category, &self.filter_table);
        self.content = snapshot;
        self.sync_choice_focus_handles(cx);
        self.sync_content_focus_handles(cx);
        self.sync_font_feature_editor(cx);
        self.search_index = SearchIndex::build(&self.content.pages);
        let matches = self.search_index.matches(&self.content.pages, &self.search);
        self.filter_table = matches.table;
        self.has_query = matches.has_query;
        let structure_changed = old_structure
            != visible_structure_signature(&self.content, self.category, &self.filter_table);
        self.rebuild_navbar(cx);
        if structure_changed {
            self.reset_list_state();
        } else {
            // Value-only publications must not throw away the user's scroll position.
            // Remeasure because errors and custom editors can still change row height.
            self.list_state.remeasure();
        }

        cx.notify();
        true
    }

    fn sync_choice_focus_handles(&mut self, cx: &gpui_kit::App) {
        let selectors = choice_selectors(&self.content);
        self.choice_focus_handles
            .retain(|selector, _| selectors.contains(selector));
        for selector in selectors {
            self.choice_focus_handles
                .entry(selector)
                .or_insert_with(|| cx.focus_handle().tab_index(0).tab_stop(true));
        }
    }

    fn sync_content_focus_handles(&mut self, cx: &gpui_kit::App) {
        let mut retained = std::mem::take(&mut self.content_focus_handles);
        let mut handles_by_page = HashMap::with_capacity(self.content.pages.len());
        for page in &self.content.pages {
            let retained_page = retained.remove(&page.category).unwrap_or_default();
            let mut page_handles = HashMap::with_capacity(page.items.len());
            for (item_index, item) in page.items.iter().enumerate() {
                let item_id = settings_page_item_id(item, item_index);
                let focus_handle = retained_page
                    .get(&item_id)
                    .cloned()
                    .unwrap_or_else(|| cx.focus_handle().tab_index(0).tab_stop(false));
                page_handles.insert(item_id, focus_handle);
            }
            handles_by_page.insert(page.category, page_handles);
        }
        self.content_focus_handles = handles_by_page;
    }

    fn content_focus_handle(&self, page_index: usize, item_index: usize) -> Option<FocusHandle> {
        let page = self.content.pages.get(page_index)?;
        let item = page.items.get(item_index)?;
        let item_id = settings_page_item_id(item, item_index);
        self.content_focus_handles
            .get(&page.category)
            .and_then(|handles| handles.get(&item_id))
            .cloned()
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub(super) fn emit(&mut self, intent: SettingsIntent, cx: &mut Context<Self>) {
        if matches!(
            intent,
            SettingsIntent::ReplaceAnsiPalette { .. }
                | SettingsIntent::AddStringListItem(_)
                | SettingsIntent::RemoveStringListItem { .. }
                | SettingsIntent::MoveStringListItem { .. }
                | SettingsIntent::AddModifierRemap
                | SettingsIntent::RemoveModifierRemap(_)
                | SettingsIntent::MoveModifierRemap { .. }
                | SettingsIntent::RemoveEnvironmentVariable(_)
                | SettingsIntent::MoveEnvironmentVariable { .. }
        ) {
            self.active_editor = None;
        }
        cx.emit(intent);
        cx.notify();
    }

    fn sync_font_feature_editor(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = font_feature_editor_snapshot(&self.content).cloned() else {
            self.font_feature_editor = None;
            return;
        };
        if let Some(editor) = &self.font_feature_editor {
            editor
                .editor
                .update(cx, |editor, cx| editor.set_snapshot(snapshot, cx));
            return;
        }

        let editor = cx.new(|cx| GpuiFontFeatureEditor::new(snapshot, cx));
        let subscription =
            cx.subscribe(
                &editor,
                |this, _, event: &FontFeatureEditorEvent, cx| match event {
                    FontFeatureEditorEvent::Replace(features) => {
                        this.emit(SettingsIntent::ReplaceFontFeatures(features.clone()), cx);
                    }
                },
            );
        self.font_feature_editor = Some(FontFeatureEditor {
            editor,
            _subscription: subscription,
        });
    }

    pub fn apply_search(&mut self, query: &str, cx: &mut Context<Self>) {
        query.clone_into(&mut self.search);
        let matches = self.search_index.matches(&self.content.pages, query);
        self.filter_table = matches.table;
        self.has_query = matches.has_query;
        self.reset_list_state();

        if let Some(page_index) = matches.best_page
            && let Some(page) = self.content.pages.get(page_index)
            && page.category != self.category
        {
            self.active_section = None;
            self.select_category(page.category, cx);
        } else {
            cx.notify();
        }
    }

    fn ensure_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ComponentInputState> {
        let initial = self.search.clone();
        let input = self.search_input.get_or_insert_with(|| {
            let input = cx.new(|cx| {
                ComponentInputState::new(window, cx)
                    .default_value(initial)
                    .placeholder(crate::i18n::t(cx, "settings-search-placeholder"))
            });
            cx.subscribe(&input, |this, input, event: &ComponentInputEvent, cx| {
                if !matches!(event, ComponentInputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                this.apply_search(&value, cx);
            })
            .detach();
            input
        });
        let placeholder = crate::i18n::t(cx, "settings-search-placeholder");
        if input.read(cx).presentation().placeholder().as_ref() != placeholder {
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx);
            });
        }
        let search = self.search.as_str();
        if input.read(cx).value().as_ref() != search {
            input.update(cx, |input, cx| input.set_value(search, window, cx));
        }
        input.clone()
    }

    pub(super) fn current_page_index(&self) -> usize {
        self.content
            .pages
            .iter()
            .position(|page| page.category == self.category)
            .unwrap_or(0)
    }

    fn current_page(&self) -> Option<&SettingsPage> {
        self.content.pages.get(self.current_page_index())
    }

    pub(super) fn visible_page_item_indices(&self) -> Vec<usize> {
        let page_index = self.current_page_index();
        self.current_page()
            .into_iter()
            .flat_map(|page| 0..page.items.len())
            .filter(|item_index| {
                self.filter_table
                    .get(page_index)
                    .and_then(|items| items.get(*item_index))
                    .copied()
                    .unwrap_or(false)
            })
            .collect()
    }

    pub(super) fn reset_list_state(&self) {
        let count = self.visible_page_item_indices().len();
        self.list_state.reset(if count == 0 && self.has_query {
            1
        } else {
            count
        });
    }

    fn update_active_section(&mut self, logical_index: usize) {
        let visible = self.visible_page_item_indices();
        self.active_section = self.current_page().and_then(|page| {
            visible
                .iter()
                .take(logical_index.saturating_add(1))
                .rev()
                .find_map(|&index| match page.items.get(index) {
                    Some(SettingsPageItem::SectionHeader { id, .. }) => Some(id.clone()),
                    _ => None,
                })
        });
    }

    pub(super) fn scroll_to_section(&self, section: &str, cx: &mut Context<Self>) {
        let Some(page) = self.current_page() else {
            return;
        };
        let visible = self.visible_page_item_indices();
        let Some(actual_index) = page.items.iter().position(
            |item| matches!(item, SettingsPageItem::SectionHeader { id, .. } if id == section),
        ) else {
            return;
        };
        if let Some(logical_index) = visible.iter().position(|index| *index == actual_index) {
            self.list_state.scroll_to(ListOffset {
                item_ix: logical_index,
                offset_in_item: px(0.0),
            });
            cx.notify();
        }
    }

    fn selected_navbar_entry_index(&self) -> usize {
        self.navbar_entries
            .iter()
            .position(|entry| {
                entry.category == self.category
                    && if entry.is_root {
                        self.active_section.is_none()
                    } else {
                        entry.section_id.as_deref() == self.active_section.as_deref()
                    }
            })
            .or_else(|| {
                self.navbar_entries
                    .iter()
                    .position(|entry| entry.is_root && entry.category == self.category)
            })
            .unwrap_or(0)
    }

    fn focused_navbar_entry(&self, window: &Window, cx: &gpui_kit::App) -> Option<usize> {
        if !self.navbar_focus.contains_focused(window, cx) {
            return None;
        }
        self.navbar_entries
            .iter()
            .position(|entry| entry.focus_handle.is_focused(window))
    }

    fn root_entry_containing(&self, entry_index: usize) -> Option<usize> {
        self.navbar_entries
            .iter()
            .enumerate()
            .take(entry_index.saturating_add(1))
            .rev()
            .find_map(|(index, entry)| entry.is_root.then_some(index))
    }

    pub(super) fn toggle_and_focus_navbar_entry(
        &mut self,
        entry_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.navbar_entries.get_mut(entry_index) else {
            return;
        };
        if !entry.is_root {
            return;
        }
        entry.expanded = !entry.expanded;
        let focus_handle = entry.focus_handle.clone();
        self.activate_navbar_entry(entry_index, false, false, window, cx);
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn activate_navbar_entry(
        &mut self,
        entry_index: usize,
        focus_content: bool,
        expand_collapsed_root: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.navbar_entries.get_mut(entry_index) else {
            return;
        };
        let category = entry.category;
        let section_id = entry.section_id.clone();
        let is_root = entry.is_root;

        if is_root && expand_collapsed_root {
            entry.expanded = true;
        }
        self.active_section = section_id.clone();
        if focus_content {
            self.pending_content_focus = Some((category, section_id.clone()));
        }

        if self.category != category {
            self.pending_section = section_id;
            self.select_category(category, cx);
            return;
        }

        if let Some(section_id) = &section_id {
            self.scroll_to_section(section_id, cx);
        } else {
            self.list_state.scroll_to_reveal_item(0);
            cx.notify();
        }
        if focus_content {
            self.schedule_pending_content_focus(window, cx);
        }
    }

    fn focus_and_scroll_to_navbar_entry(
        &mut self,
        entry_index: usize,
        scroll_strategy: gpui_kit::ScrollStrategy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(logical_index) = self
            .visible_navbar_indices()
            .iter()
            .position(|index| *index == entry_index)
        else {
            return;
        };
        self.navbar_scroll_handle
            .scroll_to_item(logical_index, scroll_strategy);
        self.activate_navbar_entry(entry_index, false, false, window, cx);
        let Some(focus_handle) = self
            .navbar_entries
            .get(entry_index)
            .map(|entry| entry.focus_handle.clone())
        else {
            return;
        };
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn on_nav_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let search_focused = self
            .search_input
            .as_ref()
            .is_some_and(|input| input.focus_handle(cx).is_focused(window));
        if !search_focused && !self.navbar_focus.contains_focused(window, cx) {
            return;
        }

        let visible = self.visible_navbar_indices();
        if visible.is_empty() {
            return;
        }
        let current = self
            .focused_navbar_entry(window, cx)
            .unwrap_or_else(|| self.selected_navbar_entry_index());
        let current_position = visible.iter().position(|index| *index == current);
        let key = event.keystroke.key.as_str();
        let target = match (key, modifiers.shift) {
            ("up", false) | ("tab", true) => current_position
                .and_then(|position| position.checked_sub(1))
                .and_then(|position| visible.get(position).copied()),
            ("down" | "tab", false) => current_position
                .and_then(|position| visible.get(position.saturating_add(1)).copied())
                .or_else(|| search_focused.then(|| visible.first().copied()).flatten()),
            ("pageup", false) => visible
                .iter()
                .copied()
                .take_while(|index| *index < current)
                .filter(|index| {
                    self.navbar_entries
                        .get(*index)
                        .is_some_and(|entry| entry.is_root)
                })
                .last(),
            ("pagedown", false) => visible.iter().copied().find(|index| {
                *index > current
                    && self
                        .navbar_entries
                        .get(*index)
                        .is_some_and(|entry| entry.is_root)
            }),
            ("home", false) => visible.first().copied(),
            ("end", false) => visible.last().copied(),
            ("right", false) => {
                if let Some(entry) = self
                    .navbar_entries
                    .get_mut(current)
                    .filter(|entry| entry.is_root && !entry.expanded)
                {
                    entry.expanded = true;
                    self.focus_and_scroll_to_navbar_entry(
                        current,
                        gpui_kit::ScrollStrategy::Top,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }
                return;
            }
            ("left", false) => {
                let Some(root_index) = self.root_entry_containing(current) else {
                    return;
                };
                if let Some(entry) = self.navbar_entries.get_mut(root_index) {
                    entry.expanded = false;
                }
                self.focus_and_scroll_to_navbar_entry(
                    root_index,
                    gpui_kit::ScrollStrategy::Top,
                    window,
                    cx,
                );
                cx.stop_propagation();
                return;
            }
            _ => return,
        };

        let Some(target) = target else {
            if key == "tab" && !modifiers.shift {
                self.focus_settings_content(window, cx);
            }
            cx.stop_propagation();
            return;
        };
        let strategy = if target < current {
            gpui_kit::ScrollStrategy::Top
        } else {
            gpui_kit::ScrollStrategy::Bottom
        };
        self.focus_and_scroll_to_navbar_entry(target, strategy, window, cx);
        cx.stop_propagation();
    }

    fn schedule_pending_content_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((category, section_id)) = self.pending_content_focus.clone() else {
            return;
        };
        if category != self.category {
            return;
        }

        let page_index = self.current_page_index();
        let visible = self.visible_page_item_indices();
        let actual_index = section_id
            .as_deref()
            .and_then(|section_id| {
                self.content
                    .pages
                    .get(page_index)?
                    .items
                    .iter()
                    .position(|item| {
                        matches!(
                            item,
                            SettingsPageItem::SectionHeader { id, .. } if id == section_id
                        )
                    })
            })
            .filter(|index| visible.contains(index))
            .or_else(|| visible.first().copied());
        let Some(actual_index) = actual_index else {
            self.pending_content_focus = None;
            return;
        };
        let Some(logical_index) = visible.iter().position(|index| *index == actual_index) else {
            self.pending_content_focus = None;
            return;
        };
        let Some(focus_handle) = self.content_focus_handle(page_index, actual_index) else {
            self.pending_content_focus = None;
            return;
        };

        self.pending_content_focus = None;
        self.list_state.scroll_to_reveal_item(logical_index);
        cx.on_next_frame(window, move |_, window, cx| {
            cx.notify();
            cx.on_next_frame(window, move |_, window, cx| {
                window.focus(&focus_handle, cx);
                window.focus_next(cx);
                cx.notify();
            });
        });
        cx.notify();
    }

    pub(super) fn focus_settings_content(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.content_focus, cx);
        window.focus_next(cx);
        cx.notify();
    }

    pub(super) fn focus_settings_navigation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.selected_navbar_entry_index();
        self.focus_and_scroll_to_navbar_entry(target, gpui_kit::ScrollStrategy::Center, window, cx);
    }

    pub(super) fn on_content_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key != "tab"
            || modifiers.control
            || modifiers.alt
            || modifiers.platform
            || modifiers.function
            || !self.content_focus.contains_focused(window, cx)
        {
            return;
        }

        let page_index = self.current_page_index();
        let visible = self.visible_page_item_indices();
        let focused_position = visible.iter().position(|actual_index| {
            self.content_focus_handle(page_index, *actual_index)
                .is_some_and(|handle| handle.contains_focused(window, cx))
        });
        let Some(focused_position) = focused_position else {
            return;
        };

        let target_position = if modifiers.shift {
            (0..focused_position).rev().find(|position| {
                self.content
                    .pages
                    .get(page_index)
                    .and_then(|page| {
                        visible
                            .get(*position)
                            .and_then(|index| page.items.get(*index))
                    })
                    .is_some_and(|item| !matches!(item, SettingsPageItem::SectionHeader { .. }))
            })
        } else {
            ((focused_position.saturating_add(1))..visible.len()).find(|position| {
                self.content
                    .pages
                    .get(page_index)
                    .and_then(|page| {
                        visible
                            .get(*position)
                            .and_then(|index| page.items.get(*index))
                    })
                    .is_some_and(|item| !matches!(item, SettingsPageItem::SectionHeader { .. }))
            })
        };

        if let Some(target_position) = target_position {
            self.list_state.scroll_to_reveal_item(target_position);
            let backwards = modifiers.shift;
            cx.on_next_frame(window, move |_, window, cx| {
                cx.notify();
                cx.on_next_frame(window, move |_, window, cx| {
                    if backwards {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    cx.notify();
                });
            });
            cx.notify();
        } else if modifiers.shift {
            let target = self.selected_navbar_entry_index();
            self.focus_and_scroll_to_navbar_entry(
                target,
                gpui_kit::ScrollStrategy::Center,
                window,
                cx,
            );
        } else {
            window.focus_next(cx);
        }
        cx.stop_propagation();
    }

    fn render_no_results(&self, cx: &gpui_kit::App) -> AnyElement {
        gpui_kit::div()
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .child(Label::new("No Results"))
            .child(
                Label::new(format!("No settings match \"{}\"", self.search))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
            .into_any_element()
    }

    fn render_current_page_items(&self, _window: &mut Window, cx: &Context<Self>) -> AnyElement {
        let current_page_index = self.current_page_index();
        let visible = self.visible_page_item_indices();
        if visible.is_empty() && self.has_query {
            return self.render_no_results(cx);
        }
        let last_non_header = visible.iter().copied().rfind(|index| {
            self.current_page().is_some_and(|page| {
                !matches!(
                    page.items.get(*index),
                    Some(SettingsPageItem::SectionHeader { .. })
                )
            })
        });

        // Copied from Zed `render_current_page_items` (`settings_ui.rs:3559-3646`).
        list(
            self.list_state.clone(),
            cx.processor(move |this, logical_index: usize, window, cx| {
                let visible = this.visible_page_item_indices();
                let Some(actual_index) = visible.get(logical_index).copied() else {
                    return gpui_kit::Empty.into_any_element();
                };
                let Some(page) = this.content.pages.get(current_page_index) else {
                    return gpui_kit::Empty.into_any_element();
                };
                let Some(item) = page.items.get(actual_index).cloned() else {
                    return gpui_kit::Empty.into_any_element();
                };
                let Some(item_focus_handle) =
                    this.content_focus_handle(current_page_index, actual_index)
                else {
                    return gpui_kit::Empty.into_any_element();
                };
                let next_is_header = visible
                    .get(logical_index.saturating_add(1))
                    .and_then(|index| page.items.get(*index))
                    .is_some_and(|item| matches!(item, SettingsPageItem::SectionHeader { .. }));
                let is_last = Some(actual_index) == last_non_header;
                let is_last_in_section = next_is_header || is_last;
                let section_title_selector = match &item {
                    SettingsPageItem::SectionHeader { title, .. } => {
                        Some(format!("settings-section-title={title}"))
                    }
                    _ => None,
                };

                gpui_kit::div()
                    .flex()
                    .flex_col()
                    .id(SharedString::from(settings_page_item_id(
                        &item,
                        actual_index,
                    )))
                    .track_focus(&item_focus_handle)
                    .when_some(section_title_selector, |this, selector| {
                        this.debug_selector(move || selector)
                    })
                    .w_full()
                    .min_w_0()
                    .child(this.render_page_item(
                        &item,
                        actual_index,
                        !is_last_in_section,
                        is_last_in_section,
                        window,
                        cx,
                    ))
                    .into_any_element()
            }),
        )
        .size_full()
        .into_any_element()
    }

    fn render_page(&self, window: &mut Window, cx: &Context<Self>) -> AnyElement {
        let open_entity = cx.entity();
        let warning = self.content.write_error.clone();
        let page_content = self.render_current_page_items(window, cx);
        let title = self
            .current_page()
            .map_or_else(|| "Settings".to_owned(), |page| page.title.clone());
        let title_selector = format!("settings-page-title={title}");

        // Copied from Zed `SettingsWindow::render_page` (`settings_ui.rs:3747-4078`).
        gpui_kit::div()
            .flex()
            .flex_col()
            .id("settings-ui-page")
            .debug_selector(|| "settings-content".to_owned())
            .role(gpui_kit::Role::Group)
            .aria_label("Settings Content")
            .track_focus(&self.content_focus)
            .on_key_down(cx.listener(|this, event, window, cx| {
                this.on_content_key_down(event, window, cx);
            }))
            .vertical_scrollbar(&self.list_state)
            .debug_selector(|| "settings-scrollbar-track".to_owned())
            .pt_2p5()
            .gap_4()
            .flex_1()
            .min_w_0()
            .bg(cx.theme().background)
            .child(
                gpui_kit::div()
                    .flex()
                    .flex_col()
                    .px_8()
                    .gap_2()
                    .child(
                        gpui_kit::div()
                            .flex()
                            .items_center()
                            .debug_selector(|| "settings-page-header".to_owned())
                            .w_full()
                            .justify_between()
                            .child(
                                div()
                                    .debug_selector(move || title_selector)
                                    .child(Label::new(title).text_lg()),
                            )
                            .child(
                                div()
                                    .id("settings-open-config-selector")
                                    .debug_selector(|| "settings-open-config".to_owned())
                                    .child(
                                        Button::new("settings-open-config")
                                            .label("Edit config.toml")
                                            .tab_index(0_isize)
                                            .outline()
                                            .on_click(move |_, _, app| {
                                                open_entity.update(app, |this, cx| {
                                                    this.emit(
                                                        SettingsIntent::Invoke(
                                                            "config:edit".to_owned(),
                                                        ),
                                                        cx,
                                                    );
                                                });
                                            }),
                                    ),
                            ),
                    )
                    .when_some(warning, |this, warning| {
                        this.child(
                            gpui_kit::component::alert::Alert::warning(
                                "settings-write-warning",
                                warning,
                            )
                            .banner(),
                        )
                    }),
            )
            .child(
                div()
                    .id("settings-content-scroll")
                    .debug_selector(|| "settings-content-scroll".to_owned())
                    .flex_1()
                    .min_h_0()
                    .size_full()
                    .tab_group()
                    .tab_index(CONTENT_GROUP_TAB_INDEX)
                    .child(page_content),
            )
            .into_any_element()
    }
}

impl EventEmitter<SettingsIntent> for GpuiSettings {}

fn settings_page_item_id(item: &SettingsPageItem, fallback_index: usize) -> String {
    let identity = match item {
        SettingsPageItem::SectionHeader { id, .. } => Some(format!("section-{id}")),
        SettingsPageItem::Setting(row) => {
            settings_row_identity(row).map(|id| format!("setting-{id}"))
        }
        SettingsPageItem::Dependent { parent, .. } => {
            settings_row_identity(parent).map(|id| format!("dependent-{id}"))
        }
    };
    identity.map_or_else(
        || format!("settings-page-item-index-{fallback_index}"),
        |identity| format!("settings-page-item-{identity}"),
    )
}

/// Zed derives the page heading from the selected root navigation entry rather than a host scope
/// label. Its section labels are human-facing headings as well, not schema identifiers.
fn normalize_navigation_titles(snapshot: &mut SettingsContent) {
    for page in &mut snapshot.pages {
        if page.title.is_empty() {
            page.category.label().clone_into(&mut page.title);
        }

        for item in &mut page.items {
            match item {
                SettingsPageItem::SectionHeader { title, .. }
                | SettingsPageItem::Setting(SettingsRow::Section(title)) => {
                    *title = human_title_case(title);
                }
                SettingsPageItem::Setting(_) | SettingsPageItem::Dependent { .. } => {}
            }
        }
    }
}

fn human_title_case(label: &str) -> String {
    if !label.is_ascii() {
        return label.to_owned();
    }
    if label
        .chars()
        .any(|character| character.is_ascii_lowercase())
    {
        return label.to_owned();
    }

    label
        .split_whitespace()
        .map(|word| match word {
            "ANSI" | "API" | "CLI" | "CPU" | "CSI" | "GPU" | "HTTP" | "HTTPS" | "ID" | "IME"
            | "JSON" | "OS" | "OSC" | "PTY" | "SSH" | "TCP" | "TLS" | "TOML" | "UDP" | "UI"
            | "URI" | "URL" | "UUID" | "VIM" | "WSL" => word.to_owned(),
            _ => {
                let mut characters = word.chars();
                characters.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>()
                        + characters.as_str().to_ascii_lowercase().as_str()
                })
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn choice_selectors(snapshot: &SettingsContent) -> HashSet<String> {
    fn visit(row: &SettingsRow, selectors: &mut HashSet<String>) {
        if let SettingsRow::Value {
            id,
            control: SettingsControl::Choice(_) | SettingsControl::Theme(_),
            ..
        } = row
        {
            selectors.insert(format!("settings-choice-{id}"));
        }
    }

    let mut selectors = HashSet::new();
    for item in snapshot.pages.iter().flat_map(|page| &page.items) {
        match item {
            SettingsPageItem::Setting(row) => visit(row, &mut selectors),
            SettingsPageItem::Dependent { parent, children } => {
                visit(parent, &mut selectors);
                for child in children {
                    visit(child, &mut selectors);
                }
            }
            SettingsPageItem::SectionHeader { .. } => {}
        }
    }
    selectors
}

impl Render for GpuiSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui_font = setup_ui_font(window, cx);
        let rem_size = window.rem_size();
        let layout_font = (ui_font.clone(), rem_size);
        if self.last_layout_font.as_ref() != Some(&layout_font) {
            self.last_layout_font = Some(layout_font);
            self.list_state.remeasure();
        }
        let search_input = self.ensure_search_input(window, cx);
        self.update_active_section(self.list_state.logical_scroll_top().item_ix);
        let active_editor = self.active_editor.clone();
        self.schedule_pending_content_focus(window, cx);

        // Copied from Zed's root split (`settings_ui.rs:4510-4597`). The native host owns the
        // platform titlebar, so this starts at Zed's inner `settings-window` element.
        div()
            .id("settings-window")
            .debug_selector(|| "gpui-settings".to_owned())
            .key_context("SettingsWindow")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleFocusNav, window, cx| {
                if this.navbar_focus.contains_focused(window, cx) {
                    this.focus_settings_content(window, cx);
                } else {
                    this.focus_settings_navigation(window, cx);
                }
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui_kit::KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "escape" {
                        if active_editor.is_some() {
                            this.focus_editor(None, cx);
                        } else {
                            this.emit(SettingsIntent::Close, cx);
                        }
                        cx.stop_propagation();
                    }
                }),
            )
            .flex()
            .flex_row()
            .size_full()
            .min_h_0()
            .font(ui_font)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when(!cfg!(target_os = "macos"), |this| {
                this.border_t_1().border_color(cx.theme().border)
            })
            .child(self.render_nav(&search_input, window, cx))
            .child(self.render_page(window, cx))
    }
}

fn font_feature_editor_snapshot(snapshot: &SettingsContent) -> Option<&FontFeatureEditorSnapshot> {
    snapshot
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .find_map(|item| match item {
            SettingsPageItem::Setting(SettingsRow::FontFeatures { editor, .. }) => Some(editor),
            SettingsPageItem::SectionHeader { .. }
            | SettingsPageItem::Setting(_)
            | SettingsPageItem::Dependent { .. } => None,
        })
}

#[derive(PartialEq, Eq)]
struct VisibleStructureSignature {
    category: SettingsCategory,
    items: Vec<String>,
}

fn visible_structure_signature(
    snapshot: &SettingsContent,
    category: SettingsCategory,
    filter_table: &[Vec<bool>],
) -> VisibleStructureSignature {
    let Some((page_index, page)) = snapshot
        .pages
        .iter()
        .enumerate()
        .find(|(_, page)| page.category == category)
    else {
        return VisibleStructureSignature {
            category,
            items: Vec::new(),
        };
    };

    let items = page
        .items
        .iter()
        .enumerate()
        .filter(|(item_index, _)| {
            filter_table
                .get(page_index)
                .and_then(|items| items.get(*item_index))
                .copied()
                .unwrap_or(false)
        })
        .map(|(_, item)| match item {
            SettingsPageItem::SectionHeader { id, .. } => format!("section:{id}"),
            SettingsPageItem::Setting(row) => format!("setting:{}", row_identity(row)),
            SettingsPageItem::Dependent { parent, children } => {
                let children = children
                    .iter()
                    .map(row_identity)
                    .collect::<Vec<_>>()
                    .join("\u{1f}");
                format!("dependent:{}:{children}", row_identity(parent))
            }
        })
        .collect();

    VisibleStructureSignature { category, items }
}

fn row_identity(row: &SettingsRow) -> &str {
    match row {
        // These rows have no domain id. Their contents can change without changing their
        // position or behavior, so key their structural identity by variant instead of value.
        SettingsRow::Section(_) => "section",
        SettingsRow::Notice { .. } => "notice",
        SettingsRow::Value { id, .. }
        | SettingsRow::AnsiPalette { id, .. }
        | SettingsRow::Action { id, .. }
        | SettingsRow::StringList { id, .. }
        | SettingsRow::ModifierRemaps { id, .. }
        | SettingsRow::Environment { id, .. }
        | SettingsRow::FontFeatures { id, .. } => id,
        SettingsRow::StatusSegments(snapshot) => &snapshot.id,
        SettingsRow::ModuleIntegrations(snapshot) => &snapshot.identity,
        SettingsRow::Remote(snapshot) => &snapshot.id,
    }
}
