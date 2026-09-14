//! Host-neutral GPUI presentation contract for creating and editing Spaces.
//!
//! The host owns the draft, validation, profile lookup, and remote catalog lifecycle. This view
//! receives an immutable projection and emits typed edits containing only primitive values and
//! opaque IDs.

use num_traits::ToPrimitive as _;

use gpui_kit::component::{
    Disableable as _, Selectable as _, Sizable as _, Size,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    color_picker::{ColorPicker as UiColorPicker, ColorPickerEvent, ColorPickerState},
    form::{field, v_form},
    group_box::GroupBox,
    h_flex,
    input::{Input, InputEvent, InputState},
    radio::{Radio, RadioGroup},
    spinner::Spinner,
    switch::Switch,
    v_flex,
};
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription,
    WeakEntity, Window, div, prelude::*, rems,
};

/// One app-defined option. `id` is opaque to `bootty-ui`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceEditorChoice {
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
    pub selected: bool,
    pub enabled: bool,
}

/// One selectable icon in product order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceEditorIcon {
    pub id: String,
    pub glyph: String,
    pub label: String,
    pub selected: bool,
}

/// The remote catalog projection for the selected location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteSpaceSnapshot {
    Hidden,
    Loading,
    Failed {
        message: String,
    },
    Ready {
        spaces: Vec<SpaceEditorChoice>,
        warning: Option<String>,
        new_name: String,
        create_backends: Vec<SpaceEditorChoice>,
        can_create: bool,
    },
}

/// A complete, disposable Space editor projection for one frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceEditorSnapshot {
    pub title: String,
    pub name: String,
    pub name_error: Option<String>,
    pub icon_search: String,
    pub icons: Vec<SpaceEditorIcon>,
    pub color: [u8; 3],
    pub tint_sidebar: bool,
    /// `None` IDs represent the host's inherited/default backend choice.
    pub backends: Vec<OptionalSpaceEditorChoice>,
    pub backend_enabled: bool,
    pub locations: Vec<SpaceEditorChoice>,
    pub location_notice: Option<String>,
    pub remote: RemoteSpaceSnapshot,
    pub can_save: bool,
    pub colors: SpaceEditorColors,
}

/// A choice whose value may explicitly inherit the host default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionalSpaceEditorChoice {
    pub id: Option<String>,
    pub label: String,
    pub selected: bool,
    pub enabled: bool,
}

/// Typed edits and actions emitted for the authoritative host owner to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpaceEditorIntent {
    SetName(String),
    SetIconSearch(String),
    SelectIcon(String),
    SetColor([u8; 3]),
    SetTintSidebar(bool),
    SelectBackend(Option<String>),
    SelectLocation(String),
    SelectRemoteSpace(String),
    RetryRemoteSpaces,
    SetNewRemoteSpaceName(String),
    SelectNewRemoteSpaceBackend(String),
    CreateRemoteSpace { name: String, backend: String },
    Save,
    Close,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpaceEditorColors {
    pub pane: Hsla,
    pub surface: Hsla,
    pub hover: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub destructive: Hsla,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorField {
    Name,
    IconSearch,
    NewRemoteName,
}

#[derive(Clone, Copy)]
struct TextFieldSpec<'a> {
    id: &'static str,
    value: &'a str,
    placeholder: &'static str,
    aria_label: &'static str,
    field: EditorField,
    validation_error: Option<&'a str>,
}

// Keep the dialog comfortable at normal sizes while allowing the content pane to take over
// scrolling when a window is short or a remote catalog is large.
const CONTROL_MAX_WIDTH_REMS: f32 = 35.0;
const ICON_GRID_MAX_HEIGHT_REMS: f32 = 9.25;
const REMOTE_LIST_MAX_HEIGHT_REMS: f32 = 10.25;

