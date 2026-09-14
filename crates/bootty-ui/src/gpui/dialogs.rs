//! Shared GPUI dialog controls and floating surfaces.
//!
//! This module owns presentation state only. Hosts project their domain models into [`DialogSpec`]
//! values and translate emitted [`DialogIntent`] values back into domain events.

use super::{OverlayPlacement, OverlayView};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IndexPath, Selectable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    command::{Command, CommandEntry, CommandGroup, CommandItem, CommandState},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement, KeyDownEvent,
    Keystroke, MouseButton, ParentElement, PromptButton, Render, SharedString, Styled,
    Subscription, Window, div, prelude::*, px, rems,
};
use std::{cell::RefCell, rc::Rc};

/// A themed confirmation. Answers retain their caller order; dismissing cancels the receiver.
pub fn prompt(
    message: &str,
    detail: Option<&str>,
    answers: &[PromptButton],
    window: &mut Window,
    cx: &mut App,
) -> futures::channel::oneshot::Receiver<usize> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    let sender = Rc::new(RefCell::new(Some(sender)));
    let message = SharedString::from(message.to_owned());
    let detail = detail.map(|detail| SharedString::from(detail.to_owned()));
    let answers = answers.to_vec();
    window.open_alert_dialog(cx, move |dialog, window, cx| {
        let confirm_sender = sender.clone();
        let close_sender = sender.clone();
        let font = super::setup_ui_font(window, cx);
        let rem = f32::from(window.rem_size());
        let width = (-2.0f32)
            .mul_add(rem, f32::from(window.viewport_size().width))
            .max(0.0)
            .min(28.0 * rem);
        dialog
            .width(px(width))
            .title(div().font(font.clone()).child(message.clone()))
            .when_some(detail.clone(), |dialog, detail| {
                dialog.description(div().font(font).child(detail))
            })
            .on_ok(move |_, _, _| {
                if let Some(sender) = confirm_sender.borrow_mut().take() {
                    _ = sender.send(0);
                }
                true
            })
            .on_close(move |_, _, _| {
                close_sender.borrow_mut().take();
            })
            .footer(
                gpui_kit::component::dialog::DialogFooter::new()
                    .flex_wrap()
                    .children(answers.iter().enumerate().rev().map(|(index, answer)| {
                        let sender = sender.clone();
                        let label = answer.label().clone();
                        let selector = format!("prompt-answer-{label}");
                        let respond = move |window: &mut Window, cx: &mut App| {
                            if let Some(sender) = sender.borrow_mut().take() {
                                _ = sender.send(index);
                            }
                            window.close_dialog(cx);
                        };
                        let confirm = respond.clone();
                        Button::new(label.clone())
                            .label(label)
                            .debug_selector(move || selector)
                            .when(index == 0, Button::primary)
                            .on_click(move |_, window, cx| respond(window, cx))
                            // Enter on a focused answer must not invoke the dialog's default.
                            .on_action(move |_: &gpui_kit::base::actions::Confirm, window, cx| {
                                confirm(window, cx);
                                cx.stop_propagation();
                            })
                    })),
            )
    });
    receiver
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DialogId(pub String);

impl DialogId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RowId(pub String);

impl RowId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActionId(pub String);

impl ActionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

/// Values captured by a rendered action, independent of later catalog ordering.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DialogPayload {
    #[default]
    None,
    Text(String),
    Session(bootty_mux::workspace::ScopedSessionTarget),
}

impl DialogPayload {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindDirection {
    Current,
    Previous,
    Next,
}

/// Actions the application can dispatch to the active command surface.
///
/// The command runtime owns keybinding resolution; this enum is the small
/// presentation seam used to perform the resulting action in the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandAction {
    Previous,
    Next,
    Confirm,
    Cancel,
    ToggleFavorite,
}

