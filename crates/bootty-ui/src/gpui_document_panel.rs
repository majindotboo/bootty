//! Host-bound documents. The host owns bytes; each editor owns a draft and its saved revision.

use crate::{
    gpui::{FileEditor, FileEditorEvent},
    gpui_git_panel::GitPanelContext,
};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_host::files::{
    FileResponse, FileSnapshot, can_format, decode_document, encode_document,
};
use bootty_host::{
    media::{MediaCancellation, MediaDescriptor, MediaKind, MediaReader},
    remote::RemoteHost,
};
use gpui_kit::component::{
    Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState, TabGroup},
    menu::{PopupMenu, PopupMenuItem},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Global, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, WeakEntity, Window, div, prelude::*,
};
use image::ImageDecoder as _;
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

gpui_kit::actions!(
    document,
    [
        #[derive(Eq)]
        CloseDocument,
        #[derive(Eq)]
        FormatDocument
    ]
);
pub struct DocumentClosed;

enum MediaPreview {
    Image(Arc<gpui_kit::RenderImage>),
    #[cfg(target_os = "macos")]
    Video(crate::gpui_video::Playback),
}

fn load_media(source: MediaReader, descriptor: &MediaDescriptor) -> anyhow::Result<MediaPreview> {
    match descriptor.kind {
        MediaKind::Image => decode_image(source).map(MediaPreview::Image),
        MediaKind::Video => {
            #[cfg(target_os = "macos")]
            {
                let content_type = if Path::new(&descriptor.name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("mov"))
                {
                    "com.apple.quicktime-movie"
                } else {
                    "public.mpeg-4"
                };
                crate::gpui_video::Playback::open(source, content_type).map(MediaPreview::Video)
            }
            #[cfg(not(target_os = "macos"))]
            anyhow::bail!("Video playback is not available on this platform")
        }
    }
}

enum ReadMode {
    Refresh,
    // Approval covers this draft, not edits made while the host read or decode is pending.
    Reload { draft: Option<String> },
}

fn decode_image(source: MediaReader) -> anyhow::Result<Arc<gpui_kit::RenderImage>> {
    let mut reader =
        image::ImageReader::new(std::io::BufReader::new(source)).with_guessed_format()?;
    // Keep decoded previews bounded; larger images need downsampling before allocation.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    anyhow::ensure!(
        u64::from(width).saturating_mul(u64::from(height)) <= 16 * 1024 * 1024,
        "image previews are limited to 16 megapixels"
    );
    // Preserve ImageReader::decode's allocation budget when accessing decoder metadata directly.
    limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(limits)?;
    let orientation = decoder.orientation()?;
    // Animated files show their first frame until the viewer owns bounded animation playback.
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let mut pixels = image.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2); // GPUI textures use BGRA.
    }
    Ok(Arc::new(gpui_kit::RenderImage::new([image::Frame::new(
        pixels,
    )])))
}

pub fn init(cx: &mut App) {
    cx.bind_keys([
        gpui_kit::KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-w"
            } else {
                "ctrl-w"
            },
            CloseDocument,
            Some("BoottyDocument"),
        ),
        gpui_kit::KeyBinding::new("shift-alt-f", FormatDocument, Some("BoottyDocument")),
    ]);
}

#[derive(Default)]
pub struct Documents(pub Vec<WeakEntity<DocumentPanel>>);
impl Global for Documents {}