pub struct GpuiSpaceEditor {
    snapshot: SpaceEditorSnapshot,
    focus: FocusHandle,
    name_input: Option<Entity<InputState>>,
    name_input_state: Option<Entity<SpaceEditorInputState>>,
    icon_search_input: Option<Entity<InputState>>,
    icon_search_input_state: Option<Entity<SpaceEditorInputState>>,
    new_remote_name_input: Option<Entity<InputState>>,
    new_remote_name_input_state: Option<Entity<SpaceEditorInputState>>,
    color_picker: Option<Entity<ColorPickerState>>,
    _color_picker_subscription: Option<Subscription>,
}

struct SpaceEditorInputState {
    owner: WeakEntity<GpuiSpaceEditor>,
    input: Entity<InputState>,
    field: EditorField,
    external_value: String,
    current_value: String,
    _subscription: Subscription,
}

impl SpaceEditorInputState {
    fn on_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let value = input.read(cx).value().to_string();
                if self.current_value == value {
                    return;
                }
                self.current_value.clone_from(&value);
                let intent = match self.field {
                    EditorField::Name => SpaceEditorIntent::SetName(value),
                    EditorField::IconSearch => SpaceEditorIntent::SetIconSearch(value),
                    EditorField::NewRemoteName => SpaceEditorIntent::SetNewRemoteSpaceName(value),
                };
                let _ = self.owner.update(cx, |editor, cx| {
                    // Search and draft fields are local interaction state. Keep the projection
                    // current until the host publishes its accepted snapshot so the next render
                    // responds immediately to typing.
                    match (&self.field, &intent) {
                        (EditorField::Name, SpaceEditorIntent::SetName(value)) => {
                            editor.snapshot.name.clone_from(value);
                        }
                        (EditorField::IconSearch, SpaceEditorIntent::SetIconSearch(value)) => {
                            editor.snapshot.icon_search.clone_from(value);
                        }
                        (
                            EditorField::NewRemoteName,
                            SpaceEditorIntent::SetNewRemoteSpaceName(value),
                        ) => {
                            if let RemoteSpaceSnapshot::Ready { new_name, .. } =
                                &mut editor.snapshot.remote
                            {
                                new_name.clone_from(value);
                            }
                        }
                        _ => {}
                    }
                    editor.emit(intent, cx);
                });
            }
            InputEvent::PressEnter { .. } => {
                let _ = self.owner.update(cx, |editor, cx| {
                    if let Some(intent) = editor.field_enter_intent(self.field) {
                        editor.emit(intent, cx);
                    }
                });
            }
            InputEvent::Focus => {
                let _ = self.owner.update(cx, |_, cx| cx.notify());
            }
            InputEvent::Blur => {}
        }
    }

    fn sync_external_value(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.external_value == value {
            return;
        }
        self.external_value.clear();
        self.external_value.push_str(value);
        if self.current_value == value {
            return;
        }
        self.current_value.clear();
        self.current_value.push_str(value);
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

impl GpuiSpaceEditor {
    pub fn new(snapshot: SpaceEditorSnapshot, cx: &mut Context<Self>) -> Self {
        Self {
            snapshot,
            focus: cx.focus_handle(),
            name_input: None,
            name_input_state: None,
            icon_search_input: None,
            icon_search_input_state: None,
            new_remote_name_input: None,
            new_remote_name_input_state: None,
            color_picker: None,
            _color_picker_subscription: None,
        }
    }

    /// Construct an editor with its primary field ready before overlay focus is captured.
    ///
    /// The modal host focuses this view before its first render, so the name input must be
    /// created by the window-owning caller rather than during `render`.
    pub fn new_with_window(
        snapshot: SpaceEditorSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (name_input, name_input_state) = Self::create_input(
            snapshot.name.clone(),
            "space name…",
            EditorField::Name,
            window,
            cx,
        );
        let (icon_search_input, icon_search_input_state) = Self::create_input(
            snapshot.icon_search.clone(),
            "search icons…",
            EditorField::IconSearch,
            window,
            cx,
        );
        let new_remote_name = match &snapshot.remote {
            RemoteSpaceSnapshot::Ready { new_name, .. } => new_name.clone(),
            _ => String::new(),
        };
        let (new_remote_name_input, new_remote_name_input_state) = Self::create_input(
            new_remote_name,
            "new remote Space",
            EditorField::NewRemoteName,
            window,
            cx,
        );
        let color = snapshot.color;
        let color_picker =
            cx.new(|cx| ColorPickerState::new(window, cx).default_value(hsla_from_rgb(color)));
        let owner = cx.entity().downgrade();
        let color_picker_subscription = cx.subscribe(&color_picker, move |_, _, event, cx| {
            let ColorPickerEvent::Change(Some(color)) = event else {
                return;
            };
            let rgb = rgb_from_hsla(*color);
            let _ = owner.update(cx, |editor, cx| {
                editor.emit(SpaceEditorIntent::SetColor(rgb), cx);
            });
        });
        Self {
            snapshot,
            focus: cx.focus_handle(),
            name_input: Some(name_input),
            name_input_state: Some(name_input_state),
            icon_search_input: Some(icon_search_input),
            icon_search_input_state: Some(icon_search_input_state),
            new_remote_name_input: Some(new_remote_name_input),
            new_remote_name_input_state: Some(new_remote_name_input_state),
            color_picker: Some(color_picker),
            _color_picker_subscription: Some(color_picker_subscription),
        }
    }

    fn create_input(
        value: String,
        placeholder: &'static str,
        field: EditorField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<InputState>, Entity<SpaceEditorInputState>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(value.clone())
                .placeholder(placeholder)
        });
        let owner = cx.entity().downgrade();
        let input_state = cx.new(|cx| {
            let subscription =
                cx.subscribe_in(&input, window, SpaceEditorInputState::on_input_event);
            SpaceEditorInputState {
                owner,
                input: input.clone(),
                field,
                external_value: value.clone(),
                current_value: value,
                _subscription: subscription,
            }
        });
        (input, input_state)
    }

    pub fn set_snapshot(&mut self, snapshot: SpaceEditorSnapshot, cx: &mut Context<Self>) {
        if self.snapshot == snapshot {
            return;
        }
        self.snapshot = snapshot;
        cx.notify();
    }

    #[must_use]
    pub const fn snapshot(&self) -> &SpaceEditorSnapshot {
        &self.snapshot
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(name_input) = &self.name_input {
            name_input.read(cx).focus_handle(cx).focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
    }

    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Whether the host-owned Save action is currently valid.
    #[must_use]
    pub const fn save_enabled(&self) -> bool {
        self.snapshot.can_save
    }

    /// Build the footer for the host-owned root dialog.
    pub fn render_dialog_footer(entity: Entity<Self>, cx: &App) -> AnyElement {
        let save_enabled = entity.read(cx).save_enabled();
        let cancel_entity = entity.clone();
        let save_entity = entity;
        h_flex()
            .id("space-editor-footer")
            .debug_selector(|| "space-editor-footer".to_owned())
            .gap_2()
            .justify_end()
            .child(
                Button::new("space-editor-cancel")
                    .debug_selector(|| "space-editor-cancel".to_owned())
                    .label("Cancel")
                    .on_click(move |_, _, cx| {
                        cancel_entity.update(cx, |editor, cx| editor.request_close(cx));
                    }),
            )
            .child(
                Button::new("space-editor-save")
                    .debug_selector(|| "space-editor-save".to_owned())
                    .primary()
                    .label("Save")
                    .disabled(!save_enabled)
                    .on_click(move |_, _, cx| {
                        save_entity.update(cx, |editor, cx| editor.request_save(cx));
                    }),
            )
            .into_any_element()
    }

    /// Emit the host-owned Save action when the current snapshot is valid.
    pub fn request_save(&self, cx: &mut Context<Self>) {
        if self.save_enabled() {
            self.emit(SpaceEditorIntent::Save, cx);
        }
    }

    /// Emit the host-owned Close action.
    pub fn request_close(&self, cx: &mut Context<Self>) {
        self.emit(SpaceEditorIntent::Close, cx);
    }

    #[expect(
        clippy::unused_self,
        reason = "This entity event helper is called from GPUI listener callbacks."
    )]
    fn emit(&self, intent: SpaceEditorIntent, cx: &mut Context<Self>) {
        cx.emit(intent);
    }

    fn field_enter_intent(&self, field: EditorField) -> Option<SpaceEditorIntent> {
        if field == EditorField::NewRemoteName {
            self.remote_create_request()
                .map(|(name, backend)| SpaceEditorIntent::CreateRemoteSpace { name, backend })
        } else {
            self.snapshot.can_save.then_some(SpaceEditorIntent::Save)
        }
    }

    fn remote_create_request(&self) -> Option<(String, String)> {
        let RemoteSpaceSnapshot::Ready {
            new_name,
            create_backends,
            can_create,
            ..
        } = &self.snapshot.remote
        else {
            return None;
        };
        let backend = create_backends
            .iter()
            .find(|choice| choice.selected && choice.enabled)?;
        can_create.then(|| (new_name.trim().to_owned(), backend.id.clone()))
    }

    fn text_field(
        &self,
        spec: TextFieldSpec<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let TextFieldSpec {
            id,
            value,
            placeholder,
            aria_label,
            field,
            validation_error,
        } = spec;
        let retained = match field {
            EditorField::Name => self.name_input.as_ref().zip(self.name_input_state.as_ref()),
            EditorField::IconSearch => self
                .icon_search_input
                .as_ref()
                .zip(self.icon_search_input_state.as_ref()),
            EditorField::NewRemoteName => self
                .new_remote_name_input
                .as_ref()
                .zip(self.new_remote_name_input_state.as_ref()),
        };
        let input = if let Some((input, state)) = retained {
            state.update(cx, |state, cx| state.sync_external_value(value, window, cx));
            input.clone()
        } else {
            Self::keyed_text_input(id, value, placeholder, field, window, cx)
        };
        let colors = self.snapshot.colors;
        let invalid = validation_error.is_some();
        let debug_id = id.to_owned();
        div()
            .id(id)
            .debug_selector(move || debug_id)
            .w_full()
            .max_w(rems(CONTROL_MAX_WIDTH_REMS))
            .min_w_0()
            .when_some(validation_error, |field, error| {
                field.aria_description(error)
            })
            .child(crate::gpui::focus_input(
                &input,
                Input::new(&input)
                    .aria_label(aria_label)
                    .with_size(Size::Medium)
                    .when(invalid, |input| input.border_color(colors.destructive))
                    .w_full(),
            ))
            .into_any_element()
    }

    fn keyed_text_input(
        id: &'static str,
        value: &str,
        placeholder: &'static str,
        field: EditorField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let state_key = SharedString::from(format!("space-editor-input-{id}"));
        let initial_value = value.to_owned();
        let initial_placeholder = placeholder.to_owned();
        let owner = cx.entity().downgrade();
        let state = window.use_keyed_state(state_key, cx, move |window, cx| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(initial_value.clone())
                    .placeholder(initial_placeholder.clone())
            });
            let subscription =
                cx.subscribe_in(&input, window, SpaceEditorInputState::on_input_event);
            SpaceEditorInputState {
                owner,
                input,
                field,
                external_value: initial_value.clone(),
                current_value: initial_value,
                _subscription: subscription,
            }
        });
        state.update(cx, |state, cx| state.sync_external_value(value, window, cx));
        state.read(cx).input.clone()
    }

    fn labeled(label: &str, content: AnyElement) -> AnyElement {
        field()
            .label(label.to_owned())
            .child(content)
            .into_any_element()
    }

    fn section_group(
        &self,
        id: &'static str,
        title: &str,
        detail: &str,
        children: impl IntoIterator<Item = AnyElement>,
    ) -> AnyElement {
        let colors = self.snapshot.colors;
        GroupBox::new()
            .id(id)
            .title(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_color(colors.muted)
                    .child(title.to_owned())
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted)
                            .child(detail.to_owned()),
                    ),
            )
            .children(children)
            .into_any_element()
    }

    fn color_editor(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let values = self.snapshot.color;
        let Some(picker) = self.color_picker.as_ref() else {
            return div()
                .text_sm()
                .text_color(self.snapshot.colors.muted)
                .child(format!(
                    "#{:02X}{:02X}{:02X}",
                    values[0], values[1], values[2]
                ))
                .into_any_element();
        };
        let desired = hsla_from_rgb(values);
        if picker.read(cx).value() != Some(desired) {
            picker.update(cx, |picker, cx| picker.set_value(desired, window, cx));
        }
        div()
            .id("space-color-picker")
            .debug_selector(|| "space-color-picker".to_owned())
            .child(
                UiColorPicker::new(picker)
                    .label(format!(
                        "#{:02X}{:02X}{:02X}",
                        values[0], values[1], values[2]
                    ))
                    .accessibility_label("Space color"),
            )
            .into_any_element()
    }

    fn backend_choices(&self, cx: &Context<Self>) -> AnyElement {
        let selected_index = self
            .snapshot
            .backends
            .iter()
            .position(|choice| choice.selected);
        let choices = self.snapshot.backends.iter().map(|choice| {
            let id = choice.id.clone();
            let enabled = self.snapshot.backend_enabled && choice.enabled;
            choice_radio(
                format!(
                    "space-backend-{}",
                    choice.id.as_deref().unwrap_or("default")
                ),
                &choice.label,
                choice.selected,
                enabled,
            )
            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                if enabled && *checked {
                    this.emit(SpaceEditorIntent::SelectBackend(id.clone()), cx);
                }
            }))
        });
        div()
            .id("space-backend-group")
            .w_full()
            .max_w(rems(CONTROL_MAX_WIDTH_REMS))
            .min_w_0()
            .flex()
            .flex_wrap()
            .gap_1()
            .overflow_x_hidden()
            .role(gpui_kit::Role::RadioGroup)
            .aria_label("Backend choices")
            .child(
                RadioGroup::horizontal("space-backend-radios")
                    .w_full()
                    .selected_index(selected_index)
                    .children(choices),
            )
            .into_any_element()
    }

    fn location_choices(&self, cx: &Context<Self>) -> AnyElement {
        let colors = self.snapshot.colors;
        let selected_index = self
            .snapshot
            .locations
            .iter()
            .position(|choice| choice.selected);
        let choices = self.snapshot.locations.iter().map(|choice| {
            let id = choice.id.clone();
            let enabled = choice.enabled;
            choice_radio(
                format!("space-location-{}", choice.id),
                &choice.label,
                choice.selected,
                enabled,
            )
            .when_some(choice.detail.clone(), |radio, detail| {
                radio.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(colors.muted)
                        .child(detail),
                )
            })
            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                if enabled && *checked {
                    this.emit(SpaceEditorIntent::SelectLocation(id.clone()), cx);
                }
            }))
        });
        div()
            .id("space-location-group")
            .w_full()
            .max_w(rems(CONTROL_MAX_WIDTH_REMS))
            .min_w_0()
            .flex()
            .flex_wrap()
            .gap_1()
            .overflow_x_hidden()
            .role(gpui_kit::Role::RadioGroup)
            .aria_label("Location choices")
            .child(
                RadioGroup::horizontal("space-location-radios")
                    .w_full()
                    .selected_index(selected_index)
                    .children(choices),
            )
            .into_any_element()
    }

    fn icon_row<'a>(
        row_index: usize,
        icons: impl Iterator<Item = &'a SpaceEditorIcon>,
        text: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        const ICON_BUTTON_WIDTH_REMS: f32 = 2.5;
        const ICON_ROW_HEIGHT_REMS: f32 = 1.75;
        icons
            .fold(
                div()
                    .id(("space-icon-row", row_index))
                    .h(rems(ICON_ROW_HEIGHT_REMS))
                    .w_full()
                    .flex()
                    .gap_1(),
                |row, icon| {
                    let id = icon.id.clone();
                    let debug_id = id.clone();
                    row.child(
                        Button::new(SharedString::from(format!("space-icon-{id}")))
                            .debug_selector(move || format!("space-icon-{debug_id}"))
                            .small()
                            .outline()
                            .w(rems(ICON_BUTTON_WIDTH_REMS))
                            .selected(icon.selected)
                            .accessibility_label(icon.label.clone())
                            .tooltip(icon.label.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.emit(SpaceEditorIntent::SelectIcon(id.clone()), cx);
                            }))
                            .child(crate::gpui::icon(&icon.glyph, 18.0, text)),
                    )
                },
            )
            .into_any_element()
    }

    fn appearance_group(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.snapshot.colors;
        let tint = self.snapshot.tint_sidebar;
        self.section_group(
            "space-editor-appearance",
            "Appearance",
            "Choose how this Space looks in the sidebar.",
            [
                Self::labeled("Icon", self.icon_picker(window, cx)),
                Self::labeled("Color", self.color_editor(window, cx)),
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w_0()
                            .child("Tint sidebar with Space color")
                            .child(
                                div()
                                    .mt(rems(0.125))
                                    .text_xs()
                                    .text_color(colors.muted)
                                    .child("Use this Space’s color in the navigation sidebar."),
                            ),
                    )
                    .child(
                        div()
                            .id("space-tint-sidebar-wrapper")
                            .debug_selector(|| "space-tint-sidebar".to_owned())
                            .child(editor_switch(tint).on_click(cx.listener(
                                move |this, checked: &bool, _, cx| {
                                    this.emit(SpaceEditorIntent::SetTintSidebar(*checked), cx);
                                },
                            ))),
                    )
                    .into_any_element(),
            ],
        )
    }

    fn icon_picker(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        const ICONS_PER_ROW: usize = 12;
        let colors = self.snapshot.colors;
        let query = self.snapshot.icon_search.trim().to_ascii_lowercase();
        let filtered_icons = self
            .snapshot
            .icons
            .iter()
            .filter(|icon| {
                query.is_empty()
                    || icon.id.to_ascii_lowercase().contains(&query)
                    || icon.label.to_ascii_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        let has_icons = !filtered_icons.is_empty();
        let search = self.text_field(
            TextFieldSpec {
                id: "space-icon-search",
                value: &self.snapshot.icon_search,
                placeholder: "search icons…",
                aria_label: "Search icons",
                field: EditorField::IconSearch,
                validation_error: None,
            },
            window,
            cx,
        );
        let row_count = filtered_icons.len().div_ceil(ICONS_PER_ROW);
        let icon_rows = gpui_kit::uniform_list(
            "space-icon-list",
            row_count,
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                range
                    .filter_map(|row_index| {
                        let start = row_index.checked_mul(ICONS_PER_ROW)?;
                        let icons = filtered_icons.get(start..)?.iter().take(ICONS_PER_ROW);
                        let row = Self::icon_row(row_index, icons, colors.text, cx);
                        Some(row.into_any_element())
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full();
        div()
            .w_full()
            .max_w(rems(CONTROL_MAX_WIDTH_REMS))
            .min_w_0()
            .child(search)
            .child(
                div()
                    .id("space-icon-grid")
                    .w_full()
                    .mt_2()
                    .h(rems(ICON_GRID_MAX_HEIGHT_REMS))
                    .min_w_0()
                    .p_1()
                    .rounded(gpui_kit::component::Theme::global(cx).radius)
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.pane)
                    .overflow_hidden()
                    .when(!has_icons, |grid| {
                        grid.child(
                            div()
                                .p_3()
                                .text_sm()
                                .text_color(colors.muted)
                                .child("no matching icons"),
                        )
                    })
                    .when(has_icons, |grid| grid.child(icon_rows)),
            )
            .into_any_element()
    }

    fn remote_space_choices(&self, spaces: &[SpaceEditorChoice], cx: &Context<Self>) -> AnyElement {
        let colors = self.snapshot.colors;
        let selected_space_index = spaces.iter().position(|choice| choice.selected);
        let space_rows = spaces
            .iter()
            .map(|choice| {
                let id = choice.id.clone();
                let enabled = choice.enabled;
                choice_radio(
                    format!("remote-space-{}", choice.id),
                    &choice.label,
                    choice.selected,
                    enabled,
                )
                .when_some(choice.detail.clone(), |button, detail| {
                    button.child(div().text_xs().child(detail))
                })
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    if enabled && *checked {
                        this.emit(SpaceEditorIntent::SelectRemoteSpace(id.clone()), cx);
                    }
                }))
            })
            .collect::<Vec<_>>();
        div()
            .id("space-remote-list")
            .w_full()
            .max_h(rems(REMOTE_LIST_MAX_HEIGHT_REMS))
            .min_h(rems(2.125))
            .min_w_0()
            .p_1()
            .rounded(gpui_kit::component::Theme::global(cx).radius)
            .border_1()
            .border_color(colors.border)
            .bg(colors.pane)
            .overflow_x_hidden()
            .overflow_y_scroll()
            .role(gpui_kit::Role::RadioGroup)
            .aria_label("Remote Space choices")
            .child(
                RadioGroup::vertical("space-remote-radios")
                    .w_full()
                    .selected_index(selected_space_index)
                    .children(space_rows),
            )
            .into_any_element()
    }

    fn remote_create_controls(
        &self,
        new_name: &str,
        create_backends: &[SpaceEditorChoice],
        can_create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected_backend_index = create_backends.iter().position(|choice| choice.selected);
        let new_name_field = self.text_field(
            TextFieldSpec {
                id: "space-new-remote-name",
                value: new_name,
                placeholder: "new remote Space",
                aria_label: "New remote Space name",
                field: EditorField::NewRemoteName,
                validation_error: None,
            },
            window,
            cx,
        );
        let backend_rows = create_backends
            .iter()
            .map(|choice| {
                let id = choice.id.clone();
                let enabled = choice.enabled;
                choice_radio(
                    format!("remote-create-backend-{}", choice.id),
                    &choice.label,
                    choice.selected,
                    enabled,
                )
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    if enabled && *checked {
                        this.emit(
                            SpaceEditorIntent::SelectNewRemoteSpaceBackend(id.clone()),
                            cx,
                        );
                    }
                }))
            })
            .collect::<Vec<_>>();
        let request = self.remote_create_request();
        div()
            .mt_3()
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().min_w_0().flex_1().child(new_name_field))
            .child(
                div()
                    .id("space-remote-create-backend-group")
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .role(gpui_kit::Role::RadioGroup)
                    .aria_label("New remote Space backend choices")
                    .child(
                        RadioGroup::horizontal("space-remote-create-backend-radios")
                            .selected_index(selected_backend_index)
                            .children(backend_rows),
                    ),
            )
            .child(
                editor_button("space-create-remote", "Create", can_create).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some((name, backend)) = request.clone() {
                            this.emit(SpaceEditorIntent::CreateRemoteSpace { name, backend }, cx);
                        }
                    },
                )),
            )
            .into_any_element()
    }

    fn remote_editor(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = self.snapshot.colors;
        match &self.snapshot.remote {
            RemoteSpaceSnapshot::Hidden => None,
            RemoteSpaceSnapshot::Loading => Some(
                div()
                    .id("space-remote-loading")
                    .mb_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_color(colors.muted)
                    .child(Spinner::new())
                    .child("Loading remote Spaces…")
                    .into_any_element(),
            ),
            RemoteSpaceSnapshot::Failed { message } => Some(
                div()
                    .id("space-remote-error")
                    .flex()
                    .flex_col()
                    .mb_3()
                    .gap_2()
                    .child(Alert::error("space-remote-error-message", message.clone()))
                    .child(
                        editor_button("space-remote-retry", "Retry", true)
                            .mt_2()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.emit(SpaceEditorIntent::RetryRemoteSpaces, cx);
                            })),
                    )
                    .into_any_element(),
            ),
            RemoteSpaceSnapshot::Ready {
                spaces,
                warning,
                new_name,
                create_backends,
                can_create,
            } => Some(
                div()
                    .w_full()
                    .max_w(rems(CONTROL_MAX_WIDTH_REMS))
                    .min_w_0()
                    .mb_3()
                    .when_some(warning.clone(), |editor, warning| {
                        editor.child(Alert::warning("space-remote-warning", warning).banner())
                    })
                    .child(self.remote_space_choices(spaces, cx))
                    .child(self.remote_create_controls(
                        new_name,
                        create_backends,
                        *can_create,
                        window,
                        cx,
                    ))
                    .into_any_element(),
            ),
        }
    }
}

