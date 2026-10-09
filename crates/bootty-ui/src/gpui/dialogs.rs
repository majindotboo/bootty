//! Shared GPUI dialog controls and floating surfaces.
//!
//! This module owns presentation state only. Hosts project their domain models into [`DialogSpec`]
//! values and translate emitted [`DialogIntent`] values back into domain events.

use super::{OverlayPlacement, OverlayView};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IndexPath, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    command::{Command, CommandEntry, CommandGroup, CommandItem, CommandState},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    kbd::Kbd,
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement, KeyDownEvent,
    Keystroke, MouseButton, ParentElement, PromptButton, Render, SharedString, Styled,
    Subscription, Window, div, prelude::*, px, rems,
};
use std::{cell::RefCell, rc::Rc};

mod attachments;
mod completion;
mod model_picker;
mod project_picker;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerControl {
    Space,
    Provider,
    Model,
    Effort,
    Project,
    Worktree,
    Permissions,
}
impl ComposerControl {
    pub const ALL: [Self; 7] = [
        Self::Space,
        Self::Provider,
        Self::Model,
        Self::Effort,
        Self::Project,
        Self::Worktree,
        Self::Permissions,
    ];
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::Space => "host",
            Self::Provider => "provider",
            Self::Model => "model",
            Self::Effort => "reasoning",
            Self::Project => "project",
            Self::Worktree => "isolation",
            Self::Permissions => "permissions",
        }
    }
    #[must_use]
    pub const fn command(self) -> &'static str {
        match self {
            Self::Space => "focus-space",
            Self::Provider => "focus-provider",
            Self::Model => "focus-model",
            Self::Effort => "focus-effort",
            Self::Project => "focus-project",
            Self::Worktree => "focus-worktree",
            Self::Permissions => "focus-permissions",
        }
    }
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Space => "Choose Space",
            Self::Provider => "Choose Provider",
            Self::Model => "Choose Model",
            Self::Effort => "Choose Reasoning Effort",
            Self::Project => "Choose Project",
            Self::Worktree => "Focus Worktree Control",
            Self::Permissions => "Choose Permissions",
        }
    }
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
    Focus(ComposerControl),
}

fn composer_focus_action(
    control: ComposerControl,
    focus: FocusHandle,
) -> (crate::gpui_actions::InvokeCommand, FocusHandle) {
    let command = format!("ui.composer.{}", control.command());
    (
        crate::gpui_actions::InvokeCommand::new(bootty_control::CommandInvocation::from_action(
            &command,
            bootty_control::Caller::Keybinding,
        )),
        focus,
    )
}

fn with_composer_tooltip(
    id: impl Into<SharedString>,
    control: impl IntoElement,
    description: impl Into<SharedString>,
    actions: Vec<(crate::gpui_actions::InvokeCommand, FocusHandle)>,
) -> gpui_kit::AnyElement {
    let id = id.into();
    let description = description.into();
    div()
        .id(id.to_string())
        .child(control)
        .tooltip(move |window, cx| {
            let description = description.clone();
            let actions = actions.clone();
            Tooltip::element(move |window, _cx| {
                let bindings = actions
                    .iter()
                    .filter_map(|(action, focus)| Kbd::binding_for_action_in(action, focus, window))
                    .collect::<Vec<_>>();
                v_flex()
                    .gap_1()
                    .child(div().child(description.clone()))
                    .when(!bindings.is_empty(), |tooltip| {
                        tooltip.child(h_flex().gap_1().children(bindings))
                    })
            })
            .build(window, cx)
        })
        .into_any_element()
}

