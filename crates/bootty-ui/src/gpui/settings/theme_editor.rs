//! Named theme authoring uses the existing shared theme commands and conflict-checked document.

use gpui_kit::base::StyledExt as _;

use super::{GpuiSettings, SettingsIntent, ThemeColorGroup, theme_colors::render_theme_preview};
use crate::{
    gpui::{
        DialogFieldKind,
        color_picker::{ColorPickerParams, ColorPickerUpdate, render_color_picker},
    },
    presentation::theme_editor::{ThemeEditorDialog, ThemeEditorEvent},
};
use bootty_config::config::{AppearanceVariant, ColorConfig};
use bootty_control::{BoundAppCommandSender, CommandCancellation};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    label::Label,
};
use gpui_kit::{
    Context, Entity, Focusable as _, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, prelude::*, rems,
};
use std::{
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

pub struct GpuiThemeEditor {
    settings: Entity<GpuiSettings>,
    document: ThemeEditorDialog,
    sender: BoundAppCommandSender,
    named: bool,
    pending_named_focus: bool,
    group: ThemeColorGroup,
    details: bool,
    inputs: HashMap<String, Entity<InputState>>,
    subscriptions: Vec<Subscription>,
    colors: ColorConfig,
}

impl GpuiThemeEditor {
    pub fn new(
        settings: Entity<GpuiSettings>,
        sender: BoundAppCommandSender,
        name: String,
        appearance: AppearanceVariant,
        colors: ColorConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut editor = Self {
            settings,
            document: ThemeEditorDialog::new(name, appearance),
            sender,
            named: false,
            pending_named_focus: false,
            group: ThemeColorGroup::default(),
            details: false,
            inputs: HashMap::new(),
            subscriptions: Vec::new(),
            colors,
        };
        let spec = editor.document.spec();
        for (id, label, value) in std::iter::once((
            "name".to_owned(),
            "Save as".to_owned(),
            spec.text.unwrap_or_default(),
        ))
        .chain(
            spec.fields
                .into_iter()
                .filter(|field| field.kind == DialogFieldKind::Text)
                .map(|field| (field.id, field.label, field.value)),
        ) {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .placeholder(label)
            });
            let field = id.clone();
            let subscription = cx.subscribe(&input, move |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.edit_field(&field, input.read(cx).value().to_string(), cx);
                }
            });
            editor.inputs.insert(id, input);
            editor.subscriptions.push(subscription);
        }
        editor.dispatch(editor.document.load(), cx);
        editor
    }

    pub(crate) fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.named {
            self.pending_named_focus = true;
            cx.notify();
        } else {
            self.settings.update(cx, |settings, cx| {
                settings.show_theme(cx);
                settings.focus(window, cx);
            });
        }
    }

    pub(crate) fn set_colors(&mut self, colors: ColorConfig) {
        self.colors = colors;
    }

    fn edit_field(&mut self, field: &str, value: String, cx: &mut Context<Self>) {
        if let Some(event) = self.document.edit_field(field, value) {
            self.dispatch(event, cx);
        }
        cx.notify();
    }

    fn dispatch(&mut self, event: ThemeEditorEvent, cx: &mut Context<Self>) {
        let ThemeEditorEvent::Submit(command) = event else {
            return;
        };
        let action = command.command.clone();
        let cancellation = CommandCancellation::new();
        let now = Instant::now();
        match self.sender.submit(
            command,
            now.checked_add(Duration::from_secs(30)).unwrap_or(now),
            cancellation.clone(),
        ) {
            Ok(receiver) => self.document.started(action, receiver, cancellation),
            Err(error) => self
                .document
                .failed(format!("Theme command could not be queued: {error:?}")),
        }
        cx.notify();
    }

    pub(crate) fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.document.is_busy() {
            return;
        }
        let event = self.document.poll();
        if event.is_none() && self.document.is_busy() {
            return;
        }
        if let Some(event) = event {
            self.dispatch(event, cx);
        }
        let spec = self.document.spec();
        for (id, value) in std::iter::once(("name".to_owned(), spec.text.unwrap_or_default()))
            .chain(
                spec.fields
                    .into_iter()
                    .filter(|field| field.kind == DialogFieldKind::Text)
                    .map(|field| (field.id, field.value)),
            )
        {
            if let Some(input) = self.inputs.get(&id)
                && input.read(cx).value().as_ref() != value
            {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        cx.notify();
    }

    pub(crate) fn restore_preview(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(event) = self.document.restore_preview() {
            self.dispatch(event, cx);
        }
        !self.document.preview_active()
    }

    pub(crate) fn preview_restored(&self) -> Option<bool> {
        (!self.document.is_busy()).then(|| !self.document.preview_active())
    }

    fn action(&mut self, action: &str, cx: &mut Context<Self>) {
        if let Some(event) = self.document.activate(action) {
            self.dispatch(event, cx);
        }
        cx.notify();
    }
}