/// Dialog interactions retain the values captured by their rendered controls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogIntent {
    Dismiss {
        dialog: DialogId,
    },
    Activate {
        dialog: DialogId,
        row: RowId,
        action: ActionId,
        payload: DialogPayload,
    },
    Preview {
        dialog: DialogId,
        row: RowId,
        action: ActionId,
        payload: DialogPayload,
    },
    TextChanged {
        dialog: DialogId,
        value: String,
    },
    FieldChanged {
        dialog: DialogId,
        field: String,
        value: String,
    },
    SelectionChanged {
        dialog: DialogId,
        row: RowId,
    },
    CycleScope {
        dialog: DialogId,
    },
    ToggleFavorite {
        dialog: DialogId,
        row: RowId,
    },
    Find {
        dialog: DialogId,
        query: String,
        direction: FindDirection,
    },
    FocusTerminal {
        dialog: DialogId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogRole {
    SearchableList,
    Prompt,
    Confirm,
    TerminalFind,
    ThemePicker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogPlacement {
    Center,
    TopRight,
    BottomRight,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogAction {
    pub id: ActionId,
    pub payload: DialogPayload,
}

impl DialogAction {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: ActionId::new(id),
            payload: DialogPayload::default(),
        }
    }

    #[must_use]
    pub fn with_payload(mut self, payload: impl Into<String>) -> Self {
        self.payload = DialogPayload::text(payload);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogRow {
    pub id: RowId,
    pub icon: Option<String>,
    pub label: String,
    /// Product identity color, subordinate to disabled and destructive states.
    pub color: Option<Hsla>,
    pub detail: Option<String>,
    pub trailing: Option<String>,
    /// A durable keybinding spelling, rendered by the platform-aware keybinding component.
    pub keybinding: Option<String>,
    pub current: bool,
    pub enabled: bool,
    pub destructive: bool,
    pub action: Option<DialogAction>,
    pub preview: Option<DialogAction>,
}

impl DialogRow {
    pub fn action(id: impl Into<String>, label: impl Into<String>, action: DialogAction) -> Self {
        Self {
            id: RowId::new(id),
            icon: None,
            label: label.into(),
            color: None,
            detail: None,
            trailing: None,
            keybinding: None,
            current: false,
            enabled: true,
            destructive: false,
            action: Some(action),
            preview: None,
        }
    }

    pub fn section(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: RowId::new(id),
            icon: None,
            label: label.into(),
            color: None,
            detail: None,
            trailing: None,
            keybinding: None,
            current: false,
            enabled: false,
            destructive: false,
            action: None,
            preview: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogField {
    pub id: String,
    pub label: String,
    pub value: String,
    pub placeholder: String,
    pub kind: DialogFieldKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogFieldKind {
    Text,
    Choice(Vec<String>),
    Color,
}

/// An owned, disposable projection of an app-owned dialog model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogSpec {
    pub id: DialogId,
    pub role: DialogRole,
    pub title: String,
    pub icon: Option<String>,
    pub hint: Option<String>,
    pub footer: Option<String>,
    pub text: Option<String>,
    pub text_hint: Option<String>,
    pub text_label: Option<String>,
    pub fields: Vec<DialogField>,
    pub busy: bool,
    pub rows: Vec<DialogRow>,
    pub empty_text: String,
    pub placement: DialogPlacement,
}

impl DialogSpec {
    /// Present rows already filtered and ranked by the product owner for `text`.
    /// Query edits leave through [`DialogIntent::TextChanged`] for a fresh projection.
    pub fn searchable(
        id: impl Into<String>,
        title: impl Into<String>,
        text: impl Into<String>,
        rows: Vec<DialogRow>,
    ) -> Self {
        Self {
            id: DialogId::new(id),
            role: DialogRole::SearchableList,
            title: title.into(),
            icon: None,
            hint: Some("Enter select   Esc close".to_owned()),
            footer: None,
            text: Some(text.into()),
            text_label: None,
            fields: Vec::new(),
            busy: false,
            text_hint: Some("filter…".to_owned()),
            rows,
            empty_text: "no matching items".to_owned(),
            placement: DialogPlacement::Center,
        }
    }

    pub fn prompt(
        id: impl Into<String>,
        title: impl Into<String>,
        value: impl Into<String>,
        value_hint: impl Into<String>,
        submit: DialogAction,
    ) -> Self {
        let id = DialogId::new(id);
        Self {
            rows: vec![DialogRow::action("submit", "Confirm", submit)],
            role: DialogRole::Prompt,
            title: title.into(),
            text: Some(value.into()),
            text_label: None,
            fields: Vec::new(),
            busy: false,
            text_hint: Some(value_hint.into()),
            hint: Some("Enter confirm   Esc close".to_owned()),
            icon: None,
            footer: None,
            empty_text: String::new(),
            placement: DialogPlacement::Center,
            id,
        }
    }
}

pub struct DialogView {
    spec: Option<DialogSpec>,
    command: Entity<CommandState>,
    command_focus: FocusHandle,
    /// Prompts own a real editor instead of focusing a hidden `CommandState` query.
    prompt_input: Entity<InputState>,
    prompt_input_focus: FocusHandle,
    fields: std::collections::HashMap<String, (Entity<InputState>, Subscription)>,
    /// Stable focus target for a confirm surface before its buttons render.
    confirm_focus: FocusHandle,
    command_keybindings: Option<Vec<(CommandAction, String)>>,
    find_input: Entity<InputState>,
    suppress_query: Option<String>,
    preserve_selected_row: Option<RowId>,
    select_initial_current: bool,
    suppress_initial_selection: bool,
    _find_input_subscription: Subscription,
    _prompt_input_subscription: Subscription,
    _command_interceptor: Subscription,
}

impl DialogView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let find_input = cx.new(|cx| InputState::new(window, cx));
        let find_input_subscription = cx.subscribe_in(
            &find_input,
            window,
            |this, _, event: &InputEvent, _window, cx| match event {
                InputEvent::Change => {
                    let value = this.find_input.read(cx).value().to_string();
                    this.change_text(value, cx);
                }
                InputEvent::PressEnter { shift, .. } => {
                    let query = this.find_input.read(cx).value().to_string();
                    this.submit_query(&query, *shift, cx);
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        let prompt_input = cx.new(|cx| InputState::new(window, cx));
        let prompt_input_subscription = cx.subscribe_in(
            &prompt_input,
            window,
            |this, _, event: &InputEvent, _window, cx| match event {
                InputEvent::Change => {
                    let value = this.prompt_input.read(cx).value().to_string();
                    this.change_text(value, cx);
                }
                // The prompt's Confirm action owns submission and blocks Root's default close.
                InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        let owner = cx.weak_entity();
        let command_interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            let (command_surface_focused, redirect_typing) = owner
                .read_with(cx, |this, app| this.command_focus_state(window, app))
                .unwrap_or((false, false));
            if redirect_typing
                && event.keystroke.key_char.as_deref().is_some_and(|text| {
                    !text.is_empty()
                        && !event.keystroke.modifiers.control
                        && !event.keystroke.modifiers.platform
                        && !event.keystroke.modifiers.alt
                })
            {
                // The key event was already resolved against the old focus path. Move focus and
                // replay it after the frame so the query's input handler inserts the character
                // without dropping its first character or reimplementing edits.
                _ = owner.update(cx, |this, cx| {
                    let focus = this.command.read(cx).focus_handle(cx);
                    focus.focus(window, cx);
                });
                let keystroke = event.keystroke.clone();
                window.defer(cx, move |window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                });
                cx.stop_propagation();
            } else if command_surface_focused && event.keystroke.key.eq_ignore_ascii_case("escape")
            {
                if has_active_root_dialog(window, cx) {
                    // CommandState clears a non-empty query before propagating Cancel. Focus the
                    // Root trap and dispatch directly to it so modal Escape always closes once.
                    if let Some(focus_trap) = gpui_kit::base::active_focus_trap(window, cx) {
                        focus_trap.focus(window, cx);
                        window.dispatch_action(Box::new(gpui_kit::base::actions::Cancel), cx);
                    }
                } else {
                    _ = owner.update(cx, |this, cx| this.cancel(cx));
                }
                cx.stop_propagation();
            }
        });
        Self {
            spec: None,
            command: cx.new(|cx| CommandState::new(window, cx)),
            command_focus: cx.focus_handle().tab_stop(true),
            prompt_input_focus: prompt_input.read(cx).focus_handle(cx),
            prompt_input,
            fields: std::collections::HashMap::new(),
            confirm_focus: cx.focus_handle().tab_stop(true),
            command_keybindings: None,
            find_input,
            suppress_query: None,
            preserve_selected_row: None,
            select_initial_current: false,
            suppress_initial_selection: false,
            _find_input_subscription: find_input_subscription,
            _prompt_input_subscription: prompt_input_subscription,
            _command_interceptor: command_interceptor,
        }
    }

    fn command_focus_state(&self, window: &Window, cx: &App) -> (bool, bool) {
        let command_role = self.spec.as_ref().is_some_and(|spec| {
            matches!(
                spec.role,
                DialogRole::SearchableList | DialogRole::ThemePicker
            )
        });
        let command_focus = self.command.read(cx).focus_handle(cx);
        let root_focus = window
            .root::<gpui_kit::component::Root>()
            .flatten()
            .is_some()
            && gpui_kit::base::active_focus_trap(window, cx).is_some_and(|trap| {
                trap.is_focused(window) && trap.contains(&command_focus, window)
            });
        (
            command_role
                && (self.command_focus.is_focused(window)
                    || command_focus.is_focused(window)
                    || root_focus),
            command_role
                && self.spec.as_ref().is_some_and(|spec| spec.text.is_some())
                && (self.command_focus.is_focused(window) || root_focus),
        )
    }

    /// Set the active window's resolved command keybindings.
    ///
    /// `None` keeps the standalone component defaults. `Some` is the runtime
    /// projection, including an empty vector when configured shortcuts are
    /// unavailable; that distinction prevents stale default hints in a host
    /// with an editable keymap.
    pub fn set_command_keybindings(
        &mut self,
        keybindings: Option<Vec<(CommandAction, String)>>,
        cx: &mut Context<Self>,
    ) {
        if self.command_keybindings == keybindings {
            return;
        }
        self.command_keybindings = keybindings;
        cx.notify();
    }

    /// Perform an application-level command action against this dialog.
    ///
    /// Selection and confirmation remain owned by `CommandState`. Focus the
    /// command query first, then dispatch its public component action on the
    /// next turn so actions arriving while the dialog's retained container is
    /// focused still use the component's selection and callback paths.
    pub fn perform(&mut self, action: CommandAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            CommandAction::Previous if self.is_command_surface() => {
                self.dispatch_command_action(
                    Box::new(gpui_kit::base::actions::SelectUp),
                    window,
                    cx,
                );
            }
            CommandAction::Next if self.is_command_surface() => {
                self.dispatch_command_action(
                    Box::new(gpui_kit::base::actions::SelectDown),
                    window,
                    cx,
                );
            }
            CommandAction::Confirm if self.is_command_surface() => self.dispatch_command_action(
                Box::new(gpui_kit::base::actions::Confirm { secondary: false }),
                window,
                cx,
            ),
            CommandAction::Confirm if self.is_prompt() => self.prompt_submit(cx),
            CommandAction::Confirm if self.is_confirm() => self.confirm_first_action(cx),
            CommandAction::Cancel => self.cancel(cx),
            CommandAction::ToggleFavorite if self.is_command_surface() => {
                self.toggle_favorite_selected(cx);
            }
            CommandAction::Confirm
            | CommandAction::Previous
            | CommandAction::Next
            | CommandAction::ToggleFavorite => {}
        }
    }

    fn is_command_surface(&self) -> bool {
        self.spec.as_ref().is_some_and(|spec| {
            matches!(
                spec.role,
                DialogRole::SearchableList | DialogRole::ThemePicker
            )
        })
    }

    fn is_prompt(&self) -> bool {
        self.spec
            .as_ref()
            .is_some_and(|spec| spec.role == DialogRole::Prompt)
    }

    fn is_confirm(&self) -> bool {
        self.spec
            .as_ref()
            .is_some_and(|spec| spec.role == DialogRole::Confirm)
    }

    fn dispatch_command_action(
        &self,
        action: Box<dyn gpui_kit::Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self.command.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
    }

    fn cancel(&self, cx: &mut Context<Self>) {
        if let Some(spec) = &self.spec {
            cx.emit(DialogIntent::Dismiss {
                dialog: spec.id.clone(),
            });
        }
    }

    fn confirm(&self, row: &DialogRow, shift: bool, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else { return };
        if spec.role == DialogRole::TerminalFind {
            cx.emit(DialogIntent::Find {
                dialog: spec.id.clone(),
                query: spec.text.clone().unwrap_or_default(),
                direction: if shift {
                    FindDirection::Previous
                } else {
                    FindDirection::Next
                },
            });
        } else if let Some(action) = &row.action {
            cx.emit(DialogIntent::Activate {
                dialog: spec.id.clone(),
                row: row.id.clone(),
                action: action.id.clone(),
                payload: action.payload.clone(),
            });
        }
    }

    fn prompt_submit(&self, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else { return };
        let Some(row) = spec.rows.iter().find(|row| row.id.0 == "submit") else {
            return;
        };
        if row.enabled {
            self.confirm(row, false, cx);
        }
    }

    fn confirm_first_action(&self, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else { return };
        if let Some(row) = spec
            .rows
            .iter()
            .find(|row| row.enabled && row.action.is_some())
        {
            self.confirm(row, false, cx);
        }
    }

    fn selection_changed(&self, row: &DialogRow, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else { return };
        cx.emit(DialogIntent::SelectionChanged {
            dialog: spec.id.clone(),
            row: row.id.clone(),
        });
        if let Some(preview) = &row.preview {
            cx.emit(DialogIntent::Preview {
                dialog: spec.id.clone(),
                row: row.id.clone(),
                action: preview.id.clone(),
                payload: preview.payload.clone(),
            });
        }
    }

    fn submit_query(&self, query: &str, shift: bool, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else { return };
        if spec.role == DialogRole::TerminalFind {
            cx.emit(DialogIntent::Find {
                dialog: spec.id.clone(),
                query: query.to_owned(),
                direction: if shift {
                    FindDirection::Previous
                } else {
                    FindDirection::Next
                },
            });
        }
    }

    pub fn present(
        &mut self,
        spec: Option<DialogSpec>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.spec == spec {
            return;
        }
        let changed = self.spec.as_ref().map(|spec| &spec.id) != spec.as_ref().map(|spec| &spec.id);
        let same_query = self
            .spec
            .as_ref()
            .zip(spec.as_ref())
            .is_some_and(|(previous, next)| previous.text == next.text);
        let preserve_selected_row = (!changed && same_query)
            .then(|| self.selected_row_id(cx))
            .flatten();
        if changed && spec.is_some() {
            // A new modal owns fresh query, selection, and scroll state. Retain the entity
            // only while that modal is open so ordinary model refreshes preserve focus.
            self.command = cx.new(|cx| CommandState::new(window, cx));
            self.suppress_query = None;
            self.select_initial_current = true;
            self.suppress_initial_selection = false;
        }
        if changed {
            self.fields.clear();
        }
        self.preserve_selected_row = preserve_selected_row;
        self.spec = spec;
        if let Some(spec) = &self.spec {
            if spec.role == DialogRole::TerminalFind {
                let query = spec.text.clone().unwrap_or_default();
                self.find_input.update(cx, |input, cx| {
                    input.set_placeholder(
                        spec.text_hint.clone().unwrap_or_else(|| "find".to_owned()),
                        window,
                        cx,
                    );
                    if input.value().as_ref() != query {
                        input.set_value(query, window, cx);
                    }
                });
            } else if matches!(
                spec.role,
                DialogRole::SearchableList | DialogRole::ThemePicker
            ) {
                let query = spec.text.clone().unwrap_or_default();
                if self.command.read(cx).query(cx).as_ref() != query {
                    // CommandState::set_query intentionally behaves like user
                    // input and invokes on_query. This sync is owner-driven;
                    // consume its callback without re-emitting TextChanged.
                    self.suppress_query = Some(query.clone());
                }
                self.command
                    .update(cx, |state, cx| state.set_query(query, window, cx));
            } else if spec.role == DialogRole::Prompt {
                let value = spec.text.clone().unwrap_or_default();
                let sync_value = self.prompt_input.read(cx).value().as_ref() != value;
                if sync_value {
                    self.suppress_query = Some(value.clone());
                }
                self.prompt_input.update(cx, |input, cx| {
                    input.set_placeholder(
                        spec.text_hint.clone().unwrap_or_else(|| "Value".to_owned()),
                        window,
                        cx,
                    );
                    if sync_value {
                        input.set_value(value, window, cx);
                    }
                });
            }
        }
        self.sync_fields(window, cx);
        cx.notify();
    }

    fn sync_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fields = self
            .spec
            .as_ref()
            .map(|spec| spec.fields.clone())
            .unwrap_or_default();
        self.fields
            .retain(|id, _| fields.iter().any(|field| &field.id == id));
        for field in fields
            .into_iter()
            .filter(|field| matches!(field.kind, DialogFieldKind::Text))
        {
            let (input, _) = self.fields.entry(field.id.clone()).or_insert_with(|| {
                let input = cx.new(|cx| InputState::new(window, cx));
                let id = field.id.clone();
                let subscription = cx.subscribe_in(
                    &input,
                    window,
                    move |this, input, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Change) {
                            let value = input.read(cx).value().to_string();
                            if let Some(spec) = &this.spec
                                && spec
                                    .fields
                                    .iter()
                                    .any(|field| field.id == id && field.value != value)
                            {
                                cx.emit(DialogIntent::FieldChanged {
                                    dialog: spec.id.clone(),
                                    field: id.clone(),
                                    value,
                                });
                            }
                        }
                    },
                );
                (input, subscription)
            });
            input.update(cx, |input, cx| {
                input.set_placeholder(field.placeholder, window, cx);
                if input.value().as_ref() != field.value {
                    input.set_value(field.value, window, cx);
                }
            });
        }
    }

    #[must_use]
    pub fn is_non_modal(&self) -> bool {
        self.spec
            .as_ref()
            .is_some_and(|spec| spec.role == DialogRole::TerminalFind)
    }

    #[must_use]
    pub fn root_id(&self) -> Option<DialogId> {
        self.spec.as_ref().map(|spec| spec.id.clone())
    }

    /// The title and close affordance belong to the window-level Root dialog. Palettes keep
    /// their compact command surface and intentionally do not add a second title row.
    #[must_use]
    pub fn root_title(&self) -> Option<String> {
        self.spec.as_ref().and_then(|spec| match spec.role {
            DialogRole::SearchableList | DialogRole::TerminalFind => None,
            DialogRole::Prompt | DialogRole::Confirm | DialogRole::ThemePicker => {
                Some(spec.title.clone())
            }
        })
    }

    fn toggle_favorite_selected(&self, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else { return };
        let Some(index) = self.command.read(cx).selected_index() else {
            return;
        };
        let destructive = gpui_kit::component::Theme::global(cx)
            .semantic_tokens()
            .colors
            .destructive;
        let (_, rows) = command_entries(spec, destructive);
        let Some(row) = command_row(&rows, index) else {
            return;
        };
        cx.emit(DialogIntent::ToggleFavorite {
            dialog: spec.id.clone(),
            row: row.id.clone(),
        });
    }

    fn change_text(&mut self, value: String, cx: &mut Context<Self>) {
        let Some(spec) = &mut self.spec else { return };
        let Some(text) = &mut spec.text else { return };
        if self.suppress_query.as_deref() == Some(value.as_str()) {
            self.suppress_query = None;
            return;
        }
        self.suppress_query = None;
        text.clone_from(&value);
        cx.emit(DialogIntent::TextChanged {
            dialog: spec.id.clone(),
            value,
        });
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((dialog, role)) = self.spec.as_ref().map(|spec| (spec.id.clone(), spec.role))
        else {
            return;
        };
        match event.keystroke.key.as_str() {
            // Root owns Escape for modal dialogs. Anchored TerminalFind still needs a local
            // dismissal path because it is intentionally outside Root's modal layer.
            "escape" if role == DialogRole::TerminalFind => self.cancel(cx),
            "enter" if role == DialogRole::Confirm && self.confirm_focus.is_focused(window) => {
                self.confirm_first_action(cx);
            }
            "tab" if role == DialogRole::ThemePicker => {
                cx.emit(DialogIntent::CycleScope { dialog });
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn on_select_up(
        &mut self,
        _: &gpui_kit::base::actions::SelectUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch_command_action(Box::new(gpui_kit::base::actions::SelectUp), window, cx);
    }

    fn on_select_down(
        &mut self,
        _: &gpui_kit::base::actions::SelectDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch_command_action(Box::new(gpui_kit::base::actions::SelectDown), window, cx);
    }

    fn on_confirm(
        &mut self,
        _: &gpui_kit::base::actions::Confirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch_command_action(
            Box::new(gpui_kit::base::actions::Confirm { secondary: false }),
            window,
            cx,
        );
    }

    fn on_cancel_action(
        &mut self,
        _: &gpui_kit::base::actions::Cancel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if has_active_root_dialog(window, cx) {
            // The Root dialog owns the modal pop. Keep the action moving so its host closes and
            // invokes the caller's cancellation callback exactly once.
            cx.propagate();
        } else {
            self.cancel(cx);
        }
    }

    fn panel(
        &mut self,
        spec: &DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        match spec.role {
            DialogRole::Prompt => self.prompt_panel(spec, window, cx),
            DialogRole::Confirm => Self::confirm_panel(spec, cx),
            DialogRole::SearchableList | DialogRole::ThemePicker => {
                self.command_panel(spec, window, cx)
            }
            DialogRole::TerminalFind => self.terminal_find_panel(spec, cx),
        }
    }

    fn prompt_panel(
        &self,
        spec: &DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let colors = gpui_kit::component::Theme::global(cx).colors;
        let destructive = gpui_kit::component::Theme::global(cx)
            .semantic_tokens()
            .colors
            .destructive;
        let submit = spec.rows.iter().find(|row| row.id.0 == "submit");
        let submit_enabled = submit.is_some_and(|row| row.enabled);
        let submit_detail = submit.and_then(|row| row.detail.clone());
        let invalid = submit_detail.is_some();
        let submit_row = submit.cloned();
        let cancel_dialog = spec.id.clone();
        let owner = cx.weak_entity();
        let submit_button = Button::new("dialog-prompt-submit")
            .primary()
            .disabled(!submit_enabled)
            .label(submit.map_or_else(|| "Confirm".to_owned(), |row| row.label.clone()))
            .on_click(move |_, _, cx| {
                if submit_enabled && let Some(row) = submit_row.clone() {
                    _ = owner.update(cx, |this, cx| {
                        this.confirm(&row, false, cx);
                    });
                }
            });
        let input = crate::gpui::focus_input(
            &self.prompt_input,
            Input::new(&self.prompt_input)
                .disabled(spec.busy)
                .w_full()
                .when(invalid, |input| input.border_color(destructive)),
        );
        let cancel_button = Button::new("dialog-prompt-cancel")
            .ghost()
            .label("Cancel")
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(DialogIntent::Dismiss {
                    dialog: cancel_dialog.clone(),
                });
            }));
        let fields = spec
            .fields
            .iter()
            .filter_map(|field| self.prompt_field(spec, field, window, cx))
            .collect::<Vec<_>>();
        let extras = Self::prompt_extra_actions(spec, cx);
        v_flex()
            .id("dialog-prompt-panel")
            .debug_selector(|| "dialog-prompt-panel".to_owned())
            .w_full()
            .gap_3()
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .when_some(spec.text_label.clone(), |this, label| {
                        this.child(div().text_sm().child(label))
                    })
                    .child(public_selector(
                        "dialog-prompt-input",
                        div().w_full().child(input),
                    )),
            )
            .when(!fields.is_empty(), |this| {
                this.child(
                    v_flex()
                        .id("dialog-prompt-fields")
                        .w_full()
                        .gap_3()
                        .max_h(rems(24.0))
                        .overflow_y_scroll()
                        .children(fields),
                )
            })
            .when_some(submit_detail, |this, detail| {
                this.child(public_selector(
                    "dialog-prompt-validation",
                    div().text_sm().text_color(destructive).child(detail),
                ))
            })
            .when_some(spec.footer.clone(), |this, footer| {
                this.child(
                    div()
                        .id("dialog-prompt-footer")
                        .text_sm()
                        .text_color(colors.muted_foreground)
                        .child(footer),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .flex_wrap()
                    .gap_2()
                    .child(public_selector("dialog-prompt-cancel", cancel_button))
                    .children(extras)
                    .child(public_selector("dialog-prompt-submit", submit_button)),
            )
            .into_any_element()
    }

    fn prompt_extra_actions(spec: &DialogSpec, cx: &Context<Self>) -> Vec<Button> {
        spec.rows
            .iter()
            .filter(|row| row.id.0 != "submit" && row.action.is_some())
            .cloned()
            .map(|row| {
                let owner = cx.weak_entity();
                Button::new(SharedString::from(format!("dialog-action-{}", row.id.0)))
                    .label(row.label.clone())
                    .disabled(!row.enabled || spec.busy)
                    .on_click(move |_, _, cx| {
                        _ = owner.update(cx, |this, cx| this.confirm(&row, false, cx));
                    })
            })
            .collect::<Vec<_>>()
    }

    fn prompt_field(
        &self,
        spec: &DialogSpec,
        field: &DialogField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let control = match &field.kind {
            DialogFieldKind::Text => {
                let (input, _) = self.fields.get(&field.id)?;
                public_selector(
                    format!("dialog-field-input-{}", field.id),
                    crate::gpui::focus_input(input, Input::new(input).disabled(spec.busy).w_full()),
                )
            }
            DialogFieldKind::Choice(options) => {
                let owner = cx.weak_entity();
                let id = field.id.clone();
                let dialog = spec.id.clone();
                let options = options.clone();
                gpui_kit::component::radio::RadioGroup::horizontal(SharedString::from(format!(
                    "dialog-field-choice-{id}"
                )))
                .disabled(spec.busy)
                .selected_index(options.iter().position(|value| value == &field.value))
                .children(options.clone())
                .on_click(move |index, _, cx| {
                    if let Some(value) = options.get(*index) {
                        _ = owner.update(cx, |_, cx| {
                            cx.emit(DialogIntent::FieldChanged {
                                dialog: dialog.clone(),
                                field: id.clone(),
                                value: value.clone(),
                            });
                        });
                    }
                })
                .into_any_element()
            }
            DialogFieldKind::Color => {
                use super::color_picker::{
                    ColorPickerParams, ColorPickerUpdate, render_color_picker,
                };
                let owner = cx.weak_entity();
                let id = field.id.clone();
                let dialog = spec.id.clone();
                render_color_picker(
                    ColorPickerParams {
                        selector: format!("dialog-color-{}-{}", spec.id.0, field.id),
                        label: &field.label,
                        value: &field.value,
                        default_label: "Default",
                        enabled: !spec.busy,
                        resettable: true,
                    },
                    std::rc::Rc::new(move |update, cx| {
                        let value = match update {
                            ColorPickerUpdate::Set(value) => value,
                            ColorPickerUpdate::Reset => String::new(),
                        };
                        _ = owner.update(cx, |_, cx| {
                            cx.emit(DialogIntent::FieldChanged {
                                dialog: dialog.clone(),
                                field: id.clone(),
                                value,
                            });
                        });
                    }),
                    window,
                    cx,
                )
            }
        };
        Some(
            v_flex()
                .id(SharedString::from(format!("dialog-field-{}", field.id)))
                .w_full()
                .gap_1()
                .child(div().text_sm().child(field.label.clone()))
                .child(control)
                .into_any_element(),
        )
    }

    fn confirm_panel(spec: &DialogSpec, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let colors = gpui_kit::component::Theme::global(cx).colors;
        let rows = spec.rows.iter().map(|row| Self::confirm_row(row, cx));
        let cancel_dialog = spec.id.clone();
        v_flex()
            .id("dialog-confirm-panel")
            .debug_selector(|| "dialog-confirm-panel".to_owned())
            .w_full()
            .gap_3()
            .when_some(spec.text.clone(), |this, text| {
                this.child(div().id("dialog-confirm-text").text_sm().child(text))
            })
            .children(rows)
            .when_some(spec.footer.clone(), |this, footer| {
                this.child(
                    div()
                        .id("dialog-confirm-footer")
                        .text_sm()
                        .text_color(colors.muted_foreground)
                        .child(footer),
                )
            })
            .child(
                h_flex().w_full().justify_end().gap_2().child(
                    Button::new("dialog-confirm-cancel")
                        .ghost()
                        .label("Cancel")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(DialogIntent::Dismiss {
                                dialog: cancel_dialog.clone(),
                            });
                        })),
                ),
            )
            .into_any_element()
    }

    fn confirm_row(row: &DialogRow, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let colors = gpui_kit::component::Theme::global(cx).colors;
        let destructive = gpui_kit::component::Theme::global(cx)
            .semantic_tokens()
            .colors
            .destructive;
        let owner = cx.weak_entity();
        let row_id = row.id.0.clone();
        let action = row.action.clone();
        let enabled = row.enabled && action.is_some();
        let mut button = Button::new(format!("dialog-confirm-action-{row_id}"))
            .label(row.label.clone())
            .disabled(!enabled);
        if row.destructive {
            button = button.danger();
        }
        let row_for_activation = row.clone();
        button = button.on_click(move |_, _, cx| {
            if enabled {
                _ = owner.update(cx, |this, cx| this.confirm(&row_for_activation, false, cx));
            }
        });
        let foreground = if row.enabled {
            if row.destructive {
                destructive
            } else {
                colors.foreground
            }
        } else {
            colors.muted_foreground
        };
        h_flex()
            .id(format!("dialog-confirm-row-{row_id}"))
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_1()
                    .when(action.is_none(), |this| {
                        this.child(div().text_color(foreground).child(row.label.clone()))
                    })
                    .when_some(row.detail.clone(), |this, detail| {
                        this.child(public_selector(
                            format!("dialog-confirm-detail-{row_id}"),
                            div()
                                .text_sm()
                                .text_color(colors.muted_foreground)
                                .child(detail),
                        ))
                    })
                    .when_some(row.trailing.clone(), |this, trailing| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(colors.muted_foreground)
                                .child(trailing),
                        )
                    }),
            )
            .when_some(row.keybinding.clone(), |this, keybinding| {
                this.child(crate::gpui::keybinding_element_from_text(&keybinding))
            })
            .when(action.is_some(), |this| {
                this.child(public_selector(
                    format!("dialog-confirm-action-{row_id}"),
                    button,
                ))
            })
            .into_any_element()
    }

    fn command_panel(
        &mut self,
        spec: &DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let destructive = gpui_kit::component::Theme::global(cx)
            .semantic_tokens()
            .colors
            .destructive;
        let (entries, rows) = command_entries(spec, destructive);
        let query_owner = cx.weak_entity();
        let select_owner = cx.weak_entity();
        let confirm_owner = cx.weak_entity();
        let cancel_owner = cx.weak_entity();
        let command = entries
            .into_iter()
            .fold(Command::new(&self.command), |command, entry| match entry {
                CommandEntry::Item(item) => command.item(item),
                CommandEntry::Group(group) => command.group(group),
                CommandEntry::Separator => command.separator(),
            });
        let keybindings = self.command_keybindings.clone();
        let mut command = command
            .w_full()
            .max_w_full()
            // The product model supplies filtered, ranked rows. Command owns query input,
            // selection, focus, and confirmation without interpreting the query again.
            .searchable(spec.text.is_some())
            // Root owns the modal frame. Keep Command's content unframed so ordinary dialogs
            // do not grow a second border, radius, and elevation inside that frame.
            .bordered(false)
            .filterable(false)
            .placeholder(
                spec.text_hint
                    .clone()
                    .unwrap_or_else(|| "Search…".to_owned()),
            )
            .empty({
                let empty_text = spec.empty_text.clone();
                move |_, _, cx| {
                    v_flex()
                        .w_full()
                        .items_center()
                        .py_6()
                        .text_sm()
                        .text_color(
                            gpui_kit::component::Theme::global(cx)
                                .colors
                                .muted_foreground,
                        )
                        .child(empty_text.clone())
                }
            })
            .max_h(gpui_kit::rems(24.0))
            .footer({
                let hint = spec
                    .hint
                    .clone()
                    .unwrap_or_else(|| "↑ ↓ navigate   Enter select   Esc close".to_owned());
                let count = spec.footer.clone();
                move |_, _, cx| command_footer(&hint, count.as_deref(), keybindings.as_deref(), cx)
            })
            .on_query(move |query, _, cx| {
                _ = query_owner.update(cx, |this, cx| this.change_text(query.to_owned(), cx));
            })
            .on_cancel(move |window, cx| {
                // Root owns dismissal for hosted dialogs. Standalone DialogView users retain
                // the same command cancellation behavior for focused unit surfaces.
                if !has_active_root_dialog(window, cx) {
                    _ = cancel_owner.update(cx, |this, cx| this.cancel(cx));
                }
            })
            .on_select({
                let rows = rows.clone();
                move |index, _, cx| {
                    if let Some(row) = command_row(&rows, index) {
                        _ = select_owner.update(cx, |this, cx| {
                            if this.suppress_initial_selection {
                                this.suppress_initial_selection = false;
                            } else {
                                this.selection_changed(row, cx);
                            }
                        });
                    }
                }
            })
            .on_confirm({
                let rows = rows.clone();
                move |index, _, cx| {
                    if let Some(row) = command_row(&rows, index) {
                        _ = confirm_owner.update(cx, |this, cx| this.confirm(row, false, cx));
                    }
                }
            });
        command = Self::command_header(command, spec, cx);
        self.sync_command_selection(&rows, spec.role == DialogRole::ThemePicker, window, cx);
        let command = command.render(window, cx);
        command.into_any_element()
    }

    fn command_header(mut command: Command, spec: &DialogSpec, cx: &Context<Self>) -> Command {
        if spec.role == DialogRole::ThemePicker {
            let owner = cx.weak_entity();
            let scope_spec = spec.clone();
            command =
                command.header(move |_, _, cx| theme_scope_control(&scope_spec, owner.clone(), cx));
        } else if spec.text.is_none() {
            let title = spec.title.clone();
            let context = spec
                .rows
                .iter()
                .filter(|row| row.action.is_none())
                .map(|row| row.label.clone())
                .collect::<Vec<_>>();
            command = command.header(move |_, _, cx| {
                let colors = gpui_kit::component::Theme::global(cx).colors;
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(title.clone()),
                    )
                    .children(context.iter().map(|text| {
                        div()
                            .text_sm()
                            .text_color(colors.muted_foreground)
                            .child(text.clone())
                    }))
            });
        }
        command
    }

    fn sync_command_selection(
        &mut self,
        rows: &[Vec<DialogRow>],
        theme_picker: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.select_initial_current {
            self.select_initial_current = false;
            if let Some(index) = initial_selection(rows, theme_picker) {
                let command = self.command.clone();
                let owner = cx.weak_entity();
                window.defer(cx, move |window, cx| {
                    if command.read(cx).selected_index() != Some(index) {
                        _ = owner.update(cx, |this, _| this.suppress_initial_selection = true);
                        command.update(cx, |state, cx| {
                            state.set_selected_index(Some(index), window, cx);
                        });
                    }
                });
            }
        } else if let Some(row_id) = self.preserve_selected_row.take()
            && let Some(index) = command_index_for_row(rows, &row_id)
        {
            let command = self.command.clone();
            window.defer(cx, move |window, cx| {
                if command.read(cx).selected_index() != Some(index) {
                    command.update(cx, |state, cx| {
                        state.set_selected_index(Some(index), window, cx);
                    });
                }
            });
        }
    }

    fn selected_row_id(&self, cx: &App) -> Option<RowId> {
        let spec = self.spec.as_ref()?;
        let selected = self.command.read(cx).selected_index()?;
        let destructive = gpui_kit::component::Theme::global(cx)
            .semantic_tokens()
            .colors
            .destructive;
        let (_, rows) = command_entries(spec, destructive);
        command_row(&rows, selected).map(|row| row.id.clone())
    }

    fn terminal_find_panel(&self, spec: &DialogSpec, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let colors = gpui_kit::component::Theme::global(cx).colors;
        let regex = spec.rows.iter().find(|row| row.id.0 == "regex");
        let case_sensitive = spec.rows.iter().find(|row| row.id.0 == "case_sensitive");
        let previous = spec.rows.iter().find(|row| row.id.0 == "previous");
        let next = spec.rows.iter().find(|row| row.id.0 == "next");
        let close_dialog = spec.id.clone();
        let bar = div()
            .w(rems(31.0))
            .max_w_full()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .size(rems(1.75))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(crate::gpui::icon("search", 14.0, colors.muted_foreground)),
            )
            .child(
                div().min_w_0().flex_1().rounded(cx.theme().radius).child(
                    crate::gpui::focus_input(
                        &self.find_input,
                        Input::new(&self.find_input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
                            .p_0(),
                    ),
                ),
            )
            .when_some(regex, |element, row| {
                element.child(Self::find_action_button(&spec.id, row, ".*", cx))
            })
            .when_some(case_sensitive, |element, row| {
                element.child(Self::find_action_button(&spec.id, row, "Aa", cx))
            })
            .when_some(previous, |element, row| {
                element.child(Self::find_action_button(&spec.id, row, "↑", cx))
            })
            .when_some(next, |element, row| {
                element.child(Self::find_action_button(&spec.id, row, "↓", cx))
            })
            .when_some(spec.footer.clone(), |element, count| {
                element.child(
                    div()
                        .min_w(rems(2.625))
                        .flex_none()
                        .text_center()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(count),
                )
            })
            .child(
                Button::new("find-close")
                    .ghost()
                    .compact()
                    .label("×")
                    .accessibility_label("Close find")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(DialogIntent::Dismiss {
                            dialog: close_dialog.clone(),
                        });
                    })),
            )
            .into_any_element();
        v_flex()
            .max_w_full()
            .child(bar)
            .when_some(spec.hint.clone(), |element, error| {
                element.child(Self::find_error(error, cx))
            })
            .into_any_element()
    }
    fn find_error(error: String, cx: &App) -> impl IntoElement {
        let colors = gpui_kit::component::Theme::global(cx).colors;
        div()
            .w(rems(31.0))
            .max_w_full()
            .p_2()
            .bg(colors.background)
            .text_xs()
            .text_color(
                gpui_kit::component::Theme::global(cx)
                    .semantic_tokens()
                    .colors
                    .destructive,
            )
            .child(error)
    }

    fn find_action_button(
        dialog: &DialogId,
        row: &DialogRow,
        glyph: &'static str,
        cx: &Context<Self>,
    ) -> Button {
        let dialog = dialog.clone();
        let row_id = row.id.clone();
        let action = row.action.clone();
        Button::new(SharedString::from(format!("find-{}", row.id.0)))
            .ghost()
            .compact()
            .label(glyph)
            .accessibility_label(row.label.clone())
            .selected(row.current)
            .toggled(row.current)
            .on_click(cx.listener(move |_, _, _, cx| {
                if let Some(action) = action.clone() {
                    cx.emit(DialogIntent::Activate {
                        dialog: dialog.clone(),
                        row: row_id.clone(),
                        action: action.id,
                        payload: action.payload,
                    });
                }
            }))
    }
}