impl Documents {
    pub(crate) fn pending(cx: &App) -> Vec<Entity<DocumentPanel>> {
        cx.try_global::<Self>()
            .map_or_else(Default::default, |documents| {
                documents
                    .0
                    .iter()
                    .filter_map(WeakEntity::upgrade)
                    .filter(|document| document.read(cx).needs_close_prompt(cx))
                    .collect()
            })
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Visibility, loading, preview and close requests vary independently"
)]
pub struct DocumentPanel {
    context: GitPanelContext,
    host_identity: String,
    path: String,
    sender: BoundAppCommandSender,
    focus: FocusHandle,
    editor: Option<Entity<FileEditor>>,
    image: Option<Arc<gpui_kit::RenderImage>>,
    #[cfg(target_os = "macos")]
    video: Option<Entity<crate::gpui_video::VideoPreview>>,
    media_load: Option<MediaCancellation>,
    digest: Option<String>,
    error: Option<String>,
    external: bool,
    pending: bool,
    formatting: bool,
    active: bool,
    host_visible: bool,
    watch: Option<bootty_host::file_watch::FileWatch>,
    preview: bool,
    line: u32,
    column: u32,
    close_prompt: bool,
    close_after_save: bool,
    pub(crate) group: Option<WeakEntity<TabGroup>>,
    subscriptions: Vec<Subscription>,
}

impl DocumentPanel {
    pub(crate) fn new(
        context: GitPanelContext,
        host_identity: String,
        path: String,
        sender: BoundAppCommandSender,
        position: (u32, u32),
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (line, column) = position;
        if cx.try_global::<Documents>().is_none() {
            cx.set_global(Documents::default());
        }
        cx.global_mut::<Documents>()
            .0
            .retain(|document| document.upgrade().is_some());
        let weak = cx.weak_entity();
        cx.global_mut::<Documents>().0.push(weak);
        let mut this = Self {
            context,
            host_identity,
            path,
            sender,
            focus: cx.focus_handle(),
            editor: None,
            image: None,
            #[cfg(target_os = "macos")]
            video: None,
            media_load: None,
            digest: None,
            error: None,
            external: false,
            pending: false,
            formatting: false,
            active: false,
            host_visible: true,
            watch: None,
            preview: false,
            line: line.saturating_sub(1),
            column: column.saturating_sub(1),
            close_prompt: false,
            close_after_save: false,
            group: None,
            subscriptions: Vec::new(),
        };
        this.watch_local_file(window, cx);
        Self::watch_document(window, cx);
        this.refresh(false, window, cx);
        this
    }

    fn watch_local_file(&self, window: &Window, cx: &Context<Self>) {
        if self.context.host_identity == "local" && self.host_identity == "local" {
            let watch_path = std::path::PathBuf::from(&self.path);
            cx.spawn_in(window, async move |weak, cx| {
                let watch = cx
                    .background_executor()
                    .spawn(async move { bootty_host::file_watch::FileWatch::new(&watch_path).ok() })
                    .await;
                _ = weak.update_in(cx, |this, _, _| this.watch = watch);
            })
            .detach();
        }
    }

