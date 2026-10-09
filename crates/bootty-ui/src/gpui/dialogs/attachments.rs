//! Local creation draft attachments; admission belongs to the native session service.
use super::*;
use crate::presentation::new_session_form::NewSessionAttachment;
use std::{path::PathBuf, sync::Arc};

impl DialogView {
    pub(super) fn choose_new_session_attachments(&self, window: &Window, cx: &Context<Self>) {
        let Some(dialog) = self.attachment_dialog() else {
            return;
        };
        let epoch = self.attachment_epoch;
        let paths = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |owner, cx| match paths.await {
            Ok(Ok(Some(paths))) => {
                _ = owner.update_in(cx, |this, window, cx| {
                    if this.attachment_epoch == epoch
                        && this.attachment_dialog().as_ref() == Some(&dialog)
                    {
                        for path in paths {
                            this.stage_new_session_attachment(path, None, window, cx);
                        }
                    }
                });
            }
            Ok(Ok(None)) => {}
            _ => {
                _ = owner.update(cx, |this, cx| {
                    this.attachment_error = Some("Could not open the file picker".into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn paste_new_session_attachments(
        &mut self,
        item: &gpui_kit::ClipboardItem,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(dialog) = self.attachment_dialog() else {
            return false;
        };
        let mut consumed = false;
        for entry in item.entries() {
            match entry {
                gpui_kit::ClipboardEntry::ExternalPaths(paths) => {
                    consumed = true;
                    for path in &paths.0 {
                        self.stage_new_session_attachment(path.clone(), None, window, cx);
                    }
                }
                gpui_kit::ClipboardEntry::Image(image) => {
                    consumed = true;
                    if self.spec.as_ref().is_some_and(|spec| {
                        spec.attachments
                            .len()
                            .saturating_add(self.attachment_imports)
                            >= 16
                    }) {
                        self.attachment_error = Some("Attach at most 16 files".into());
                        continue;
                    }
                    if image.bytes().len() > bootty_agents::MAX_NATIVE_ATTACHMENT_IMAGE_BYTES {
                        self.attachment_error = Some("Image exceeds 8 MB".into());
                        continue;
                    }
                    let bytes = image.bytes().to_vec();
                    let dialog = dialog.clone();
                    let epoch = self.attachment_epoch;
                    self.attachment_imports = self.attachment_imports.saturating_add(1);
                    cx.spawn_in(window, async move |owner, cx| {
                        let result = cx
                            .background_executor()
                            .spawn(async move { crate::gpui_agent_session::clipboard_png(&bytes) })
                            .await;
                        _ = owner.update_in(cx, |this, window, cx| {
                            this.attachment_imports = this.attachment_imports.saturating_sub(1);
                            if this.attachment_epoch != epoch
                                || this.attachment_dialog().as_ref() != Some(&dialog)
                            {
                                cx.notify();
                                return;
                            }
                            match result {
                                Ok(file) => this.stage_new_session_attachment(
                                    file.path().to_owned(),
                                    Some(crate::attachment_source::AttachmentTemporary::Clipboard(
                                        Arc::new(file),
                                    )),
                                    window,
                                    cx,
                                ),
                                Err(error) => {
                                    this.attachment_error = Some(error);
                                    cx.notify();
                                }
                            }
                        });
                    })
                    .detach();
                }
                gpui_kit::ClipboardEntry::String(_) => {}
            }
        }
        if consumed {
            cx.notify();
        }
        consumed
    }

    fn attachment_dialog(&self) -> Option<DialogId> {
        self.spec
            .as_ref()
            .filter(|spec| {
                spec.id.0 == crate::presentation::dialogs::NEW_SESSION_ID
                    && spec.multiline
                    && !spec.busy
                    && spec.fields.iter().any(|field| field.id == "provider")
            })
            .map(|spec| spec.id.clone())
    }

    pub(super) fn stage_new_session_attachment(
        &mut self,
        path: PathBuf,
        temporary: Option<crate::attachment_source::AttachmentTemporary>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.attachment_dialog() else {
            return;
        };
        let epoch = self.attachment_epoch;
        let insertion = self.prompt_textarea.read(cx).selected_range();
        let content = self.prompt_textarea.read(cx).value();
        if self.spec.as_ref().is_some_and(|spec| {
            spec.attachments
                .len()
                .saturating_add(self.attachment_imports)
                >= 16
        }) {
            self.attachment_error = Some("Attach at most 16 files".into());
            cx.notify();
            return;
        }
        self.attachment_imports = self.attachment_imports.saturating_add(1);
        self.attachment_error = None;
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let metadata =
                        std::fs::metadata(&path).map_err(|_| "Could not read the file")?;
                    if !path.is_absolute() || !metadata.is_file() {
                        return Err("Choose a local file".to_owned());
                    }
                    if metadata.len() > bootty_agents::MAX_NATIVE_ATTACHMENT_FILE_BYTES {
                        return Err("File exceeds 50 MB".to_owned());
                    }
                    let preview = crate::gpui_agent_session::attachment_preview(&path);
                    Ok(NewSessionAttachment {
                        path,
                        size_bytes: metadata.len(),
                        prompt_ranges: Vec::new(),
                        preview,
                        temporary,
                    })
                })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                this.attachment_imports = this.attachment_imports.saturating_sub(1);
                if this.attachment_epoch != epoch
                    || this.attachment_dialog().as_ref() != Some(&dialog)
                {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(attachment) => {
                        let Some(spec) = this.spec.as_mut() else {
                            return;
                        };
                        if !spec
                            .attachments
                            .iter()
                            .any(|existing| existing.path == attachment.path)
                        {
                            let id = attachment.path.to_string_lossy().into_owned();
                            let name = attachment
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy();
                            let token = crate::gpui_prompt_attachments::token(
                                id,
                                &name,
                                attachment.size_bytes,
                            );
                            spec.attachments.push(attachment);
                            this.prompt_textarea.update(cx, |input, cx| {
                                let insertion = if input.value() == content {
                                    insertion
                                } else {
                                    input.value().len()..input.value().len()
                                };
                                input.set_selected_range(insertion, cx);
                                if let Err(error) = input.replace_with_token(token, window, cx) {
                                    this.attachment_error = Some(error.to_string());
                                }
                                input.focus_handle(cx).focus(window, cx);
                            });
                            cx.emit(DialogIntent::AttachmentsChanged {
                                dialog,
                                attachments: spec.attachments.clone(),
                            });
                        }
                    }
                    Err(error) => this.attachment_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn new_session_attachment_picker(
        &self,
        spec: &DialogSpec,
        cx: &Context<Self>,
    ) -> Button {
        Button::new("new-session-attach")
            .icon(gpui_kit::assets::IconName::Paperclip)
            .small()
            .ghost()
            .accessibility_label("Attach files")
            .tooltip("Attach files")
            .disabled(spec.busy || self.attachment_imports > 0)
            .on_click(
                cx.listener(|this, _, window, cx| this.choose_new_session_attachments(window, cx)),
            )
    }

    pub(super) fn render_new_session_attachments(
        &self,
        spec: &DialogSpec,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div().flex().flex_wrap().gap_2().children(
                    spec.attachments
                        .iter()
                        .filter(|attachment| {
                            self.prompt_textarea.read(cx).tokens().iter().any(|span| {
                                span.token().id().as_ref() == attachment.path.to_string_lossy()
                            })
                        })
                        .filter_map(|attachment| {
                            let preview = attachment.preview.clone()?;
                            Some(
                                div()
                                    .w_16()
                                    .h_16()
                                    .overflow_hidden()
                                    .rounded(cx.theme().radius)
                                    .child(
                                        gpui_kit::img(preview)
                                            .size_full()
                                            .object_fit(gpui_kit::ObjectFit::Contain),
                                    ),
                            )
                        }),
                ),
            )
            .when(self.attachment_imports > 0, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Attaching…"),
                )
            })
            .when_some(self.attachment_error.clone(), |row, error| {
                row.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }

    pub(super) fn publish_active_attachments(&self, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else { return };
        if spec.id.0 != crate::presentation::dialogs::NEW_SESSION_ID {
            return;
        }
        let tokens = self.prompt_textarea.read(cx).tokens();
        cx.emit(DialogIntent::AttachmentsChanged {
            dialog: spec.id.clone(),
            attachments: spec
                .attachments
                .iter()
                .filter(|attachment| {
                    tokens
                        .iter()
                        .any(|span| span.token().id().as_ref() == attachment.path.to_string_lossy())
                })
                .map(|attachment| {
                    let mut attachment = attachment.clone();
                    attachment.prompt_ranges = tokens
                        .iter()
                        .filter(|span| {
                            span.token().id().as_ref() == attachment.path.to_string_lossy()
                        })
                        .map(gpui_kit::component::input::InlineTokenSpan::range)
                        .collect();
                    attachment
                })
                .collect(),
        });
    }
}
