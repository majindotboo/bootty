#[cfg(target_os = "macos")]
use std::time::Instant;

use bootty_control::CommandOutcome;
#[cfg(target_os = "macos")]
use gpui_kit::component::{WindowExt as _, notification::Notification};

use super::*;
use crate::commands::BrowserRequest;

#[cfg(target_os = "macos")]
enum DocumentReceiver {
    Page(async_channel::Receiver<Result<String, bootty_browser::CredentialError>>),
    Annotation(
        async_channel::Receiver<
            Result<bootty_browser::AnnotationCaptureContext, bootty_browser::AnnotationError>,
        >,
        async_channel::Receiver<Result<String, bootty_browser::CredentialError>>,
    ),
}

#[cfg(target_os = "macos")]
#[derive(PartialEq)]
enum CaptureDocument {
    Page(String),
    Annotation {
        context: bootty_browser::AnnotationCaptureContext,
        document: String,
    },
}

pub(super) struct PendingCapture {
    request: BrowserRequest,
    #[cfg(target_os = "macos")]
    page: CapturePage,
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
struct CapturePage {
    id: u64,
    revisions: (u64, u64),
    address: String,
    window: bootty_control::CommandTarget,
    native_window: std::num::NonZeroU32,
    bounds: BrowserBounds,
    geometry: bootty_computer::HostCaptureRegion,
    annotation: Option<super::annotations::AnnotationCaptureIntent>,
}

impl BrowserPanel {
    pub(super) fn render_capture_action(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        Button::new("browser-screenshot")
            .icon(gpui_kit::assets::IconName::Camera)
            .ghost()
            .small()
            .size_6()
            // Other hosts need an exact-window capture backend before enabling this.
            .disabled(
                !cfg!(target_os = "macos")
                    || self.capture_request.is_some()
                    || self
                        .selected_tab()
                        .is_none_or(|tab| tab.view.is_none() || tab.loading),
            )
            .accessibility_label("Capture screenshot")
            .tooltip("Capture screenshot")
            .on_click(cx.listener(|this, _, window, cx| {
                this.submit("browser.capture", Vec::new(), window, cx);
            }))
            .into_any_element()
    }

