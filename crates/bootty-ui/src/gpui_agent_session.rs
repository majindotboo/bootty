//! Native conversation presentation; provider processes and transcripts belong to bootty-agents.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    ops::Mul as _,
    sync::Arc,
    time::{Duration, Instant},
};

use bootty_agents::{
    AgentKind, NativeSessionRecord, NativeSessionSnapshot, NativeSessionStatus,
    NativeTranscriptItem,
};
use bootty_browser::{Annotation, annotation_batch};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
    CommandTarget,
};
use gpui_kit::component::{
    ActiveTheme as _, Colorize as _, Disableable as _, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    collapsible::Collapsible,
    input::{InputEvent, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    text::{TextView, TextViewState},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ListAlignment,
    ListState, ParentElement, Render, Styled, Subscription, Window, div, list, prelude::*,
};
use image::{ImageFormat, ImageReader};
use serde_json::Value;

#[path = "gpui_agent_completion.rs"]
mod completion;

#[path = "gpui_agent_attachments.rs"]
mod attachments;
use attachments::ComposerAttachment;
pub use attachments::{attachment_preview, clipboard_png};
#[path = "gpui_agent_models.rs"]
mod models;
pub use models::reasoning_label;

#[path = "gpui_agent_citations.rs"]
mod citations;
pub use citations::init_reaction_keys;
use citations::{ResponseCitation, ResponseSelection};
#[path = "gpui_agent_timeline.rs"]
mod timeline;
use timeline::TranscriptRow;
#[path = "gpui_agent_requests.rs"]
mod requests;
use requests::{is_approval, is_claude_question};

#[derive(Clone, Debug, Eq, PartialEq)]
struct AnnotationPreviewKey {
    annotation_id: u64,
    image_id: String,
    destination: String,
    generation: u64,
}

enum AnnotationPreview {
    Loading(AnnotationPreviewKey),
    Ready {
        key: AnnotationPreviewKey,
        image: Arc<gpui_kit::RenderImage>,
    },
    Unavailable(AnnotationPreviewKey),
}

impl AnnotationPreview {
    const fn key(&self) -> &AnnotationPreviewKey {
        match self {
            Self::Loading(key) | Self::Ready { key, .. } | Self::Unavailable(key) => key,
        }
    }
}

fn annotation_preview_key(
    annotation: &Annotation,
    target: &CommandTarget,
) -> Option<AnnotationPreviewKey> {
    Some(AnnotationPreviewKey {
        annotation_id: annotation.id,
        image_id: annotation.image.as_ref()?.id.clone(),
        destination: target.handle.clone(),
        generation: target.generation,
    })
}

fn decode_annotation_preview(bytes: &[u8]) -> Option<Arc<gpui_kit::RenderImage>> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1600);
    limits.max_image_height = Some(1600);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let mut pixels = reader.decode().ok()?.thumbnail(128, 80).into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(gpui_kit::RenderImage::new([image::Frame::new(
        pixels,
    )])))
}

#[derive(Clone)]
pub enum OpenNativeSession {
    SideChatCreated {
        source: CommandTarget,
        record: Box<NativeSessionRecord>,
    },
    Related {
        target: CommandTarget,
    },
    DetachAnnotation {
        target: CommandTarget,
        annotation: Box<Annotation>,
    },
    SentAnnotations {
        target: CommandTarget,
        annotations: Vec<Annotation>,
    },
}

struct SubmittedPrompt {
    text: String,
    revision: u64,
    annotations: Vec<Annotation>,
    citations: Vec<ResponseCitation>,
    attachments: Vec<String>,
}

enum ToolDiffs {
    Loading,
    Ready(Vec<(String, Entity<crate::gpui_git_panel::GitDiffPanel>, f32)>),
    Failed(String),
}

