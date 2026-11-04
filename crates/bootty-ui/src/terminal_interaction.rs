use bootty_terminal::terminal_search::TerminalSearchOptions;
mod copy_mode;
mod selection;

use super::AppEffect;
use crate::{
    error_catalog::ErrorNotice,
    input::focus::InputFocus,
    platform::{write_clipboard_html, write_clipboard_text},
};
use crate::{
    gpui::{InputEvent, Modifiers, PointerButton},
    keymap::CopyToClipboard,
    product_dialogs::terminal_find::{
        FindDirection, TerminalFindIntent, TerminalFindModel, TerminalFindOutput,
        TerminalFindResult,
    },
};
use anyhow::Result;
use bootty_mux::terminal::{ActiveTerminal, TerminalRuntime};
use bootty_terminal::geometry::{SurfaceRect, TerminalSurface, ViewTransform};
use bootty_terminal::{
    terminal_engine::{TerminalCopyModeAction, TerminalSearchDirection, TerminalSelectionFormat},
    terminal_input_model::KeyInput,
};

use copy_mode::{
    CopyModeKeyAction, copy_mode_action_for_event, copy_mode_action_for_input,
    copy_mode_input_should_pass_to_app, copy_mode_key_input_present, copy_mode_key_may_emit_text,
    copy_mode_key_should_pass_to_app, copy_shortcut_pressed, direct_copy_shortcut_pressed,
};
use selection::{TerminalSelectionAction, TerminalSelectionRouteContext, TerminalSelectionRouter};

const fn terminal_find_direction(direction: FindDirection) -> TerminalSearchDirection {
    match direction {
        FindDirection::Current => TerminalSearchDirection::Current,
        FindDirection::Previous => TerminalSearchDirection::Previous,
        FindDirection::Next => TerminalSearchDirection::Next,
    }
}