fn command_entries(
    spec: &DialogSpec,
    destructive_color: Hsla,
) -> (Vec<CommandEntry>, std::rc::Rc<Vec<Vec<DialogRow>>>) {
    let mut sections: Vec<(Option<String>, Vec<DialogRow>)> = Vec::new();
    let mut heading = None;
    let mut rows = Vec::new();
    for row in &spec.rows {
        // Quick-action palettes keep decision context in their header, outside navigation.
        if spec.text.is_none() && row.action.is_none() {
            continue;
        }
        let is_section =
            !row.enabled && row.action.is_none() && row.icon.is_none() && row.detail.is_none();
        if is_section {
            if heading.is_some() || !rows.is_empty() {
                sections.push((heading.take(), std::mem::take(&mut rows)));
            }
            heading = Some(row.label.clone());
        } else {
            rows.push(row.clone());
        }
    }
    if heading.is_some() || !rows.is_empty() {
        sections.push((heading, rows));
    }

    let indexed_rows = std::rc::Rc::new(
        sections
            .iter()
            .map(|(_, rows)| rows.clone())
            .collect::<Vec<_>>(),
    );
    let entries = sections
        .into_iter()
        .map(|(heading, rows)| {
            let mut group = CommandGroup::new();
            if let Some(heading) = heading {
                group = group.label(heading);
            }
            CommandEntry::Group(
                group.items(
                    rows.into_iter()
                        .map(|item| command_item(item, destructive_color)),
                ),
            )
        })
        .collect();
    (entries, indexed_rows)
}