struct WorkingIndicator {
    tick: Option<gpui_kit::Task<()>>,
    animate: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderAvailability {
    Disabled,
    Enabled,
    Resuming,
}

pub struct NativeAgentSessionView {
    record: NativeSessionRecord,
    related: Vec<RelatedConversation>,
    focus: FocusHandle,
    effort_focus: FocusHandle,
    permissions_focus: FocusHandle,
    sender: BoundAppCommandSender,
    composer: Entity<TextareaState>,
    completion: Option<Entity<crate::gpui_composer_completion::ComposerCompletion>>,
    completion_subscriptions: Vec<Subscription>,
    draft_revision: u64,
    applications: Vec<bootty_agents::NativeApplicationMention>,
    attachments: Vec<ComposerAttachment>,
    attachment_imports: usize,
    attachment_previews: BTreeMap<String, Arc<gpui_kit::RenderImage>>,
    attachment_preview_attempts: BTreeSet<String>,
    models: Option<Vec<bootty_agents::NativeModelOption>>,
    model_picker: Option<Entity<models::ModelPickerState>>,
    model_picker_subscription: Option<Subscription>,
    annotations: Vec<Annotation>,
    expanded_annotation: Option<u64>,
    annotation_preview: Option<AnnotationPreview>,
    transcript: BTreeMap<String, Entity<TextViewState>>,
    tool_diffs: BTreeMap<String, ToolDiffs>,
    subagent_details: BTreeMap<String, Vec<Entity<TextViewState>>>,
    transcript_subscriptions: BTreeMap<String, Vec<Subscription>>,
    message_focus: BTreeMap<String, FocusHandle>,
    expanded_messages: BTreeSet<String>,
    copied_message: Option<String>,
    copy_feedback: Option<gpui_kit::Task<()>>,
    citations: Vec<ResponseCitation>,
    selected_response: Option<ResponseSelection>,
    reaction_menu: Option<(Entity<gpui_kit::component::menu::PopupMenu>, Subscription)>,
    citation_editor: Entity<TextareaState>,
    editing_citation: Option<(String, gpui_kit::Point<gpui_kit::Pixels>)>,
    timeline: Vec<TranscriptRow>,
    history_turns: Vec<(usize, String, gpui_kit::SharedString)>,
    history_page: Option<bootty_agents::NativeHistoryPage>,
    history_live_snapshot: Option<NativeSessionSnapshot>,
    history_loaded: bool,
    history_scroll: gpui_kit::ScrollHandle,
    hovered_turn: Option<usize>,
    work_disclosures: BTreeMap<String, bool>,
    working_indicator: WorkingIndicator,
    tool_disclosures: BTreeMap<String, bool>,
    list: ListState,
    availability: ProviderAvailability,
    requests: Entity<requests::NativeRequestView>,
    pending: BTreeSet<String>,
    cancellation_requested: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
pub struct RelatedConversation {
    pub target: CommandTarget,
    pub title: String,
    pub model: Option<String>,
    pub status: NativeSessionStatus,
}

impl NativeAgentSessionView {
    #[expect(
        clippy::too_many_lines,
        reason = "Construct the retained conversation view and its subscriptions together"
    )]
    pub fn new(
        record: NativeSessionRecord,
        sender: BoundAppCommandSender,
        provider_enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .submit_on_enter(true)
                .placeholder(format!(
                    "Message {}…",
                    provider_name(record.config.provider)
                ))
        });
        let subscription = cx.subscribe_in(&composer, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                this.draft_revision = this.draft_revision.wrapping_add(1);
            }
            if matches!(event, InputEvent::PressEnter { shift: false, .. })
                && !this
                    .completion
                    .as_ref()
                    .is_some_and(|completion| completion.read(cx).blocks_submission(window, cx))
            {
                this.send_prompt(window, cx);
            }
            cx.notify();
        });
        let citation_editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 4)
                .submit_on_enter(true)
                .placeholder("Add a comment…")
        });
        let citation_subscription =
            cx.subscribe_in(&citation_editor, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    this.save_citation_comment(window, cx);
                }
                cx.notify();
            });
        let requests = cx.new(|_| requests::NativeRequestView::new(record.target()));
        let request_subscription = cx.subscribe_in(
            &requests,
            window,
            |this, _, response: &requests::NativeRequestResponse, window, cx| {
                this.request_command(
                    &response.target,
                    response.operation.id(),
                    response.arguments.clone(),
                    window,
                    cx,
                );
                this.sync_requests(window, cx);
            },
        );
        let mut this = Self {
            record,
            related: Vec::new(),
            focus: cx.focus_handle(),
            effort_focus: cx.focus_handle(),
            permissions_focus: cx.focus_handle(),
            sender,
            composer,
            completion: None,
            completion_subscriptions: Vec::new(),
            draft_revision: 0,
            applications: Vec::new(),
            attachments: Vec::new(),
            attachment_imports: 0,
            attachment_previews: BTreeMap::new(),
            attachment_preview_attempts: BTreeSet::new(),
            models: None,
            model_picker: None,
            model_picker_subscription: None,
            annotations: Vec::new(),
            expanded_annotation: None,
            annotation_preview: None,
            transcript: BTreeMap::new(),
            tool_diffs: BTreeMap::new(),
            subagent_details: BTreeMap::new(),
            transcript_subscriptions: BTreeMap::new(),
            message_focus: BTreeMap::new(),
            expanded_messages: BTreeSet::new(),
            copied_message: None,
            copy_feedback: None,
            citations: Vec::new(),
            selected_response: None,
            reaction_menu: None,
            citation_editor,
            editing_citation: None,
            timeline: Vec::new(),
            history_turns: Vec::new(),
            history_page: None,
            history_live_snapshot: None,
            history_loaded: false,
            history_scroll: gpui_kit::ScrollHandle::new(),
            hovered_turn: None,
            work_disclosures: BTreeMap::new(),
            working_indicator: WorkingIndicator {
                tick: None,
                animate: true,
            },
            tool_disclosures: BTreeMap::new(),
            // Tail following handles long histories; short histories keep a nonnegative
            // top anchor so their first paint does not depend on a scroll event.
            // Overdraw is the virtualized viewport boundary, not product spacing.
            list: ListState::new(0, ListAlignment::Top, gpui_kit::px(256.)),
            availability: if provider_enabled {
                ProviderAvailability::Enabled
            } else {
                ProviderAvailability::Disabled
            },
            requests,
            pending: BTreeSet::new(),
            cancellation_requested: false,
            error: None,
            _subscriptions: vec![subscription, citation_subscription, request_subscription],
        };
        this.restore_initial_message(window, cx);
        this.configure_timeline(cx);
        this.sync_transcript(&[], window, cx);
        this.sync_requests(window, cx);
        this.load_attachment_previews(window, cx);
        this
    }

    fn configure_timeline(&self, cx: &Context<Self>) {
        self.list.set_follow_mode(gpui_kit::FollowMode::Tail);
        let owner = cx.weak_entity();
        self.list.set_scroll_handler(move |_, _, cx| {
            _ = owner.update(cx, |_, cx| cx.notify());
        });
    }

    const fn provider_enabled(&self) -> bool {
        !matches!(self.availability, ProviderAvailability::Disabled)
    }
    const fn resuming(&self) -> bool {
        matches!(self.availability, ProviderAvailability::Resuming)
    }

    pub(crate) fn set_availability(
        &mut self,
        enabled: bool,
        resuming: bool,
        cx: &mut Context<Self>,
    ) {
        let availability = if !enabled {
            ProviderAvailability::Disabled
        } else if resuming {
            ProviderAvailability::Resuming
        } else {
            ProviderAvailability::Enabled
        };
        if self.availability != availability {
            self.availability = availability;
            self.requests
                .update(cx, |requests, cx| requests.set_enabled(enabled, cx));
            cx.notify();
        }
    }

    /// Publish the committed draft projection; durable annotation notes stay browser-owned.
    pub(crate) fn set_annotations(
        &mut self,
        target: &CommandTarget,
        annotations: Vec<Annotation>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if *target != self.record.target() {
            return Err("Conversation target is stale".to_owned());
        }
        for annotation in &annotations {
            annotation.validate().map_err(|error| error.to_string())?;
            if !annotation.is_attached_to(&self.record.id) || annotation.note.trim().is_empty() {
                return Err("Annotation does not belong to this conversation draft".to_owned());
            }
        }
        if self.annotations != annotations {
            if self
                .expanded_annotation
                .is_some_and(|id| !annotations.iter().any(|annotation| annotation.id == id))
            {
                self.expanded_annotation = None;
            }
            self.annotations = annotations;
            let target = self.record.target();
            if self.annotation_preview.as_ref().is_some_and(|preview| {
                !self.annotations.iter().any(|annotation| {
                    annotation_preview_key(annotation, &target).as_ref() == Some(preview.key())
                })
            }) {
                self.annotation_preview = None;
            }
            cx.notify();
        }
        Ok(())
    }

    fn load_annotation_preview(&mut self, annotation_id: u64, window: &Window, cx: &Context<Self>) {
        if self.expanded_annotation != Some(annotation_id) {
            return;
        }
        let Some(annotation) = self
            .annotations
            .iter()
            .find(|annotation| annotation.id == annotation_id)
            .cloned()
        else {
            return;
        };
        let target = self.record.target();
        let Some(key) = annotation_preview_key(&annotation, &target) else {
            return;
        };
        if self
            .annotation_preview
            .as_ref()
            .is_some_and(|preview| preview.key() == &key)
        {
            return;
        }
        self.annotation_preview = Some(AnnotationPreview::Loading(key.clone()));
        let store = bootty_browser::AnnotationStore::new(
            &crate::gpui_workspace::browser_profile_directory(),
        );
        let conversation = target.handle.clone();
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let bytes = store
                        .load_image(&annotation, &conversation)
                        .map_err(|_| ())?;
                    decode_annotation_preview(&bytes).ok_or(())
                })
                .await;
            _ = owner.update(cx, |this, cx| {
                if !matches!(
                    this.annotation_preview.as_ref(),
                    Some(AnnotationPreview::Loading(current)) if current == &key
                ) {
                    return;
                }
                if this.record.target() != target
                    || this.expanded_annotation != Some(annotation_id)
                    || !this.annotations.iter().any(|current| {
                        annotation_preview_key(current, &target).as_ref() == Some(&key)
                    })
                {
                    this.annotation_preview = None;
                    return;
                }
                this.annotation_preview = Some(match result {
                    Ok(image) => AnnotationPreview::Ready { key, image },
                    Err(()) => AnnotationPreview::Unavailable(key),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn restore_initial_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.record.snapshot.status,
            NativeSessionStatus::Error | NativeSessionStatus::Stopped
        ) && self.composer.read(cx).value().is_empty()
            && let Some(message) = &self.record.pending_initial_message
        {
            self.composer.update(cx, |composer, cx| {
                composer.set_value(message.clone(), window, cx);
            });
            if self.attachments.is_empty() {
                self.attachments = self
                    .record
                    .attachments
                    .iter()
                    .cloned()
                    .map(|reference| ComposerAttachment {
                        reference,
                        preview: None,
                    })
                    .collect();
            }
        }
    }

    pub(crate) fn update_record(
        &mut self,
        mut record: NativeSessionRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if record.id != self.record.id || record.generation < self.record.generation {
            return;
        }
        if record.generation == self.record.generation {
            if record.snapshot.revision < self.record.snapshot.revision {
                return;
            }
            let current = self
                .history_live_snapshot
                .as_ref()
                .unwrap_or(&self.record.snapshot);
            if record.snapshot == *current
                && record.title == self.record.title
                && record.config.permissions == self.record.config.permissions
                && record.permissions_pending == self.record.permissions_pending
            {
                return;
            }
        } else {
            // A resumed generation owns fresh request tokens; old responses cannot affect it.
            self.pending.clear();
            self.cancellation_requested = false;
            self.error = None;
        }
        if record.generation != self.record.generation || is_busy(record.snapshot.status) {
            self.history_page = None;
            self.history_live_snapshot = None;
            self.history_loaded = false;
        } else if let Some(page) = &self.history_page {
            self.history_live_snapshot = Some(record.snapshot.clone());
            record.snapshot.transcript =
                history_transcript(&record.snapshot.transcript, record.side_chat.as_ref(), page);
        }
        let previous = std::mem::replace(&mut self.record, record);
        self.restore_initial_message(window, cx);
        let generation_changed = previous.generation != self.record.generation;
        if generation_changed {
            self.annotation_preview = None;
            self.attachment_preview_attempts.clear();
            self.models = None;
            self.model_picker = None;
            self.model_picker_subscription = None;
        }
        if !is_busy(self.record.snapshot.status) {
            self.cancellation_requested = false;
        }
        if previous.snapshot.transcript != self.record.snapshot.transcript {
            self.sync_transcript(&previous.snapshot.transcript, window, cx);
        } else if previous.snapshot.status != self.record.snapshot.status {
            self.refresh_timeline();
        }
        self.sync_requests(window, cx);
        self.load_attachment_previews(window, cx);
        self.load_models(window, cx);
        self.sync_model_picker(window, cx);
        if generation_changed && let Some(annotation_id) = self.expanded_annotation {
            self.load_annotation_preview(annotation_id, window, cx);
        }
        cx.notify();
    }

    fn sync_transcript(
        &mut self,
        previous: &[NativeTranscriptItem],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .record
            .snapshot
            .transcript
            .iter()
            .map(|item| item.id.as_str())
            .collect::<BTreeSet<_>>();
        let previous = previous
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect::<std::collections::HashMap<_, _>>();
        let changed = self
            .record
            .snapshot
            .transcript
            .iter()
            .filter(|item| {
                previous
                    .get(item.id.as_str())
                    .is_none_or(|old| **old != **item)
            })
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>();
        self.transcript
            .retain(|id, _| current.contains(id.as_str()));
        self.transcript_subscriptions
            .retain(|id, _| self.transcript.contains_key(id));
        self.message_focus
            .retain(|id, _| current.contains(id.as_str()));
        self.expanded_messages
            .retain(|id| current.contains(id.as_str()));
        if self
            .selected_response
            .as_ref()
            .is_some_and(|selection| !self.transcript.contains_key(&selection.citation.message_id))
        {
            self.selected_response = None;
        }
        self.tool_disclosures
            .retain(|id, _| current.contains(id.as_str()));
        self.subagent_details.retain(|id, _| {
            self.record
                .snapshot
                .transcript
                .iter()
                .any(|item| item.subagent.as_ref().is_some_and(|agent| agent.id == *id))
        });
        for item in &self.record.snapshot.transcript {
            if let Some(state) = self.transcript.get(&item.id) {
                if previous
                    .get(item.id.as_str())
                    .is_none_or(|old| old.text != item.text || old.citations != item.citations)
                {
                    if self
                        .selected_response
                        .as_ref()
                        .is_some_and(|selection| selection.citation.message_id == item.id)
                    {
                        self.selected_response = None;
                    }
                    state.update(cx, |state, cx| {
                        state.set_text(&transcript_markdown(item), cx);
                    });
                }
            } else {
                let state = cx.new(|cx| TextViewState::markdown(&transcript_markdown(item), cx));
                let focus = cx.focus_handle();
                let mut subscriptions = vec![
                    cx.on_focus_in(&focus, window, |_, _, cx| cx.notify()),
                    cx.on_focus_out(&focus, window, |_, _, _, cx| cx.notify()),
                ];
                if item.role == "assistant" {
                    let subscription =
                        Self::observe_response_selection(item.id.clone(), &state, window, cx);
                    subscriptions.push(subscription);
                }
                self.message_focus.insert(item.id.clone(), focus);
                self.transcript_subscriptions
                    .insert(item.id.clone(), subscriptions);
                self.transcript.insert(item.id.clone(), state);
            }
            if item.complete && is_tool_output(&item.role) {
                self.tool_disclosures
                    .entry(item.id.clone())
                    .or_insert(false);
            }
        }
        self.work_disclosures
            .retain(|id, _| current.contains(id.as_str()));
        self.refresh_timeline();
        for (ix, row) in self.timeline.iter().enumerate() {
            if matches!(row, TranscriptRow::Message { id, .. } if changed.contains(id)) {
                self.list.remeasure_items(ix..ix.saturating_add(1));
            }
        }
        self.sync_tool_diffs(&previous, window, cx);
    }

    fn sync_tool_diffs(
        &mut self,
        previous: &std::collections::HashMap<&str, &NativeTranscriptItem>,
        window: &Window,
        cx: &Context<Self>,
    ) {
        self.tool_diffs.retain(|id, _| {
            self.record.snapshot.transcript.iter().any(|item| {
                item.id == *id
                    && item
                        .tool
                        .as_ref()
                        .is_some_and(|tool| tool.name == "fileChange")
            })
        });
        let changed_diffs = self
            .record
            .snapshot
            .transcript
            .iter()
            .filter(|item| {
                previous.get(item.id.as_str()).is_none_or(|old| {
                    old.tool.as_ref().map(|tool| (&tool.name, &tool.input))
                        != item.tool.as_ref().map(|tool| (&tool.name, &tool.input))
                })
            })
            .filter_map(|item| {
                item.tool
                    .as_ref()
                    .filter(|tool| tool.name == "fileChange")
                    .map(|tool| (item.id.clone(), tool.input.clone()))
            })
            .collect::<Vec<_>>();
        for (id, input) in changed_diffs {
            self.load_tool_diffs(id, input, window, cx);
        }
    }

    fn sync_requests(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.requests.update(cx, |requests, cx| {
            requests.set_enabled(self.provider_enabled(), cx);
            requests.update(
                self.record.target(),
                &self.record.snapshot.requests,
                &self.pending,
                window,
                cx,
            );
        });
    }

    fn request_command(
        &mut self,
        target: &CommandTarget,
        operation: &str,
        arguments: Vec<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.record.target() != *target {
            return;
        }
        self.command(operation, arguments, window, cx);
    }

    fn command(
        &mut self,
        operation: &str,
        arguments: Vec<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !self.provider_enabled() && operation != "interrupt" {
            return;
        }
        let pending = if matches!(operation, "approve" | "respond" | "subagent-read") {
            let Some(id) = arguments.first() else {
                return;
            };
            if operation == "subagent-read" {
                format!("subagent-read:{id}")
            } else {
                format!("request:{id}")
            }
        } else {
            operation.to_owned()
        };
        if self.pending.contains(&pending) {
            return;
        }
        let submitted_draft = (operation == "prompt").then(|| SubmittedPrompt {
            text: self.composer.read(cx).value().to_string(),
            revision: self.draft_revision,
            annotations: self.annotations.clone(),
            citations: self.active_citations(cx),
            attachments: self.active_attachment_ids(cx),
        });
        let mut invocation = CommandInvocation::new(
            format!("agents.native.{operation}"),
            Vec::new(),
            Caller::Internal,
        );
        invocation.target = Some(self.record.target());
        let target = self.record.target();
        invocation.arguments = vec![self.record.id.clone(), self.record.generation.to_string()];
        invocation.arguments.extend(arguments);
        let receiver = match self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.error = Some(format!("Could not send agent command: {error:?}"));
                cx.notify();
                return;
            }
        };
        self.pending.insert(pending.clone());
        self.error = None;
        let operation = operation.to_owned();
        let generation = self.record.generation;
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if this.record.generation != generation {
                    return;
                }
                this.pending.remove(&pending);
                match result {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        if operation == "interrupt" {
                            this.cancellation_requested = is_busy(this.record.snapshot.status);
                        }
                        if let Some(draft) = submitted_draft {
                            this.clear_submitted_draft(draft, target, window, cx);
                        }
                        this.accept_command_result(&operation, value, window, cx);
                    }
                    Ok(outcome) => {
                        this.error = Some(
                            crate::commands::command_outcome_message(&outcome)
                                .unwrap_or_else(|| "Agent command failed".to_owned()),
                        );
                    }
                    Err(error) => {
                        this.error = Some(format!("Agent command did not complete: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn accept_command_result(
        &mut self,
        operation: &str,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if operation == "history" {
            match serde_json::from_value::<bootty_agents::NativeHistoryPage>(value) {
                Ok(page) => {
                    if self.history_live_snapshot.is_none() {
                        self.history_live_snapshot = Some(self.record.snapshot.clone());
                    }
                    let transcript = history_transcript(
                        &self
                            .history_live_snapshot
                            .as_ref()
                            .unwrap_or(&self.record.snapshot)
                            .transcript,
                        self.record.side_chat.as_ref(),
                        &page,
                    );
                    let previous =
                        std::mem::replace(&mut self.record.snapshot.transcript, transcript);
                    self.history_page = Some(page);
                    self.sync_transcript(&previous, window, cx);
                }
                Err(error) => self.error = Some(format!("Could not read earlier history: {error}")),
            }
        } else if operation == "fork" {
            if let Ok(record) = serde_json::from_value::<NativeSessionRecord>(value) {
                cx.emit(OpenNativeSession::SideChatCreated {
                    source: self.record.target(),
                    record: Box::new(record),
                });
            } else {
                self.error = Some("The side chat did not return its session identity".into());
            }
        } else if operation == "subagent-read" {
            match serde_json::from_value::<bootty_agents::NativeSubagentDetail>(value) {
                Ok(detail) => {
                    let states = detail
                        .transcript
                        .iter()
                        .filter(|item| !item.text.is_empty())
                        .map(|item| {
                            cx.new(|cx| TextViewState::markdown(&transcript_markdown(item), cx))
                        })
                        .collect();
                    self.subagent_details.insert(detail.agent.id, states);
                }
                Err(error) => {
                    self.error = Some(format!("Could not read the subagent transcript: {error}"));
                }
            }
        } else {
            self.accept_result(value, window, cx);
        }
    }

    fn accept_result(&mut self, value: Value, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(record) = serde_json::from_value::<NativeSessionRecord>(value.clone()) {
            self.update_record(record, window, cx);
        } else if let Ok(snapshot) = serde_json::from_value::<NativeSessionSnapshot>(value)
            && snapshot.revision >= self.record.snapshot.revision
        {
            let mut record = self.record.clone();
            record.snapshot = snapshot;
            self.update_record(record, window, cx);
        }
    }

    fn clear_submitted_draft(
        &mut self,
        draft: SubmittedPrompt,
        target: CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SubmittedPrompt {
            text,
            revision,
            annotations,
            citations,
            attachments,
        } = draft;
        if self.draft_revision == revision
            && self.composer.read(cx).value().as_ref() == text.as_str()
        {
            self.composer.update(cx, |composer, cx| {
                composer.set_value("", window, cx);
            });
        }
        if !annotations.is_empty() {
            // The durable owner removes only unchanged submitted versions.
            cx.emit(OpenNativeSession::SentAnnotations {
                target,
                annotations,
            });
        }
        self.clear_submitted_citations(&citations, cx);
        let apps = self.active_applications(cx);
        self.applications
            .retain(|mention| apps.iter().any(|active| active.id == mention.id));
        let active = self.active_attachment_ids(cx);
        self.attachments.retain(|attachment| {
            !attachments.contains(&attachment.reference.id)
                || active.contains(&attachment.reference.id)
        });
    }

    fn send_prompt(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.provider_enabled()
            || !self.can_prompt()
            || self.pending.contains("prompt")
            || self.pending.contains("configure")
            || self.attachment_imports > 0
        {
            return;
        }
        let attachment_ids = self.active_attachment_ids(cx);
        let mut message = self.composer.read(cx).value().to_string();
        let authored_bytes = message.len();
        if !self.annotations.is_empty() {
            match annotation_batch(&self.annotations) {
                Ok(batch) => {
                    if !message.trim().is_empty() {
                        message.push_str("\n\n");
                    }
                    message.push_str(&batch);
                }
                Err(error) => {
                    self.error = Some(error.to_string());
                    cx.notify();
                    return;
                }
            }
        }
        if message.len() > 64 * 1024 {
            self.error = Some(
                "This message and its attachments exceed the conversation’s prompt limit."
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        if !message.trim().is_empty()
            || !attachment_ids.is_empty()
            || !self.active_citations(cx).is_empty()
        {
            match self.prompt_arguments(message, authored_bytes, cx) {
                Ok(arguments) => self.command("prompt", arguments, window, cx),
                Err(error) => {
                    self.error = Some(error);
                    cx.notify();
                }
            }
        }
    }

    fn can_prompt(&self) -> bool {
        (self.record.snapshot.status == NativeSessionStatus::Idle
            && !self.record.permissions_pending
            && !self.resuming())
            || (self.record.snapshot.status == NativeSessionStatus::Working
                && matches!(
                    self.record.config.provider,
                    AgentKind::Codex | AgentKind::Pi
                ))
    }

    pub(crate) fn set_animate_working(&mut self, animate: bool, cx: &mut Context<Self>) {
        if self.working_indicator.animate != animate {
            self.working_indicator.animate = animate;
            cx.notify();
        }
    }

    fn has_running_activity(&self) -> bool {
        self.record.snapshot.status == NativeSessionStatus::Working
            || self.record.snapshot.transcript.iter().any(|item| {
                item.subagent
                    .as_ref()
                    .is_some_and(|agent| agent.status == bootty_agents::NativeToolStatus::Running)
            })
    }

    fn sync_activity_tick(&mut self, window: &Window, cx: &Context<Self>) {
        if !self.has_running_activity() {
            self.working_indicator.tick = None;
        } else if self.working_indicator.tick.is_none() {
            self.working_indicator.tick = Some(cx.spawn_in(window, async move |owner, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let keep = owner
                        .update(cx, |this, cx| {
                            if !this.has_running_activity() {
                                this.working_indicator.tick = None;
                                return false;
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !keep {
                        break;
                    }
                }
            }));
        }
    }

    fn render_working(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let label = if self.record.snapshot.transport_lost {
            "Reconnecting…".to_owned()
        } else if self.record.snapshot.status == NativeSessionStatus::Starting {
            "Connecting…".to_owned()
        } else if self.record.snapshot.status == NativeSessionStatus::Waiting {
            status_name(&self.record.snapshot).to_owned()
        } else if let Some(elapsed) = self.record.snapshot.working_elapsed(Instant::now()) {
            format!(
                "Working for {}",
                crate::clock::format_working_duration(elapsed)
            )
        } else {
            "Working…".to_owned()
        };
        let animate = self.record.snapshot.status != NativeSessionStatus::Waiting
            && self.working_indicator.animate
            && window.is_window_active()
            && !cx.reduce_motion();
        div()
            .id("native-working")
            .debug_selector(|| "native-working".into())
            .flex()
            .justify_center()
            .px_4()
            .py_1()
            .child(
                div()
                    .w_full()
                    .max_w(gpui_kit::rems(48.))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().foreground.opacity(0.72))
                    .child(if animate {
                        gpui_kit::component::spinner::Spinner::new()
                            .icon(gpui_kit::assets::IconName::LoaderCircle)
                            .color(cx.theme().foreground.opacity(0.72))
                            .ease(|value| value)
                            .small()
                            .into_any_element()
                    } else {
                        gpui_kit::component::Icon::new(gpui_kit::assets::IconName::LoaderCircle)
                            .small()
                            .into_any_element()
                    })
                    .child(label),
            )
    }

    fn render_tool_output(
        &self,
        item: &NativeTranscriptItem,
        owner: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        let open = self
            .tool_disclosures
            .get(&item.id)
            .copied()
            .unwrap_or(false);
        let output = div()
            .w_full()
            .max_w(gpui_kit::rems(48.0))
            .min_w_0()
            .id(gpui_kit::SharedString::from(format!(
                "message:{}:{}",
                self.record.id, item.id
            )))
            .debug_selector({
                let id = item.id.clone();
                move || format!("native-message-{id}")
            })
            .child(
                Collapsible::new()
                    .open(open)
                    .child(self.render_tool_disclosure(item, open, owner, cx))
                    .content(self.render_tool_details(item, cx)),
            );
        div()
            .w_full()
            .min_w_0()
            .px_4()
            .py_1()
            .flex()
            .justify_center()
            .child(output)
            .into_any_element()
    }

    fn render_tool_disclosure(
        &self,
        item: &NativeTranscriptItem,
        open: bool,
        owner: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> Button {
        let id = item.id.clone();
        let name = item
            .tool
            .as_ref()
            .map_or("Execution details", |tool| tool_label(&tool.name));
        Button::new(gpui_kit::SharedString::from(format!(
            "tool-output:{}:{id}",
            self.record.id
        )))
        .w_full()
        .px_0()
        .small()
        .ghost()
        .accessibility_label(format!("{} {name}", if open { "Hide" } else { "Show" }))
        .child(Self::render_tool_heading(item, open, cx))
        .on_click(move |_, _, cx| {
            _ = owner.update(cx, |this, cx| {
                this.tool_disclosures.insert(id.clone(), !open);
                if let Some(ix) = this.timeline.iter().position(|row| {
                    matches!(row, TranscriptRow::Message { id: message, .. } if message == &id)
                }) {
                    this.list.remeasure_items(ix..ix.saturating_add(1));
                }
                cx.notify();
            });
        })
    }

    fn render_tool_heading(item: &NativeTranscriptItem, open: bool, cx: &App) -> gpui_kit::Div {
        let name = item
            .tool
            .as_ref()
            .map_or("Execution details", |tool| tool_label(&tool.name));
        let status = item
            .tool
            .as_ref()
            .map(|tool| tool_status_label(tool.status));
        let failed = item
            .tool
            .as_ref()
            .is_some_and(|tool| tool.status == bootty_agents::NativeToolStatus::Failed);
        let (icon, summary) = item.tool.as_ref().map_or_else(
            || (gpui_kit::assets::IconName::Wrench, String::new()),
            tool_input_summary,
        );
        div()
            .debug_selector({
                let id = item.id.clone();
                move || format!("native-tool-heading-{id}")
            })
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .text_color(cx.theme().foreground.opacity(0.72))
            .child(gpui_kit::component::Icon::new(icon).small())
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(gpui_kit::rems(12.0))
                    .truncate()
                    .child(name.to_owned()),
            )
            .child(div().flex_1().min_w_0().truncate().child(summary))
            .when_some(status, |row, status| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .when(failed, |status| status.text_color(cx.theme().danger))
                        .child(status),
                )
            })
            .child(
                gpui_kit::component::Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small(),
            )
    }

    fn load_tool_diffs(&mut self, id: String, input: String, window: &Window, cx: &Context<Self>) {
        self.tool_diffs.insert(id.clone(), ToolDiffs::Loading);
        let cwd = self.record.config.cwd.clone();
        cx.spawn_in(window, async move |owner, cx| {
            let current_input = input.clone();
            let result = cx
                .background_executor()
                .spawn(async move { parse_tool_diffs(&input, &cwd) })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if !this.record.snapshot.transcript.iter().any(|item| {
                    item.id == id
                        && item
                            .tool
                            .as_ref()
                            .is_some_and(|tool| tool.input == current_input)
                }) {
                    return;
                }
                let state = match result {
                    Ok(files) => ToolDiffs::Ready(
                        files
                            .into_iter()
                            .map(|(path, file)| {
                                let rows = file
                                    .hunks
                                    .iter()
                                    .map(|hunk| hunk.lines.len().saturating_add(1))
                                    .fold(0_usize, usize::saturating_add)
                                    .clamp(2, 14);
                                let rows = num_traits::ToPrimitive::to_f32(&rows).unwrap_or(14.0);
                                let view = cx.new(|cx| {
                                    let mut view =
                                        crate::gpui_git_panel::GitDiffPanel::new(window, cx);
                                    view.show_file_diff(file, window, cx);
                                    view
                                });
                                (path, view, rows)
                            })
                            .collect(),
                    ),
                    Err(error) => ToolDiffs::Failed(error),
                };
                for (index, row) in this.timeline.iter().enumerate() {
                    let contains = match row {
                        TranscriptRow::Message { id: message, .. } => message == &id,
                        TranscriptRow::Work { members, .. } => members.iter().any(|index| {
                            this.record
                                .snapshot
                                .transcript
                                .get(*index)
                                .is_some_and(|item| item.id == id)
                        }),
                        TranscriptRow::History { .. } => false,
                    };
                    if contains {
                        this.list.remeasure_items(index..index.saturating_add(1));
                    }
                }
                this.tool_diffs.insert(id, state);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_tool_details(&self, item: &NativeTranscriptItem, cx: &App) -> gpui_kit::Div {
        if let Some(diffs) = self.tool_diffs.get(&item.id) {
            let body = div().pt_2().flex().flex_col().gap_2();
            return match diffs {
                ToolDiffs::Loading => body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(gpui_kit::component::spinner::Spinner::new().small())
                        .child("Loading changes…"),
                ),
                ToolDiffs::Failed(error) => body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(format!("Could not show the file diff: {error}")),
                ),
                ToolDiffs::Ready(files) => body.children(files.iter().map(|(path, view, rows)| {
                    div()
                        .w_full()
                        .min_w_0()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_md()
                        .overflow_hidden()
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(path.clone()),
                        )
                        .child(
                            div()
                                .h(cx.theme().mono_font_size.mul(rows.mul_add(1.5, 1.0)))
                                .min_w_0()
                                .child(view.clone()),
                        )
                })),
            };
        }
        div()
            .pt_2()
            .flex()
            .flex_col()
            .gap_2()
            .when_some(
                item.tool.as_ref().filter(|tool| !tool.input.is_empty()),
                |body, tool| {
                    body.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Input"),
                    )
                    .child(
                        TextView::markdown(
                            gpui_kit::SharedString::from(format!(
                                "tool-input:{}:{}",
                                self.record.id, item.id
                            )),
                            tool_input_markdown(tool),
                        )
                        .selectable(true),
                    )
                },
            )
            .when_some(
                self.transcript
                    .get(&item.id)
                    .filter(|_| !item.text.is_empty()),
                |body, state| {
                    body.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Output"),
                    )
                    .child(TextView::new(state).selectable(true))
                },
            )
    }

    fn render_subagent(
        &self,
        item: &NativeTranscriptItem,
        agent: &bootty_agents::NativeSubagent,
        owner: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> gpui_kit::Div {
        let id = item.id.clone();
        let open = self.tool_disclosures.get(&id).copied().unwrap_or(false);
        let header = subagent_header(agent, open, cx);
        let read_owner = owner.clone();
        let child_id = agent.id.clone();
        let pending = self.pending.contains(&format!("subagent-read:{child_id}"));
        let content = div()
            .pl_5()
            .py_2()
            .flex()
            .flex_col()
            .gap_2()
            .when(!agent.prompt.is_empty(), |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(agent.prompt.clone()),
                )
            })
            .when_some(self.transcript.get(&item.id), |view, state| {
                view.child(TextView::new(state).selectable(true))
            })
            .when(
                agent.thread_id.is_some() && agent.owner_session == self.record.snapshot.session_id,
                |view| {
                    view.child(
                        Button::new(format!("subagent-read:{child_id}"))
                            .ghost()
                            .small()
                            .label(if pending {
                                "Loading transcript…"
                            } else {
                                "View transcript"
                            })
                            .disabled(pending)
                            .on_click(move |_, window, cx| {
                                _ = read_owner.update(cx, |this, cx| {
                                    this.command(
                                        "subagent-read",
                                        vec![child_id.clone()],
                                        window,
                                        cx,
                                    );
                                });
                            }),
                    )
                },
            )
            .when_some(self.subagent_details.get(&agent.id), |view, states| {
                view.children(
                    states
                        .iter()
                        .map(|state| TextView::new(state).selectable(true)),
                )
            });
        div().px_4().py_1().child(
            Collapsible::new()
                .open(open)
                .child(
                    Button::new(format!("subagent-toggle:{id}"))
                        .ghost()
                        .w_full()
                        .justify_start()
                        .child(header)
                        .on_click(move |_, _, cx| {
                            _ = owner.update(cx, |this, cx| {
                                this.tool_disclosures.insert(id.clone(), !open);
                                cx.notify();
                            });
                        }),
                )
                .content(content),
        )
    }

    fn render_message(
        &self,
        ix: usize,
        owner: gpui_kit::WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        let Some(item) = self.record.snapshot.transcript.get(ix) else {
            return div().into_any_element();
        };
        if let Some(agent) = &item.subagent {
            return self
                .render_subagent(item, agent, owner, cx)
                .into_any_element();
        }
        if is_tool_output(&item.role) || item.tool.is_some() {
            return self.render_tool_output(item, owner, cx);
        }
        let user = item.role == "user";
        let quiet = matches!(item.role.as_str(), "notice" | "thinking" | "reasoning");
        let focus = self.message_focus.get(&item.id);
        let focused = focus.is_some_and(|focus| focus.contains_focused(window, cx));
        let row = div()
            .id(gpui_kit::SharedString::from(format!(
                "message:{}:{}",
                self.record.id, item.id
            )))
            .debug_selector({
                let id = item.id.clone();
                move || format!("native-message-{id}")
            })
            .group("native-message")
            .when_some(focus, |row, focus| row.track_focus(focus).tab_stop(false))
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .when(user, gpui_kit::Styled::items_end)
            .child(self.render_message_body(item, owner.clone(), cx))
            .when(
                item.complete && matches!(item.role.as_str(), "user" | "assistant"),
                |row| row.child(self.message_metadata(item, focused, owner, cx)),
            );
        div()
            .w_full()
            .min_w_0()
            .px_4()
            .when(quiet, gpui_kit::Styled::py_1)
            .when(!quiet, gpui_kit::Styled::py_2)
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(gpui_kit::rems(48.0))
                    .min_w_0()
                    .flex()
                    .when(user, gpui_kit::Styled::justify_end)
                    .child(row),
            )
            .into_any_element()
    }

    fn render_message_body(
        &self,
        item: &NativeTranscriptItem,
        owner: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> gpui_kit::Div {
        let user = item.role == "user";
        let collapsible =
            user && (item.text.encode_utf16().count() > 600 || item.text.lines().count() > 8);
        let expanded = self.expanded_messages.contains(&item.id);
        let bubble_background = cx.theme().accent.mix_oklab(cx.theme().background, 0.5);
        let title = match item.role.as_str() {
            "thinking" | "reasoning" => "Thinking",
            "change" => "Changes",
            _ => "",
        };
        let id = item.id.clone();
        div()
            .debug_selector({
                let id = item.id.clone();
                move || format!("native-message-body-{id}")
            })
            .min_w_0()
            .block()
            .when(user, |row| {
                row.max_w(gpui_kit::relative(0.8)).p_2().rounded(cx.theme().radius)
                    .bg(bubble_background).text_color(cx.theme().accent_foreground)
            })
            .when(matches!(item.role.as_str(), "notice" | "thinking" | "reasoning"), |row| {
                row.text_sm().text_color(cx.theme().muted_foreground)
            })
            .when(!title.is_empty(), |row| {
                row.child(div().text_xs().text_color(cx.theme().muted_foreground).child(title))
            })
            .when_some(self.transcript.get(&item.id), |body, state| {
                body.child(div().min_w_0().relative()
                    .when(collapsible && !expanded, |text| text.max_h(gpui_kit::rems(11.)).overflow_hidden())
                    .child(Self::message_text(state, &item.id, owner.clone()))
                    .when(collapsible && !expanded, |text| text.child(
                        div().absolute().left_0().right_0().bottom_0().h_7()
                            .bg(gpui_kit::linear_gradient(180.,
                                gpui_kit::linear_color_stop(bubble_background.opacity(0.), 0.),
                                gpui_kit::linear_color_stop(bubble_background, 1.))),
                    )))
            })
            .when(!item.citations.is_empty(), |row| row.child(Self::render_sent_citations(item, &owner, cx).mt_1()))
            .when(!item.attachments.is_empty(), |row| row.child(self.render_sent_attachments(item, cx).mt_1()))
            .when(collapsible, |row| {
                row.child(Button::new(gpui_kit::SharedString::from(format!("expand-message:{id}")))
                    .debug_selector({
                        let id = id.clone();
                        move || format!("expand-message:{id}")
                    })
                    .ghost().xsmall()
                    .self_end()
                    .label(if expanded { "Show less" } else { "Show full message" })
                    .on_click(move |_, _, cx| {
                        _ = owner.update(cx, |this, cx| {
                            if !this.expanded_messages.remove(&id) {
                                this.expanded_messages.insert(id.clone());
                            }
                            if let Some(ix) = this.timeline.iter().position(|row| matches!(row, TranscriptRow::Message { id: message, .. } if message == &id)) {
                                this.list.remeasure_items(ix..ix.saturating_add(1));
                            }
                            cx.notify();
                        });
                    }))
            })
    }
    fn message_text(
        state: &Entity<TextViewState>,
        id: &str,
        owner: gpui_kit::WeakEntity<Self>,
    ) -> TextView {
        let id = id.to_owned();
        TextView::new(state)
            .selectable(true)
            .on_link_click(move |url, _, _, cx| {
                if let Some(ix) = url
                    .strip_prefix("bootty-citation:")
                    .and_then(|ix| ix.parse::<usize>().ok())
                {
                    _ = owner.update(cx, |this, cx| {
                        let message = this
                            .record
                            .snapshot
                            .transcript
                            .iter()
                            .find(|item| item.id == id && item.role == "user")
                            .and_then(|item| item.citations.get(ix))
                            .map(|citation| citation.message_id.clone());
                        if let Some(message) = message {
                            this.reveal_response(&message, cx);
                        }
                    });
                } else {
                    cx.open_url(url);
                }
            })
    }

    fn message_metadata(
        &self,
        item: &NativeTranscriptItem,
        focused: bool,
        owner: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> gpui_kit::Div {
        let id = item.id.clone();
        let copied = self.copied_message.as_deref() == Some(id.as_str());
        let timestamp = if item.role == "user" {
            item.created_at
        } else {
            item.updated_at.or(item.created_at)
        }
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%b %-d, %-I:%M %p")
                .to_string()
        });
        // Reserve the metadata lane so hover/focus never moves the transcript.
        div()
            .h_8()
            .w_full()
            .flex()
            .items_center()
            .when(item.role == "user", gpui_kit::Styled::justify_end)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .opacity(0.)
                    .text_color(cx.theme().muted_foreground)
                    .when(focused, |bar| bar.opacity(1.))
                    .group_hover("native-message", |bar| bar.opacity(1.))
                    .when_some(timestamp, |bar, timestamp| {
                        bar.child(div().text_xs().child(timestamp))
                    })
                    .child(
                        Button::new(gpui_kit::SharedString::from(format!("copy-message:{id}")))
                            .debug_selector({
                                let id = id.clone();
                                move || format!("copy-message:{id}")
                            })
                            .icon(if copied {
                                IconName::Check
                            } else {
                                IconName::Copy
                            })
                            .ghost()
                            .xsmall()
                            .accessibility_label(if copied {
                                "Copied message"
                            } else {
                                "Copy message"
                            })
                            .tooltip(if copied {
                                "Copied message"
                            } else {
                                "Copy message"
                            })
                            .on_click({
                                let owner = owner.clone();
                                move |_, window, cx| {
                                    _ = owner
                                        .update(cx, |this, cx| this.copy_message(&id, window, cx));
                                }
                            }),
                    )
                    .when(
                        item.role == "assistant" && item.complete && self.provider_enabled(),
                        |bar| {
                            let response = item.id.clone();
                            bar.child(
                                Button::new(format!("side-chat:{}", item.id))
                                    .icon(gpui_kit::assets::IconName::GitFork)
                                    .ghost()
                                    .xsmall()
                                    .accessibility_label("Start side chat")
                                    .tooltip("Start side chat")
                                    .on_click(move |_, window, cx| {
                                        _ = owner.update(cx, |this, cx| {
                                            this.command(
                                                "fork",
                                                vec![response.clone()],
                                                window,
                                                cx,
                                            );
                                        });
                                    }),
                            )
                        },
                    ),
            )
    }

    pub(crate) fn set_related_conversations(
        &mut self,
        related: Vec<RelatedConversation>,
        cx: &mut Context<Self>,
    ) {
        self.related = related;
        cx.notify();
    }

    fn render_related_conversations(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div().px_4().when(!self.related.is_empty(), |view| {
            view.child(
                Collapsible::new()
                    .open(false)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Side chats · {}", self.related.len())),
                    )
                    .content(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(self.related.iter().map(|related| {
                                let target = related.target.clone();
                                let state = match related.status {
                                    NativeSessionStatus::Working => "Working…",
                                    NativeSessionStatus::Waiting => "Needs input",
                                    NativeSessionStatus::Error => "Error",
                                    _ => "",
                                };
                                Button::new(format!("related:{}", target.handle))
                                    .ghost()
                                    .small()
                                    .justify_start()
                                    .child(crate::gpui::sized_icon(
                                        "git-fork",
                                        crate::gpui::IconSize::Small,
                                        cx.theme().muted_foreground,
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .child(related.title.clone()),
                                    )
                                    .when_some(related.model.clone(), |button, model| {
                                        button.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(model),
                                        )
                                    })
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(state),
                                    )
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(OpenNativeSession::Related {
                                            target: target.clone(),
                                        });
                                    }))
                            })),
                    ),
            )
        })
    }

    fn copy_message(&mut self, id: &str, window: &Window, cx: &mut Context<Self>) {
        let Some(item) = self
            .record
            .snapshot
            .transcript
            .iter()
            .find(|item| item.id == id)
        else {
            return;
        };
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(item.display_text()));
        self.copied_message = Some(id.to_owned());
        self.copy_feedback = Some(cx.spawn_in(window, async move |owner, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            _ = owner.update(cx, |this, cx| {
                this.copied_message = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl NativeAgentSessionView {
    fn render_annotations(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        div()
            .id("native-annotation-attachments")
            .flex()
            .flex_col()
            .gap_2()
            .flex_shrink_0()
            .child(
                div().flex().flex_wrap().gap_2().children(
                    self.annotations
                        .iter()
                        .map(|annotation| self.render_annotation_chip(annotation, cx)),
                ),
            )
            .when_some(
                self.expanded_annotation.and_then(|id| {
                    self.annotations
                        .iter()
                        .find(|annotation| annotation.id == id)
                }),
                |body, annotation| body.child(self.render_annotation_details(annotation, cx)),
            )
            .into_any_element()
    }

    fn render_annotation_chip(
        &self,
        annotation: &Annotation,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let id = annotation.id;
        let label = annotation.note.trim().chars().take(48).collect::<String>();
        let description = format!(
            "{}\n{}\n{}",
            annotation.address,
            annotation_selection_details(annotation),
            annotation.note
        );
        let preview_annotation = annotation.clone();
        let preview_key = annotation_preview_key(annotation, &self.record.target());
        let preview = self
            .annotation_preview
            .as_ref()
            .and_then(|preview| match preview {
                AnnotationPreview::Ready { key, image } if preview_key.as_ref() == Some(key) => {
                    Some(image.clone())
                }
                _ => None,
            });
        div()
            .id(gpui_kit::SharedString::from(format!(
                "annotation-chip:{id}"
            )))
            .flex()
            .items_center()
            .min_w_0()
            .max_w_full()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().warning.opacity(0.34))
            .bg(cx.theme().warning.opacity(0.11))
            .child(
                Button::new(gpui_kit::SharedString::from(format!(
                    "native-annotation:{id}"
                )))
                .label(label)
                .icon(gpui_kit::assets::IconName::MousePointerClick)
                .small()
                .ghost()
                .min_w_0()
                .text_color(cx.theme().foreground)
                .selected(self.expanded_annotation == Some(id))
                .accessibility_label(format!(
                    "{} browser annotation details, {}",
                    if self.expanded_annotation == Some(id) {
                        "Hide"
                    } else {
                        "Show"
                    },
                    annotation.note
                ))
                .tooltip(description)
                .when_some(preview, |button, image| {
                    button
                        .h_auto()
                        .py_1()
                        .child(Self::render_annotation_thumbnail(image, cx))
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !this.annotations.contains(&preview_annotation) {
                        return;
                    }
                    this.expanded_annotation = (this.expanded_annotation != Some(id)).then_some(id);
                    if this.expanded_annotation == Some(id) {
                        this.load_annotation_preview(id, window, cx);
                    }
                    cx.notify();
                })),
            )
            .child(Self::render_annotation_remove(annotation, cx))
    }

    fn render_annotation_remove(annotation: &Annotation, cx: &Context<Self>) -> Button {
        let id = annotation.id;
        let captured = annotation.clone();
        Button::new(gpui_kit::SharedString::from(format!(
            "native-detach-annotation:{id}"
        )))
        .icon(IconName::Close)
        .xsmall()
        .ghost()
        .accessibility_label("Remove annotation attachment")
        .tooltip("Remove attachment; keep saved annotation")
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.emit(OpenNativeSession::DetachAnnotation {
                target: this.record.target(),
                annotation: Box::new(captured.clone()),
            });
        }))
    }

    fn render_annotation_thumbnail(image: Arc<gpui_kit::RenderImage>, cx: &App) -> gpui_kit::Div {
        div()
            .w_16()
            .h_10()
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(cx.theme().radius)
            .child(
                gpui_kit::img(image)
                    .size_full()
                    .object_fit(gpui_kit::ObjectFit::Contain),
            )
    }

    fn render_annotation_details(
        &self,
        annotation: &Annotation,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let target = self.record.target();
        let preview_key = annotation_preview_key(annotation, &target);
        let preview = self
            .annotation_preview
            .as_ref()
            .and_then(|preview| match preview {
                AnnotationPreview::Ready { key, image } if preview_key.as_ref() == Some(key) => {
                    Some(image.clone())
                }
                _ => None,
            });
        let preview_status = if annotation.image.is_some() {
            match self.annotation_preview.as_ref() {
                Some(AnnotationPreview::Loading(key)) if preview_key.as_ref() == Some(key) => {
                    Some("Loading…")
                }
                Some(AnnotationPreview::Unavailable(key)) if preview_key.as_ref() == Some(key) => {
                    Some("Preview unavailable")
                }
                _ => None,
            }
        } else {
            None
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .max_h_32()
            .overflow_y_scrollbar()
            .text_sm()
            .child(annotation.note.clone())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(annotation_selection_details(annotation)),
            )
            .when_some(preview, |details, image| {
                details.child(Self::render_annotation_preview(annotation, image))
            })
            .when_some(preview_status, |details, status| {
                details.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(status),
                )
            })
            .into_any_element()
    }

    fn render_annotation_preview(
        annotation: &Annotation,
        image: Arc<gpui_kit::RenderImage>,
    ) -> impl IntoElement {
        div()
            .id(gpui_kit::SharedString::from(format!(
                "native-annotation-preview:{}",
                annotation.id
            )))
            .w_32()
            .h_20()
            .flex_shrink_0()
            .overflow_hidden()
            .rounded_md()
            .child(
                gpui_kit::img(image)
                    .size_full()
                    .object_fit(gpui_kit::ObjectFit::Contain),
            )
    }

    fn render_composer(&self, window: &Window, cx: &Context<Self>) -> gpui_kit::Div {
        let owner = cx.weak_entity();
        let token_owner = owner.clone();
        let token_click_owner = owner.clone();
        let composer = div()
            .id("native-composer")
            .debug_selector(|| "native-composer".into())
            .when(self.completion_active(window, cx), |view| {
                view.key_context("ComposerCompletion")
            })
            .relative()
            .w_full()
            .max_w(gpui_kit::rems(48.0))
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_xl()
            .bg(cx.theme().muted.opacity(0.35))
            .border_1()
            .border_color(cx.theme().border)
            .when(!self.attachments.is_empty(), |body| {
                body.child(self.render_attachments(cx))
            })
            .when(!self.annotations.is_empty(), |body| {
                body.child(self.render_annotations(cx))
            })
            .child(
                Textarea::new(&self.composer)
                    .token(move |context, window, cx| {
                        token_owner.upgrade().map_or_else(
                            || div().into_any_element(),
                            |owner| {
                                owner.update(cx, |this, cx| {
                                    this.render_prompt_token(context, window, cx)
                                })
                            },
                        )
                    })
                    .on_token_click(move |event, window, cx| {
                        _ = token_click_owner.update(cx, |this, cx| {
                            this.open_citation_token(
                                event.token().id(),
                                event.bounds().bottom_left(),
                                window,
                                cx,
                            );
                        });
                    })
                    .appearance(false)
                    .bordered(false)
                    .on_paste(move |item, window, cx| {
                        owner
                            .update(cx, |this, cx| this.paste_attachments(item, window, cx))
                            .unwrap_or(false)
                    })
                    .aria_label(format!(
                        "Message to {}",
                        provider_name(self.record.config.provider)
                    )),
            )
            .child(self.render_composer_controls(cx))
            .children(self.completion.clone());
        div()
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .px_4()
            .py_3()
            .child(composer)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One footer owns the adjacent composer controls"
    )]
    fn render_composer_controls(&self, cx: &Context<Self>) -> impl IntoElement {
        let send_pending = self.pending.contains("prompt");
        let send_label = if self.record.snapshot.status == NativeSessionStatus::Working {
            "Steer agent"
        } else {
            "Send message"
        };
        let interrupt_pending = self.pending.contains("interrupt");
        let ready = self.can_prompt();
        let inactive = matches!(
            self.record.snapshot.status,
            NativeSessionStatus::Starting
                | NativeSessionStatus::Stopped
                | NativeSessionStatus::Error
        );
        let interruptible = is_busy(self.record.snapshot.status) || send_pending;
        let resumable = !self.record.snapshot.transport_lost
            && matches!(
                self.record.snapshot.status,
                NativeSessionStatus::Stopped | NativeSessionStatus::Error
            );
        let options = div()
            .id("native-composer-options")
            .flex_1()
            .min_w_0()
            .overflow_x_scroll()
            .flex()
            .items_center()
            .gap_2()
            .child(self.render_model_controls(cx))
            .child(
                div()
                    .track_focus(&self.permissions_focus)
                    .child(self.render_permission_control(cx)),
            )
            .child(self.render_context_usage(cx))
            .when(
                !self.provider_enabled() || self.cancellation_requested,
                |row| {
                    row.child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if self.provider_enabled() {
                                "Stopping…".to_owned()
                            } else {
                                format!(
                                    "{} is disabled in Settings",
                                    provider_name(self.record.config.provider)
                                )
                            }),
                    )
                },
            );
        div()
            .flex()
            .id("native-composer-controls")
            .min_w_0()
            .items_center()
            .justify_start()
            .gap_2()
            .child(options)
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("native-attach")
                            .debug_selector(|| "native-attach".into())
                            .child(crate::gpui::sized_icon(
                                "paperclip",
                                crate::gpui::IconSize::Small,
                                cx.theme().foreground,
                            ))
                            .ghost()
                            .small()
                            .accessibility_label("Attach files")
                            .tooltip("Attach files")
                            .disabled(inactive || !self.provider_enabled())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_attachments(window, cx);
                            })),
                    )
                    .when(interruptible, |row| {
                        row.child(
                            Button::new("native-interrupt")
                                .debug_selector(|| "native-interrupt".into())
                                .label("Interrupt")
                                .icon(IconName::Pause)
                                .small()
                                .ghost()
                                .loading(interrupt_pending)
                                .disabled(interrupt_pending)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.command("interrupt", vec![], window, cx);
                                })),
                        )
                    })
                    .when(resumable, |row| {
                        row.child(
                            Button::new("native-resume")
                                .debug_selector(|| "native-resume".into())
                                .label("Resume")
                                .small()
                                .primary()
                                .loading(self.pending.contains("resume"))
                                .disabled(
                                    !self.provider_enabled() || self.pending.contains("resume"),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.command("resume", vec![], window, cx);
                                })),
                        )
                    })
                    .when(!resumable, |row| {
                        row.child(
                            Button::new("native-send")
                                .debug_selector(|| "native-send".into())
                                .icon(IconName::ArrowUp)
                                .small()
                                .primary()
                                .rounded_full()
                                .accessibility_label(send_label)
                                .tooltip(send_label)
                                .loading(send_pending)
                                .disabled(
                                    !self.provider_enabled()
                                        || !ready
                                        || send_pending
                                        || self.pending.contains("configure")
                                        || self.attachment_imports > 0
                                        || (self.composer.read(cx).value().trim().is_empty()
                                            && self.attachments.is_empty()
                                            && self.annotations.is_empty()
                                            && self.active_citations(cx).is_empty()),
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.send_prompt(window, cx)),
                                ),
                        )
                    }),
            )
    }

    fn render_context_usage(&self, cx: &Context<Self>) -> gpui_kit::Div {
        use gpui_kit::component::{popover::Popover, progress::ProgressCircle};
        let Some(usage) = self.record.snapshot.usage.as_ref() else {
            return div();
        };
        let counters = usage.get("last").unwrap_or(usage);
        let used = counters
            .get("totalTokens")
            .and_then(Value::as_u64)
            .or_else(|| {
                let input = counters.get("input").and_then(Value::as_u64)?;
                Some(["output", "cacheRead", "cacheWrite"].into_iter().fold(
                    input,
                    |tokens, field| {
                        tokens.saturating_add(
                            counters.get(field).and_then(Value::as_u64).unwrap_or(0),
                        )
                    },
                ))
            });
        let Some(used) = used else {
            return div();
        };
        let limit = usage
            .get("modelContextWindow")
            .and_then(Value::as_u64)
            .filter(|limit| *limit > 0);
        let percent = limit.map(|limit| {
            u16::try_from(
                used.saturating_mul(100)
                    .checked_div(limit)
                    .unwrap_or(0)
                    .min(100),
            )
            .unwrap_or(100)
        });
        let label =
            percent.map_or_else(|| format!("{used} tokens"), |percent| format!("{percent}%"));
        let details = limit.map_or_else(
            || format!("{used} tokens used"),
            |limit| format!("Context: {used} / {limit} tokens"),
        );
        let color = if percent.is_some_and(|percent| percent >= 90) {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        div().child(
            Popover::new("native-context-usage")
                .max_w(gpui_kit::rems(20.))
                .trigger(
                    Button::new("native-context-trigger")
                        .ghost()
                        .small()
                        .accessibility_label(details.clone())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    ProgressCircle::new("native-context-ring")
                                        .value(f32::from(percent.unwrap_or(0)))
                                        .color(color)
                                        .size(gpui_kit::px(16.)),
                                )
                                .child(label),
                        ),
                )
                .content(move |_, _, _| div().p_3().text_sm().child(details.clone())),
        )
    }
}

