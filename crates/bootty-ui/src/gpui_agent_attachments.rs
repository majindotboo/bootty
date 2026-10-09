//! Composer attachment ingress; the native service owns admitted bytes and identities.
use super::*;
use std::{io::Write as _, path::PathBuf};

#[derive(Clone)]
pub(super) struct ComposerAttachment {
    pub reference: bootty_agents::NativeAttachmentReference,
    pub preview: Option<Arc<gpui_kit::RenderImage>>,
}

impl NativeAgentSessionView {
    pub(super) fn prompt_arguments(
        &self,
        message: String,
        authored_bytes: usize,
        cx: &App,
    ) -> Result<Vec<String>, String> {
        let images = self
            .annotations
            .iter()
            .filter(|annotation| annotation.image.is_some())
            .collect::<Vec<_>>();
        if images.len() > 4 {
            return Err("Include at most four images".into());
        }
        let images = if images.is_empty() {
            String::new()
        } else {
            let records = serde_json::to_string(&images).map_err(|error| error.to_string())?;
            if records.len() > 64 * 1024 {
                return Err("Image context exceeds the prompt limit".into());
            }
            records
        };
        let ids = self.active_attachment_ids(cx);
        let mut ranges = std::collections::BTreeMap::<String, Vec<std::ops::Range<usize>>>::new();
        for span in self.composer.read(cx).tokens() {
            let id = span.token().id().as_ref();
            if ids.iter().any(|attachment| attachment == id) {
                ranges.entry(id.to_owned()).or_default().push(span.range());
            }
        }
        Ok(vec![
            message,
            images,
            serde_json::to_string(&ids).map_err(|error| error.to_string())?,
            serde_json::to_string(&self.active_citations(cx)).map_err(|error| error.to_string())?,
            serde_json::to_string(&self.active_applications(cx))
                .map_err(|error| error.to_string())?,
            authored_bytes.to_string(),
            serde_json::to_string(&ranges).map_err(|error| error.to_string())?,
        ])
    }