/// Dialog interactions retain the values captured by their rendered controls.
#[derive(Clone, Debug, PartialEq)]
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
    ApplicationsChanged {
        dialog: DialogId,
        applications: Vec<bootty_agents::NativeApplicationMention>,
    },
    AttachmentsChanged {
        dialog: DialogId,
        attachments: Vec<crate::presentation::new_session_form::NewSessionAttachment>,
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

impl DialogIntent {
    #[must_use]
    pub const fn dialog_id(&self) -> &DialogId {
        match self {
            Self::Dismiss { dialog }
            | Self::Activate { dialog, .. }
            | Self::Preview { dialog, .. }
            | Self::TextChanged { dialog, .. }
            | Self::FieldChanged { dialog, .. }
            | Self::ApplicationsChanged { dialog, .. }
            | Self::AttachmentsChanged { dialog, .. }
            | Self::SelectionChanged { dialog, .. }
            | Self::CycleScope { dialog }
            | Self::ToggleFavorite { dialog, .. }
            | Self::Find { dialog, .. }
            | Self::FocusTerminal { dialog } => dialog,
        }
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogSpaceChoice {
    pub label: String,
    pub icon: String,
    pub color: [u8; 3],
}

/// An owned, disposable projection of an app-owned dialog model.
#[derive(Clone, Debug, PartialEq)]
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
    pub spaces: Vec<DialogSpaceChoice>,
    pub attachments: Vec<crate::presentation::new_session_form::NewSessionAttachment>,
    pub applications: Vec<bootty_agents::NativeApplicationMention>,
    pub completion: Option<crate::CompletionScope>,
    pub models: Vec<bootty_agents::NativeModelOption>,
    pub projects: Vec<bootty_git::ProjectPickerEntry>,
    pub project_labels: std::collections::BTreeMap<String, String>,
    pub selected_project: Option<String>,
    pub models_loading: bool,
    pub model_error: Option<String>,
    pub selected_model: Option<String>,
    pub busy: bool,
    /// Multiline prompts use Enter for a newline and require the explicit submit button.
    pub multiline: bool,
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
            spaces: Vec::new(),
            attachments: Vec::new(),
            applications: Vec::new(),
            completion: None,
            models: Vec::new(),
            projects: Vec::new(),
            project_labels: std::collections::BTreeMap::new(),
            selected_project: None,
            models_loading: false,
            model_error: None,
            selected_model: None,
            busy: false,
            multiline: false,
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
            spaces: Vec::new(),
            attachments: Vec::new(),
            applications: Vec::new(),
            completion: None,
            models: Vec::new(),
            projects: Vec::new(),
            project_labels: std::collections::BTreeMap::new(),
            selected_project: None,
            models_loading: false,
            model_error: None,
            selected_model: None,
            busy: false,
            multiline: false,
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
    prompt_textarea: Entity<TextareaState>,
    attachment_imports: usize,
    attachment_error: Option<String>,
    attachment_epoch: u64,
    new_session_content: Option<gpui_kit::component::input::InputContent>,
    _prompt_textarea_subscription: Subscription,
    fields: std::collections::HashMap<String, (Entity<InputState>, Subscription)>,
    model_picker: Option<model_picker::NewModelPicker>,
    project_picker: Option<project_picker::NewProjectPicker>,
    completion_sender: Option<bootty_control::BoundAppCommandSender>,
    completion: Option<Entity<crate::gpui_composer_completion::ComposerCompletion>>,
    completion_subscriptions: Vec<Subscription>,
    /// Stable focus target for a confirm surface before its buttons render.
    confirm_focus: FocusHandle,
    control_focus: std::collections::BTreeMap<&'static str, FocusHandle>,
    provider_focus: FocusHandle,
    command_keybindings: Option<Vec<(CommandAction, String)>>,
    find_input: Entity<InputState>,
    suppress_query: Option<String>,
    preserve_selected_row: Option<RowId>,
    select_initial_current: bool,
    suppress_initial_selection: bool,
    _find_input_subscription: Subscription,
    _prompt_input_subscription: Subscription,
    _command_interceptor: Subscription,
    _new_session_interceptor: Subscription,
}

impl DialogView {
    fn composer_control_focus(
        cx: &Context<Self>,
    ) -> (
        std::collections::BTreeMap<&'static str, FocusHandle>,
        FocusHandle,
    ) {
        let control_focus = ComposerControl::ALL
            .into_iter()
            .map(|control| (control.field(), cx.focus_handle()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let provider_focus = control_focus
            .get("provider")
            .cloned()
            .unwrap_or_else(|| cx.focus_handle());
        (control_focus, provider_focus)
    }

    fn command_interceptor(cx: &mut Context<Self>) -> Subscription {
        let owner = cx.weak_entity();
        cx.intercept_keystrokes(move |event, window, cx| {
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
        })
    }

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
        let prompt_textarea = cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 8));
        let prompt_textarea_subscription =
            Self::subscribe_prompt_content(&prompt_textarea, window, cx);
        let command_interceptor = Self::command_interceptor(cx);
        let (control_focus, provider_focus) = Self::composer_control_focus(cx);
        Self {
            spec: None,
            command: cx.new(|cx| CommandState::new(window, cx)),
            command_focus: cx.focus_handle().tab_stop(true),
            prompt_input_focus: prompt_input.read(cx).focus_handle(cx),
            prompt_input,
            fields: std::collections::HashMap::new(),
            model_picker: None,
            project_picker: None,
            completion_sender: None,
            completion: None,
            completion_subscriptions: Vec::new(),
            confirm_focus: cx.focus_handle().tab_stop(true),
            control_focus,
            provider_focus,
            command_keybindings: None,
            find_input,
            suppress_query: None,
            preserve_selected_row: None,
            select_initial_current: false,
            suppress_initial_selection: false,
            _find_input_subscription: find_input_subscription,
            _prompt_input_subscription: prompt_input_subscription,
            prompt_textarea,
            attachment_imports: 0,
            attachment_error: None,
            attachment_epoch: 0,
            new_session_content: None,
            _prompt_textarea_subscription: prompt_textarea_subscription,
            _command_interceptor: command_interceptor,
            _new_session_interceptor: Self::new_session_interceptor(cx),
        }
    }

    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "GPUI subscribe_in requires a mutable Context"
    )]
    fn subscribe_prompt_content(
        prompt_textarea: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            prompt_textarea,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => this.change_prompt_content(input, cx),
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
                InputEvent::PressEnter { .. } => {}
            },
        )
    }

    fn new_session_interceptor(cx: &mut Context<Self>) -> Subscription {
        let owner = cx.weak_entity();
        cx.intercept_keystrokes(move |event, window, cx| {
            let new_session_enter = owner
                .read_with(cx, |this, app| {
                    !this.completion_active(window, app)
                        && this.spec.as_ref().is_some_and(|spec| {
                            spec.id.0 == crate::presentation::dialogs::NEW_SESSION_ID
                                && spec.multiline
                        })
                        && this
                            .prompt_textarea
                            .read(app)
                            .focus_handle(app)
                            .is_focused(window)
                        && event.keystroke.key.eq_ignore_ascii_case("enter")
                        && !event.keystroke.modifiers.shift
                        && !event.keystroke.modifiers.alt
                })
                .unwrap_or(false);
            if new_session_enter {
                let action = if event.keystroke.modifiers.platform {
                    "start-session-background"
                } else {
                    "enter-session"
                };
                _ = owner.update(cx, |this, cx| this.new_session_submit(action, cx));
                cx.stop_propagation();
            }
        })
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
        if matches!(action, CommandAction::Focus(_)) {
            self.perform_completion(action, window, cx);
            return;
        }
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
            | CommandAction::ToggleFavorite
            | CommandAction::Focus(_) => {}
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

    fn change_prompt_content(&mut self, input: &Entity<TextareaState>, cx: &mut Context<Self>) {
        if self
            .spec
            .as_ref()
            .is_some_and(|spec| spec.id.0 == crate::presentation::dialogs::NEW_SESSION_ID)
        {
            self.new_session_content = Some(input.read(cx).content());
            if let Some(spec) = self.spec.as_mut() {
                let applications = input
                    .read(cx)
                    .tokens()
                    .iter()
                    .filter_map(|span| {
                        let mut mention = spec
                            .applications
                            .iter()
                            .find(|mention| mention.id == span.token().id().as_ref())?
                            .clone();
                        mention.prompt_range = span.range();
                        Some(mention)
                    })
                    .fold(
                        std::collections::BTreeMap::new(),
                        |mut mentions, mention| {
                            mentions.insert(mention.id.clone(), mention);
                            mentions
                        },
                    )
                    .into_values()
                    .collect::<Vec<_>>();
                if applications != spec.applications {
                    spec.applications.clone_from(&applications);
                    cx.emit(DialogIntent::ApplicationsChanged {
                        dialog: spec.id.clone(),
                        applications,
                    });
                }
            }
        }
        self.change_text(input.read(cx).value().to_string(), cx);
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
        if spec.id.0 == crate::presentation::dialogs::NEW_SESSION_ID && self.attachment_imports > 0
        {
            return;
        }
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
            if row.id.0 == "submit" {
                self.publish_active_attachments(cx);
            }
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

    fn new_session_submit(&self, action: &str, cx: &mut Context<Self>) {
        if self.attachment_imports > 0 {
            return;
        }
        let Some(spec) = self.spec.as_ref().filter(|spec| !spec.busy) else {
            return;
        };
        self.publish_active_attachments(cx);
        cx.emit(DialogIntent::Activate {
            dialog: spec.id.clone(),
            row: RowId::new("submit"),
            action: ActionId::new(action),
            payload: DialogPayload::default(),
        });
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
            self.model_picker = None;
            self.project_picker = None;
            self.attachment_epoch = self.attachment_epoch.wrapping_add(1);
            self.attachment_error = None;
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
            } else if spec.role == DialogRole::Prompt && !spec.multiline {
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
        self.sync_prompt_textarea(window, cx);
        self.sync_fields(window, cx);
        self.sync_new_model_picker(window, cx);
        self.sync_new_project_picker(window, cx);
        self.sync_completion(window, cx);
        cx.notify();
    }

    fn sync_prompt_textarea(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(spec) = &self.spec
            && spec.role == DialogRole::Prompt
            && spec.multiline
        {
            let value = spec.text.clone().unwrap_or_default();
            if self.prompt_textarea.read(cx).value().as_ref() != value {
                self.suppress_query = Some(value.clone());
                let content = self
                    .new_session_content
                    .clone()
                    .filter(|content| {
                        spec.id.0 == crate::presentation::dialogs::NEW_SESSION_ID
                            && content.text().as_ref() == value
                            && content.tokens().iter().all(|span| {
                                span.token().id().starts_with("skill:")
                                    || spec.attachments.iter().any(|attachment| {
                                        span.token().id().as_ref()
                                            == attachment.path.to_string_lossy()
                                    })
                            })
                    })
                    .unwrap_or_else(|| value.into());
                self.prompt_textarea
                    .update(cx, |input, cx| input.set_value(content, window, cx));
            }
            self.prompt_textarea.update(cx, |input, cx| {
                input.set_placeholder(spec.text_hint.clone().unwrap_or_default(), window, cx);
            });
        }
    }

    fn sync_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fields = self
            .spec
            .as_ref()
            .map_or_else(Default::default, |spec| spec.fields.clone());
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
            DialogRole::Prompt if Self::is_agent_session_spec(spec) => None,
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
            "escape"
                if role == DialogRole::TerminalFind
                    || (dialog.0 == crate::presentation::dialogs::NEW_SESSION_ID
                        && role == DialogRole::Prompt) =>
            {
                self.cancel(cx);
            }
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
        if spec.multiline && spec.id.0 == crate::presentation::dialogs::NEW_SESSION_ID {
            return self.new_session_panel(spec, window, cx);
        }
        let colors = gpui_kit::component::Theme::global(cx).colors;
        let destructive = gpui_kit::component::Theme::global(cx)
            .semantic_tokens()
            .colors
            .destructive;
        let submit = spec.rows.iter().find(|row| row.id.0 == "submit");
        let submit_enabled = !spec.busy && submit.is_some_and(|row| row.enabled);
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
        let input = self.prompt_text_control(spec, invalid, cx);
        let cancel_button = Button::new("dialog-prompt-cancel")
            .disabled(spec.multiline && spec.busy)
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
            .child(Self::prompt_control_row(
                spec,
                "dialog-value",
                spec.text_label.as_deref(),
                public_selector("dialog-prompt-input", input),
            ))
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

    fn new_session_panel(
        &self,
        spec: &DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let owner = cx.weak_entity();
        let submit = spec.rows.iter().find(|row| row.id.0 == "submit");
        let error = submit.and_then(|row| row.detail.clone());
        if Self::is_agent_session_spec(spec) {
            return self.agent_session_panel(spec, submit, error, window, cx);
        }
        let mode = spec.fields.iter().find(|field| field.id == "mode");
        let details = spec
            .fields
            .iter()
            .filter(|field| matches!(field.kind, DialogFieldKind::Text))
            .filter_map(|field| self.prompt_field(spec, field, window, cx))
            .map(|control| div().flex_1().min_w(rems(8.0)).child(control))
            .collect::<Vec<_>>();
        v_flex()
            .id("dialog-prompt-panel")
            .debug_selector(|| "dialog-prompt-panel".to_owned())
            .w_full()
            .gap_3()
            .child(div().text_xl().child(spec.title.clone()))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .justify_between()
                    .gap_2()
                    .when_some(
                        spec.rows.iter().find(|row| row.id.0 == "choose-project"),
                        |row, project| {
                            row.child(self.render_new_project_picker(spec, project, window, cx))
                        },
                    )
                    .when_some(mode, |row, mode| {
                        row.child(Self::new_session_mode(spec, mode, cx))
                    }),
            )
            .child(public_selector(
                "dialog-prompt-input",
                Textarea::new(&self.prompt_textarea)
                    .token(crate::gpui_prompt_attachments::render)
                    .on_paste(move |item, window, cx| {
                        owner
                            .update(cx, |this, cx| {
                                this.paste_new_session_attachments(item, window, cx)
                            })
                            .unwrap_or(false)
                    })
                    .aria_label(
                        spec.text_label
                            .clone()
                            .unwrap_or_else(|| "Prompt".to_owned()),
                    )
                    .disabled(spec.busy)
                    .w_full()
                    .when(error.is_some(), |input| {
                        input.border_color(cx.theme().danger)
                    }),
            ))
            .when(
                !spec.attachments.is_empty()
                    || self.attachment_imports > 0
                    || self.attachment_error.is_some(),
                |row| row.child(self.render_new_session_attachments(spec, cx)),
            )
            .child(
                h_flex()
                    .id("dialog-prompt-fields")
                    .w_full()
                    .gap_1()
                    .flex_wrap()
                    .when(
                        spec.fields.iter().any(|field| field.id == "provider"),
                        |row| row.child(self.new_session_attachment_picker(spec, cx)),
                    )
                    .children(spec.fields.iter().filter_map(|field| {
                        if field.id == "isolation" {
                            Some(self.new_session_worktree(spec, field, cx))
                        } else {
                            self.new_session_choice(spec, field, false, cx)
                        }
                    })),
            )
            .when(!details.is_empty(), |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .items_start()
                        .gap_2()
                        .flex_wrap()
                        .children(details),
                )
            })
            .child(self.new_session_footer(spec, submit, error, cx))
            .into_any_element()
    }

    fn is_agent_session_spec(spec: &DialogSpec) -> bool {
        // Cmd+N can switch between modes; an Agent tab fixes the mode and omits its selector.
        let agent_mode = spec
            .fields
            .iter()
            .any(|field| field.id == "mode" && field.value == "Agent");
        let fixed_agent_tab = !spec.fields.iter().any(|field| field.id == "mode")
            && spec.fields.iter().any(|field| field.id == "provider");
        spec.id.0 == crate::presentation::dialogs::NEW_SESSION_ID
            && spec.multiline
            && (agent_mode || fixed_agent_tab)
    }

    fn agent_session_panel(
        &self,
        spec: &DialogSpec,
        submit: Option<&DialogRow>,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let owner = cx.weak_entity();
        let prompt_focused = self
            .prompt_textarea
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let prompt_border = if error.is_some() {
            cx.theme().danger
        } else if prompt_focused {
            cx.theme().ring
        } else {
            cx.theme().border
        };
        let choices = self.agent_session_choices(spec, window, cx);

        v_flex()
            .id("dialog-prompt-panel")
            .debug_selector(|| "dialog-prompt-panel".to_owned())
            .w_full()
            .max_w(rems(48.0))
            .min_w_0()
            .items_center()
            .gap_4()
            .child(self.agent_session_header(spec, window, cx))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .child(
                        v_flex()
                            .id("agent-session-composer")
                            .relative()
                            .when(self.completion_active(window, cx), |view| {
                                view.key_context("ComposerCompletion")
                            })
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().muted.opacity(0.35))
                            .border_1()
                            .border_color(prompt_border)
                            .when(
                                !spec.attachments.is_empty()
                                    || self.attachment_imports > 0
                                    || self.attachment_error.is_some(),
                                |row| row.child(self.render_new_session_attachments(spec, cx)),
                            )
                            .child(public_selector(
                                "dialog-prompt-input",
                                Textarea::new(&self.prompt_textarea)
                                    .token(crate::gpui_prompt_attachments::render)
                                    .on_paste(move |item, window, cx| {
                                        owner
                                            .update(cx, |this, cx| {
                                                this.paste_new_session_attachments(item, window, cx)
                                            })
                                            .unwrap_or(false)
                                    })
                                    .aria_label(
                                        spec.text_label
                                            .clone()
                                            .unwrap_or_else(|| "Prompt".to_owned()),
                                    )
                                    .appearance(false)
                                    .bordered(false)
                                    .disabled(spec.busy)
                                    .w_full(),
                            ))
                            .child(self.agent_session_controls(spec, choices, submit, cx))
                            .children(self.completion.clone()),
                    )
                    .child(self.agent_session_project_footer(spec, window, cx)),
            )
            .children(Self::new_model_error(spec, cx))
            .child(self.new_session_footer(spec, submit, error, cx))
            .into_any_element()
    }

    fn agent_session_choices(
        &self,
        spec: &DialogSpec,
        window: &Window,
        cx: &Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let width = f32::from(self.prompt_textarea.read(cx).input_bounds().size.width);
        let rem = f32::from(window.rem_size());
        spec.fields
            .iter()
            .filter(|field| {
                !(matches!(field.id.as_str(), "isolation" | "mode")
                    || field.id == "provider"
                        && spec.fields.iter().any(|candidate| candidate.id == "model"))
            })
            .filter_map(|field| {
                // Keep the access policy readable after combining provider and model.
                let compact = width
                    < rem
                        * match field.id.as_str() {
                            "host" => 50.,
                            "permissions" => 32.,
                            _ => 40.,
                        };
                let choice = self.new_session_choice(spec, field, compact, cx)?;
                let control = div()
                    .flex_shrink_0()
                    .min_w_0()
                    .when_some(self.control_focus.get(field.id.as_str()), |view, focus| {
                        view.track_focus(focus)
                    })
                    .child(public_selector(
                        format!("dialog-field-choice-control-{}", field.id),
                        choice,
                    ));
                let (description, focus_controls) = match field.id.as_str() {
                    "host" => (
                        "Choose where the agent will run.",
                        vec![ComposerControl::Space],
                    ),
                    "provider" | "model" => (
                        "Choose a provider and model.",
                        vec![ComposerControl::Provider, ComposerControl::Model],
                    ),
                    "reasoning" => (
                        "Choose the model's reasoning effort.",
                        vec![ComposerControl::Effort],
                    ),
                    "permissions" => (
                        "Choose how the agent can access your system.",
                        vec![ComposerControl::Permissions],
                    ),
                    _ => ("Choose an agent setting.", Vec::new()),
                };
                let focus = self
                    .model_picker
                    .as_ref()
                    .map(|picker| picker.state.focus_handle(cx));
                let actions = focus_controls
                    .into_iter()
                    .filter_map(|control| {
                        let focus = match control {
                            ComposerControl::Provider | ComposerControl::Model => focus.clone(),
                            _ => self.control_focus.get(control.field()).cloned(),
                        }?;
                        Some(composer_focus_action(control, focus))
                    })
                    .collect();
                Some(with_composer_tooltip(
                    format!("dialog-field-tooltip-{}", field.id),
                    control,
                    description,
                    actions,
                ))
            })
            .collect::<Vec<_>>()
    }

    fn agent_session_controls(
        &self,
        spec: &DialogSpec,
        choices: Vec<gpui_kit::AnyElement>,
        submit: Option<&DialogRow>,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        h_flex()
            .id("dialog-prompt-fields")
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                h_flex()
                    .id("composer-control-options")
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .text_sm()
                    .whitespace_nowrap()
                    .overflow_x_scroll()
                    .gap_2()
                    .when_some(
                        spec.fields.iter().find(|field| field.id == "mode"),
                        |row, mode| row.child(Self::new_session_mode(spec, mode, cx)),
                    )
                    .children(choices)
                    .when(spec.models_loading, |row| {
                        row.child(
                            h_flex()
                                .debug_selector(|| "new-session-model-loading".to_owned())
                                .items_center()
                                .gap_2()
                                .child(gpui_kit::component::spinner::Spinner::new().small())
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Loading models…"),
                                ),
                        )
                    }),
            )
            .child(public_selector(
                "dialog-prompt-attach",
                self.new_session_attachment_picker(spec, cx),
            ))
            .when_some(submit, |row, submit| {
                row.child(
                    Self::new_agent_session_start(spec, submit, cx)
                        .disabled(!submit.enabled || spec.busy || self.attachment_imports > 0),
                )
            })
            .into_any_element()
    }

    fn agent_session_header(
        &self,
        spec: &DialogSpec,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let project = Self::agent_project_name(spec);
        let project_choice = spec.rows.iter().find(|row| row.id.0 == "choose-project");
        h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .justify_center()
            .flex_wrap()
            .gap_1()
            .text_xl()
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(cx.theme().foreground)
            .child(if project.is_some() {
                "What should we build in"
            } else {
                "What should we build?"
            })
            .when_some(project, |row, name| {
                row.child(project_choice.map_or_else(
                    || div().child(name.to_owned()).into_any_element(),
                    |choice| self.render_new_project_picker(spec, choice, window, cx),
                ))
                .child("?")
            })
            .into_any_element()
    }

    fn agent_project_name(spec: &DialogSpec) -> Option<&str> {
        let from_project_choice = spec
            .rows
            .iter()
            .find(|row| row.id.0 == "choose-project")
            .map(|row| row.label.strip_suffix('…').unwrap_or(&row.label))
            .filter(|name| !name.is_empty());
        from_project_choice.or_else(|| {
            spec.footer
                .as_deref()
                .map(|path| path.trim_end_matches(['/', '\\']))
                .and_then(|path| path.rsplit(['/', '\\']).next())
                .filter(|name| !name.is_empty())
        })
    }

    fn new_session_footer(
        &self,
        spec: &DialogSpec,
        submit: Option<&DialogRow>,
        error: Option<String>,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let agent_session = Self::is_agent_session_spec(spec);
        v_flex()
            .w_full()
            .gap_2()
            .when_some(error, |this, error| {
                this.child(public_selector(
                    "dialog-prompt-validation",
                    div().text_sm().text_color(cx.theme().danger).child(error),
                ))
            })
            .when_some(spec.hint.clone(), |this, hint| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(hint),
                )
            })
            .child(h_flex().w_full().justify_end().gap_2().flex_wrap().when(
                !agent_session,
                |row| {
                    row.when_some(submit, |row, submit| {
                        row.child(
                            Self::new_session_start(spec, submit, cx).disabled(
                                !submit.enabled || spec.busy || self.attachment_imports > 0,
                            ),
                        )
                    })
                },
            ))
            .into_any_element()
    }

    fn agent_session_project_footer(
        &self,
        spec: &DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        h_flex()
            .id("dialog-prompt-footer")
            .min_w_0()
            .items_center()
            .justify_between()
            .mx_3()
            .px_2()
            .py_1()
            .bg(cx.theme().muted.opacity(0.35))
            .border_1()
            .border_color(cx.theme().border)
            .rounded_b(cx.theme().radius)
            .gap_2()
            .when_some(
                spec.fields.iter().find(|field| field.id == "isolation"),
                |row, field| {
                    row.child(
                        div()
                            .when_some(self.control_focus.get("isolation"), |view, focus| {
                                view.track_focus(focus)
                            })
                            .child(self.new_session_worktree(spec, field, cx)),
                    )
                },
            )
            .when(
                !spec.fields.iter().any(|field| field.id == "isolation"),
                |row| {
                    row.child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(super::sized_icon(
                                "folder",
                                super::IconSize::Small,
                                cx.theme().muted_foreground,
                            ))
                            .child("Current checkout"),
                    )
                },
            )
            .when_some(
                self.new_session_worktree_options(spec, window, cx),
                ParentElement::child,
            )
            .into_any_element()
    }

    fn new_session_worktree_options(
        &self,
        spec: &DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::component::popover::Popover> {
        let starting = spec.fields.iter().find(|field| field.id == "start-ref")?;
        let (input, _) = self.fields.get(&starting.id)?;
        let focus = input.read(cx).focus_handle(cx);
        let start_ref = if starting.value.trim().is_empty() {
            "HEAD"
        } else {
            starting.value.trim()
        };
        let fields = ["start-ref", "branch", "folder"]
            .into_iter()
            .filter_map(|id| {
                let mut field = spec.fields.iter().find(|field| field.id == id)?.clone();
                if id != "start-ref" {
                    field.label.push_str(" (optional)");
                }
                self.prompt_field(spec, &field, window, cx)
            })
            .collect::<Vec<_>>();
        Some(gpui_kit::component::popover::Popover::new("dialog-worktree-options")
            .anchor(gpui_kit::Anchor::BottomRight)
            .track_focus(&focus)
            .trigger(Button::new("dialog-worktree-options-trigger")
                .debug_selector(|| "dialog-worktree-options-trigger".into())
                .ghost()
                .small()
                .min_w_0()
                .max_w(rems(18.))
                .disabled(spec.busy)
                .dropdown_caret(true)
                .child(div().min_w_0().text_ellipsis().child(format!("Start from {start_ref}")))
                .accessibility_label("Worktree options")
                .tooltip("Choose the starting branch, tag or commit. Branch and folder names are generated from your prompt unless overridden."))
            .child(v_flex().w(rems(22.)).gap_3().children(fields)))
    }

    fn new_session_start(spec: &DialogSpec, row: &DialogRow, cx: &Context<Self>) -> Button {
        let owner = cx.weak_entity();
        let submit = row.clone();
        Button::new("dialog-prompt-submit")
            .primary()
            .loading(spec.busy)
            .disabled(!row.enabled || spec.busy)
            .label(row.label.clone())
            .on_click(move |_, _, cx| {
                _ = owner.update(cx, |this, cx| this.confirm(&submit, false, cx));
            })
    }

    fn new_agent_session_start(spec: &DialogSpec, row: &DialogRow, cx: &Context<Self>) -> Button {
        let owner = cx.weak_entity();
        let submit = row.clone();
        Button::new("dialog-prompt-submit")
            .icon(gpui_kit::component::IconName::ArrowUp)
            .small()
            .primary()
            .rounded_full()
            .loading(spec.busy)
            .disabled(!row.enabled || spec.busy)
            .accessibility_label(row.label.clone())
            .tooltip(row.label.clone())
            .on_click(move |_, _, cx| {
                _ = owner.update(cx, |this, cx| this.confirm(&submit, false, cx));
            })
    }

    fn new_session_mode(spec: &DialogSpec, field: &DialogField, cx: &Context<Self>) -> ButtonGroup {
        let DialogFieldKind::Choice(options) = &field.kind else {
            return ButtonGroup::new("new-session-mode");
        };
        ButtonGroup::new("new-session-mode")
            .small()
            .outline()
            .children(options.iter().map(|value| {
                let owner = cx.weak_entity();
                let dialog = spec.id.clone();
                let field_id = field.id.clone();
                let value = value.clone();
                Button::new(SharedString::from(format!("new-session-mode-{value}")))
                    .label(value.clone())
                    .selected(value == field.value)
                    .disabled(spec.busy)
                    .on_click(move |_, _, cx| {
                        _ = owner.update(cx, |_, cx| {
                            cx.emit(DialogIntent::FieldChanged {
                                dialog: dialog.clone(),
                                field: field_id.clone(),
                                value: value.clone(),
                            });
                        });
                    })
            }))
    }

    fn new_session_choice(
        &self,
        spec: &DialogSpec,
        field: &DialogField,
        compact: bool,
        cx: &Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let DialogFieldKind::Choice(options) = &field.kind else {
            return None;
        };
        if field.id == "mode" || (field.id == "profile" && options.len() < 2) {
            return None;
        }
        if field.id == "model"
            || (field.id == "provider"
                && !spec.fields.iter().any(|candidate| candidate.id == "model"))
        {
            return self.render_new_model_picker(spec, field, cx);
        }
        let owner = cx.weak_entity();
        let dialog = spec.id.clone();
        let field = field.clone();
        let options = options.clone();
        let spaces = spec.spaces.clone();
        Some(
            Button::new(SharedString::from(format!(
                "dialog-field-choice-{}",
                field.id
            )))
            .ghost()
            .small()
            .min_w_0()
            .max_w(rems(16.0))
            .dropdown_caret(true)
            .disabled(spec.busy)
            .accessibility_label(format!("{}: {}", field.label, field.value))
            .child(Self::new_session_choice_content(
                &field.id,
                &field.value,
                &spec.spaces,
                compact,
                cx,
            ))
            .dropdown_menu(move |mut menu, _, _| {
                if field.id == "reasoning" {
                    menu = menu.label("Reasoning");
                }
                for value in &options {
                    let owner = owner.clone();
                    let dialog = dialog.clone();
                    let field_id = field.id.clone();
                    let label = value.clone();
                    let spaces = spaces.clone();
                    let item = PopupMenuItem::element(move |_, cx| {
                        div()
                            .id(SharedString::from(format!(
                                "new-session-choice-{field_id}-{label}"
                            )))
                            .role(gpui_kit::Role::MenuItem)
                            .aria_label(label.clone())
                            .child(Self::new_session_choice_content(
                                &field_id, &label, &spaces, false, cx,
                            ))
                    })
                    .checked(value == &field.value);
                    let field_id = field.id.clone();
                    let value = value.clone();
                    menu = menu.item(item.on_click(move |_, _, cx| {
                        _ = owner.update(cx, |_, cx| {
                            cx.emit(DialogIntent::FieldChanged {
                                dialog: dialog.clone(),
                                field: field_id.clone(),
                                value: value.clone(),
                            });
                        });
                    }));
                }
                menu
            })
            .into_any_element(),
        )
    }

    fn new_session_worktree(
        &self,
        spec: &DialogSpec,
        field: &DialogField,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let owner = cx.weak_entity();
        let dialog = spec.id.clone();
        let switch = Switch::new("new-session-worktree")
            .small()
            .label("Worktree")
            .checked(field.value == "New worktree")
            .disabled(spec.busy)
            .on_click(move |checked, _, cx| {
                _ = owner.update(cx, |_, cx| {
                    cx.emit(DialogIntent::FieldChanged {
                        dialog: dialog.clone(),
                        field: "isolation".to_owned(),
                        value: if *checked {
                            "New worktree"
                        } else {
                            "Current checkout"
                        }
                        .to_owned(),
                    });
                });
            });
        let actions = self
            .control_focus
            .get("isolation")
            .cloned()
            .map_or_else(Vec::new, |focus| {
                vec![composer_focus_action(ComposerControl::Worktree, focus)]
            });
        with_composer_tooltip(
            "new-session-worktree-tooltip",
            switch,
            "Create this session in a new Git worktree.",
            actions,
        )
    }

    fn new_session_choice_content(
        field: &str,
        value: &str,
        spaces: &[DialogSpaceChoice],
        compact: bool,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        let space = (field == "host")
            .then(|| spaces.iter().find(|space| space.label == value))
            .flatten();
        let icon = match (field, value) {
            ("permissions", value) => bootty_agents::NativePermissionMode::ALL
                .into_iter()
                .find(|mode| mode.label() == value)
                .map_or("lock", bootty_agents::NativePermissionMode::icon),
            ("provider", "Codex") => bootty_agents::AgentKind::Codex.icon(),
            ("provider", "Claude") => bootty_agents::AgentKind::Claude.icon(),
            ("provider", "Pi") => bootty_agents::AgentKind::Pi.icon(),
            ("provider", _) => "bot",
            ("icon", "Automatic") => "folder",
            ("icon", value) => value,
            ("host", _) => space.map_or("folder", |space| space.icon.as_str()),
            ("profile", _) => "circle-user",
            ("model", _) => "cpu",
            ("reasoning", _) => "brain",
            ("isolation", _) => "git-branch",
            _ => "settings-2",
        };
        let tint = if field == "provider" {
            super::theme::provider_color(value, cx)
        } else {
            cx.theme().foreground
        };
        h_flex()
            .min_w_0()
            .gap_1()
            .text_color(tint)
            .child(super::sized_icon(
                icon,
                super::IconSize::Small,
                space.map_or(tint, |space| {
                    let [red, green, blue] = space.color;
                    gpui_kit::rgb(u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue))
                        .into()
                }),
            ))
            .when(!compact, |row| {
                row.child(div().min_w_0().text_sm().text_ellipsis().child(
                    if field == "reasoning" {
                        crate::gpui_agent_session::reasoning_label(value)
                    } else {
                        value.to_owned()
                    },
                ))
            })
            .into_any_element()
    }

    fn prompt_text_control(
        &self,
        spec: &DialogSpec,
        invalid: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let destructive = gpui_kit::component::Theme::global(cx)
            .semantic_tokens()
            .colors
            .destructive;
        if spec.multiline {
            Textarea::new(&self.prompt_textarea)
                .token(crate::gpui_prompt_attachments::render)
                .disabled(spec.busy)
                .w_full()
                .into_any_element()
        } else {
            crate::gpui::focus_input(
                &self.prompt_input,
                Input::new(&self.prompt_input)
                    .disabled(spec.busy)
                    .w_full()
                    .when(invalid, |input| input.border_color(destructive)),
            )
            .into_any_element()
        }
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
                    crate::gpui::focus_input(
                        input,
                        Input::new(input)
                            .aria_label(field.label.clone())
                            .disabled(spec.busy)
                            .w_full(),
                    ),
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
        Some(Self::prompt_control_row(
            spec,
            &format!("dialog-field-{}", field.id),
            Some(&field.label),
            control,
        ))
    }

    fn prompt_control_row(
        _: &DialogSpec,
        id: &str,
        label: Option<&str>,
        control: gpui_kit::AnyElement,
    ) -> gpui_kit::AnyElement {
        v_flex()
            .id(SharedString::from(id.to_owned()))
            .w_full()
            .gap_1()
            .when_some(label, |row, label| {
                row.child(div().text_sm().child(label.to_owned()))
            })
            .child(control)
            .into_any_element()
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
            Some(DialogRole::Prompt) if self.spec.as_ref().is_some_and(|spec| spec.multiline) => {
                self.prompt_textarea.read(cx).focus_handle(cx)
            }
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
            return div().into_any_element();
        };
        let command_focus = self.command_focus.clone();
        let prompt_input_focus = if spec.multiline {
            self.prompt_textarea.read(cx).focus_handle(cx)
        } else {
            self.prompt_input_focus.clone()
        };
        let confirm_focus = self.confirm_focus.clone();
        let panel = self.panel(&spec, window, cx);
        let body = div()
            .when(Self::is_agent_session_spec(&spec), |this| {
                this.w_full().max_w(rems(44.0)).max_h_full().min_h_0()
            })
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
                    .on_action(cx.listener(Self::on_cancel_action))
                    .on_action(
                        cx.listener(|this, _: &gpui_kit::base::actions::Confirm, _, cx| {
                            if !this.spec.as_ref().is_some_and(|spec| spec.multiline) {
                                this.prompt_submit(cx);
                            }
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
            .child(panel);
        if Self::is_agent_session_spec(&spec) {
            body.id("dialog-agent-form")
                .overflow_y_scroll()
                .into_any_element()
        } else {
            body.into_any_element()
        }
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