impl EventEmitter<OpenNativeSession> for NativeAgentSessionView {}
impl Focusable for NativeAgentSessionView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

fn history_transcript(
    live: &[NativeTranscriptItem],
    fork: Option<&bootty_agents::NativeSideChat>,
    page: &bootty_agents::NativeHistoryPage,
) -> Vec<NativeTranscriptItem> {
    // Both sources are bounded. The copied prefix belongs before the first provider page,
    // while older provider pages remain reachable through their existing cursors.
    let prefix = fork
        .filter(|_| !page.has_older)
        .and_then(|fork| fork.copied_transcript(live))
        .unwrap_or_default();
    prefix
        .iter()
        .filter(|item| {
            !page
                .transcript
                .iter()
                .any(|provider| provider.id == item.id)
        })
        .chain(&page.transcript)
        .cloned()
        .collect()
}
impl NativeAgentSessionView {
    fn needs_jump_to_latest(&self) -> bool {
        !self.list.is_following_tail()
            || self
                .history_page
                .as_ref()
                .is_some_and(|page| !page.at_latest)
    }

    fn render_jump_to_latest(cx: &Context<Self>) -> impl IntoElement {
        div().flex().justify_center().child(
            Button::new("native-jump-latest")
                .debug_selector(|| "native-jump-latest".into())
                .icon(IconName::ChevronDown)
                .ghost()
                .small()
                .accessibility_label("Jump to latest")
                .tooltip("Jump to latest")
                .on_click(cx.listener(|this, _, window, cx| {
                    if this
                        .history_page
                        .as_ref()
                        .is_some_and(|page| !page.at_latest)
                    {
                        this.command("history", vec!["latest".into()], window, cx);
                    } else {
                        this.list.set_follow_mode(gpui_kit::FollowMode::Tail);
                    }
                    cx.notify();
                })),
        )
    }