impl GpuiThemeEditor {
    pub(crate) fn begin_authoring(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.named = true;
        self.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn show_workspace(&mut self, cx: &mut Context<Self>) {
        if !self.restore_preview(cx) {
            return;
        }
        self.named = false;
        cx.notify();
    }

    fn render_document_fields(&self, busy: bool, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let mut content = div().v_flex().gap_2();
        for (id, label) in [
            ("load-name", "Theme to load"),
            ("name", "Save as"),
            ("import-path", "Import TOML or iTerm2 file"),
            ("source", "Source"),
            ("license", "License"),
        ] {
            if matches!(id, "import-path" | "source" | "license") && !self.details {
                continue;
            }
            if let Some(input) = self.inputs.get(id) {
                content = content.child(
                    div()
                        .v_flex()
                        .gap_1()
                        .child(Label::new(label).text_sm())
                        .child(crate::gpui::focus_input(
                            input,
                            Input::new(input)
                                .aria_label(label)
                                .disabled(busy)
                                .small()
                                .w_full(),
                        )),
                );
            }
        }

        content = content.child(
            Button::new("theme-author-details")
                .label("Import and attribution")
                .ghost()
                .small()
                .selected(self.details)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.details = !this.details;
                    cx.notify();
                })),
        );
        content.into_any_element()
    }

    fn render_swatches(
        &self,
        spec: &crate::gpui::DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let mut swatches = div().flex().flex_wrap().gap_3();
        for field in spec.fields.iter().filter(|field| {
            field.kind == DialogFieldKind::Color
                && if field.id.starts_with("palette-") {
                    self.group == ThemeColorGroup::Palette
                } else {
                    ThemeColorGroup::for_setting(&field.id) == Some(self.group)
                }
        }) {
            let owner = cx.entity();
            let id = field.id.clone();
            let value = field.value.clone();
            let label = field.label.replace('-', " ");
            let control = render_color_picker(
                ColorPickerParams {
                    selector: format!("theme-author-color-{id}"),
                    label: &label,
                    value: &value,
                    default_label: "Theme default",
                    enabled: !spec.busy,
                    resettable: !id.starts_with("palette-") && !value.is_empty(),
                },
                Rc::new(move |update, app| {
                    owner.update(app, |this, cx| {
                        let value = match update {
                            ColorPickerUpdate::Set(value) => value,
                            ColorPickerUpdate::Reset => String::new(),
                        };
                        this.edit_field(&id, value, cx);
                    });
                }),
                window,
                cx,
            );
            swatches = swatches.child(
                div()
                    .v_flex()
                    .gap_2()
                    .w(rems(12.))
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(Label::new(label).text_sm())
                    .child(control),
            );
        }

        swatches.into_any_element()
    }

    fn render_named_theme(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let stacked = f32::from(window.viewport_size().width) < f32::from(window.rem_size()) * 45.0;
        let spec = self.document.spec();
        let mut content = div()
            .id("named-theme-scroll")
            .debug_selector(|| "named-theme-scroll".to_owned())
            .v_flex()
            .gap_3()
            .when(!stacked, Styled::flex_1)
            .when(stacked, Styled::flex_shrink_0)
            .min_w_0()
            .p_4();
        content = content
            .child(Label::new("Edit a named theme").text_lg())
            .child(
            Label::new(
                "Built-in themes are saved as copies. Color edits preview the current workspace.",
            )
            .text_sm()
            .text_color(cx.theme().muted_foreground),
        );
        content = content.child(self.render_document_fields(spec.busy, cx));
        let mut actions = div().flex().flex_wrap().gap_2();
        for (action, label) in [
            ("load", "Load"),
            ("import", "Import…"),
            ("save", "Save and apply"),
        ] {
            actions = actions.child(
                Button::new(SharedString::from(format!("theme-author-{action}")))
                    .label(label)
                    .small()
                    .disabled(
                        spec.busy
                            || (action == "save"
                                && spec.rows.first().is_none_or(|row| !row.enabled)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if action == "import" && !this.details {
                            this.details = true;
                            cx.notify();
                        } else {
                            this.action(action, cx);
                        }
                    })),
            );
        }
        content = content.child(actions);
        content = content.child(self.render_named_groups(cx));
        let swatches = self.render_swatches(&spec, window, cx);
        content = content
            .child(swatches)
            .when_some(spec.footer, |this, notice| {
                this.child(
                    Label::new(notice)
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
            });
        if let Some(error) = spec.rows.first().and_then(|row| row.detail.clone()) {
            content = content.child(Label::new(error).text_color(cx.theme().danger));
        }
        let editor = if stacked {
            content.into_any_element()
        } else {
            content.overflow_y_scrollbar().into_any_element()
        };
        let layout = div()
            .id("named-theme-layout-scroll")
            .debug_selector(|| "named-theme-layout-scroll".to_owned())
            .flex()
            .when(stacked, Styled::flex_col)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_4()
            .child(editor)
            .child(div().p_4().child(render_theme_preview(
                &self.colors,
                self.settings.read(cx).draft.draft_document(),
                cx,
            )));
        if stacked {
            layout.overflow_y_scrollbar().into_any_element()
        } else {
            layout.into_any_element()
        }
    }

    fn render_named_groups(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let mut groups = div().flex().flex_wrap().gap_1();
        for group in ThemeColorGroup::ALL
            .into_iter()
            .filter(|group| !matches!(group, ThemeColorGroup::Window | ThemeColorGroup::Sidebar))
        {
            groups = groups.child(
                Button::new(SharedString::from(format!("theme-author-group-{group:?}")))
                    .label(group.label())
                    .small()
                    .ghost()
                    .selected(self.group == group)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.group = group;
                        cx.notify();
                    })),
            );
        }
        groups
    }
}