    fn watch_document(window: &Window, cx: &Context<Self>) {
        cx.spawn_in(window, async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                if weak
                    .update_in(cx, |this, window, cx| {
                        this.poll_document(window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn poll_document(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.active
            && self.host_visible
            && self.group.is_some()
            && (self.error.is_some()
                || self
                    .watch
                    .as_ref()
                    .is_none_or(bootty_host::file_watch::FileWatch::take_changed))
        {
            self.refresh(false, window, cx);
        }
        let position = self
            .editor
            .as_ref()
            .map(|editor| editor.read(cx).cursor_position(cx));
        if let Some((line, column)) = position
            && (line, column) != (self.line, self.column)
        {
            self.line = line;
            self.column = column;
            cx.emit(PanelEvent::LayoutChanged);
        }
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }
    pub(crate) fn host_identity(&self) -> &str {
        &self.host_identity
    }
    pub(crate) fn needs_close_prompt(&self, cx: &App) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).is_dirty(cx) || editor.read(cx).save_in_flight())
    }
    pub(crate) fn saving(&self, cx: &App) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).save_in_flight())
    }
    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            editor.update(cx, FileEditor::request_save);
        }
    }

    fn can_format(&self) -> bool {
        can_format(Path::new(&self.path)) && self.editor.is_some() && !self.pending && !self.preview
    }

    fn format(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.can_format() {
            return;
        }
        let Some(editor) = &self.editor else { return };
        let source = editor.read(cx).contents(cx);
        let encoded = match encode_document(&source) {
            Ok(encoded) => encoded,
            Err(error) => {
                self.show_error("Could not format this file.", error, cx);
                return;
            }
        };
        let mut invocation = CommandInvocation::from_action("files.format", Caller::Internal);
        invocation.target = Some(self.context.target.clone());
        invocation.arguments = vec![self.path.clone(), encoded];
        let receiver = match self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_mins(2))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.show_error("Could not format this file.", format!("{error:?}"), cx);
                return;
            }
        };
        self.pending = true;
        self.formatting = true;
        self.error = None;
        cx.spawn_in(window, async move |weak, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, window, cx| {
                this.receive_format(outcome, &source, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn receive_format(
        &mut self,
        outcome: Result<CommandOutcome, std::sync::mpsc::RecvError>,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending = false;
        self.formatting = false;
        let result = match outcome {
            Ok(CommandOutcome::Success { value, .. }) => {
                serde_json::from_value::<FileResponse>(value)
                    .map_err(|error| error.to_string())
                    .and_then(|response| match response {
                        FileResponse::Formatted { content_base64 } => {
                            decode_document(&content_base64).map_err(|error| error.to_string())
                        }
                        _ => Err("Unexpected format response".to_owned()),
                    })
            }
            Ok(outcome) => Err(crate::commands::command_outcome_message(&outcome)
                .unwrap_or_else(|| "Format failed".to_owned())),
            Err(error) => Err(error.to_string()),
        };
        match result {
            Ok(formatted) => {
                if let Some(editor) = &self.editor
                    && !editor.update(cx, |editor, cx| {
                        editor.apply_format(source, formatted, window, cx)
                    })
                {
                    self.error = Some(crate::i18n::t(cx, "document-changed-during-format"));
                }
            }
            Err(error) => self.show_error("Could not format this file.", error, cx),
        }
        cx.notify();
    }
    pub(crate) fn go_to(
        &mut self,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.line = line.saturating_sub(1);
        self.column = column.saturating_sub(1);
        if let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| {
                editor.set_cursor_position(self.line, self.column, window, cx);
            });
        }
        // Setting the cursor focuses the source editor; restore the visible presentation last.
        self.focus_handle(cx).focus(window, cx);
        cx.emit(PanelEvent::LayoutChanged);
    }

    fn request(
        &mut self,
        command: &str,
        args: Vec<String>,
        saved: Option<String>,
        reload: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let read = if reload {
            ReadMode::Reload {
                draft: self.draft(cx),
            }
        } else {
            ReadMode::Refresh
        };
        let mut invocation = CommandInvocation::from_action(command, Caller::Internal);
        invocation.target = Some(self.context.target.clone());
        invocation.arguments = args;
        let receiver = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_mins(2))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        );
        let receiver = match receiver {
            Ok(receiver) => receiver,
            Err(error) => {
                self.fail(
                    format!("File command unavailable: {error:?}"),
                    saved.is_some(),
                    cx,
                );
                return;
            }
        };
        self.pending = true;
        cx.spawn_in(window, async move |weak, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, window, cx| {
                this.receive_outcome(outcome, saved, read, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn receive_outcome(
        &mut self,
        outcome: Result<CommandOutcome, std::sync::mpsc::RecvError>,
        saved: Option<String>,
        read: ReadMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending = false;
        let response = match outcome {
            Ok(CommandOutcome::Success { value, .. }) => {
                serde_json::from_value::<FileResponse>(value).map_err(|error| error.to_string())
            }
            Ok(outcome) => Err(crate::commands::command_outcome_message(&outcome)
                .unwrap_or_else(|| "File command failed".to_owned())),
            Err(error) => Err(error.to_string()),
        };
        match response {
            Ok(FileResponse::Document(snapshot)) => {
                self.receive(snapshot, &read, window, cx);
            }
            Ok(FileResponse::Media(descriptor)) => {
                self.receive_media(descriptor, read, window, cx);
            }
            Ok(FileResponse::Saved {
                digest,
                durability_warning,
            }) => {
                let Some(saved) = saved else {
                    self.fail("Unexpected save response".to_owned(), false, cx);
                    return;
                };
                self.digest = Some(digest);
                self.error = None;
                self.external = false;
                if let Some(editor) = &self.editor {
                    editor.update(cx, |editor, cx| {
                        editor.mark_saved(saved, durability_warning, cx);
                    });
                }
                if self.close_after_save {
                    self.close_after_save = false;
                    if self.editor.as_ref().is_some_and(|editor| {
                        !editor.read(cx).is_dirty(cx) && editor.read(cx).save_allows_close()
                    }) {
                        Self::close(cx);
                    }
                }
            }
            Ok(
                FileResponse::Directory(_)
                | FileResponse::Location { .. }
                | FileResponse::Formatted { .. },
            ) => self.fail(
                "Unexpected directory response".to_owned(),
                saved.is_some(),
                cx,
            ),
            Err(error) => self.fail(error, saved.is_some(), cx),
        }
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    fn show_error(
        &mut self,
        summary: &str,
        details: impl std::fmt::Display,
        cx: &mut Context<Self>,
    ) {
        eprintln!("File operation failed for {}: {details}", self.path);
        self.error = Some(summary.to_owned());
        cx.notify();
    }

    fn fail(&mut self, error: String, saving: bool, cx: &mut Context<Self>) {
        let summary = if saving {
            "Could not save this file. Your changes are still here."
        } else {
            "Could not open this file."
        };
        self.show_error(summary, error, cx);
        if saving && let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| editor.save_failed(summary.to_owned(), cx));
            self.error = None;
        }
        self.close_after_save = false;
    }

    fn refresh(&mut self, reload: bool, window: &Window, cx: &mut Context<Self>) {
        if self.host_identity != self.context.host_identity {
            self.error = Some(
                "This document belongs to a different host. Open it from that host’s Files panel."
                    .to_owned(),
            );
            return;
        }
        if !self.pending {
            self.request(
                "files.read",
                vec![self.path.clone()],
                None,
                reload,
                window,
                cx,
            );
        }
    }

    fn draft(&self, cx: &App) -> Option<String> {
        self.editor
            .as_ref()
            .map(|editor| editor.read(cx).contents(cx))
    }

    fn accepts_snapshot(&self, read: &ReadMode, cx: &App) -> bool {
        match read {
            ReadMode::Refresh => !self.needs_close_prompt(cx),
            ReadMode::Reload { draft } => self.draft(cx).as_ref() == draft.as_ref(),
        }
    }

    fn receive(
        &mut self,
        snapshot: FileSnapshot,
        read: &ReadMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(read, ReadMode::Refresh) && self.digest.as_ref() == Some(&snapshot.digest) {
            self.external = false;
            self.error = None;
            return;
        }
        let contents = match snapshot.contents() {
            Ok(contents) => contents,
            Err(error) => {
                self.fail(error.to_string(), false, cx);
                return;
            }
        };
        if !self.accepts_snapshot(read, cx) {
            self.external = true;
            return;
        }
        self.digest = Some(snapshot.digest);
        self.error = None;
        self.external = false;
        self.image = None;
        #[cfg(target_os = "macos")]
        {
            self.video = None;
        }
        if let Some(editor) = &self.editor {
            let (line, column) = editor.read(cx).cursor_position(cx);
            editor.update(cx, |editor, cx| {
                editor.replace_snapshot(contents, window, cx);
            });
            let focus = window.focused(cx);
            editor.update(cx, |editor, cx| {
                editor.set_cursor_position(line, column, window, cx);
            });
            if let Some(focus) = focus {
                focus.focus(window, cx);
            } else {
                window.blur(cx);
            }
            self.line = line;
            self.column = column;
        } else {
            self.create_editor(contents, window, cx);
        }
    }

    fn receive_media(
        &mut self,
        descriptor: MediaDescriptor,
        read: ReadMode,
        window: &Window,
        cx: &Context<Self>,
    ) {
        if matches!(read, ReadMode::Refresh) && self.digest.as_ref() == Some(&descriptor.revision) {
            self.external = false;
            self.error = None;
            return;
        }
        if !self.accepts_snapshot(&read, cx) {
            self.external = true;
            return;
        }
        let remote = self.context.remote.clone().map(RemoteHost::new);
        self.pending = true;
        cx.spawn_in(window, async move |weak, cx| {
            let opening = descriptor.clone();
            let source = cx
                .background_executor()
                .spawn(async move { MediaReader::open(&opening, remote.as_ref()) })
                .await;
            let result = match source {
                Ok(source) => {
                    let cancellation = source.cancellation();
                    let accepted = weak
                        .update_in(cx, |this, _, cx| {
                            if !this.accepts_snapshot(&read, cx) {
                                this.external = true;
                                this.pending = false;
                                cx.notify();
                                return false;
                            }
                            this.media_load = Some(cancellation);
                            true
                        })
                        .unwrap_or(false);
                    if !accepted {
                        return;
                    }
                    let media = descriptor.clone();
                    cx.background_executor()
                        .spawn(async move { load_media(source, &media) })
                        .await
                }
                Err(error) => Err(error),
            };
            _ = weak.update_in(cx, |this, window, cx| {
                this.media_load = None;
                this.pending = false;
                if !this.accepts_snapshot(&read, cx) {
                    this.external = true;
                    cx.notify();
                    return;
                }
                match result {
                    Ok(preview) => {
                        if this.focus_handle(cx).contains_focused(window, cx) {
                            this.focus.focus(window, cx);
                        }
                        this.editor = None;
                        this.image = None;
                        #[cfg(target_os = "macos")]
                        {
                            this.video = None;
                        }
                        match preview {
                            MediaPreview::Image(image) => this.image = Some(image),
                            #[cfg(target_os = "macos")]
                            MediaPreview::Video(playback) => {
                                this.video =
                                    Some(cx.new(|cx| {
                                        crate::gpui_video::VideoPreview::new(playback, cx)
                                    }));
                            }
                        }
                        this.digest = Some(descriptor.revision);
                        this.external = false;
                        this.error = None;
                    }
                    Err(error) => this.show_error(
                        if descriptor.kind == MediaKind::Video {
                            "Could not play this video."
                        } else {
                            "Could not open this image."
                        },
                        error,
                        cx,
                    ),
                }
                cx.emit(PanelEvent::LayoutChanged);
                cx.notify();
            });
        })
        .detach();
    }

    fn create_editor(&mut self, contents: String, window: &mut Window, cx: &mut Context<Self>) {
        let path = Path::new(&self.path);
        let editor = cx.new(|cx| FileEditor::new_for_path(contents, path, window, cx));
        let subscription = cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &FileEditorEvent, window, cx| {
                this.on_editor_event(event, window, cx);
            },
        );
        let observer = cx.observe(&editor, |_, _, cx| {
            cx.emit(PanelEvent::LayoutChanged);
            cx.notify();
        });
        self.subscriptions.extend([subscription, observer]);
        self.editor = Some(editor.clone());
        let focus = window.focused(cx);
        editor.update(cx, |editor, cx| {
            editor.set_cursor_position(self.line, self.column, window, cx);
        });
        if self.focus.is_focused(window) && self.active && !self.preview {
            let focus = editor.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        } else if !self.active || self.preview {
            if let Some(focus) = focus {
                focus.focus(window, cx);
            } else {
                window.blur(cx);
            }
        }
    }

    fn on_editor_event(
        &mut self,
        event: &FileEditorEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let FileEditorEvent::Save { contents } = event;
        if self.pending {
            self.fail(
                "A file operation is still running; retry Save when it finishes.".to_owned(),
                true,
                cx,
            );
            return;
        }
        let Some(digest) = self.digest.clone() else {
            self.fail(
                "Document revision unavailable; reload before saving.".to_owned(),
                true,
                cx,
            );
            return;
        };
        match encode_document(contents) {
            Ok(encoded) => self.request(
                "files.save",
                vec![self.path.clone(), digest, encoded],
                Some(contents.clone()),
                false,
                window,
                cx,
            ),
            Err(error) => self.fail(error.to_string(), true, cx),
        }
    }

    pub(crate) fn retain_subscription(&mut self, subscription: Subscription) {
        self.subscriptions.push(subscription);
    }

    pub(crate) fn set_host_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.host_visible = visible;
        if !visible {
            self.pause_video(cx);
        }
    }

    // Keep one lifecycle hook signature until video playback supports other platforms.
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            clippy::unused_self,
            clippy::missing_const_for_fn,
            clippy::needless_pass_by_ref_mut
        )
    )]
    fn pause_video(&self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(video) = &self.video {
            video.update(cx, crate::gpui_video::VideoPreview::pause);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = cx;
    }

    pub(crate) fn set_preview(&mut self, preview: bool) {
        self.preview = preview && self.is_markdown();
    }

    fn is_markdown(&self) -> bool {
        Path::new(&self.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
            })
    }

    fn request_reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.needs_close_prompt(cx) {
            self.refresh(true, window, cx);
            return;
        }
        if self.close_prompt || self.pending {
            return;
        }
        self.close_prompt = true;
        let answer = crate::gpui::prompt(
            crate::i18n::t(cx, "document-discard-reload").as_str(),
            Some(&self.path),
            &[
                gpui_kit::PromptButton::Other(crate::i18n::t(cx, "common-reload").into()),
                gpui_kit::PromptButton::Cancel(crate::i18n::t(cx, "common-cancel").into()),
            ],
            window,
            cx,
        );
        cx.spawn_in(window, async move |weak, cx| {
            let answer = answer.await;
            _ = weak.update_in(cx, |this, window, cx| {
                this.close_prompt = false;
                if matches!(answer, Ok(0)) {
                    this.refresh(true, window, cx);
                }
            });
        })
        .detach();
    }

    fn close(cx: &mut Context<Self>) {
        cx.emit(DocumentClosed);
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving(cx) || self.close_prompt {
            return;
        }
        if !self.needs_close_prompt(cx) {
            Self::close(cx);
            return;
        }
        self.close_prompt = true;
        let answer = crate::gpui::prompt(
            crate::i18n::t(cx, "document-save-before-close").as_str(),
            Some(&self.path),
            &[
                gpui_kit::PromptButton::Other(crate::i18n::t(cx, "common-save").into()),
                gpui_kit::PromptButton::Other(crate::i18n::t(cx, "common-discard").into()),
                gpui_kit::PromptButton::Cancel(crate::i18n::t(cx, "common-cancel").into()),
            ],
            window,
            cx,
        );
        cx.spawn_in(window, async move |weak, cx| {
            let answer = answer.await;
            _ = weak.update_in(cx, |this, _, cx| {
                this.close_prompt = false;
                match answer {
                    Ok(0) => {
                        this.close_after_save = true;
                        this.save(cx);
                    }
                    Ok(1) => {
                        // Explicit discard permits the Dock close while retaining no draft on disk.
                        if let Some(editor) = &this.editor {
                            let contents = editor.read(cx).contents(cx);
                            editor.update(cx, |editor, cx| editor.mark_saved(contents, None, cx));
                        }
                        Self::close(cx);
                    }
                    _ => {}
                }
            });
        })
        .detach();
    }
}