    pub(crate) fn contains_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }
    fn load_history(&mut self, window: &Window, cx: &mut Context<Self>) {
        if matches!(
            self.record.config.provider,
            AgentKind::Codex | AgentKind::Pi
        ) && self.record.snapshot.status == NativeSessionStatus::Idle
            && self.record.pending_initial_message.is_none()
            && !self.pending.contains("prompt")
            && self
                .record
                .side_chat
                .as_ref()
                .is_none_or(|fork| fork.seeded_identity.is_some())
            && !self.history_loaded
        {
            self.history_loaded = true;
            self.command("history", vec!["latest".into()], window, cx);
        }
    }
}
impl Render for NativeAgentSessionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load_models(window, cx);
        self.load_history(window, cx);
        self.sync_activity_tick(window, cx);
        let owner = cx.weak_entity();
        div()
            .id(gpui_kit::SharedString::from(self.record.id.clone()))
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                if this.selected_response.take().is_some() {
                    cx.notify();
                }
            }))
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if this.handle_selection_key(event, window, cx)
                        || (event.keystroke.key == "escape"
                            && this.dismiss_citation_controls(window, cx))
                    {
                        cx.stop_propagation();
                    }
                }),
            )
            .when_some(
                self.error.as_ref().or_else(|| {
                    (!self.record.snapshot.transport_lost)
                        .then_some(self.record.snapshot.error.as_ref())
                        .flatten()
                }),
                |body, error| {
                    body.child(
                        div()
                            .px_4()
                            .py_2()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error.clone()),
                    )
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .child(self.render_history_gutter(window, cx))
                    .child(div().flex_1().min_h_0().min_w_0().flex().flex_col().when(
                        !self.timeline.is_empty(),
                        |body| {
                            body.child(
                                list(self.list.clone(), move |ix, window, cx| {
                                    owner.upgrade().map_or_else(
                                        || div().into_any_element(),
                                        |owner| {
                                            owner.read(cx).render_timeline_row(
                                                ix,
                                                owner.downgrade(),
                                                window,
                                                cx,
                                            )
                                        },
                                    )
                                })
                                .with_sizing_behavior(gpui_kit::ListSizingBehavior::Auto)
                                .size_full(),
                            )
                        },
                    )),
            )
            .when(self.needs_jump_to_latest(), |body| {
                body.child(Self::render_jump_to_latest(cx))
            })
            .when(!self.record.snapshot.requests.is_empty(), |body| {
                body.child(self.requests.clone())
            })
            .when(
                self.record.snapshot.transport_lost
                    || self.record.snapshot.status == NativeSessionStatus::Starting
                    || is_busy(self.record.snapshot.status)
                    || self.pending.contains("prompt"),
                |body| body.child(self.render_working(window, cx)),
            )
            .child(self.render_related_conversations(cx))
            .child(self.render_composer(window, cx))
            .child(self.render_selection_toolbar(window, cx))
    }
}