impl EventEmitter<SpaceEditorIntent> for GpuiSpaceEditor {}

impl Focusable for GpuiSpaceEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name_input.as_ref().map_or_else(
            || self.focus.clone(),
            |input| input.read(cx).focus_handle(cx),
        )
    }
}

impl Render for GpuiSpaceEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.snapshot.colors;
        let name_field = self.text_field(
            TextFieldSpec {
                id: "space-editor-name",
                value: &self.snapshot.name,
                placeholder: "space name…",
                aria_label: "Space name",
                field: EditorField::Name,
                validation_error: self.snapshot.name_error.as_deref(),
            },
            window,
            cx,
        );
        let appearance_group = self.appearance_group(window, cx);
        let mut connection_children = vec![
            Self::labeled("Backend", self.backend_choices(cx)),
            Self::labeled("Location", self.location_choices(cx)),
        ];
        if let Some(notice) = self.snapshot.location_notice.clone() {
            connection_children.push(
                Alert::info("space-location-notice", notice)
                    .banner()
                    .into_any_element(),
            );
        }
        if let Some(remote) = self.remote_editor(window, cx) {
            connection_children.push(Self::labeled("Remote Space", remote));
        }
        let connection_group = self.section_group(
            "space-editor-connection",
            "Connection",
            "Select the backend and location for this Space.",
            connection_children,
        );
        let form = v_form()
            .with_size(Size::Medium)
            .child(field().label("Space name").child(name_field))
            .child(field().label_indent(false).child(appearance_group))
            .child(field().label_indent(false).child(connection_group));
        v_flex()
            .id("gpui-space-editor")
            .w_full()
            .max_h(gpui_kit::px(
                f32::from(window.rem_size())
                    .mul_add(-5.0, f32::from(window.viewport_size().height) * 0.8)
                    .max(0.0),
            ))
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .text_color(colors.text)
            .child(
                div()
                    .id("space-editor-scroll")
                    .debug_selector(|| "space-editor-scroll".to_owned())
                    .w_full()
                    .min_w_0()
                    .min_h_0()
                    .flex_1()
                    .px_4()
                    .py_3()
                    .overflow_x_hidden()
                    .overflow_y_scroll()
                    .scrollbar_width(rems(0.375))
                    .child(form)
                    .when_some(self.snapshot.name_error.clone(), |editor, error| {
                        editor.child(
                            div()
                                .debug_selector(|| "space-editor-name-error".to_owned())
                                .child(
                                    Alert::error("space-editor-name-error-alert", error).banner(),
                                ),
                        )
                    }),
            )
    }
}