impl Drop for DocumentPanel {
    fn drop(&mut self) {
        if let Some(load) = &self.media_load {
            load.cancel();
        }
    }
}

impl EventEmitter<PanelEvent> for DocumentPanel {}
impl EventEmitter<DocumentClosed> for DocumentPanel {}
impl Focusable for DocumentPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.preview {
            return self.focus.clone();
        }
        self.editor.as_ref().map_or_else(
            || self.focus.clone(),
            |editor| editor.read(cx).focus_handle(cx),
        )
    }
}
impl BasePanel for DocumentPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.document"
    }
    fn closable(&self, _: &App) -> bool {
        // Dock has no asynchronous close veto. Route every close through the document
        // request until its panel contract supports waiting for save/discard/cancel.
        false
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.pause_video(cx);
        self.group = None;
        self.active = false;
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if !active {
            self.pause_video(cx);
        }
        if active {
            self.refresh(false, window, cx);
        }
    }
    fn dump(&self, cx: &App) -> PanelState {
        let (line, column) = self
            .editor
            .as_ref()
            .map_or((self.line, self.column), |editor| {
                editor.read(cx).cursor_position(cx)
            });
        PanelState {
            panel_name: self.panel_name().to_owned(),
            children: Vec::new(),
            info: PanelInfo::panel(
                serde_json::json!({"host":self.host_identity,"path":self.path,"line":line.saturating_add(1),"column":column.saturating_add(1),"preview":self.preview}),
            ),
        }
    }
}
impl Panel for DocumentPanel {
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        let name = self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path);
        Some(
            format!(
                "{name}{}",
                if self.needs_close_prompt(cx) {
                    " •"
                } else {
                    ""
                }
            )
            .into(),
        )
    }
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tab_name(cx).unwrap_or_default()
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn title_suffix(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        Some(
            Button::new("close-document")
                .icon(IconName::Close)
                .ghost()
                .xsmall()
                .size_4()
                .accessibility_label(crate::i18n::t(cx, "common-close"))
                .tooltip(crate::i18n::t(cx, "common-close"))
                .disabled(self.saving(cx))
                .on_click(cx.listener(|this, _, window, cx| this.request_close(window, cx))),
        )
    }
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let save = cx.weak_entity();
        let revert = save.clone();
        let format = save.clone();
        menu.item(
            PopupMenuItem::new(crate::i18n::t(cx, "common-save"))
                .disabled(self.pending || !self.needs_close_prompt(cx))
                .on_click(move |_, _, cx| {
                    _ = save.update(cx, Self::save);
                }),
        )
        .item(
            PopupMenuItem::new(crate::i18n::t(cx, "document-revert"))
                .disabled(self.pending)
                .on_click(move |_, window, cx| {
                    _ = revert.update(cx, |this, cx| this.request_reload(window, cx));
                }),
        )
        .item(
            PopupMenuItem::new(crate::i18n::t(cx, "document-format"))
                .disabled(!self.can_format())
                .on_click(move |_, window, cx| {
                    _ = format.update(cx, |this, cx| this.format(window, cx));
                }),
        )
    }
}
impl Render for DocumentPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div()
            .id("document-panel")
            .track_focus(&self.focus)
            .key_context("BoottyDocument")
            .on_action(cx.listener(|this, _: &CloseDocument, window, cx| {
                this.request_close(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &FormatDocument, window, cx| {
                this.format(window, cx);
                cx.stop_propagation();
            }))
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0();
        if let Some(error) = &self.error
            && self.has_content()
        {
            body = body.child(gpui_kit::component::alert::Alert::error(
                "document-error",
                error.clone(),
            ));
        }
        if self.external {
            body = body.child(self.external_change_banner(cx));
        }
        body.child(self.render_content(cx))
            .when(self.editor.is_some(), |body| {
                body.child(self.render_status(cx))
            })
    }
}