fn is_tool_output(role: &str) -> bool {
    role == "tool"
}
const fn is_busy(status: NativeSessionStatus) -> bool {
    matches!(
        status,
        NativeSessionStatus::Working | NativeSessionStatus::Waiting
    )
}
fn status_name(snapshot: &NativeSessionSnapshot) -> &'static str {
    if snapshot.transport_lost {
        return "Reconnecting";
    }
    match snapshot.status {
        NativeSessionStatus::Starting => "Connecting",
        NativeSessionStatus::Idle | NativeSessionStatus::Stopped => "Ready",
        NativeSessionStatus::Working => "Working",
        NativeSessionStatus::Waiting => {
            if snapshot.requests.iter().any(|request| {
                is_claude_question(request)
                    || matches!(
                        request.method.as_str(),
                        "item/tool/requestUserInput" | "pi.select" | "pi.input" | "pi.editor"
                    )
                    || (request.method == "mcpServer/elicitation/request"
                        && !request.is_mcp_approval())
            }) {
                "Needs input"
            } else if snapshot
                .requests
                .iter()
                .any(|request| request.is_mcp_approval() || is_approval(&request.method))
            {
                "Needs approval"
            } else {
                "Waiting"
            }
        }
        NativeSessionStatus::Error => "Failed",
    }
}
fn tool_label(name: &str) -> &str {
    match name {
        "commandExecution" | "Bash" | "bash" | "exec_command" => "Run",
        "Read" | "read" | "read_file" => "Read",
        "fileChange" | "Edit" | "edit" | "Write" | "write" | "apply_patch" => "Edit",
        "Grep" | "grep" | "Glob" | "glob" | "search" | "web_search" => "Search",
        "codemode" | "code_mode" => "Run code",
        "write_stdin" => "Read command output",
        "terminal_read" => "Read terminal",
        "list_terminals" => "List terminals",
        "get_workspace_info" => "Read Space",
        "list_agents" => "List agents",
        "get_agent_status" => "Read agent status",
        "get_agent_activity" => "Read recent activity",
        "list_models" => "List models",
        "list_providers" => "List providers",
        "list_profiles" => "List profiles",
        "inspect_provider" => "Inspect provider",
        "browser_snapshot" => "Read browser",
        "computer_snapshot" => "Capture window",
        "computer_input" => "Use window",
        "spawn_shell" => "Start terminal",
        "read_spawned_terminal" => "Read child terminal",
        "paste_spawned_terminal" => "Paste into child terminal",
        "submit_spawned_terminal" => "Submit child terminal input",
        "interrupt_spawned_terminal" => "Interrupt child terminal",
        "close_spawned_terminal" => "Close child terminal",
        "spawn_agent" => "Start agent",
        "interrupt_spawned_agent" => "Interrupt child agent",
        "stop_spawned_agent" => "Stop child agent",
        _ => name,
    }
}