fn command_item(row: DialogRow, destructive_color: Hsla) -> CommandItem {
    let enabled = row.enabled;
    let current = row.current;
    let search_label = row.label.clone();
    let prompt_button = row.id.0 == "submit";
    let destructive = row.destructive;
    CommandItem::new()
        .label(search_label)
        .checked(current)
        .disabled(!enabled)
        .child(move |_, cx| {
            let colors = &gpui_kit::component::Theme::global(cx).colors;
            if prompt_button {
                return Button::new(SharedString::from("dialog-button-submit"))
                    .primary()
                    .disabled(!enabled)
                    .label(row.label.clone())
                    .into_any_element();
            }
            let foreground = if !enabled {
                colors.muted_foreground
            } else if destructive {
                destructive_color
            } else {
                row.color.unwrap_or(colors.foreground)
            };
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .when_some(row.icon.clone(), |this, icon| {
                    this.child(crate::gpui::icon(&icon, 14.0, foreground))
                })
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .text_color(foreground)
                        .child(div().truncate().child(row.label.clone()))
                        .when_some(row.detail.clone(), |this, detail| {
                            this.child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(colors.muted_foreground)
                                    .child(detail),
                            )
                        }),
                )
                .when_some(row.keybinding.clone(), |this, keybinding| {
                    this.child(crate::gpui::keybinding_element_from_text(&keybinding))
                })
                .when_some(row.trailing.clone(), |this, trailing| {
                    this.when(row.keybinding.is_none(), |this| {
                        this.child(
                            div()
                                .flex_none()
                                .truncate()
                                .text_xs()
                                .text_color(colors.muted_foreground)
                                .child(trailing),
                        )
                    })
                })
                .into_any_element()
        })
}