    pub(super) fn invalidate_capture(&self) {
        if let Some(pending) = &self.capture_request {
            pending.request.cancel_capture();
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[allow(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "Keep the capture dispatch interface shared while this host lacks exact-window capture"
    )]
    pub(crate) fn capture_request(
        &mut self,
        request: BrowserRequest,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        request.complete(CommandOutcome::Unsupported {
            message: "Browser page capture currently requires macOS".into(),
        });
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn capture_request(
        &mut self,
        request: BrowserRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((native_window, directory, access, target)) = request.capture_context() else {
            request.complete(CommandOutcome::Denied {
                message: "Browser capture requires its host-issued capture policy".into(),
            });
            return;
        };
        if self.window_target.as_ref() != Some(target) {
            request.complete(stale_capture());
            return;
        }
        let directory = directory.to_owned();
        let prepared = self.prepare_capture(&request, native_window, window);
        let (page, initial_document) = match prepared {
            Ok(prepared) => prepared,
            Err(outcome) => {
                request.complete(outcome);
                return;
            }
        };
        self.capture_request = Some(PendingCapture {
            request: request.clone(),
            page: page.clone(),
        });
        cx.notify();
        cx.spawn_in(window, async move |owner, cx| {
            let outcome = capture_candidate(
                &owner,
                cx,
                &request,
                &page,
                initial_document,
                directory,
                access,
            )
            .await
            .unwrap_or_else(|outcome| outcome);
            _ = cx.update(|window, cx| {
                owner.update(cx, |this, cx| {
                    this.finish_capture(&request, &outcome, window, cx);
                })
            });
            request.complete(outcome);
        })
        .detach();
    }

    #[cfg(target_os = "macos")]
    fn prepare_capture(
        &self,
        request: &BrowserRequest,
        native_window: std::num::NonZeroU32,
        window: &mut Window,
    ) -> Result<(CapturePage, DocumentReceiver), CommandOutcome> {
        if self.capture_request.is_some()
            || !self.visible
            || !self.active
            || self.resetting
            || self.confirm_reset
        {
            return Err(stale_capture());
        }
        let tab = self
            .selected_tab()
            .filter(|tab| !tab.loading && Some(tab.id) == request.page)
            .ok_or_else(stale_capture)?;
        let view = tab.view.as_ref().ok_or_else(stale_capture)?;
        let bounds = view.capture_bounds().map_err(|_| stale_capture())?;
        let (observed, geometry) = crate::window::native_browser_capture_region(window, bounds)
            .ok_or_else(stale_capture)?;
        if observed != native_window {
            return Err(stale_capture());
        }
        let address = view.current_address().map_err(|_| stale_capture())?;
        if address.len() > 8192 {
            return Err(stale_capture());
        }
        let annotation = match request.action {
            crate::commands::BrowserAction::CaptureAnnotation(id) => {
                let intent = self
                    .annotation_capture_intent(id)
                    .ok_or_else(stale_capture)?;
                if intent.record.page != tab.id
                    || intent.window != *self.window_target.as_ref().ok_or_else(stale_capture)?
                {
                    return Err(stale_capture());
                }
                Some(intent)
            }
            _ => None,
        };
        let page = CapturePage {
            id: tab.id,
            revisions: (tab.load_revision, tab.view_revision),
            address,
            window: self.window_target.clone().ok_or_else(stale_capture)?,
            native_window,
            bounds,
            geometry,
            annotation,
        };
        let document = page.document_receiver(view, true)?;
        Ok((page, document))
    }

    #[cfg(target_os = "macos")]
    fn finish_capture(
        &mut self,
        request: &BrowserRequest,
        outcome: &CommandOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .capture_request
            .as_ref()
            .is_some_and(|pending| pending.request == *request)
        {
            if let Some(intent) = self
                .capture_request
                .as_ref()
                .and_then(|pending| pending.page.annotation.as_ref())
                .cloned()
                && !matches!(outcome, CommandOutcome::Success { .. })
            {
                let message = crate::commands::command_outcome_message(outcome);
                self.finish_annotation_capture_ui(&intent, message.as_deref());
            }
            self.capture_request = None;
            match outcome {
                CommandOutcome::Success { value, .. } => {
                    if let Some(path) = value.get("path").and_then(serde_json::Value::as_str) {
                        window.push_notification(
                            Notification::success(bootty_git::project::display_path(
                                path,
                                crate::strings::home_dir().as_deref(),
                            ))
                            .title("Screenshot saved"),
                            cx,
                        );
                    }
                }
                outcome => {
                    if let Some(message) = crate::commands::command_outcome_message(outcome) {
                        window.push_notification(
                            Notification::error(
                                crate::error_catalog::ErrorNotice::from_text(message).to_string(),
                            )
                            .title("Capture failed"),
                            cx,
                        );
                    }
                }
            }
            cx.notify();
        }
    }

    #[cfg(target_os = "macos")]
    fn capture_current(
        &self,
        request: &BrowserRequest,
        page: &CapturePage,
        window: &mut Window,
    ) -> bool {
        !request.capture_cancelled()
            && request
                .capture_deadline()
                .is_some_and(|deadline| Instant::now() < deadline)
            && self.visible
            && self.active
            && !self.resetting
            && !self.confirm_reset
            && self.window_target.as_ref() == Some(&page.window)
            && page.annotation.as_ref().is_none_or(|captured| {
                self.annotation_capture_intent(captured.id)
                    .is_some_and(|current| {
                        current.record == captured.record
                            && current.recipient == captured.recipient
                            && current.window == captured.window
                    })
            })
            && self
                .capture_request
                .as_ref()
                .is_some_and(|pending| pending.request == *request)
            && self.selected_tab().is_some_and(|tab| {
                tab.id == page.id
                    && !tab.loading
                    && (tab.load_revision, tab.view_revision) == page.revisions
                    && tab.view.as_ref().is_some_and(|view| {
                        view.capture_bounds().ok() == Some(page.bounds)
                            && view.current_address().ok().as_ref() == Some(&page.address)
                    })
            })
            && crate::window::native_browser_capture_region(window, page.bounds).is_some_and(
                |(native, geometry)| native == page.native_window && geometry == page.geometry,
            )
    }

    #[cfg(target_os = "macos")]
    pub(super) fn invalidate_capture_geometry(&self) {
        if let Some(pending) = &self.capture_request
            && self
                .selected_tab()
                .and_then(|tab| tab.view.as_ref())
                .and_then(|view| view.capture_bounds().ok())
                != Some(pending.page.bounds)
        {
            self.invalidate_capture();
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(
    clippy::future_not_send,
    reason = "GPUI spawn_in runs this on its UI executor with a thread-local AsyncWindowContext; only native capture and publication enter background_executor"
)]
async fn capture_candidate(
    owner: &gpui_kit::WeakEntity<BrowserPanel>,
    cx: &mut gpui_kit::AsyncWindowContext,
    request: &BrowserRequest,
    page: &CapturePage,
    initial_document: DocumentReceiver,
    directory: std::path::PathBuf,
    access: bootty_computer::ComputerAccess,
) -> Result<CommandOutcome, CommandOutcome> {
    let deadline = request.capture_deadline().unwrap_or_else(Instant::now);
    let document = document_token(initial_document, deadline, cx.background_executor()).await?;
    let current = cx
        .update(|window, cx| {
            owner.update(cx, |this, _| this.capture_current(request, page, window))
        })
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false);
    if !current {
        return Err(stale_capture());
    }
    let native_window = page.native_window;
    let geometry = capture_geometry(page, &document)?;
    let candidate = cx
        .background_executor()
        .spawn(async move {
            crate::commands::runtime::computer::browser_capture_candidate(
                native_window,
                &geometry,
                access,
            )
        })
        .await
        .map_err(crate::commands::runtime::computer::computer_error_outcome)?;
    let final_document = cx
        .update(|window, cx| {
            owner.update(cx, |this, _| {
                if !this.capture_current(request, page, window) {
                    return Err(stale_capture());
                }
                let view = this
                    .selected_tab()
                    .and_then(|tab| tab.view.as_ref())
                    .ok_or_else(stale_capture)?;
                page.document_receiver(view, false)
            })
        })
        .ok()
        .and_then(Result::ok)
        .ok_or_else(stale_capture)??;
    let final_document = document_token(final_document, deadline, cx.background_executor()).await?;
    if document != final_document {
        return Err(stale_capture());
    }
    cx.update(|window, cx| {
        owner.update(cx, |this, _| {
            if !this.capture_current(request, page, window) {
                return Err(stale_capture());
            }
            // This is the publication boundary. Later navigation cannot roll back a save.
            request
                .begin()
                .map_err(crate::commands::runtime::command_outcome_for_mux_error)
        })
    })
    .ok()
    .and_then(Result::ok)
    .ok_or_else(stale_capture)??;
    publish_capture(owner, cx, page, document, candidate, directory).await
}

#[cfg(target_os = "macos")]
fn stale_capture() -> CommandOutcome {
    CommandOutcome::StaleTarget {
        message: "The browser page changed before its screenshot could be saved.".into(),
    }
}

#[cfg(target_os = "macos")]
async fn document_token(
    receiver: DocumentReceiver,
    deadline: Instant,
    executor: &gpui_kit::BackgroundExecutor,
) -> Result<CaptureDocument, CommandOutcome> {
    let receive = async {
        match receiver {
            DocumentReceiver::Page(receiver) => receiver
                .recv()
                .await
                .ok()
                .and_then(Result::ok)
                .map(CaptureDocument::Page),
            DocumentReceiver::Annotation(receiver, document) => {
                let context = receiver.recv().await.ok().and_then(Result::ok)?;
                context.validate().ok()?;
                let document = document.recv().await.ok().and_then(Result::ok)?;
                if context.document != document {
                    return None;
                }
                Some(CaptureDocument::Annotation { context, document })
            }
        }
    };
    match futures::future::select(
        Box::pin(receive),
        Box::pin(executor.timer(deadline.saturating_duration_since(Instant::now()))),
    )
    .await
    {
        futures::future::Either::Left((Some(token), _)) => Ok(token),
        futures::future::Either::Left(_) => Err(stale_capture()),
        futures::future::Either::Right(_) => Err(CommandOutcome::deadline_exceeded()),
    }
}

#[cfg(target_os = "macos")]
impl CapturePage {
    fn document_receiver(
        &self,
        view: &bootty_browser::BrowserView,
        prepare: bool,
    ) -> Result<DocumentReceiver, CommandOutcome> {
        if let Some(intent) = &self.annotation {
            let receiver = if prepare {
                view.prepare_annotation_capture(intent.record.id, intent.id)
            } else {
                view.annotation_capture_context(intent.record.id, intent.id)
            }
            .map_err(|_| stale_capture())?;
            let document = view
                .capture_credential_document()
                .map_err(|_| stale_capture())?;
            Ok(DocumentReceiver::Annotation(receiver, document))
        } else {
            view.capture_credential_document()
                .map(DocumentReceiver::Page)
                .map_err(|_| stale_capture())
        }
    }
}

#[cfg(target_os = "macos")]
fn capture_geometry(
    page: &CapturePage,
    document: &CaptureDocument,
) -> Result<bootty_computer::HostCaptureRegion, CommandOutcome> {
    let CaptureDocument::Annotation { context, .. } = document else {
        return Ok(page.geometry.clone());
    };
    let intent = page.annotation.as_ref().ok_or_else(stale_capture)?;
    context
        .validate_selection(intent.record.anchor.selection.as_ref())
        .map_err(|_| stale_capture())?;
    let crop = annotation_crop(context)?;
    let viewport = context.viewport;
    let rect = &page.geometry.rect;
    let geometry = bootty_computer::HostCaptureRegion {
        frame_width: page.geometry.frame_width,
        frame_height: page.geometry.frame_height,
        rect: bootty_computer::DisplayBounds {
            x: rect.x + (crop.x - viewport.x) * rect.width / viewport.width,
            y: rect.y + (crop.y - viewport.y) * rect.height / viewport.height,
            width: crop.width * rect.width / viewport.width,
            height: crop.height * rect.height / viewport.height,
        },
    };
    geometry.validate().map_err(|_| stale_capture())?;
    Ok(geometry)
}

#[cfg(target_os = "macos")]
fn annotation_crop(
    context: &bootty_browser::AnnotationCaptureContext,
) -> Result<bootty_browser::AnnotationRect, CommandOutcome> {
    context.validate().map_err(|_| stale_capture())?;
    let selection = context.selection;
    bootty_browser::AnnotationRect {
        x: selection.x - 16.0,
        y: selection.y - 16.0,
        width: selection.width + 32.0,
        height: selection.height + 32.0,
    }
    .intersection(&context.viewport)
    .ok_or_else(stale_capture)
}

#[cfg(target_os = "macos")]
#[allow(
    clippy::future_not_send,
    reason = "This continuation stays on GPUI's UI executor; only image-store I/O enters background_executor"
)]
async fn publish_capture(
    owner: &gpui_kit::WeakEntity<BrowserPanel>,
    cx: &mut gpui_kit::AsyncWindowContext,
    page: &CapturePage,
    document: CaptureDocument,
    candidate: bootty_computer::ComputerResult,
    directory: std::path::PathBuf,
) -> Result<CommandOutcome, CommandOutcome> {
    if page.annotation.is_none() {
        return Ok(cx
            .background_executor()
            .spawn(async move {
                crate::commands::runtime::computer::publish_browser_capture(candidate, &directory)
            })
            .await);
    }
    let (Some(intent), CaptureDocument::Annotation { context, .. }) = (&page.annotation, document)
    else {
        return Err(stale_capture());
    };
    let intent = intent.clone();
    let store = intent.store.clone();
    let result = cx
        .background_executor()
        .spawn(async move {
            let geometry = annotation_image_geometry(&candidate, &context)?;
            let snapshot = bootty_computer::ComputerResultSnapshot::try_from(candidate)
                .map_err(|error| error.to_string())?;
            store
                .commit_image(snapshot.png(), &geometry)
                .map_err(|error| error.to_string())
        })
        .await;
    let completion = cx
        .update(|window, cx| {
            owner.update(cx, |this, cx| {
                this.annotation_capture_committed(intent, result, window, cx)
            })
        })
        .ok()
        .and_then(Result::ok)
        .ok_or_else(stale_capture)?;
    // Publication was admitted. Report the observed association, even if the request deadline passed.
    match completion.recv().await {
        Ok(Ok(())) => Ok(CommandOutcome::Success {
            value: serde_json::json!({"attached":true}),
            warnings: Vec::new(),
        }),
        Ok(Err(message)) => Ok(CommandOutcome::Failed {
            code: "annotation_capture_failed".into(),
            message,
        }),
        Err(_) => Err(stale_capture()),
    }
}

#[cfg(target_os = "macos")]
fn annotation_image_geometry(
    candidate: &bootty_computer::ComputerResult,
    context: &bootty_browser::AnnotationCaptureContext,
) -> Result<bootty_browser::AnnotationImageGeometry, String> {
    let bootty_computer::ComputerResult::Snapshot {
        region: Some(source),
        requested_region: Some(requested),
        pixel_width,
        pixel_height,
        ..
    } = candidate
    else {
        return Err("Annotation capture source geometry is unavailable".into());
    };
    let rect = |value: &bootty_computer::DisplayBounds| bootty_browser::AnnotationRect {
        x: value.x,
        y: value.y,
        width: value.width,
        height: value.height,
    };
    let requested_crop =
        annotation_crop(context).map_err(|_| "Annotation capture geometry is invalid")?;
    let crop = bootty_browser::AnnotationRect {
        x: requested_crop.x + (source.x - requested.x) * requested_crop.width / requested.width,
        y: requested_crop.y + (source.y - requested.y) * requested_crop.height / requested.height,
        width: source.width * requested_crop.width / requested.width,
        height: source.height * requested_crop.height / requested.height,
    };
    Ok(bootty_browser::AnnotationImageGeometry {
        viewport: context.viewport,
        selection: context.selection,
        crop,
        requested_source: rect(requested),
        source: rect(source),
        pixel_width: *pixel_width,
        pixel_height: *pixel_height,
    })
}