const fn tool_status_label(status: bootty_agents::NativeToolStatus) -> &'static str {
    match status {
        bootty_agents::NativeToolStatus::Running => "Running",
        bootty_agents::NativeToolStatus::Completed => "Completed",
        bootty_agents::NativeToolStatus::Failed => "Failed",
        bootty_agents::NativeToolStatus::Interrupted => "Interrupted",
        bootty_agents::NativeToolStatus::Declined => "Declined",
    }
}

fn tool_input_summary(
    tool: &bootty_agents::NativeToolCall,
) -> (gpui_kit::assets::IconName, String) {
    let input = serde_json::from_str::<Value>(&tool.input).ok();
    let command = input.as_ref().and_then(|input| {
        input
            .get("command")
            .or_else(|| input.get("cmd"))
            .and_then(Value::as_str)
    });
    let path = input.as_ref().and_then(|input| {
        input
            .get("file_path")
            .or_else(|| input.get("filePath"))
            .or_else(|| input.get("path"))
            .or_else(|| {
                input
                    .as_array()
                    .and_then(|changes| changes.first())
                    .and_then(|change| change.get("path"))
            })
            .and_then(Value::as_str)
    });
    let query = input.as_ref().and_then(|input| {
        input
            .get("pattern")
            .or_else(|| input.get("query"))
            .and_then(Value::as_str)
    });
    let code = input
        .as_ref()
        .and_then(|input| input.get("code"))
        .and_then(Value::as_str);
    let icon = if command.is_some()
        || matches!(
            tool.name.as_str(),
            "commandExecution"
                | "Bash"
                | "bash"
                | "exec_command"
                | "terminal_read"
                | "list_terminals"
                | "spawn_shell"
                | "read_spawned_terminal"
                | "paste_spawned_terminal"
                | "submit_spawned_terminal"
                | "interrupt_spawned_terminal"
                | "close_spawned_terminal"
        ) {
        gpui_kit::assets::IconName::SquareTerminal
    } else if matches!(tool.name.as_str(), "computer_snapshot" | "computer_input") {
        gpui_kit::assets::IconName::Monitor
    } else if tool.name == "browser_snapshot" {
        gpui_kit::assets::IconName::Globe
    } else if matches!(
        tool.name.as_str(),
        "spawn_agent"
            | "list_agents"
            | "get_agent_status"
            | "get_agent_activity"
            | "interrupt_spawned_agent"
            | "stop_spawned_agent"
    ) {
        gpui_kit::assets::IconName::Bot
    } else if code.is_some() {
        gpui_kit::assets::IconName::SquareCode
    } else if path.is_some() || tool.name == "fileChange" {
        gpui_kit::assets::IconName::File
    } else if query.is_some() {
        gpui_kit::assets::IconName::Search
    } else {
        gpui_kit::assets::IconName::Wrench
    };
    let summary = command
        .or(path)
        .or(query)
        .or(code)
        .unwrap_or_else(|| if input.is_some() { "" } else { &tool.input })
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_owned();
    (icon, summary)
}