    pub(super) fn choose_attachments(&self, window: &Window, cx: &Context<Self>) {
        if !self.provider_enabled() {
            return;
        }
        let paths = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |owner, cx| match paths.await {
            Ok(Ok(Some(paths))) => {
                _ = owner.update_in(cx, |this, window, cx| {
                    for path in paths {
                        this.import_attachment(path, None, window, cx);
                    }
                });
            }
            Ok(Ok(None)) => {}
            _ => {
                _ = owner.update(cx, |this, cx| {
                    this.error = Some("Could not open the file picker".into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn paste_attachments(
        &mut self,
        item: &gpui_kit::ClipboardItem,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut consumed = false;
        for entry in item.entries() {
            match entry {
                gpui_kit::ClipboardEntry::ExternalPaths(paths) => {
                    consumed = true;
                    for path in &paths.0 {
                        self.import_attachment(path.clone(), None, window, cx);
                    }
                }
                gpui_kit::ClipboardEntry::Image(image) => {
                    consumed = true;
                    if self
                        .attachments
                        .len()
                        .saturating_add(self.attachment_imports)
                        >= 16
                    {
                        self.error = Some("Attach at most 16 files".into());
                        continue;
                    }
                    if image.bytes().len() > bootty_agents::MAX_NATIVE_ATTACHMENT_IMAGE_BYTES {
                        self.error = Some("Image exceeds 8 MB".into());
                        continue;
                    }
                    let target = self.record.target();
                    let bytes = image.bytes().to_vec();
                    self.attachment_imports = self.attachment_imports.saturating_add(1);
                    cx.spawn_in(window, async move |owner, cx| {
                        let result = cx
                            .background_executor()
                            .spawn(async move { clipboard_png(&bytes) })
                            .await;
                        _ = owner.update_in(cx, |this, window, cx| {
                            this.attachment_imports = this.attachment_imports.saturating_sub(1);
                            if this.record.target() != target {
                                cx.notify();
                                return;
                            }
                            match result {
                                Ok(file) => this.import_attachment(
                                    file.path().to_owned(),
                                    Some(crate::attachment_source::AttachmentTemporary::Clipboard(
                                        Arc::new(file),
                                    )),
                                    window,
                                    cx,
                                ),
                                Err(error) => {
                                    this.error = Some(error);
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

    pub(super) fn import_attachment(
        &mut self,
        path: PathBuf,
        temporary: Option<crate::attachment_source::AttachmentTemporary>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .attachments
            .len()
            .saturating_add(self.attachment_imports)
            >= 16
        {
            self.error = Some("Attach at most 16 files".into());
            cx.notify();
            return;
        }
        let insertion = self.composer.read(cx).selected_range();
        let revision = self.draft_revision;
        let target = self.record.target();
        let mut invocation = CommandInvocation::new(
            "agents.native.import",
            vec![
                self.record.id.clone(),
                self.record.generation.to_string(),
                path.to_string_lossy().into_owned(),
            ],
            Caller::Internal,
        );
        invocation.target = Some(target.clone());
        let Ok(receiver) = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) else {
            self.error = Some("Could not attach the file".into());
            cx.notify();
            return;
        };
        self.attachment_imports = self.attachment_imports.saturating_add(1);
        self.error = None;
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let result = receiver.recv();
                    let preview = attachment_preview(&path);
                    drop(temporary);
                    (result, preview)
                })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                this.attachment_imports = this.attachment_imports.saturating_sub(1);
                if this.record.target() != target {
                    return;
                }
                match result {
                    (Ok(CommandOutcome::Success { value, .. }), preview) => {
                        match serde_json::from_value(value) {
                            Ok(reference) => {
                                let reference: bootty_agents::NativeAttachmentReference = reference;
                                if let Some(preview) = &preview {
                                    this.attachment_previews
                                        .insert(reference.id.clone(), preview.clone());
                                }
                                let token = crate::gpui_prompt_attachments::token(
                                    reference.id.clone(),
                                    &reference.name,
                                    reference.size_bytes,
                                );
                                this.attachments
                                    .push(ComposerAttachment { reference, preview });
                                this.composer.update(cx, |input, cx| {
                                    if this.draft_revision == revision {
                                        input.set_selected_range(insertion, cx);
                                    } else {
                                        input.set_selected_range(
                                            input.value().len()..input.value().len(),
                                            cx,
                                        );
                                    }
                                    if let Err(error) = input.replace_with_token(token, window, cx)
                                    {
                                        this.error = Some(error.to_string());
                                    }
                                    input.focus_handle(cx).focus(window, cx);
                                });
                                this.draft_revision = this.draft_revision.wrapping_add(1);
                            }
                            Err(_) => this.error = Some("Invalid attachment response".into()),
                        }
                    }
                    (Ok(outcome), _) => {
                        this.error = crate::commands::command_outcome_message(&outcome);
                    }
                    (Err(_), _) => this.error = Some("Could not attach the file".into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_attachments(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div().flex().flex_col().gap_2().child(
            div().flex().flex_wrap().gap_2().children(
                self.attachments
                    .iter()
                    .filter(|attachment| {
                        self.active_attachment_ids(cx)
                            .contains(&attachment.reference.id)
                    })
                    .filter_map(|attachment| {
                        let preview = attachment.preview.as_ref()?;
                        let id = attachment.reference.id.clone();
                        Some(
                            div()
                                .relative()
                                .w_20()
                                .h_20()
                                .rounded_md()
                                .overflow_hidden()
                                .child(
                                    gpui_kit::img(preview.clone())
                                        .size_full()
                                        .object_fit(gpui_kit::ObjectFit::Contain),
                                )
                                .child(
                                    div().absolute().top_0().right_0().child(
                                        Button::new(gpui_kit::SharedString::from(format!(
                                            "remove-preview:{id}"
                                        )))
                                        .icon(IconName::Close)
                                        .xsmall()
                                        .ghost()
                                        .accessibility_label(format!(
                                            "Remove {}",
                                            attachment.reference.name
                                        ))
                                        .tooltip(format!("Remove {}", attachment.reference.name))
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.remove_attachment(&id, window, cx);
                                            }),
                                        ),
                                    ),
                                ),
                        )
                    }),
            ),
        )
    }

    pub(super) fn active_attachment_ids(&self, cx: &App) -> Vec<String> {
        self.composer
            .read(cx)
            .tokens()
            .iter()
            .map(|span| span.token().id().to_string())
            .filter(|id| {
                self.attachments
                    .iter()
                    .any(|attachment| &attachment.reference.id == id)
            })
            .collect()
    }

    fn remove_attachment(&self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let ranges = self
            .composer
            .read(cx)
            .tokens()
            .iter()
            .filter(|span| span.token().id().as_ref() == id)
            .map(gpui_kit::component::input::InlineTokenSpan::range)
            .collect::<Vec<_>>();
        self.composer.update(cx, |input, cx| {
            use gpui_kit::EntityInputHandler as _;
            for range in ranges.into_iter().rev() {
                input.set_selected_range(range, cx);
                input.replace_text_in_range(None, "", window, cx);
            }
        });
        cx.notify();
    }

    pub(super) fn load_attachment_previews(&mut self, window: &Window, cx: &Context<Self>) {
        let images = self
            .record
            .snapshot
            .transcript
            .iter()
            .flat_map(|item| &item.attachments)
            .filter(|reference| reference.kind == bootty_agents::NativeAttachmentKind::Image)
            .map(|reference| reference.id.clone())
            .collect::<BTreeSet<_>>();
        for id in images {
            if self.attachment_previews.contains_key(&id)
                || !self.attachment_preview_attempts.insert(id.clone())
            {
                continue;
            }
            let target = self.record.target();
            let mut invocation = CommandInvocation::new(
                "agents.native.attachment-preview",
                vec![
                    self.record.id.clone(),
                    self.record.generation.to_string(),
                    id.clone(),
                ],
                Caller::Internal,
            );
            invocation.target = Some(target.clone());
            let Ok(receiver) = self.sender.submit(
                invocation,
                Instant::now()
                    .checked_add(Duration::from_secs(60))
                    .unwrap_or_else(Instant::now),
                CommandCancellation::new(),
            ) else {
                continue;
            };
            cx.spawn_in(window, async move |owner, cx| {
                let preview = cx
                    .background_executor()
                    .spawn(async move {
                        use base64::Engine as _;
                        let CommandOutcome::Success { value, .. } = receiver.recv().ok()? else {
                            return None;
                        };
                        let encoded = value.get("png")?.as_str()?;
                        let limit = bootty_agents::MAX_NATIVE_ATTACHMENT_PREVIEW_BYTES;
                        if base64::encoded_len(limit, true).is_none_or(|max| encoded.len() > max) {
                            return None;
                        }
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(encoded)
                            .ok()?;
                        if bytes.len() > limit {
                            return None;
                        }
                        decode_annotation_preview(&bytes)
                    })
                    .await;
                _ = owner.update(cx, |this, cx| {
                    if this.record.target() == target
                        && let Some(preview) = preview
                    {
                        this.attachment_previews.insert(id, preview);
                        this.list.remeasure();
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    pub(super) fn render_sent_attachments(
        &self,
        item: &NativeTranscriptItem,
        cx: &App,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_2()
            .when(
                item.attachments
                    .iter()
                    .any(|reference| self.attachment_previews.contains_key(&reference.id)),
                |row| {
                    row.child(div().flex().flex_shrink_0().flex_wrap().gap_2().children(
                        item.attachments.iter().filter_map(|reference| {
                            self.attachment_previews.get(&reference.id).map(|preview| {
                                div()
                                    .id(gpui_kit::SharedString::from(format!(
                                        "attachment-preview:{}",
                                        reference.id
                                    )))
                                    .debug_selector({
                                        let id = reference.id.clone();
                                        move || format!("attachment-preview:{id}")
                                    })
                                    .w_20()
                                    .h_20()
                                    .rounded_md()
                                    .overflow_hidden()
                                    .child(
                                        gpui_kit::img(preview.clone())
                                            .size_full()
                                            .object_fit(gpui_kit::ObjectFit::Contain),
                                    )
                            })
                        }),
                    ))
                },
            )
            .child(div().flex().flex_shrink_0().flex_wrap().gap_2().children(
                item.attachments.iter().map(|reference| {
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .child(crate::gpui::sized_icon(
                            crate::gpui_prompt_attachments::icon(&reference.name),
                            crate::gpui::IconSize::Small,
                            cx.theme().muted_foreground,
                        ))
                        .child(reference.name.clone())
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::gpui_prompt_attachments::size(reference.size_bytes)),
                        )
                }),
            ))
    }
}

pub fn clipboard_png(bytes: &[u8]) -> Result<tempfile::NamedTempFile, String> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Image exceeds 8 MB".into());
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Unsupported image")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| "Unsupported image")?;
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|_| "Could not read the image")?;
    if png.get_ref().len() > 8 * 1024 * 1024 {
        return Err("Image exceeds 8 MB".into());
    }
    let mut file = tempfile::Builder::new()
        .prefix("Screenshot-")
        .suffix(".png")
        .tempfile()
        .map_err(|_| "Could not attach the image")?;
    file.write_all(png.get_ref())
        .map_err(|_| "Could not attach the image")?;
    Ok(file)
}

pub fn attachment_preview(path: &std::path::Path) -> Option<Arc<gpui_kit::RenderImage>> {
    if std::fs::metadata(path).ok()?.len() > 50 * 1024 * 1024 {
        return None;
    }
    let mut reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut pixels = reader.decode().ok()?.thumbnail(160, 160).into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(gpui_kit::RenderImage::new([image::Frame::new(
        pixels,
    )])))
}
