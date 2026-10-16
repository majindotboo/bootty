//! Composer attachment ingress; the native service owns admitted bytes and identities.
use super::*;
use crate::gpui_image_viewer::{ImagePreview, ViewedImage};
use gpui_kit::component::menu::ContextMenuExt as _;
use std::path::PathBuf;

#[derive(Clone)]
pub(super) struct ComposerAttachment {
    pub reference: bootty_agents::NativeAttachmentReference,
    pub preview: Option<ImagePreview>,
}

pub(super) struct PendingAttachmentImport {
    name: String,
    cancellation: Option<CommandCancellation>,
}

impl NativeAgentSessionView {
    fn begin_attachment_import(
        &mut self,
        name: String,
        cancellation: Option<CommandCancellation>,
    ) -> u64 {
        let id = self.next_attachment_import;
        self.next_attachment_import = id.wrapping_add(1);
        self.attachment_imports
            .insert(id, PendingAttachmentImport { name, cancellation });
        id
    }

    pub(super) fn cancel_attachment_imports(&mut self) {
        for import in std::mem::take(&mut self.attachment_imports).into_values() {
            if let Some(cancellation) = import.cancellation {
                _ = cancellation.cancel();
            }
        }
    }

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
            serde_json::to_string(
                &self
                    .completion
                    .as_ref()
                    .map(|completion| completion.read(cx).provider_mentions(cx))
                    .transpose()?
                    .unwrap_or_default(),
            )
            .map_err(|error| error.to_string())?,
            serde_json::to_string(&self.active_conversation_mentions(cx))
                .map_err(|error| error.to_string())?,
        ])
    }

    pub(super) fn choose_attachments(&self, window: &Window, cx: &Context<Self>) {
        if !self.provider_enabled() {
            return;
        }
        let paths = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |owner, cx| match paths.await {
            Ok(Ok(Some(paths))) => {
                _ = owner.update_in(cx, |this, window, cx| {
                    for path in paths {
                        this.attach_path(path, window, cx);
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
        let mut images = self.active_image_count(cx);
        for entry in item.entries() {
            match entry {
                gpui_kit::ClipboardEntry::ExternalPaths(paths) => {
                    consumed = true;
                    for path in &paths.0 {
                        self.attach_path(path.clone(), window, cx);
                    }
                }
                entry => {
                    let attachment =
                        match crate::attachment_source::ClipboardAttachment::from_entry(entry) {
                            Ok(Some(attachment)) => attachment,
                            Ok(None) => continue,
                            Err(error) => {
                                consumed = true;
                                self.error = Some(error);
                                continue;
                            }
                        };
                    consumed = true;
                    if self.attachment_slots_full(cx) {
                        self.error = Some("Attach at most 16 files".into());
                        continue;
                    }
                    if attachment.is_image() && images >= bootty_agents::MAX_NATIVE_PROMPT_IMAGES {
                        self.error = Some(too_many_images());
                        continue;
                    }
                    if attachment.is_image() {
                        images = images.saturating_add(1);
                    }
                    let target = self.record.target();
                    let import = self.begin_attachment_import(attachment.label().into(), None);
                    cx.spawn_in(window, async move |owner, cx| {
                        let result = cx
                            .background_executor()
                            .spawn(async move { attachment.write() })
                            .await;
                        _ = owner.update_in(cx, |this, window, cx| {
                            if this.attachment_imports.remove(&import).is_none() {
                                return;
                            }
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
                                    this.error =
                                        Some(super::diagnostic("Could not paste the image", error));
                                    cx.notify();
                                }
                            }
                        });
                    })
                    .detach();
                }
            }
        }
        if consumed {
            cx.notify();
        }
        consumed
    }

    pub(super) fn attach_path(&mut self, path: PathBuf, window: &Window, cx: &mut Context<Self>) {
        if self.attachment_slots_full(cx) {
            self.error = Some("Attach at most 16 files".into());
            cx.notify();
            return;
        }
        let target = self.record.target();
        let insertion = self.composer.read(cx).selected_range();
        let revision = self.draft_revision;
        let import = self.begin_attachment_import(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            None,
        );
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if !path.is_absolute() {
                        return Err("Choose a local file or folder");
                    }
                    let metadata =
                        std::fs::metadata(&path).map_err(|_| "Could not read the attachment")?;
                    Ok::<_, &str>((path, metadata.is_dir()))
                })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if this.attachment_imports.remove(&import).is_none() {
                    return;
                }
                if this.record.target() != target {
                    return;
                }
                match result {
                    Ok((path, true)) => {
                        this.insert_attachment_token(
                            crate::gpui_prompt_attachments::directory_token(&path),
                            insertion,
                            revision,
                            window,
                            cx,
                        );
                    }
                    Ok((path, false)) => {
                        this.composer.update(cx, |input, cx| {
                            if this.draft_revision == revision {
                                input.set_selected_range(insertion, cx);
                            } else {
                                input.set_selected_range(
                                    input.value().len()..input.value().len(),
                                    cx,
                                );
                            }
                        });
                        this.import_attachment(path, None, window, cx);
                    }
                    Err(error) => {
                        this.error = Some(super::diagnostic("Could not attach the file", error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Opens the viewer on the composer's image `id`, stepping through the draft's images.
    pub(super) fn open_attachment(&self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.attachment_previews.contains_key(id) {
            return;
        }
        let active = self.active_attachment_ids(cx);
        let draft = self
            .attachments
            .iter()
            .map(|attachment| &attachment.reference)
            .filter(|reference| active.contains(&reference.id))
            .collect::<Vec<_>>();
        self.view_images(draft, id, window, cx);
    }

    /// Opens the viewer on the sent image `id`, stepping through every image the conversation
    /// shows.
    fn view_sent_image(&self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self
            .record
            .snapshot
            .transcript
            .iter()
            .flat_map(|item| &item.attachments)
            .collect::<Vec<_>>();
        self.view_images(shown, id, window, cx);
    }

    /// Opens the viewer on `id` among the `references` that have a preview.
    fn view_images(
        &self,
        references: Vec<&bootty_agents::NativeAttachmentReference>,
        id: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut index = 0;
        let images = references
            .into_iter()
            .filter_map(|reference| Some((reference, self.attachment_previews.get(&reference.id)?)))
            .enumerate()
            .map(|(position, (reference, preview))| {
                if reference.id == id {
                    index = position;
                }
                ViewedImage {
                    name: reference.name.clone(),
                    preview: preview.clone(),
                }
            })
            .collect();
        crate::gpui_image_viewer::open(images, index, window, cx);
    }

    /// Attaches `text` as a file named from `prefix`, such as a terminal selection.
    pub(crate) fn attach_text(
        &mut self,
        prefix: String,
        text: String,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.attachment_slots_full(cx) {
            self.error = Some("Attach at most 16 files".into());
            cx.notify();
            return;
        }
        let target = self.record.target();
        let import = self.begin_attachment_import(prefix.clone(), None);
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::attachment_source::text_file(&prefix, &text) })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if this.attachment_imports.remove(&import).is_none()
                    || this.record.target() != target
                {
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
                        this.error =
                            Some(super::diagnostic("Could not attach the selection", error));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Names lines of a project file in the message, the way an `@` file mention does, and
    /// focuses the composer.
    pub fn mention_lines(
        &mut self,
        path: &str,
        (first, last): (usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.record.config.cwd.to_string_lossy();
        let relative = path
            .strip_prefix(cwd.as_ref())
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(path);
        let lines = if last > first {
            format!("{first}-{last}")
        } else {
            first.to_string()
        };
        let mention = if relative.contains(char::is_whitespace) {
            format!("@\"{relative}\":{lines} ")
        } else {
            format!("@{relative}:{lines} ")
        };
        self.composer.update(cx, |input, cx| {
            input.insert(mention, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Writes a comment on changed lines into the draft: the file and lines it is about, those
    /// lines, and the comment.
    pub fn add_diff_comment(
        &mut self,
        comment: &crate::gpui_git_panel::DiffComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let anchor = &comment.anchor;
        let lines = if anchor.line > anchor.start_line {
            format!("{}-{}", anchor.start_line, anchor.line)
        } else {
            anchor.start_line.to_string()
        };
        let path = if anchor.path.contains(char::is_whitespace) {
            format!("@\"{}\"", anchor.path)
        } else {
            format!("@{}", anchor.path)
        };
        let side = if anchor.side == bootty_git::diff::DiffSide::Left {
            " (as it was before the change)"
        } else {
            ""
        };
        let text = format!(
            "{path}:{lines}{side}\n```\n{}\n```\n{}\n\n",
            comment.quote,
            comment.body.trim()
        );
        self.composer.update(cx, |input, cx| {
            input.insert(text, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn import_attachment(
        &mut self,
        path: PathBuf,
        temporary: Option<crate::attachment_source::AttachmentTemporary>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.attachment_slots_full(cx) {
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
        let cancellation = CommandCancellation::new();
        let Ok(receiver) = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            cancellation.clone(),
        ) else {
            self.error = Some("Could not attach the file".into());
            cx.notify();
            return;
        };
        let import = self.begin_attachment_import(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            Some(cancellation),
        );
        self.error = None;
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let result = receiver.recv();
                    let preview = ImagePreview::from_file(&path);
                    drop(temporary);
                    (result, preview)
                })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if this.attachment_imports.remove(&import).is_none() {
                    return;
                }
                if this.record.target() != target {
                    return;
                }
                match result {
                    (Ok(CommandOutcome::Success { value, .. }), preview) => {
                        match serde_json::from_value(value) {
                            Ok(reference) => {
                                let reference: bootty_agents::NativeAttachmentReference = reference;
                                if reference.kind == bootty_agents::NativeAttachmentKind::Image
                                    && this.active_image_count(cx)
                                        >= bootty_agents::MAX_NATIVE_PROMPT_IMAGES
                                {
                                    this.error = Some(too_many_images());
                                    cx.notify();
                                    return;
                                }
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
                                this.insert_attachment_token(
                                    token, insertion, revision, window, cx,
                                );
                            }
                            Err(_) => this.error = Some("Invalid attachment response".into()),
                        }
                    }
                    (Ok(outcome), _) => {
                        this.error = super::outcome_error("Could not attach the file", &outcome);
                    }
                    (Err(_), _) => this.error = Some("Could not attach the file".into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_attachment_import(
        id: u64,
        import: &PendingAttachmentImport,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        div()
            .debug_selector(move || format!("attachment-import:{id}"))
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(gpui_kit::component::spinner::Spinner::new().small())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Attaching {}…", import.name)),
            )
            .child(
                Button::new(("cancel-attachment-import", id))
                    .debug_selector(move || format!("cancel-attachment-import:{id}"))
                    .label("Cancel")
                    .ghost()
                    .small()
                    .accessibility_label(format!("Cancel attaching {}", import.name))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(import) = this.attachment_imports.remove(&id)
                            && let Some(cancellation) = import.cancellation
                        {
                            _ = cancellation.cancel();
                        }
                        this.composer.read(cx).focus_handle(cx).focus(window, cx);
                        cx.notify();
                    })),
            )
    }

    fn insert_attachment_token(
        &mut self,
        token: gpui_kit::component::input::InlineToken,
        insertion: std::ops::Range<usize>,
        revision: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer.update(cx, |input, cx| {
            if self.draft_revision == revision {
                input.set_selected_range(insertion, cx);
            } else {
                input.set_selected_range(input.value().len()..input.value().len(), cx);
            }
            if let Err(error) = input.replace_with_token(token, window, cx) {
                self.error = Some(error.to_string());
            }
            input.focus_handle(cx).focus(window, cx);
        });
        self.draft_revision = self.draft_revision.wrapping_add(1);
    }

    pub(super) fn render_attachments(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(
                self.attachment_imports
                    .iter()
                    .map(|(&id, import)| Self::render_attachment_import(id, import, cx)),
            )
            .child(
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
                                    .w(gpui_kit::rems(7.0))
                                    .h_20()
                                    .rounded_md()
                                    .overflow_hidden()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        Button::new(gpui_kit::SharedString::from(format!(
                                            "open-preview:{id}"
                                        )))
                                        .ghost()
                                        .p_0()
                                        .size_full()
                                        .accessibility_label(format!(
                                            "View {}",
                                            attachment.reference.name
                                        ))
                                        .child(
                                            gpui_kit::img(preview.thumbnail.clone())
                                                .size_full()
                                                .object_fit(gpui_kit::ObjectFit::Cover),
                                        )
                                        .on_click(
                                            cx.listener({
                                                let id = id.clone();
                                                move |this, _, window, cx| {
                                                    this.open_attachment(&id, window, cx);
                                                }
                                            }),
                                        ),
                                    )
                                    .child(
                                        div().absolute().top_0().right_0().occlude().child(
                                            Button::new(gpui_kit::SharedString::from(format!(
                                                "remove-preview:{id}"
                                            )))
                                            .debug_selector({
                                                let id = id.clone();
                                                move || format!("remove-preview:{id}")
                                            })
                                            .icon(IconName::Close)
                                            .xsmall()
                                            .ghost()
                                            .accessibility_label(format!(
                                                "Remove {}",
                                                attachment.reference.name
                                            ))
                                            .element_tooltip(format!(
                                                "Remove {}",
                                                attachment.reference.name
                                            ))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                cx.stop_propagation();
                                                this.remove_attachment(&id, window, cx);
                                            })),
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

    /// Removed chips keep their entries so undo can restore them; only chips in the draft count.
    fn attachment_slots_full(&self, cx: &App) -> bool {
        self.active_attachment_ids(cx)
            .len()
            .saturating_add(self.attachment_imports.len())
            >= 16
    }

    fn active_image_count(&self, cx: &App) -> usize {
        let active = self.active_attachment_ids(cx);
        self.attachments
            .iter()
            .filter(|attachment| {
                attachment.reference.kind == bootty_agents::NativeAttachmentKind::Image
                    && active.contains(&attachment.reference.id)
            })
            .count()
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
            .chain(
                self.record
                    .queued_messages
                    .iter()
                    .flat_map(|message| &message.attachments),
            )
            .filter(|reference| reference.kind == bootty_agents::NativeAttachmentKind::Image)
            .map(|reference| reference.id.clone())
            .collect::<BTreeSet<_>>();
        // A preview lives only while a message, queued draft, or the composer still shows it.
        let shown = |id: &String| {
            images.contains(id)
                || self
                    .attachments
                    .iter()
                    .any(|attachment| &attachment.reference.id == id)
        };
        self.attachment_previews.retain(|id, _| shown(id));
        self.attachment_preview_attempts.retain(|id| shown(id));
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
                // `None` means the preview couldn't be fetched, so a later update retries it; an
                // image that can't be decoded stays tried.
                let preview = cx
                    .background_executor()
                    .spawn(async move {
                        let CommandOutcome::Success { value, .. } = receiver.recv().ok()? else {
                            return None;
                        };
                        Some(preview_from_reply(&value))
                    })
                    .await;
                _ = owner.update(cx, |this, cx| {
                    if this.record.target() != target {
                        return;
                    }
                    let Some(preview) = preview else {
                        this.attachment_preview_attempts.remove(&id);
                        return;
                    };
                    if let Some(preview) = preview {
                        // Only the rows that show this image change height.
                        let has_image = |index: &usize| {
                            this.record
                                .snapshot
                                .transcript
                                .get(*index)
                                .is_some_and(|item| {
                                    item.attachments.iter().any(|reference| reference.id == id)
                                })
                        };
                        for (ix, row) in this.timeline.iter().enumerate() {
                            let changed = match row {
                                super::TranscriptRow::Message { index, .. } => has_image(index),
                                super::TranscriptRow::Work { members, .. } => {
                                    members.iter().any(has_image)
                                }
                                super::TranscriptRow::Failure { .. }
                                | super::TranscriptRow::Runway => false,
                            };
                            if changed {
                                this.list.remeasure_items(ix..ix.saturating_add(1));
                            }
                        }
                        this.attachment_previews.insert(id, preview);
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
        owner: &gpui_kit::WeakEntity<Self>,
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
                                let (menu_owner, menu_id) = (owner.clone(), reference.id.clone());
                                div()
                                    .id(gpui_kit::SharedString::from(format!(
                                        "attachment-menu:{}",
                                        reference.id
                                    )))
                                    .context_menu(move |menu, _, _| {
                                        Self::attachment_menu(&menu_owner, &menu_id, menu)
                                    })
                                    .child(
                                        Button::new(gpui_kit::SharedString::from(format!(
                                            "attachment-preview:{}",
                                            reference.id
                                        )))
                                        .ghost()
                                        .p_0()
                                        .accessibility_label(format!("View {}", reference.name))
                                        .on_click({
                                            let owner = owner.clone();
                                            let id = reference.id.clone();
                                            move |_, window, cx| {
                                                _ = owner.update(cx, |this, cx| {
                                                    this.view_sent_image(&id, window, cx);
                                                });
                                            }
                                        })
                                        .debug_selector({
                                            let id = reference.id.clone();
                                            move || format!("attachment-preview:{id}")
                                        })
                                        .w(gpui_kit::rems(7.0))
                                        .h_20()
                                        .rounded_md()
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            gpui_kit::img(preview.thumbnail.clone())
                                                .size_full()
                                                .object_fit(gpui_kit::ObjectFit::Cover),
                                        ),
                                    )
                            })
                        }),
                    ))
                },
            )
            // A preview already names its image; only attachments without one get a chip.
            .child(
                div().flex().flex_shrink_0().flex_wrap().gap_2().children(
                    item.attachments
                        .iter()
                        .filter(|reference| !self.attachment_previews.contains_key(&reference.id))
                        .map(|reference| Self::sent_file_chip(reference, owner, cx)),
                ),
            )
    }
}

impl NativeAgentSessionView {
    /// A sent file without a preview: its name and size. A click opens the stored copy in the app
    /// this computer uses; its menu also shows it in the file manager.
    fn sent_file_chip(
        reference: &bootty_agents::NativeAttachmentReference,
        owner: &gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> impl IntoElement {
        let (menu_owner, menu_id) = (owner.clone(), reference.id.clone());
        let (open_owner, open_id) = (owner.clone(), reference.id.clone());
        div()
            .id(gpui_kit::SharedString::from(format!(
                "sent-attachment:{}",
                reference.id
            )))
            .debug_selector({
                let id = reference.id.clone();
                move || format!("sent-attachment:{id}")
            })
            // A click opens the stored copy in the app this computer uses.
            .on_click(move |_, window, cx| {
                _ = open_owner.update(cx, |this, cx| {
                    this.submit_command("attachment-open", vec![open_id.clone()], None, window, cx);
                });
            })
            .cursor_pointer()
            .hover(|chip| chip.bg(cx.theme().muted.opacity(0.4)))
            .context_menu(move |menu, _, _| Self::attachment_menu(&menu_owner, &menu_id, menu))
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
    }

    /// Open and show in the file manager, for a sent attachment's stored copy.
    fn attachment_menu(
        owner: &gpui_kit::WeakEntity<Self>,
        id: &str,
        menu: gpui_kit::component::menu::PopupMenu,
    ) -> gpui_kit::component::menu::PopupMenu {
        let item = |label: &'static str, operation: &'static str| {
            let (owner, id) = (owner.clone(), id.to_owned());
            gpui_kit::component::menu::PopupMenuItem::new(label).on_click(move |_, window, cx| {
                _ = owner.update(cx, |this, cx| {
                    this.submit_command(operation, vec![id.clone()], None, window, cx);
                });
            })
        };
        menu.item(item("Open", "attachment-open"))
            .item(item(crate::commands::reveal_label(), "attachment-reveal"))
    }
}

/// The preview image an attachment-preview reply carries, within its byte bound.
fn preview_from_reply(value: &serde_json::Value) -> Option<ImagePreview> {
    use base64::Engine as _;
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
    ImagePreview::new(bytes.into())
}

fn too_many_images() -> String {
    format!(
        "Attach at most {} images",
        bootty_agents::MAX_NATIVE_PROMPT_IMAGES
    )
}