fn transcript_markdown(item: &NativeTranscriptItem) -> String {
    let text = item.display_text();
    if item.tool.as_ref().is_some_and(|tool| {
        matches!(
            tool.name.as_str(),
            "commandExecution" | "Bash" | "bash" | "exec_command" | "write_stdin"
        )
    }) {
        code_block(&text, "text")
    } else {
        text
    }
}

fn parse_tool_diffs(
    input: &str,
    cwd: &std::path::Path,
) -> Result<Vec<(String, bootty_git::diff::FileDiff)>, String> {
    #[derive(serde::Deserialize)]
    struct Change {
        path: String,
        diff: String,
    }
    if input.len() > 8 * 1024 * 1024 {
        return Err("The provider diff exceeds 8 MB".into());
    }
    let changes: Vec<Change> = serde_json::from_str(input).map_err(|error| error.to_string())?;
    changes
        .into_iter()
        .map(|change| {
            let path = std::path::Path::new(&change.path);
            let relative = path.strip_prefix(cwd).unwrap_or(path);
            // Out-of-project changes remain visible; these display-only diffs cannot submit Git reviews.
            let relative = if relative.is_absolute() {
                relative.file_name().map_or(relative, std::path::Path::new)
            } else {
                relative
            };
            let file = bootty_git::diff::FileDiff::parse(
                relative.to_string_lossy().into_owned(),
                None,
                (!change.diff.is_empty()).then_some(change.diff.as_str()),
            )?;
            Ok((change.path, file))
        })
        .collect()
}