fn choice_radio(id: impl Into<SharedString>, label: &str, selected: bool, enabled: bool) -> Radio {
    let id = id.into();
    let debug_id = id.clone();
    Radio::new(id)
        .debug_selector(move || debug_id.to_string())
        .label(label.to_owned())
        .checked(selected)
        .disabled(!enabled)
}

fn editor_button(id: impl Into<SharedString>, label: &'static str, enabled: bool) -> Button {
    let id = id.into();
    let debug_id = id.clone();
    Button::new(id)
        .debug_selector(move || debug_id.to_string())
        .label(label)
        .small()
        .outline()
        .disabled(!enabled)
}

fn editor_switch(selected: bool) -> Switch {
    Switch::new("space-tint-sidebar-switch")
        .checked(selected)
        .accessibility_label("Tint sidebar with Space color")
}

fn hsla_from_rgb([red, green, blue]: [u8; 3]) -> Hsla {
    gpui_kit::Rgba {
        r: f32::from(red) / 255.0,
        g: f32::from(green) / 255.0,
        b: f32::from(blue) / 255.0,
        a: 1.0,
    }
    .into()
}

fn rgb_from_hsla(color: Hsla) -> [u8; 3] {
    let color = gpui_kit::Rgba::from(color);
    [color.r, color.g, color.b].map(|channel| {
        (channel.clamp(0.0, 1.0) * 255.0)
            .round()
            .to_u8()
            .unwrap_or(0)
    })
}