fn public_selector(id: impl Into<String>, child: impl IntoElement) -> gpui_kit::AnyElement {
    let id = id.into();
    let debug_selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || debug_selector)
        .child(child)
        .into_any_element()
}

fn command_row(rows: &[Vec<DialogRow>], index: IndexPath) -> Option<&DialogRow> {
    rows.get(index.section)?.get(index.row)
}

fn command_index_for_row(rows: &[Vec<DialogRow>], row_id: &RowId) -> Option<IndexPath> {
    rows.iter().enumerate().find_map(|(section, rows)| {
        rows.iter()
            .position(|row| &row.id == row_id)
            .map(|row| IndexPath::new(row).section(section))
    })
}

fn initial_selection(rows: &[Vec<DialogRow>], select_current: bool) -> Option<IndexPath> {
    let find = |predicate: fn(&DialogRow) -> bool| {
        rows.iter().enumerate().find_map(|(section, rows)| {
            rows.iter()
                .enumerate()
                .find(|(_, row)| predicate(row))
                .map(|(row, _)| IndexPath::new(row).section(section))
        })
    };
    select_current
        .then(|| find(|row| row.enabled && row.current))
        .flatten()
        .or_else(|| find(|row| row.enabled))
}

fn theme_scope_control(
    spec: &DialogSpec,
    owner: gpui_kit::WeakEntity<DialogView>,
    cx: &App,
) -> gpui_kit::AnyElement {
    let colors = gpui_kit::component::Theme::global(cx).colors;
    let active = spec
        .footer
        .as_deref()
        .and_then(|footer| footer.rsplit_once('·').map(|(_, scope)| scope.trim()))
        .unwrap_or("All")
        .to_owned();
    let dialog = spec.id.clone();
    div()
        .mb_3()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(colors.muted_foreground)
                .child("Scope"),
        )
        .child(
            Button::new("theme-scope-cycle")
                .ghost()
                .compact()
                .label(active)
                .accessibility_label("Cycle theme scope")
                .on_click(move |_, _, cx| {
                    _ = owner.update(cx, |_, cx| {
                        cx.emit(DialogIntent::CycleScope {
                            dialog: dialog.clone(),
                        });
                    });
                }),
        )
        .into_any_element()
}