impl gpui_kit::Focusable for GpuiThemeEditor {
    fn focus_handle(&self, cx: &gpui_kit::App) -> gpui_kit::FocusHandle {
        if self.named
            && let Some(input) = self.inputs.get("name")
        {
            return input.focus_handle(cx);
        }
        self.settings.read(cx).focus_handle()
    }
}

impl Render for GpuiThemeEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut mode = div()
            .flex()
            .gap_1()
            .p_3()
            .border_b_1()
            .border_color(cx.theme().border);
        for (named, label) in [(false, "Workspace colors"), (true, "Named theme")] {
            mode = mode.child(
                Button::new(SharedString::from(format!("theme-editor-mode-{named}")))
                    .debug_selector(move || format!("theme-editor-mode-{named}"))
                    .label(label)
                    .ghost()
                    .small()
                    .selected(self.named == named)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if !named && !this.restore_preview(cx) {
                            return;
                        }
                        this.named = named;
                        this.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        let body = if self.named {
            self.render_named_theme(window, cx)
        } else {
            self.settings.clone().into_any_element()
        };
        if self.named && self.pending_named_focus && !self.document.is_busy() {
            self.pending_named_focus = false;
            cx.defer_in(window, |this, window, cx| {
                if this.named {
                    this.focus_handle(cx).focus(window, cx);
                }
            });
        }
        div()
            .v_flex()
            .size_full()
            .min_h_0()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if this.named && event.keystroke.key == "escape" {
                    this.settings
                        .update(cx, |settings, cx| settings.emit(SettingsIntent::Close, cx));
                    cx.stop_propagation();
                }
            }))
            .child(mode)
            .child(body)
    }
}