const fn neutral_find_direction(direction: TerminalSearchDirection) -> FindDirection {
    match direction {
        TerminalSearchDirection::Current => FindDirection::Current,
        TerminalSearchDirection::Previous => FindDirection::Previous,
        TerminalSearchDirection::Next => FindDirection::Next,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalFocusIntent {
    #[default]
    None,
    Terminal,
    Find,
}

#[derive(Debug, Default)]
pub struct TerminalInteractionOutcome {
    pub(super) events: Vec<InputEvent>,
    pub(super) effects: Vec<AppEffect>,
    pub(super) last_error: Option<ErrorNotice>,
    pub(super) focus_intent: TerminalFocusIntent,
    pub(super) handled_count: usize,
}

pub struct TerminalInteractionInput<'a> {
    pub(super) events: Vec<InputEvent>,
    pub(super) modifiers: Modifiers,
    pub(super) pressed_mouse_button: Option<PointerButton>,
    pub(super) input_focus: InputFocus,
    pub(super) terminal_input_enabled: bool,
    pub(super) surface: Option<TerminalSurface>,
    pub(super) view: ViewTransform,
    pub(super) chrome_handle_rects: &'a [SurfaceRect],
    pub(super) copy_on_select: bool,
}

#[derive(Debug, Default)]
pub struct TerminalDirectInputOutcome {
    pub(super) consumed: bool,
    pub(super) copy_mode_active: bool,
    pub(super) effects: Vec<AppEffect>,
    pub(super) last_error: Option<ErrorNotice>,
    pub(super) focus_intent: TerminalFocusIntent,
}

type SelectionResponse =
    bootty_terminal::terminal_session::PendingWorkerResponse<Result<Option<Vec<u8>>, String>>;
struct PendingSelection {
    plain: SelectionResponse,
    html: Option<SelectionResponse>,
    plain_value: Option<Vec<u8>>,
    format: Option<CopyToClipboard>,
}

pub struct TerminalInteractionRuntime {
    selection: TerminalSelectionRouter,
    pending_selections: Vec<PendingSelection>,
    find_dialog: Option<TerminalFindModel>,
    find_return_focus: bool,
    last_search: String,
    pending_word_search: bool,
    search_options: TerminalSearchOptions,
    last_search_direction: TerminalSearchDirection,
    last_error: Option<ErrorNotice>,
    pending_focus_intent: TerminalFocusIntent,
}

impl Default for TerminalInteractionRuntime {
    fn default() -> Self {
        Self {
            selection: TerminalSelectionRouter::default(),
            pending_selections: Vec::new(),
            find_dialog: None,
            find_return_focus: false,
            last_search: String::new(),
            pending_word_search: false,
            search_options: TerminalSearchOptions::default(),
            last_search_direction: TerminalSearchDirection::Next,
            last_error: None,
            pending_focus_intent: TerminalFocusIntent::None,
        }
    }
}

impl TerminalInteractionRuntime {
    pub(super) const fn take_find_dialog(&mut self) -> Option<TerminalFindModel> {
        self.find_dialog.take()
    }

    pub(super) const fn find_dialog(&self) -> Option<&TerminalFindModel> {
        self.find_dialog.as_ref()
    }

    pub(super) fn restore_find_dialog(&mut self, dialog: TerminalFindModel) {
        self.find_dialog = Some(dialog);
    }

    pub(super) fn dismiss_find(&mut self) {
        self.find_dialog = None;
        self.find_return_focus = false;
    }

    pub(super) fn close_overlay_dialogs(
        &mut self,
        terminal: &mut ActiveTerminal,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        if self.find_dialog.is_some() {
            self.clear_find_search(terminal);
        }
        self.finish(TerminalInteractionOutcome::default())
    }

    pub(super) const fn find_action_opens_dialog(
        &self,
        action: &crate::app_actions::TerminalFindAction,
    ) -> bool {
        matches!(action, crate::app_actions::TerminalFindAction::Prompt)
            || self.last_search.is_empty()
                && matches!(
                    action,
                    crate::app_actions::TerminalFindAction::Previous
                        | crate::app_actions::TerminalFindAction::Next
                )
    }

    pub(super) fn apply_find_event(
        &mut self,
        terminal: &mut ActiveTerminal,
        mut dialog: TerminalFindModel,
        event: TerminalFindOutput,
        focused_pane_id: Option<&str>,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        let mut outcome = TerminalInteractionOutcome::default();
        match event {
            TerminalFindOutput::Close => {
                self.clear_find_search(terminal);
                outcome.focus_intent = TerminalFocusIntent::Terminal;
            }
            TerminalFindOutput::FocusFind => {
                self.find_dialog = Some(dialog);
                outcome.focus_intent = TerminalFocusIntent::Find;
            }
            TerminalFindOutput::FocusTerminal => {
                self.find_dialog = Some(dialog);
                outcome.focus_intent = TerminalFocusIntent::Terminal;
            }
            TerminalFindOutput::Search { query, direction } => {
                self.search_options = dialog.options();
                let direction = terminal_find_direction(direction);
                self.search_terminal(terminal, focused_pane_id, &query, direction);
                dialog.clear_result();
                dialog.set_error(self.last_error.as_ref().map(ErrorNotice::raw_message));
                if direction != TerminalSearchDirection::Current && self.find_return_focus {
                    outcome.focus_intent = TerminalFocusIntent::Terminal;
                }
                self.find_dialog = Some(dialog);
            }
        }
        outcome.effects.push(AppEffect::RequestRepaint);
        self.finish(outcome)
    }

    fn open_find(
        &mut self,
        terminal: &mut ActiveTerminal,
        direction: TerminalSearchDirection,
        focused_pane_id: Option<&str>,
    ) -> TerminalFocusIntent {
        let query = self.last_search.clone();
        self.dismiss_find();
        let mut dialog = TerminalFindModel::with_enter_direction(
            query.clone(),
            neutral_find_direction(direction),
        );
        dialog.set_options(self.search_options);
        if !query.is_empty() {
            let _result = self.search_terminal(
                terminal,
                focused_pane_id,
                &query,
                TerminalSearchDirection::Current,
            );
            dialog.clear_result();
            dialog.set_error(self.last_error.as_ref().map(ErrorNotice::raw_message));
        }
        self.find_dialog = Some(dialog);
        self.find_return_focus = false;
        TerminalFocusIntent::Find
    }

    pub(super) fn apply_find_action(
        &mut self,
        terminal: &mut ActiveTerminal,
        action: crate::app_actions::TerminalFindAction,
        focused_pane_id: Option<&str>,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        let mut outcome = TerminalInteractionOutcome::default();
        match action {
            crate::app_actions::TerminalFindAction::ToggleRegex
            | crate::app_actions::TerminalFindAction::ToggleCaseSensitive => {
                let mut dialog = self.find_dialog.take().unwrap_or_else(|| {
                    let mut dialog = TerminalFindModel::new(self.last_search.clone());
                    dialog.set_options(self.search_options);
                    dialog
                });
                let intent =
                    if matches!(action, crate::app_actions::TerminalFindAction::ToggleRegex) {
                        TerminalFindIntent::ToggleRegex
                    } else {
                        TerminalFindIntent::ToggleCaseSensitive
                    };
                if let Some(event) = dialog.apply(intent) {
                    return self.apply_find_event(terminal, dialog, event, focused_pane_id);
                }
            }
            crate::app_actions::TerminalFindAction::Prompt => {
                outcome.focus_intent =
                    self.open_find(terminal, TerminalSearchDirection::Next, focused_pane_id);
            }
            crate::app_actions::TerminalFindAction::Close => {
                self.clear_find_search(terminal);
                outcome.focus_intent = TerminalFocusIntent::Terminal;
            }
            crate::app_actions::TerminalFindAction::Search(query) => {
                self.search_terminal(
                    terminal,
                    focused_pane_id,
                    &query,
                    TerminalSearchDirection::Current,
                );
            }
            crate::app_actions::TerminalFindAction::SearchSelection => {
                self.request_selection(terminal, None);
            }
            action @ (crate::app_actions::TerminalFindAction::Previous
            | crate::app_actions::TerminalFindAction::Next) => {
                let direction =
                    if matches!(action, crate::app_actions::TerminalFindAction::Previous) {
                        TerminalSearchDirection::Previous
                    } else {
                        TerminalSearchDirection::Next
                    };
                let query = self.last_search.clone();
                if query.is_empty() {
                    outcome.focus_intent =
                        self.open_find(terminal, TerminalSearchDirection::Next, focused_pane_id);
                } else {
                    self.search_terminal(terminal, focused_pane_id, &query, direction);
                }
            }
        }
        outcome.effects.push(AppEffect::RequestRepaint);
        self.finish(outcome)
    }

    pub(super) fn enter_copy_mode(
        &mut self,
        terminal: &mut ActiveTerminal,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        let mut outcome = TerminalInteractionOutcome::default();
        match TerminalRuntime::enter_copy_mode(terminal) {
            Ok(()) => outcome.effects.push(AppEffect::RequestRepaint),
            Err(error) => self.record_error("Could not enter copy mode", &error),
        }
        self.finish(outcome)
    }

    pub(super) fn copy_selection_or_request(
        &mut self,
        terminal: &mut ActiveTerminal,
        format: CopyToClipboard,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        let mut outcome = TerminalInteractionOutcome::default();
        if !self.copy_selection(terminal, format) {
            outcome.effects.push(AppEffect::RequestCopy);
        }
        self.finish(outcome)
    }

    pub(super) fn handle_direct_input(
        &mut self,
        terminal: &mut ActiveTerminal,
        input: KeyInput,
        copy_mode_active: bool,
    ) -> TerminalDirectInputOutcome {
        self.begin_operation();
        let mut outcome = TerminalDirectInputOutcome::default();
        let mut copy_mode_active = copy_mode_active;
        outcome.copy_mode_active = copy_mode_active;
        if copy_mode_active {
            if let Some(action) = copy_mode_action_for_input(input) {
                copy_mode_active =
                    self.apply_copy_mode_key_action(terminal, action, &mut outcome.effects);
                outcome.consumed = true;
            } else if !copy_mode_input_should_pass_to_app(input) {
                outcome.consumed = true;
            }
        }
        if !outcome.consumed
            && direct_copy_shortcut_pressed(input)
            && self.copy_selection(terminal, CopyToClipboard::Mixed)
        {
            outcome.consumed = true;
        }
        outcome.copy_mode_active = copy_mode_active;
        self.finish_direct(outcome)
    }

    pub(super) fn handle_input(
        &mut self,
        terminal: &mut ActiveTerminal,
        input: TerminalInteractionInput<'_>,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        self.poll_selections(terminal);
        if self.pending_word_search
            && let Ok(frame) = terminal.extract_frame()
            && !frame.search_query.is_empty()
            && frame.search_query != self.last_search
        {
            self.last_search.clone_from(&frame.search_query);
            self.search_options = TerminalSearchOptions::default();
            self.pending_word_search = false;
        }
        if let Ok(frame) = terminal.extract_frame()
            && let Some(dialog) = self.find_dialog.as_mut()
        {
            dialog.update_from_frame(&frame);
        }
        let TerminalInteractionInput {
            events,
            modifiers,
            pressed_mouse_button,
            input_focus,
            terminal_input_enabled,
            surface,
            view,
            chrome_handle_rects,
            copy_on_select,
        } = input;
        let selection_surface = terminal_input_enabled.then_some(surface).flatten();
        let mouse_tracking = self.terminal_mouse_tracking_for_selection(
            terminal,
            &events,
            terminal_input_enabled,
            pressed_mouse_button,
        );
        let chrome_handle_rects = chrome_handle_rects.to_vec();
        let (events, mut selection_actions) = self.selection.route_events(
            events,
            TerminalSelectionRouteContext {
                surface: selection_surface,
                view,
                mouse_tracking,
                frame_modifiers: modifiers,
                chrome_handle_rects: &chrome_handle_rects,
            },
        );
        selection_actions.extend(self.selection.autoscroll_actions(
            selection_surface,
            view,
            modifiers,
        ));
        let mut outcome = TerminalInteractionOutcome {
            events,
            ..TerminalInteractionOutcome::default()
        };
        outcome.handled_count = self.apply_selection_actions(
            terminal,
            selection_actions,
            copy_on_select,
            &mut outcome.effects,
        );
        outcome.handled_count =
            outcome
                .handled_count
                .saturating_add(self.consume_copy_mode_events(
                    terminal,
                    &mut outcome.events,
                    input_focus,
                    terminal_input_enabled,
                    &mut outcome.effects,
                ));
        if terminal_input_enabled {
            outcome.handled_count = outcome.handled_count.saturating_add(
                self.consume_copy_shortcut_for_selection(terminal, &mut outcome.events),
            );
        }
        self.finish(outcome)
    }

    fn terminal_mouse_tracking_for_selection(
        &mut self,
        terminal: &mut ActiveTerminal,
        events: &[InputEvent],
        terminal_input_enabled: bool,
        pressed_mouse_button: Option<PointerButton>,
    ) -> bool {
        let primary_drag_active = pressed_mouse_button == Some(PointerButton::Left);
        if !terminal_input_enabled
            || !events.iter().any(|event| match event {
                InputEvent::PointerButton {
                    button: PointerButton::Left,
                    ..
                } => true,
                InputEvent::PointerMoved(_) => primary_drag_active,
                _ => false,
            })
        {
            return false;
        }
        match TerminalRuntime::is_mouse_tracking(terminal) {
            Ok(mouse_tracking) => mouse_tracking,
            Err(error) => {
                self.record_error("Could not select text", &error);
                false
            }
        }
    }

    fn apply_selection_actions(
        &mut self,
        terminal: &mut ActiveTerminal,
        actions: Vec<TerminalSelectionAction>,
        copy_on_select: bool,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let count = actions.len();
        for action in actions {
            let is_end = matches!(&action, TerminalSelectionAction::End(_));
            let result = match action {
                TerminalSelectionAction::Begin(event) => {
                    TerminalRuntime::begin_selection(terminal, event)
                }
                TerminalSelectionAction::Scroll(delta) => {
                    TerminalRuntime::scroll_viewport_delta(terminal, delta)
                }
                TerminalSelectionAction::Update(event) => {
                    TerminalRuntime::update_selection(terminal, event)
                }
                TerminalSelectionAction::End(event) => {
                    TerminalRuntime::end_selection(terminal, event)
                }
            };
            match result {
                Ok(()) => {
                    effects.push(AppEffect::RequestRepaint);
                    if copy_on_select && is_end {
                        self.copy_selection(terminal, CopyToClipboard::Mixed);
                    }
                }
                Err(error) => self.record_error("Could not select text", &error),
            }
        }
        count
    }

    fn consume_copy_mode_events(
        &mut self,
        terminal: &mut ActiveTerminal,
        events: &mut Vec<InputEvent>,
        input_focus: InputFocus,
        terminal_input_enabled: bool,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        if !terminal_input_enabled
            || (self.find_dialog.is_some() && input_focus != InputFocus::Terminal)
            || !copy_mode_key_input_present(events)
        {
            return 0;
        }
        let mut copy_mode_active = match TerminalRuntime::copy_mode_active(terminal) {
            Ok(active) => active,
            Err(error) => {
                self.record_error("Could not use copy mode", &error);
                false
            }
        };
        if !copy_mode_active {
            return 0;
        }
        let mut count = 0_usize;
        let mut retained = Vec::with_capacity(events.len());
        let mut suppress_next_text = false;
        let mut pass_next_text_to_app = false;
        let mut find_prompt_opened = false;
        for event in events.drain(..) {
            if find_prompt_opened {
                if matches!(event, InputEvent::ImeCommit(_))
                    && std::mem::take(&mut suppress_next_text)
                {
                    count = count.saturating_add(1);
                } else {
                    suppress_next_text = false;
                    retained.push(event);
                }
                continue;
            }
            if !copy_mode_active {
                if matches!(event, InputEvent::ImeCommit(_))
                    && std::mem::take(&mut suppress_next_text)
                {
                    count = count.saturating_add(1);
                } else {
                    retained.push(event);
                }
                continue;
            }
            match &event {
                InputEvent::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if copy_mode_key_should_pass_to_app(*key, *modifiers) => {
                    pass_next_text_to_app = copy_mode_key_may_emit_text(*key);
                    retained.push(event);
                }
                InputEvent::ImeCommit(_) if std::mem::take(&mut pass_next_text_to_app) => {
                    retained.push(event);
                }
                _ if matches!(event, InputEvent::Key { .. } | InputEvent::ImeCommit(_)) => {
                    pass_next_text_to_app = false;
                    count = count.saturating_add(1);
                    if let Some(action) =
                        copy_mode_action_for_event(&event, &mut suppress_next_text)
                    {
                        let opens_find = matches!(action, CopyModeKeyAction::SearchPrompt(_));
                        copy_mode_active =
                            self.apply_copy_mode_key_action(terminal, action, effects);
                        find_prompt_opened = opens_find;
                    }
                }
                _ => {
                    pass_next_text_to_app = false;
                    retained.push(event);
                }
            }
        }
        *events = retained;
        count
    }

    fn consume_copy_shortcut_for_selection(
        &mut self,
        terminal: &mut ActiveTerminal,
        events: &mut Vec<InputEvent>,
    ) -> usize {
        let Some(index) = events.iter().position(copy_shortcut_pressed) else {
            return 0;
        };
        if !self.copy_selection(terminal, CopyToClipboard::Mixed) {
            return 0;
        }
        events.remove(index);
        1
    }

    fn apply_copy_mode_key_action(
        &mut self,
        terminal: &mut ActiveTerminal,
        action: CopyModeKeyAction,
        effects: &mut Vec<AppEffect>,
    ) -> bool {
        match action {
            CopyModeKeyAction::Terminal(action) => {
                self.apply_copy_mode_terminal_action(terminal, action, effects)
            }
            CopyModeKeyAction::SearchPrompt(direction) => {
                self.record_search_direction(direction);
                let focus = self.open_find(terminal, direction, None);
                self.find_return_focus = true;
                self.pending_focus_intent = focus;
                effects.push(AppEffect::RequestRepaint);
                true
            }
            CopyModeKeyAction::SearchWord(direction) => self.apply_copy_mode_terminal_action(
                terminal,
                TerminalCopyModeAction::SearchWord(direction),
                effects,
            ),
            CopyModeKeyAction::SearchRepeat(repeat) => {
                let direction = repeat.direction(self.last_search_direction);
                let query = self.last_search.clone();
                if !query.trim().is_empty() {
                    let _result = self.search_terminal_with_direction_recording(
                        terminal, None, &query, direction, false,
                    );
                    if let Some(dialog) = self.find_dialog.as_mut() {
                        dialog.clear_result();
                        dialog.set_error(self.last_error.as_ref().map(ErrorNotice::raw_message));
                    }
                    effects.push(AppEffect::RequestRepaint);
                }
                true
            }
        }
    }

    fn apply_copy_mode_terminal_action(
        &mut self,
        terminal: &mut ActiveTerminal,
        action: TerminalCopyModeAction,
        effects: &mut Vec<AppEffect>,
    ) -> bool {
        if let TerminalCopyModeAction::SearchWord(direction) = &action {
            self.pending_word_search = true;
            self.record_search_direction(*direction);
        }
        let search_direction = match &action {
            TerminalCopyModeAction::Search { direction, .. }
            | TerminalCopyModeAction::SearchWord(direction) => Some(*direction),
            _ => None,
        };
        // Route later events in this batch after an ordered cancellation.
        let cancels = match &action {
            TerminalCopyModeAction::Cancel
            | TerminalCopyModeAction::CopySelectionAndCancel
            | TerminalCopyModeAction::CopyEndOfLineAndCancel => true,
            TerminalCopyModeAction::CancelOrClearSelection => terminal
                .extract_frame()
                .is_ok_and(|frame| frame.copy_mode.is_none_or(|mode| !mode.selecting)),
            _ => false,
        };
        match TerminalRuntime::handle_copy_mode_action(terminal, action) {
            Ok(outcome) => {
                if let Some(bytes) = outcome.copied
                    && let Err(error) = write_clipboard_text(&String::from_utf8_lossy(&bytes))
                {
                    self.record_error("Could not copy to the clipboard", &error);
                }
                if let Some(search) = outcome.search {
                    self.last_search = search.query;
                    self.search_options = TerminalSearchOptions::default();
                    if let Some(dialog) = self.find_dialog.as_mut() {
                        dialog.set_options(self.search_options);
                    }
                    if let Some(direction) = search_direction {
                        self.record_search_direction(direction);
                    }
                    if let Some(dialog) = self.find_dialog.as_mut() {
                        dialog.clear_result();
                        dialog.set_error(self.last_error.as_ref().map(ErrorNotice::raw_message));
                    }
                }
                effects.push(AppEffect::RequestRepaint);
                !cancels && outcome.active
            }
            Err(error) => {
                self.record_error("Could not use copy mode", &error);
                false
            }
        }
    }

    fn copy_selection(&mut self, terminal: &mut ActiveTerminal, format: CopyToClipboard) -> bool {
        if terminal
            .extract_frame()
            .is_ok_and(|frame| frame.selections.is_empty())
        {
            return false;
        }
        self.request_selection(terminal, Some(format))
    }

    fn request_selection(
        &mut self,
        terminal: &mut ActiveTerminal,
        format: Option<CopyToClipboard>,
    ) -> bool {
        let result = (|| -> Result<PendingSelection> {
            let kind = match format {
                Some(CopyToClipboard::Html) => TerminalSelectionFormat::Html,
                Some(CopyToClipboard::Vt) => TerminalSelectionFormat::Vt,
                _ => TerminalSelectionFormat::PlainText,
            };
            let plain = terminal.format_selection(kind)?;
            let html = if format == Some(CopyToClipboard::Mixed) {
                Some(terminal.format_selection(TerminalSelectionFormat::Html)?)
            } else {
                None
            };
            Ok(PendingSelection {
                plain,
                html,
                plain_value: None,
                format,
            })
        })();
        match result {
            Ok(pending) => {
                self.pending_selections.push(pending);
                true
            }
            Err(error) => {
                self.record_error("Could not copy the selection", &error);
                false
            }
        }
    }

    fn poll_selections(&mut self, terminal: &mut ActiveTerminal) {
        let pending = std::mem::take(&mut self.pending_selections);
        for mut request in pending {
            let result = (|| -> Result<bool> {
                if request.plain_value.is_none() {
                    let Some(value) = request.plain.try_receive()? else {
                        return Ok(false);
                    };
                    let Some(value) = value.map_err(anyhow::Error::msg)? else {
                        return Ok(true);
                    };
                    request.plain_value = Some(value);
                }
                let html = if let Some(response) = &request.html {
                    let Some(value) = response.try_receive()? else {
                        return Ok(false);
                    };
                    value.map_err(anyhow::Error::msg)?
                } else {
                    None
                };
                if let Some(bytes) = request.plain_value.take() {
                    let text = String::from_utf8_lossy(&bytes);
                    match request.format {
                        None => {
                            self.search_terminal(
                                terminal,
                                None,
                                text.trim(),
                                TerminalSearchDirection::Current,
                            );
                        }
                        Some(CopyToClipboard::Html) => write_clipboard_html(&text, None)?,
                        Some(CopyToClipboard::Mixed) => {
                            if let Some(html) = html {
                                write_clipboard_html(&String::from_utf8_lossy(&html), Some(&text))?;
                            }
                        }
                        _ => write_clipboard_text(&text)?,
                    }
                }
                Ok(true)
            })();
            match result {
                Ok(false) => self.pending_selections.push(request),
                Ok(true) => {}
                Err(error) => self.record_error("Could not copy the selection", &error),
            }
        }
    }

    fn clear_search(&mut self, terminal: &mut ActiveTerminal) {
        if let Err(error) = terminal.search_viewport("", TerminalSearchDirection::Current) {
            self.record_error("Could not clear the search", &error);
        }
    }

    fn clear_find_search(&mut self, terminal: &mut ActiveTerminal) {
        self.dismiss_find();
        self.clear_search(terminal);
    }

    fn search_terminal(
        &mut self,
        terminal: &mut ActiveTerminal,
        focused_pane_id: Option<&str>,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> TerminalFindResult {
        self.search_terminal_with_direction_recording(
            terminal,
            focused_pane_id,
            query,
            direction,
            true,
        )
    }

    fn search_terminal_with_direction_recording(
        &mut self,
        terminal: &mut ActiveTerminal,
        focused_pane_id: Option<&str>,
        query: &str,
        direction: TerminalSearchDirection,
        record_direction: bool,
    ) -> TerminalFindResult {
        if query.is_empty() {
            self.clear_search(terminal);
            return TerminalFindResult::default();
        }
        self.pending_word_search = false;
        query.clone_into(&mut self.last_search);
        if record_direction {
            self.record_search_direction(direction);
        }
        let copy_mode_active = match TerminalRuntime::copy_mode_active(terminal) {
            Ok(active) => active,
            Err(error) => {
                self.record_error("Could not search the terminal", &error);
                false
            }
        };
        if copy_mode_active {
            return self.search_copy_mode_terminal(terminal, query, direction);
        }
        if let Some(pane_id) = focused_pane_id
            && let Some(source) = terminal.focused_terminal_runtime(pane_id)
        {
            return self.resolve_find_result(search_runtime(
                source,
                query,
                direction,
                self.search_options,
            ));
        }
        self.resolve_find_result(search_runtime(
            terminal,
            query,
            direction,
            self.search_options,
        ))
    }

    fn resolve_find_result(&mut self, result: Result<TerminalFindResult>) -> TerminalFindResult {
        match result {
            Ok(result) => result,
            Err(error) => {
                self.record_error("Could not search the terminal", &error);
                TerminalFindResult::default()
            }
        }
    }

    fn search_copy_mode_terminal(
        &mut self,
        terminal: &mut ActiveTerminal,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> TerminalFindResult {
        match TerminalRuntime::handle_copy_mode_action(
            terminal,
            TerminalCopyModeAction::SearchWithOptions {
                options: self.search_options,
                query: query.to_owned(),
                direction,
            },
        ) {
            Ok(_) => TerminalFindResult::default(),
            Err(error) => {
                self.record_error("Could not search the terminal", &error);
                TerminalFindResult::default()
            }
        }
    }

    fn record_search_direction(&mut self, direction: TerminalSearchDirection) {
        if direction != TerminalSearchDirection::Current {
            self.last_search_direction = direction;
        }
    }

    fn begin_operation(&mut self) {
        self.last_error = None;
        self.pending_focus_intent = TerminalFocusIntent::None;
    }

    fn record_error(&mut self, action: &str, error: &impl ToString) {
        self.last_error = Some(ErrorNotice::failed(action, error.to_string()));
    }

    fn finish(&mut self, mut outcome: TerminalInteractionOutcome) -> TerminalInteractionOutcome {
        let (last_error, focus_intent) = self.take_operation_state();
        outcome.last_error = last_error;
        if focus_intent != TerminalFocusIntent::None {
            outcome.focus_intent = focus_intent;
        }
        outcome
    }

    fn finish_direct(
        &mut self,
        mut outcome: TerminalDirectInputOutcome,
    ) -> TerminalDirectInputOutcome {
        (outcome.last_error, outcome.focus_intent) = self.take_operation_state();
        outcome
    }

    fn take_operation_state(&mut self) -> (Option<ErrorNotice>, TerminalFocusIntent) {
        (
            self.last_error.take(),
            std::mem::take(&mut self.pending_focus_intent),
        )
    }
}

fn search_runtime(
    terminal: &mut dyn TerminalRuntime,
    query: &str,
    direction: TerminalSearchDirection,
    options: TerminalSearchOptions,
) -> Result<TerminalFindResult> {
    terminal.search_viewport_with_options(query, direction, options)?;
    Ok(TerminalFindResult::default())
}