fn keycap_hint(hint: &str, cx: &App) -> gpui_kit::AnyElement {
    let muted = gpui_kit::component::Theme::global(cx)
        .colors
        .muted_foreground;
    div()
        .flex()
        .items_center()
        .gap_1()
        .children(hint.split_whitespace().map(|word| {
            hint_key(word)
                .and_then(|key| Keystroke::parse(key).ok())
                .map_or_else(
                    || {
                        div()
                            .text_color(muted)
                            .child(word.to_owned())
                            .into_any_element()
                    },
                    |key| crate::gpui::keybinding_element(&key),
                )
        }))
        .into_any_element()
}

fn keycap_bindings(
    bindings: &[(CommandAction, String)],
    hint: &str,
    cx: &App,
) -> gpui_kit::AnyElement {
    let labels = [
        (CommandAction::Previous, "Navigate".to_owned()),
        (CommandAction::Next, "Navigate".to_owned()),
        (
            CommandAction::Confirm,
            hint_label(hint, "Enter").unwrap_or("Confirm").to_owned(),
        ),
        (
            CommandAction::Cancel,
            hint_label(hint, "Esc").unwrap_or("Close").to_owned(),
        ),
        (CommandAction::ToggleFavorite, "Favorite".to_owned()),
    ];
    h_flex()
        .items_center()
        .gap_2()
        .children(labels.into_iter().filter_map(|(action, label)| {
            if action == CommandAction::ToggleFavorite
                && !hint
                    .split_whitespace()
                    .any(|word| word.eq_ignore_ascii_case("favorite"))
            {
                return None;
            }
            let binding = bindings
                .iter()
                .find_map(|(candidate, key)| (*candidate == action).then_some(key))?;
            Some(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(crate::gpui::keybinding_element_from_text(binding))
                    .child(
                        div()
                            .text_xs()
                            .text_color(
                                gpui_kit::component::Theme::global(cx)
                                    .colors
                                    .muted_foreground,
                            )
                            .child(label),
                    )
                    .into_any_element(),
            )
        }))
        .into_any_element()
}