fn tool_input_markdown(tool: &bootty_agents::NativeToolCall) -> String {
    if matches!(tool.name.as_str(), "codemode" | "code_mode")
        && let Ok(Value::Object(input)) = serde_json::from_str::<Value>(&tool.input)
        && input.len() == 1
        && let Some(code) = input.get("code").and_then(Value::as_str)
    {
        return code_block(code, "javascript");
    }
    let input = serde_json::from_str::<Value>(&tool.input)
        .ok()
        .and_then(|input| serde_json::to_string_pretty(&input).ok());
    code_block(
        input.as_deref().unwrap_or(&tool.input),
        if input.is_some() { "json" } else { "" },
    )
}

fn code_block(input: &str, language: &str) -> String {
    let fence = "`".repeat(
        input
            .split(|character| character != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .max(2)
            .saturating_add(1),
    );
    format!("{fence}{language}\n{input}\n{fence}")
}

fn annotation_selection_details(annotation: &Annotation) -> String {
    match &annotation.anchor.selection {
        None => annotation.anchor.selector.clone(),
        Some(bootty_browser::AnnotationSelection::Region {
            x,
            y,
            width,
            height,
        }) => format!("Region (page CSS pixels): x {x}, y {y}, width {width}, height {height}"),
        Some(bootty_browser::AnnotationSelection::Drawing { points }) => {
            let coordinates = points
                .iter()
                .map(|[x, y]| format!("({x}, {y})"))
                .collect::<Vec<_>>()
                .join(" → ");
            format!(
                "Drawing ({} points, page CSS pixels): {coordinates}",
                points.len()
            )
        }
    }
}

const fn provider_name(provider: AgentKind) -> &'static str {
    match provider {
        AgentKind::Codex => "Codex",
        AgentKind::Pi => "Pi",
        AgentKind::Claude => "Claude",
    }
}
impl EventEmitter<gpui_kit::component::dock::PanelEvent> for NativeAgentSessionView {}
impl gpui_kit::component::dock::BasePanel for NativeAgentSessionView {
    fn panel_name(&self) -> &'static str {
        "bootty.native-session"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn zoomable(&self, _: &App) -> bool {
        false
    }
}
impl gpui_kit::component::dock::Panel for NativeAgentSessionView {
    fn tab_name(&self, _: &App) -> Option<gpui_kit::SharedString> {
        Some(self.record.title.clone().into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.record.title.clone()
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

fn subagent_header(agent: &bootty_agents::NativeSubagent, open: bool, cx: &App) -> gpui_kit::Div {
    let status = match agent.status {
        bootty_agents::NativeToolStatus::Running => "Working…",
        bootty_agents::NativeToolStatus::Completed => "Completed",
        bootty_agents::NativeToolStatus::Failed => "Failed",
        bootty_agents::NativeToolStatus::Interrupted => "Interrupted",
        bootty_agents::NativeToolStatus::Declined => "Declined",
    };
    let title = if agent.title.is_empty() {
        agent
            .prompt
            .lines()
            .next()
            .unwrap_or("Agent")
            .chars()
            .take(80)
            .collect()
    } else {
        agent.title.clone()
    };
    let elapsed = agent
        .started_at
        .map(|started| {
            (agent
                .completed_at
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis()))
            .saturating_sub(started)
        })
        .and_then(|elapsed| u64::try_from(elapsed).ok())
        .map(|elapsed| crate::clock::format_working_duration(Duration::from_millis(elapsed)));
    div()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(crate::gpui::sized_icon(
            if open {
                "chevron-down"
            } else {
                "chevron-right"
            },
            crate::gpui::IconSize::Small,
            cx.theme().muted_foreground,
        ))
        .child(crate::gpui::sized_icon(
            "bot",
            crate::gpui::IconSize::Small,
            cx.theme().muted_foreground,
        ))
        .child(div().flex_1().min_w_0().text_ellipsis().child(title))
        .when_some(agent.model.clone(), |view, model| {
            view.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(model),
            )
        })
        .child(
            div()
                .text_xs()
                .text_color(if agent.status == bootty_agents::NativeToolStatus::Failed {
                    cx.theme().danger
                } else {
                    cx.theme().muted_foreground
                })
                .child(status),
        )
        .when_some(elapsed, |view, elapsed| {
            view.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(elapsed),
            )
        })
}