impl DocumentPanel {
    const fn has_content(&self) -> bool {
        let present = self.editor.is_some() || self.image.is_some();
        #[cfg(target_os = "macos")]
        let present = present || self.video.is_some();
        present
    }

    fn render_content(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        #[cfg(target_os = "macos")]
        if let Some(video) = &self.video {
            return video.clone().into_any_element();
        }
        if let Some(image) = &self.image {
            return div()
                .id("document-image")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .p_4()
                .child(
                    gpui_kit::img(image.clone())
                        .size_full()
                        .object_fit(gpui_kit::ObjectFit::Contain),
                )
                .into_any_element();
        }
        let Some(editor) = &self.editor else {
            return div()
                .flex_1()
                .min_h_0()
                .p_4()
                .flex()
                .flex_col()
                .items_start()
                .gap_2()
                .text_sm()
                .child(if self.pending {
                    "Opening…".to_owned()
                } else {
                    self.error
                        .clone()
                        .unwrap_or_else(|| "File unavailable".to_owned())
                })
                .when(!self.pending, |body| {
                    body.child(
                        Button::new("retry-file")
                            .small()
                            .ghost()
                            .label("Retry")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.refresh(false, window, cx)),
                            ),
                    )
                })
                .into_any_element();
        };
        if self.preview {
            div()
                .id("document-preview")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .child(
                    div().p_4().child(
                        gpui_kit::component::text::TextView::markdown(
                            "document-markdown",
                            editor.read(cx).contents(cx),
                        )
                        .selectable(true)
                        .scrollable(false),
                    ),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .child(editor.clone())
                .into_any_element()
        }
    }

    fn render_status(&self, cx: &Context<Self>) -> impl IntoElement {
        let position = self
            .editor
            .as_ref()
            .filter(|_| !self.preview)
            .map(|editor| editor.read(cx).cursor_position(cx));

        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .text_xs()
            .when(self.is_markdown() && self.editor.is_some(), |row| {
                row.child(
                    Button::new("preview-document")
                        .label(crate::i18n::t(
                            cx,
                            if self.preview {
                                "common-edit"
                            } else {
                                "common-preview"
                            },
                        ))
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.preview = !this.preview;
                            this.focus_handle(cx).focus(window, cx);
                            cx.emit(PanelEvent::LayoutChanged);
                            cx.notify();
                        })),
                )
            })
            .when(
                can_format(Path::new(&self.path)) && self.editor.is_some(),
                |row| {
                    row.child(
                        Button::new("format-document")
                            .label(crate::i18n::t(
                                cx,
                                if self.formatting {
                                    "document-formatting"
                                } else {
                                    "document-format-short"
                                },
                            ))
                            .ghost()
                            .small()
                            .disabled(!self.can_format())
                            .tooltip(crate::i18n::t(cx, "document-format-shortcut"))
                            .on_click(cx.listener(|this, _, window, cx| this.format(window, cx))),
                    )
                },
            )
            .when(self.editor.is_some() && !self.preview, |row| {
                row.child(crate::i18n::t(cx, "document-multi-cursor-hint"))
            })
            .when_some(position, |row, (line, column)| {
                row.child(format!(
                    "{}:{}",
                    line.saturating_add(1),
                    column.saturating_add(1)
                ))
            })
    }
}

impl DocumentPanel {
    fn external_change_banner(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .child(crate::i18n::t(cx, "document-external-change")),
            )
            .child(
                Button::new("revert-document")
                    .label(crate::i18n::t(cx, "document-revert"))
                    .small()
                    .ghost()
                    .flex_shrink_0()
                    .disabled(self.pending)
                    .on_click(cx.listener(|this, _, window, cx| this.request_reload(window, cx))),
            )
    }
}