fn hint_label<'a>(hint: &'a str, key: &str) -> Option<&'a str> {
    let words = hint.split_whitespace().collect::<Vec<_>>();
    words
        .iter()
        .position(|word| word.eq_ignore_ascii_case(key))
        .and_then(|index| words.get(index.saturating_add(1)).copied())
}

fn has_active_root_dialog(window: &mut Window, cx: &mut App) -> bool {
    window
        .root::<gpui_kit::component::Root>()
        .flatten()
        .is_some()
        && window.has_active_dialog(cx)
}

fn hint_key(word: &str) -> Option<&str> {
    Some(match word {
        "Enter" => "enter",
        "Esc" => "escape",
        "Tab" => "tab",
        "↑" => "up",
        "↓" => "down",
        "←" => "left",
        "→" => "right",
        "⌘F" | "Cmd+F" => "cmd-f",
        "Ctrl+Shift+F" => "ctrl-shift-f",
        _ => return None,
    })
}

impl EventEmitter<DialogIntent> for DialogView {}
impl EventEmitter<gpui_kit::DismissEvent> for DialogView {}

impl OverlayView for DialogView {
    fn overlay_placement(&self) -> OverlayPlacement {
        match self.spec.as_ref().map(|spec| spec.placement) {
            Some(DialogPlacement::TopRight) => OverlayPlacement::TopRight,
            Some(DialogPlacement::BottomRight) => OverlayPlacement::BottomRight,
            Some(DialogPlacement::Center) | None => OverlayPlacement::Center,
        }
    }
}

