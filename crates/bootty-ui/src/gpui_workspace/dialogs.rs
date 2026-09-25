//! Own one dialog presentation and its retained GPUI views per workspace window.

use super::{Colors, GpuiWorkspace, schedule_focus};
use crate::gpui::{
    DialogId, DialogIntent, DialogSpec, DialogView, GpuiSpaceEditor, OverlayHost,
    SpaceEditorColors, SpaceEditorIntent, SpaceEditorSnapshot,
};
use crate::presentation::dialogs::DialogProjection;
use gpui_kit::component::{WindowExt as _, dialog::Dialog};
use gpui_kit::{
    Context, Entity, FocusHandle, Focusable as _, ParentElement as _, Styled as _, Subscription,
    Window, prelude::*, px,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Presentation {
    TerminalFind,
    // Root bakes its title and chrome into open_dialog. A workflow changing its title
    // must reopen the Root even while it keeps the same dialog id.
    Modal { id: DialogId, title: Option<String> },
    SpaceEditor,
}

impl Presentation {
    const fn is_modal(&self) -> bool {
        !matches!(self, Self::TerminalFind)
    }
}

struct SpaceEditorView {
    view: Entity<GpuiSpaceEditor>,
    _subscription: Subscription,
}

pub(super) struct WorkspaceDialogs {
    pub view: Entity<DialogView>,
    pub overlay: Entity<OverlayHost>,
    presentation: Option<Presentation>,
    space_editor: Option<SpaceEditorView>,
    _subscriptions: [Subscription; 2],
}

impl WorkspaceDialogs {
    pub fn new(window: &mut Window, cx: &mut Context<GpuiWorkspace>) -> Self {
        let view = cx.new(|cx| DialogView::new(window, cx));
        let dialog_subscription = cx.subscribe(&view, |this, _, intent: &DialogIntent, cx| {
            let mut effects = Vec::new();
            if intent.dialog_id().0 == crate::presentation::dialogs::TERMINAL_FIND_ID {
                this.state.apply_terminal_find_dialog_intent(intent);
            } else {
                this.state.apply_dialog_intent(intent, &mut effects);
            }
            this.pending_effects.extend(effects);
            cx.notify();
        });
        let overlay = cx.new(|_| OverlayHost::new());
        let overlay_subscription =
            cx.subscribe(&overlay, |this, _, _: &gpui_kit::DismissEvent, cx| {
                if this.dialogs.presentation == Some(Presentation::TerminalFind) {
                    this.dialogs.presentation = None;
                    this.state.close_overlay_dialogs();
                }
                cx.notify();
            });
        Self {
            view,
            overlay,
            presentation: None,
            space_editor: None,
            _subscriptions: [dialog_subscription, overlay_subscription],
        }
    }

    pub fn present(
        &mut self,
        projection: Option<DialogProjection>,
        colors: Colors,
        terminal_focus: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<GpuiWorkspace>,
    ) {
        match projection {
            Some(DialogProjection::Dialog(spec)) => self.present_dialog(*spec, window, cx),
            Some(DialogProjection::SpaceEditor(snapshot)) => {
                self.present_space_editor(*snapshot, colors, window, cx);
            }
            None => {
                self.view
                    .update(cx, |view, cx| view.present(None, window, cx));
                let closed_modal = self
                    .presentation
                    .as_ref()
                    .is_some_and(Presentation::is_modal)
                    && window.has_active_dialog(cx);
                self.clear_presentation(window, cx);
                if closed_modal && let Some(focus) = terminal_focus {
                    // A picker may select a new terminal while Root remembers the old trigger.
                    schedule_focus(focus, window, cx);
                }
            }
        }
    }

    fn clear_presentation(&mut self, window: &mut Window, cx: &mut Context<GpuiWorkspace>) {
        match self.presentation.take() {
            Some(Presentation::TerminalFind) => {
                self.overlay.update(cx, |host, cx| {
                    host.clear(window, cx);
                });
            }
            Some(_) if window.has_active_dialog(cx) => window.close_dialog(cx),
            _ => {}
        }
    }

    fn present_dialog(
        &mut self,
        spec: DialogSpec,
        window: &mut Window,
        cx: &mut Context<GpuiWorkspace>,
    ) {
        let id = spec.id.clone();
        self.view
            .update(cx, |view, cx| view.present(Some(spec), window, cx));
        if self.view.read(cx).is_non_modal() {
            if self.presentation != Some(Presentation::TerminalFind) {
                self.clear_presentation(window, cx);
                self.overlay
                    .update(cx, |host, cx| host.present(self.view.clone(), window, cx));
                self.presentation = Some(Presentation::TerminalFind);
            }
            return;
        }
        let title = self.view.read(cx).root_title();
        let presentation = Presentation::Modal {
            id,
            title: title.clone(),
        };
        if self.presentation.as_ref() == Some(&presentation) && window.has_active_dialog(cx) {
            return;
        }
        self.clear_presentation(window, cx);
        let view = self.view.clone();
        let show_root_chrome = title.is_some();
        let workspace = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, _| {
            let content_view = view.clone();
            let workspace = workspace.clone();
            dialog
                .w(px(f32::from(window.rem_size()) * 37.5))
                .max_w(px(f32::from(window.rem_size()) * 45.0))
                .when(!show_root_chrome, |dialog| dialog.p_0().gap_0())
                .when_some(title.clone(), Dialog::title)
                .close_button(show_root_chrome)
                .on_cancel(move |_, _, cx| {
                    let _ = workspace.update(cx, |workspace, cx| {
                        workspace.state.close_overlay_dialogs();
                        cx.notify();
                    });
                    true
                })
                .content(move |content, _, _| {
                    content
                        .when(!show_root_chrome, gpui_kit::Styled::p_0)
                        .child(content_view.clone())
                })
        });
        self.presentation = Some(presentation);
        schedule_focus(self.view.focus_handle(cx), window, cx);
    }

    fn present_space_editor(
        &mut self,
        mut snapshot: SpaceEditorSnapshot,
        colors: Colors,
        window: &mut Window,
        cx: &mut Context<GpuiWorkspace>,
    ) {
        self.view
            .update(cx, |view, cx| view.present(None, window, cx));
        let opening =
            self.presentation != Some(Presentation::SpaceEditor) || !window.has_active_dialog(cx);
        if opening {
            self.clear_presentation(window, cx);
        }
        snapshot.colors = space_editor_colors(colors);
        let title = snapshot.title.clone();
        let view = if let Some(editor) = &self.space_editor {
            editor
                .view
                .update(cx, |editor, cx| editor.set_snapshot(snapshot, cx));
            editor.view.clone()
        } else {
            let view = cx.new(|cx| GpuiSpaceEditor::new_with_window(snapshot, window, cx));
            let subscription = cx.subscribe(&view, |this, _, intent: &SpaceEditorIntent, cx| {
                this.state.apply_space_editor_ui_intent(intent.clone());
                cx.notify();
            });
            self.space_editor = Some(SpaceEditorView {
                view: view.clone(),
                _subscription: subscription,
            });
            view
        };
        if !opening {
            return;
        }
        let workspace = cx.weak_entity();
        let editor = view.clone();
        window.open_dialog(cx, move |dialog, window, app| {
            let editor = editor.clone();
            let workspace = workspace.clone();
            dialog
                .w(px(f32::from(window.rem_size()) * 37.5))
                .max_w(px(f32::from(window.rem_size()) * 45.0))
                .title(title.clone())
                .footer(GpuiSpaceEditor::render_dialog_footer(editor.clone(), app))
                .on_cancel(move |_, _, cx| {
                    let _ = workspace.update(cx, |workspace, cx| {
                        workspace.state.close_overlay_dialogs();
                        cx.notify();
                    });
                    true
                })
                .content(move |content, _, _| content.p_0().child(editor.clone()))
        });
        cx.defer_in(window, move |_, window, cx| {
            view.update(cx, |editor, cx| editor.focus(window, cx));
        });
        self.presentation = Some(Presentation::SpaceEditor);
    }
}

const fn space_editor_colors(colors: Colors) -> SpaceEditorColors {
    SpaceEditorColors {
        pane: colors.pane,
        surface: colors.surface,
        hover: colors.hover,
        border: colors.border,
        text: colors.text,
        muted: colors.muted,
        accent: colors.accent,
        destructive: colors.destructive,
    }
}