impl Focusable for DialogView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.spec.as_ref().map(|spec| spec.role) {
            Some(DialogRole::TerminalFind) => self.find_input.read(cx).focus_handle(cx),
            Some(DialogRole::Prompt) => self.prompt_input_focus.clone(),
            Some(DialogRole::Confirm) => self.confirm_focus.clone(),
            Some(DialogRole::SearchableList | DialogRole::ThemePicker) | None => {
                if self.spec.as_ref().is_some_and(|spec| spec.text.is_none()) {
                    // Before its first render Command still targets its default search input.
                    // Focus our retained frame until the non-searchable model is installed.
                    self.command_focus.clone()
                } else {
                    self.command.read(cx).focus_handle(cx)
                }
            }
        }
    }
}

impl Render for DialogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(spec) = self.spec.clone() else {
            return div();
        };
        let command_focus = self.command_focus.clone();
        let prompt_input_focus = self.prompt_input_focus.clone();
        let confirm_focus = self.confirm_focus.clone();
        div()
            .when(spec.role == DialogRole::ThemePicker, |this| {
                this.key_context("Command BoottyThemePicker")
            })
            .when(
                matches!(
                    spec.role,
                    DialogRole::SearchableList | DialogRole::ThemePicker
                ),
                |this| {
                    this.when(spec.role != DialogRole::ThemePicker, |this| {
                        this.key_context("Command")
                    })
                    .track_focus(&command_focus)
                    .on_action(cx.listener(Self::on_select_up))
                    .on_action(cx.listener(Self::on_select_down))
                    .on_action(cx.listener(Self::on_confirm))
                    .on_action(cx.listener(Self::on_cancel_action))
                },
            )
            .when(spec.role == DialogRole::Prompt, |this| {
                this.key_context("Input")
                    .track_focus(&prompt_input_focus)
                    .on_action(
                        cx.listener(|this, _: &gpui_kit::base::actions::Confirm, _, cx| {
                            this.prompt_submit(cx);
                            cx.stop_propagation();
                        }),
                    )
            })
            .when(spec.role == DialogRole::Confirm, |this| {
                this.key_context("BoottyDialog")
                    .track_focus(&confirm_focus)
                    .on_action(
                        cx.listener(|this, _: &gpui_kit::base::actions::Confirm, _, cx| {
                            this.confirm_first_action(cx);
                        }),
                    )
            })
            .on_key_down(cx.listener(Self::on_key_down))
            .child(self.panel(&spec, window, cx))
    }
}

fn command_footer(
    hint: &str,
    count: Option<&str>,
    keybindings: Option<&[(CommandAction, String)]>,
    cx: &App,
) -> gpui_kit::AnyElement {
    let colors = gpui_kit::component::Theme::global(cx).colors;
    h_flex()
        .w_full()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(colors.border)
        .justify_between()
        .items_center()
        .gap_3()
        .child(keybindings.map_or_else(
            || keycap_hint(hint, cx),
            |bindings| keycap_bindings(bindings, hint, cx),
        ))
        .when_some(count, |this, count| {
            this.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(count.to_owned()),
            )
        })
        .into_any_element()
}
