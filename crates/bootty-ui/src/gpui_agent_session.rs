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
    menu::DropdownMenu as _,
    scroll::ScrollableElement as _,
    text::{TextView, TextViewState},
};
use gpui_kit::{
    App, Context, Entity, EntityInputHandler as _, EventEmitter, FocusHandle, Focusable,
    IntoElement, ListAlignment, ListState, ParentElement, Render, Styled, Subscription, Window,
    div, prelude::*,
};
use image::{ImageFormat, ImageReader};
use serde_json::Value;

#[path = "gpui_agent_completion.rs"]
mod completion;

#[path = "gpui_agent_attachments.rs"]
mod attachments;
pub use attachments::attachment_preview;
use attachments::{ComposerAttachment, PendingAttachmentImport};
#[path = "gpui_agent_models.rs"]
mod models;
#[path = "gpui_agent_subagents.rs"]
mod subagents;
pub use models::permission_choice_content;
pub use models::reasoning_label;

#[path = "gpui_agent_citations.rs"]
mod citations;
pub use citations::init_reaction_keys;
use citations::{ResponseCitation, ResponseSelection};
#[path = "gpui_agent_timeline.rs"]
mod timeline;
use timeline::TranscriptRow;
#[path = "gpui_agent_questions.rs"]
mod questions;
#[path = "gpui_agent_requests.rs"]
mod requests;
use requests::{is_approval, is_claude_question};
#[path = "gpui_agent_context.rs"]
mod context_card;
#[path = "gpui_agent_drafts.rs"]
mod drafts;
#[path = "gpui_agent_followups.rs"]
mod followups;
#[path = "gpui_agent_plan.rs"]
mod plan;
pub use context_card::ConversationContext;

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

fn decode_image_preview(bytes: &[u8], annotation: bool) -> Option<Arc<gpui_kit::RenderImage>> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1600);
    limits.max_image_height = Some(1600);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let mut pixels = if annotation {
        image.thumbnail(128, 80)
    } else {
        image
    }
    .into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(gpui_kit::RenderImage::new([image::Frame::new(
        pixels,
    )])))
}

#[derive(Clone)]
pub enum OpenNativeSession {
    Speech {
        mode: crate::gpui_speech::SpeechMode,
    },
    Changes {
        target: crate::gpui::chrome::SessionTarget,
    },
    SideChatCreated {
        source: CommandTarget,
        record: Box<NativeSessionRecord>,
        /// A queued message moved to the side chat's composer.
        draft: Option<String>,
    },
    Related {
        target: CommandTarget,
    },
    Subagent {
        source: CommandTarget,
        id: String,
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
    Ready(Vec<ToolDiff>),
    Failed(String),
}

struct ToolDiff {
    path: String,
    view: Entity<crate::gpui_git_panel::GitDiffPanel>,
    rows: f32,
    counts: Option<(usize, usize)>,
}

struct WorkingIndicator {
    tick: Option<gpui_kit::Task<()>>,
    animate: bool,
    subagents_expanded: bool,
    side_chats_expanded: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderAvailability {
    Disabled,
    Enabled,
    Resuming,
}

struct PresentationPreferences {
    streaming: bootty_config::config::ResponseStreaming,
    collapse_composer_on_scroll: bool,
}

pub struct NativeAgentSessionView {
    record: NativeSessionRecord,
    related: Vec<RelatedConversation>,
    context_card: Option<ConversationContext>,
    wide: bool,
    speech: Entity<crate::gpui_speech::SpeechInput>,
    _speech_subscription: Subscription,
    focus: FocusHandle,
    permissions_focus: FocusHandle,
    available_efforts: Vec<String>,
    sender: BoundAppCommandSender,
    composer: Entity<TextareaState>,
    completion: Option<Entity<crate::gpui_composer_completion::ComposerCompletion>>,
    completion_subscriptions: Vec<Subscription>,
    draft_revision: u64,
    draft_save: Option<gpui_kit::Task<()>>,
    follow_up_behavior: bootty_config::config::FollowUpBehavior,
    send_shortcut: bootty_config::config::ComposerSendShortcut,
    presentation: PresentationPreferences,
    queued_messages: followups::QueuedMessages,
    applications: Vec<bootty_agents::NativeApplicationMention>,
    attachments: Vec<ComposerAttachment>,
    attachment_imports: BTreeMap<u64, PendingAttachmentImport>,
    next_attachment_import: u64,
    attachment_previews: BTreeMap<String, Arc<gpui_kit::RenderImage>>,
    attachment_preview_attempts: BTreeSet<String>,
    models: Option<Vec<bootty_agents::NativeModelOption>>,
    provider_permissions: Option<bootty_agents::NativePermissionMode>,
    model_picker: Option<Entity<models::ModelPickerState>>,
    model_picker_subscription: Option<Subscription>,
    annotations: Vec<Annotation>,
    expanded_annotation: Option<u64>,
    annotation_preview: Option<AnnotationPreview>,
    transcript: BTreeMap<String, Entity<TextViewState>>,
    /// Where file links were added to each message's markdown, so selections map to its text.
    link_marks: BTreeMap<String, crate::transcript_links::LinkMarks>,
    tool_diffs: BTreeMap<String, ToolDiffs>,
    async_questions: BTreeMap<String, Entity<questions::AsyncQuestionsView>>,
    subagent_details: BTreeMap<String, subagents::SubagentConversation>,
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
    hovered_turn: Option<usize>,
    work_disclosures: BTreeMap<String, bool>,
    working_indicator: WorkingIndicator,
    tool_disclosures: BTreeMap<String, bool>,
    list: ListState,
    availability: ProviderAvailability,
    requests: Entity<requests::NativeRequestView>,
    pending: BTreeSet<String>,
    error: Option<String>,
    command_error: Option<(String, String)>,
    dismissed_error: Option<String>,
    subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
pub struct RelatedConversation {
    pub target: CommandTarget,
    pub title: String,
    pub model: Option<String>,
    pub status: NativeSessionStatus,
    pub revision: u64,
    pub subagent: Option<bootty_agents::NativeSubagentDetail>,
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
                .auto_grow(1, 10)
                .placeholder(format!(
                    "Message {}…",
                    record.config.provider.display_name()
                ))
        });
        let subscription = cx.subscribe_in(&composer, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.draft_revision = this.draft_revision.wrapping_add(1);
                this.schedule_draft_save(cx);
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
        let speech = cx.new(crate::gpui_speech::SpeechInput::new);
        let speech_subscription = cx.subscribe_in(
            &speech,
            window,
            |this, _, event: &crate::gpui_speech::SpeechInputEvent, window, cx| match event {
                crate::gpui_speech::SpeechInputEvent::Toggle(mode) => {
                    cx.emit(OpenNativeSession::Speech { mode: *mode });
                }
                crate::gpui_speech::SpeechInputEvent::Error(error) => {
                    crate::gpui_speech::notify_error(error, window, cx);
                }
                crate::gpui_speech::SpeechInputEvent::Final { text } => {
                    this.composer.update(cx, |input, cx| {
                        input.replace_text_in_range(None, text, window, cx);
                    });
                }
                crate::gpui_speech::SpeechInputEvent::Offer(offer) => {
                    this.start_voice_call(offer.clone(), window, cx);
                }
                crate::gpui_speech::SpeechInputEvent::VoiceEnded => this.end_voice_call(),
            },
        );
        let mut this = Self {
            record,
            related: Vec::new(),
            context_card: None,
            wide: false,
            speech,
            _speech_subscription: speech_subscription,
            focus: cx.focus_handle(),
            permissions_focus: cx.focus_handle(),
            sender,
            composer,
            completion: None,
            completion_subscriptions: Vec::new(),
            draft_revision: 0,
            draft_save: None,
            follow_up_behavior: bootty_config::config::FollowUpBehavior::default(),
            send_shortcut: bootty_config::config::ComposerSendShortcut::default(),
            presentation: PresentationPreferences {
                streaming: bootty_config::config::ResponseStreaming::default(),
                collapse_composer_on_scroll: true,
            },
            queued_messages: followups::QueuedMessages::default(),
            applications: Vec::new(),
            attachments: Vec::new(),
            attachment_imports: BTreeMap::new(),
            next_attachment_import: 0,
            attachment_previews: BTreeMap::new(),
            attachment_preview_attempts: BTreeSet::new(),
            models: None,
            provider_permissions: None,
            model_picker: None,
            available_efforts: Vec::new(),
            model_picker_subscription: None,
            annotations: Vec::new(),
            expanded_annotation: None,
            annotation_preview: None,
            transcript: BTreeMap::new(),
            link_marks: BTreeMap::new(),
            tool_diffs: BTreeMap::new(),
            async_questions: BTreeMap::new(),
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
            hovered_turn: None,
            work_disclosures: BTreeMap::new(),
            working_indicator: WorkingIndicator {
                subagents_expanded: false,
                side_chats_expanded: false,
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
            error: None,
            command_error: None,
            dismissed_error: None,
            subscriptions: vec![subscription, citation_subscription, request_subscription],
        };
        this.restore_initial_message(window, cx);
        this.restore_saved_draft(window, cx);
        // A Codex conversation's speech runs on its own account, and it can hold a voice call;
        // the host sets any other's account.
        if this.record.config.provider == AgentKind::Codex {
            let (program, directory) = (
                this.record.config.program.clone(),
                this.record.config.account_directory.clone(),
            );
            this.speech.update(cx, |speech, cx| {
                speech.set_captured_account(program, directory, cx);
                speech.set_voice_supported(true, cx);
            });
        }
        this.configure_timeline(cx);
        this.sync_transcript(&[], window, cx);
        this.sync_requests(window, cx);
        this.load_attachment_previews(window, cx);
        let owner = cx.weak_entity();
        this.subscriptions
            .push(cx.intercept_keystrokes(move |event, window, cx| {
                let override_behavior = owner
                    .read_with(cx, |this, cx| {
                        if !this.composer.read(cx).focus_handle(cx).is_focused(window)
                            || !event.keystroke.key.eq_ignore_ascii_case("enter")
                            || event.keystroke.modifiers.alt
                            || this.completion.as_ref().is_some_and(|completion| {
                                completion.read(cx).blocks_submission(window, cx)
                            })
                        {
                            return None;
                        }
                        this.send_shortcut.submission(
                            this.composer.read(cx).value().contains('\n'),
                            event.keystroke.modifiers.secondary(),
                            event.keystroke.modifiers.shift,
                        )
                    })
                    .ok()
                    .flatten();
                if let Some(opposite) = override_behavior {
                    _ = owner.update(cx, |this, cx| this.send_prompt_mode(opposite, window, cx));
                    cx.stop_propagation();
                }
            }));
        this
    }

    fn configure_timeline(&self, cx: &Context<Self>) {
        self.list.set_follow_mode(gpui_kit::FollowMode::Tail);
        let owner = cx.weak_entity();
        self.list.set_scroll_handler(move |event, window, cx| {
            let near_start = event.is_scrolled && event.visible_range.start <= 2;
            let owner = owner.clone();
            window.defer(cx, move |window, cx| {
                _ = owner.update(cx, |this, cx| {
                    if near_start
                        && matches!(
                            this.record.snapshot.status,
                            NativeSessionStatus::Idle
                                | NativeSessionStatus::Working
                                | NativeSessionStatus::Waiting
                        )
                        && !this.pending.contains("history")
                        && this
                            .history_page
                            .as_ref()
                            .is_some_and(|page| page.has_older)
                    {
                        this.command("history", vec!["older".into()], window, cx);
                    }
                    cx.notify();
                });
            });
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
                    decode_image_preview(&bytes, true).ok_or(())
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
            && let Some(message) = self.record.pending_initial_message.clone()
        {
            let attachments = self.record.attachments.clone();
            self.restore_draft(&message, &attachments, window, cx);
        }
    }

    /// Puts a message back in the composer. Sending reads attachments from their chips, so each
    /// one returns to its place, or to the end when its place is unknown.
    pub(super) fn restore_draft(
        &mut self,
        message: &str,
        attachments: &[bootty_agents::NativeAttachmentReference],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        {
            let mut placed = attachments
                .iter()
                .flat_map(|attachment| {
                    let chip = crate::gpui_prompt_attachments::token(
                        attachment.id.clone(),
                        &attachment.name,
                        attachment.size_bytes,
                    );
                    attachment
                        .prompt_ranges
                        .iter()
                        .filter(|range| message.get((*range).clone()) == Some(chip.text().as_ref()))
                        .map(|range| (range.clone(), chip.clone()))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            placed.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
            let unplaced = attachments
                .iter()
                .filter(|attachment| {
                    !placed
                        .iter()
                        .any(|(_, chip)| chip.id().as_ref() == attachment.id)
                })
                .map(|attachment| {
                    crate::gpui_prompt_attachments::token(
                        attachment.id.clone(),
                        &attachment.name,
                        attachment.size_bytes,
                    )
                })
                .collect::<Vec<_>>();
            self.composer.update(cx, |composer, cx| {
                composer.set_value(message.to_owned(), window, cx);
                for (range, chip) in placed {
                    _ = composer.replace_range_with_token(range, chip, window, cx);
                }
                for chip in unplaced {
                    let end = composer.value().len();
                    _ = composer.replace_range_with_token(end..end, chip, window, cx);
                }
            });
            for reference in attachments {
                if !self
                    .attachments
                    .iter()
                    .any(|attachment| attachment.reference.id == reference.id)
                {
                    self.attachments.push(ComposerAttachment {
                        reference: reference.clone(),
                        preview: None,
                    });
                }
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
                && record.config.model == self.record.config.model
                && record.config.reasoning_effort == self.record.config.reasoning_effort
                && record.config.fast_mode == self.record.config.fast_mode
                && record.permissions_pending == self.record.permissions_pending
                && record.queued_messages == self.record.queued_messages
            {
                return;
            }
        } else {
            // A resumed generation owns fresh request tokens; old responses cannot affect it.
            self.pending.clear();
            self.error = None;
            self.command_error = None;
            self.dismissed_error = None;
            self.cancel_attachment_imports();
        }
        if record.generation != self.record.generation {
            self.history_loaded = false;
        }
        if let Some(page) = &self.history_page {
            self.history_live_snapshot = Some(record.snapshot.clone());
            record.snapshot.transcript =
                history_transcript(&record.snapshot.transcript, record.side_chat.as_ref(), page);
        }
        let previous = std::mem::replace(&mut self.record, record);
        self.restore_initial_message(window, cx);
        let generation_changed = previous.generation != self.record.generation;
        if generation_changed {
            self.async_questions.clear();
            self.speech
                .update(cx, super::gpui_speech::SpeechInput::stop);
            self.annotation_preview = None;
            self.attachment_preview_attempts.clear();
            if previous.config.account_directory != self.record.config.account_directory
                || previous.config.program != self.record.config.program
                || previous.config.arguments != self.record.config.arguments
            {
                self.models = None;
                self.provider_permissions = None;
                self.model_picker = None;
                self.model_picker_subscription = None;
            }
        }
        if generation_changed || previous.snapshot.transcript != self.record.snapshot.transcript {
            self.sync_transcript(&previous.snapshot.transcript, window, cx);
        } else if previous.snapshot.status != self.record.snapshot.status {
            self.refresh_timeline();
        }
        self.sync_requests(window, cx);
        if matches!(
            self.record.snapshot.status,
            NativeSessionStatus::Stopped | NativeSessionStatus::Error
        ) {
            self.speech
                .update(cx, super::gpui_speech::SpeechInput::stop);
        }
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
        self.sync_async_questions(window, cx);
        self.retain_subagent_details();
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
        self.link_marks
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
        for item in &self.record.snapshot.transcript {
            if let Some(state) = self.transcript.get(&item.id) {
                if previous.get(item.id.as_str()).is_none_or(|old| {
                    old.text != item.text
                        || old.citations != item.citations
                        || old.complete != item.complete
                }) {
                    if self
                        .selected_response
                        .as_ref()
                        .is_some_and(|selection| selection.citation.message_id == item.id)
                    {
                        self.selected_response = None;
                    }
                    let (markdown, marks) = streamed_markdown(item, self.presentation.streaming);
                    self.link_marks.insert(item.id.clone(), marks);
                    state.update(cx, |state, cx| state.set_text(&markdown, cx));
                }
            } else {
                let (markdown, marks) = streamed_markdown(item, self.presentation.streaming);
                self.link_marks.insert(item.id.clone(), marks);
                let state = cx.new(|cx| TextViewState::markdown(&markdown, cx));
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

    fn retain_subagent_details(&mut self) {
        self.subagent_details.retain(|id, _| {
            self.record
                .snapshot
                .transcript
                .iter()
                .any(|item| item.subagent.as_ref().is_some_and(|agent| agent.id == *id))
                || self.related.iter().any(|related| {
                    related
                        .subagent
                        .as_ref()
                        .is_some_and(|detail| detail.agent.id == *id)
                })
        });
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

    fn sync_async_questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.async_questions.retain(|id, _| {
            self.record
                .snapshot
                .transcript
                .iter()
                .any(|item| item.id == *id && !item.questions.is_empty())
        });
        for item in &self.record.snapshot.transcript {
            if item.questions.is_empty()
                || self
                    .async_questions
                    .get(&item.id)
                    .is_some_and(|view| view.read(cx).questions() == item.questions)
            {
                continue;
            }
            let view = cx.new(|cx| {
                questions::AsyncQuestionsView::new(
                    self.record.target(),
                    item.id.clone(),
                    item.questions.clone(),
                    window,
                    cx,
                )
            });
            cx.subscribe_in(
                &view,
                window,
                |this, _, answer: &questions::AsyncQuestionAnswer, window, cx| {
                    if this.record.target() != answer.target
                        || !this.can_prompt()
                        || !this.async_questions.contains_key(&answer.item)
                    {
                        return;
                    }
                    // A question reply has its own editor; preserve the conversation's draft.
                    this.submit_command("prompt", vec![answer.text.clone()], None, window, cx);
                    this.sync_requests(window, cx);
                    this.composer.read(cx).focus_handle(cx).focus(window, cx);
                },
            )
            .detach();
            self.async_questions.insert(item.id.clone(), view);
        }
    }

    fn sync_requests(&self, window: &mut Window, cx: &mut Context<Self>) {
        let enabled = self.provider_enabled()
            && self.can_prompt()
            && !self.pending.contains("prompt")
            && !self.pending.contains("queue");
        for questions in self.async_questions.values() {
            questions.update(cx, |questions, cx| questions.set_enabled(enabled, cx));
        }
        let changes = self
            .record
            .snapshot
            .requests
            .iter()
            .filter_map(|request| {
                requests::approval_change(request, &self.record.snapshot.transcript)
                    .map(|change| (request.id.clone(), change))
            })
            .collect::<BTreeMap<_, _>>();
        self.requests.update(cx, |requests, cx| {
            requests.set_enabled(self.provider_enabled(), cx);
            requests.update(
                self.record.target(),
                &self.record.snapshot.requests,
                &self.pending,
                (&changes, &self.record.config.cwd),
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
        let submitted_draft = matches!(operation, "prompt" | "queue").then(|| SubmittedPrompt {
            text: self.composer.read(cx).value().to_string(),
            revision: self.draft_revision,
            annotations: self.annotations.clone(),
            citations: self.active_citations(cx),
            attachments: self.active_attachment_ids(cx),
        });
        self.submit_command(operation, arguments, submitted_draft, window, cx);
    }

    fn submit_command(
        &mut self,
        operation: &str,
        arguments: Vec<String>,
        submitted_draft: Option<SubmittedPrompt>,
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
        let mut invocation = CommandInvocation::new(
            format!("agents.native.{operation}"),
            Vec::new(),
            Caller::Internal,
        );
        invocation.target = Some(self.record.target());
        let target = self.record.target();
        invocation.arguments = vec![self.record.id.clone(), self.record.generation.to_string()];
        invocation.arguments.extend(arguments);
        let Ok(receiver) = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) else {
            self.error =
                Some("Could not send the request. Reopen this conversation and retry.".into());
            cx.notify();
            return;
        };
        self.pending.insert(pending.clone());
        self.error = None;
        self.command_error = None;
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
                // A failed fork keeps its queued message where it was.
                if operation == "fork" && !matches!(result, Ok(CommandOutcome::Success { .. })) {
                    this.queued_messages.take_side_chat();
                }
                match result {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        if value.get("provider_writer").and_then(Value::as_str) != Some("blocked")
                            && let Some(draft) = submitted_draft
                        {
                            this.clear_submitted_draft(draft, target, window, cx);
                        }
                        if operation == "subagent-read" {
                            this.accept_subagent_read(&pending, value, cx);
                        } else {
                            this.accept_command_result(&operation, value, window, cx);
                        }
                    }
                    Ok(_) | Err(_) if operation == "subagent-read" => {
                        this.subagent_read_error(&pending);
                    }
                    Ok(outcome) => {
                        this.command_error = Some(command_failure(&operation, &outcome));
                    }
                    Err(_) => {
                        this.error = Some(
                            "Could not send the request. Reopen this conversation and retry."
                                .into(),
                        );
                    }
                }
                this.sync_requests(window, cx);
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
                Ok(mut page) => {
                    if let Some(loaded) = &self.history_page {
                        page.retain_loaded(loaded);
                    }
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
                    let has_older = page.has_older;
                    self.history_page = Some(page);
                    self.sync_transcript(&previous, window, cx);
                    // Fill the turn index through bounded provider pages without moving the viewport.
                    if has_older
                        && matches!(
                            self.record.snapshot.status,
                            NativeSessionStatus::Idle
                                | NativeSessionStatus::Working
                                | NativeSessionStatus::Waiting
                        )
                    {
                        self.command("history", vec!["older".into()], window, cx);
                    }
                }
                Err(_) => {
                    self.error = Some("Could not read earlier history. Scroll up to retry.".into());
                }
            }
        } else if operation == "fork" {
            let moved = self.queued_messages.take_side_chat();
            if let Ok(record) = serde_json::from_value::<NativeSessionRecord>(value) {
                cx.emit(OpenNativeSession::SideChatCreated {
                    source: self.record.target(),
                    record: Box::new(record),
                    draft: moved.as_ref().map(|(_, text)| text.clone()),
                });
                // The message now lives in the side chat, so it leaves this queue.
                if let Some((id, _)) = moved {
                    self.command("queue-cancel", vec![id], window, cx);
                }
            } else {
                self.error = Some("The side chat did not return its session identity".into());
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
        self.send_prompt_mode(false, window, cx);
    }

    fn send_prompt_mode(&mut self, opposite: bool, window: &Window, cx: &mut Context<Self>) {
        if self.speech.read(cx).dictating(cx) {
            self.speech
                .update(cx, super::gpui_speech::SpeechInput::finish_dictation);
            return;
        }
        if let Some((command, argument)) = self.composer_command(cx) {
            let owner = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                _ = owner.update(cx, |this, cx| {
                    this.run_composer_command(command, argument, window, cx);
                });
            });
            return;
        }
        if !self.provider_enabled()
            || !self.can_prompt()
            || self.pending.contains("prompt")
            || self.pending.contains("queue")
            || self.pending.contains("configure")
            || !self.attachment_imports.is_empty()
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
                Ok(arguments) => {
                    let behavior = if opposite {
                        self.follow_up_behavior.opposite()
                    } else {
                        self.follow_up_behavior
                    };
                    // A stopping turn can't take a steer; its message waits in the queue.
                    let operation = if is_busy(self.record.snapshot.status)
                        && (behavior == bootty_config::config::FollowUpBehavior::Queue
                            || self.record.snapshot.stopping())
                    {
                        "queue"
                    } else {
                        "prompt"
                    };
                    self.command(operation, arguments, window, cx);
                }
                Err(error) => {
                    self.error = Some(error);
                    cx.notify();
                }
            }
        }
    }

    fn can_prompt(&self) -> bool {
        if self.record.snapshot.provider_writer_blocked() {
            return false;
        }
        (self.record.snapshot.status == NativeSessionStatus::Idle
            && !self.record.permissions_pending
            && !self.resuming())
            || self.record.snapshot.status == NativeSessionStatus::Working
            || (self.record.snapshot.status == NativeSessionStatus::Waiting
                && self.follow_up_behavior == bootty_config::config::FollowUpBehavior::Queue)
    }

    pub(crate) fn set_follow_up_behavior(
        &mut self,
        behavior: bootty_config::config::FollowUpBehavior,
        cx: &mut Context<Self>,
    ) {
        if self.follow_up_behavior != behavior {
            self.follow_up_behavior = behavior;
            cx.notify();
        }
    }

    pub(crate) const fn set_send_shortcut(
        &mut self,
        shortcut: bootty_config::config::ComposerSendShortcut,
    ) {
        self.send_shortcut = shortcut;
    }

    pub(crate) fn set_presentation_preferences(
        &mut self,
        streaming: bootty_config::config::ResponseStreaming,
        collapse: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.presentation.streaming != streaming {
            self.presentation.streaming = streaming;
            self.sync_transcript(&[], window, cx);
        }
        if self.presentation.collapse_composer_on_scroll != collapse {
            self.presentation.collapse_composer_on_scroll = collapse;
            cx.notify();
        }
    }

    pub(crate) fn set_animate_working(&mut self, animate: bool, cx: &mut Context<Self>) {
        if self.working_indicator.animate != animate {
            self.working_indicator.animate = animate;
            cx.notify();
        }
    }

    /// Working motion runs while the agent works, unless the user turned it off. Waiting on the
    /// user holds still.
    fn moving(&self, cx: &App) -> bool {
        self.record.snapshot.status != NativeSessionStatus::Waiting
            && self.working_indicator.animate
            && !cx.reduce_motion()
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

    fn render_activity(
        &self,
        busy: bool,
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
        let animate = window.is_visible() && self.moving(cx);
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
                    .gap_3()
                    .text_sm()
                    .text_color(cx.theme().foreground.opacity(0.72))
                    .when(
                        busy && !self.record.snapshot.transcript.iter().any(|item| {
                            item.compaction_status()
                                == Some(bootty_agents::NativeToolStatus::Running)
                        }),
                        |row| {
                            row.child(
                                div()
                                    .id("native-progress-status")
                                    .debug_selector(|| "native-progress-status".into())
                                    .min_w_0()
                                    .h_7()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(if animate {
                                        crate::motion::spinner(cx.theme().foreground.opacity(0.72))
                                            .into_any_element()
                                    } else {
                                        // A paused spinner reads as frozen; waiting on the user is a clock.
                                        gpui_kit::component::Icon::new(
                                            if self.record.snapshot.status
                                                == NativeSessionStatus::Waiting
                                            {
                                                gpui_kit::assets::IconName::Clock
                                            } else {
                                                gpui_kit::assets::IconName::LoaderCircle
                                            },
                                        )
                                        .small()
                                        .into_any_element()
                                    })
                                    .child(label),
                            )
                        },
                    )
                    .child(self.render_subagent_group(cx)),
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
            )
            .when(!open, |output| {
                let Some(ToolDiffs::Ready(files)) = self.tool_diffs.get(&item.id) else {
                    return output;
                };
                output.child(
                    div()
                        .pt_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(files.iter().map(|file| {
                            div()
                                .debug_selector({
                                    let path = file.path.clone();
                                    let counts = file.counts;
                                    move || {
                                        format!(
                                            "native-file-change:{path}:+{}:-{}",
                                            counts.map_or(0, |c| c.0),
                                            counts.map_or(0, |c| c.1)
                                        )
                                    }
                                })
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .child(div().flex_1().min_w_0().truncate().child(file.path.clone()))
                                .when_some(file.counts, |row, (added, removed)| {
                                    row.child(
                                        div()
                                            .text_color(cx.theme().success)
                                            .child(format!("+{added}")),
                                    )
                                    .child(
                                        div()
                                            .text_color(cx.theme().danger)
                                            .child(format!("−{removed}")),
                                    )
                                })
                        })),
                )
            });
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
        let name = self.tool_name(item);
        Button::new(gpui_kit::SharedString::from(format!(
            "tool-output:{}:{id}",
            self.record.id
        )))
        .debug_selector({
            let id = id.clone();
            move || format!("tool-disclosure:{id}")
        })
        .w_full()
        .px_0()
        .small()
        .ghost()
        .accessibility_label(format!(
            "{} {name} details",
            if open { "Hide" } else { "Show" }
        ))
        .child(self.render_tool_heading(item, open, cx))
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

    fn tool_name<'a>(&self, item: &'a NativeTranscriptItem) -> std::borrow::Cow<'a, str> {
        match self.tool_diffs.get(&item.id) {
            Some(ToolDiffs::Ready(files)) => std::borrow::Cow::Owned(format!(
                "{} {} {}",
                match item.tool.as_ref().map(|tool| tool.status) {
                    Some(bootty_agents::NativeToolStatus::Completed) => "Edited",
                    Some(bootty_agents::NativeToolStatus::Running) => "Editing",
                    _ => "Changes to",
                },
                files.len(),
                if files.len() == 1 { "file" } else { "files" }
            )),
            _ => item
                .tool
                .as_ref()
                .map_or_else(|| "Execution details".into(), |tool| tool_label(&tool.name)),
        }
    }

    fn render_tool_heading(
        &self,
        item: &NativeTranscriptItem,
        open: bool,
        cx: &App,
    ) -> gpui_kit::Div {
        let name = self.tool_name(item);
        // Completing is the normal case; only the other outcomes say so.
        let status = item
            .tool
            .as_ref()
            .filter(|tool| tool.status != bootty_agents::NativeToolStatus::Completed)
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
                    .child(name.into_owned()),
            )
            .child(div().flex_1().min_w_0().truncate().map(|row| {
                if item
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.status == bootty_agents::NativeToolStatus::Running)
                {
                    row.when(self.moving(cx), |row| row.opacity(crate::motion::pulse()))
                        .child(summary)
                } else {
                    row.child(summary)
                }
            }))
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
                                tool_diff(&path, file, &this.record.config.cwd, window, cx)
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
                ToolDiffs::Failed(error) => {
                    body.child(div().text_sm().text_color(cx.theme().danger).child(
                        crate::error_catalog::ErrorNotice::from_text(error.as_str()).to_string(),
                    ))
                }
                ToolDiffs::Ready(files) => body.children(tool_diff_files(files, cx)),
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
                    .child(capped_scroll(
                        format!("tool-input-scroll:{}", item.id),
                        12.,
                        TextView::markdown(
                            gpui_kit::SharedString::from(format!(
                                "tool-input:{}:{}",
                                self.record.id, item.id
                            )),
                            tool_input_markdown(tool),
                        )
                        .selectable(true),
                    ))
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
                    .child(capped_scroll(
                        format!("tool-output-scroll:{}", item.id),
                        20.,
                        TextView::new(state).selectable(true),
                    ))
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
        let tab_owner = owner.clone();
        let tab_id = agent.id.clone();
        let child_id = agent.id.clone();
        let pending = self.pending.contains(&format!("subagent-read:{child_id}"));
        let content = div()
            .child(
                Button::new(format!("subagent-tab:{tab_id}"))
                    .ghost()
                    .small()
                    .label("Open in tab")
                    .on_click(move |_, _, cx| {
                        _ = tab_owner.update(cx, |this, cx| {
                            cx.emit(OpenNativeSession::Subagent {
                                source: this.record.target(),
                                id: tab_id.clone(),
                            });
                        });
                    }),
            )
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
                agent.readable() && agent.owner_session == self.record.snapshot.session_id,
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
                        .states
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
        meta: bool,
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
        if let Some(status) = item.compaction_status() {
            return self.render_compaction(item, status, window, cx);
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
            .when(!item.text.is_empty() || item.questions.is_empty(), |row| {
                row.child(self.render_message_body(item, owner.clone(), cx))
            })
            .when_some(self.async_questions.get(&item.id), |row, questions| {
                row.gap_3().child(questions.clone())
            })
            .when(self.record.snapshot.undelivered(item), |row| {
                row.child(Self::undelivered_notice(item, owner.clone(), cx))
            })
            .when(
                meta && item.complete
                    && matches!(item.role.as_str(), "user" | "assistant" | "plan"),
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
            "plan" => "Plan",
            _ => "",
        };
        let id = item.id.clone();
        div()
            .debug_selector({
                let id = item.id.clone();
                move || format!("native-message-body-{id}")
            })
            .min_w_0()
            .flex()
            .flex_col()
            .when(user, |row| {
                // Rich-text lists have no intrinsic width. Give their wrapping
                // layout the same bounded lane as other user message content.
                row.w_full().max_w(gpui_kit::relative(0.8)).p_2().rounded(cx.theme().radius)
                    .bg(bubble_background).text_color(cx.theme().accent_foreground)
            })
            .when(matches!(item.role.as_str(), "notice" | "thinking" | "reasoning"), |row| {
                row.text_sm().text_color(cx.theme().muted_foreground)
            })
            .when(item.role == "plan", |row| {
                row.gap_1().p_3().rounded(cx.theme().radius).border_1().border_color(cx.theme().border)
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
            .when(!item.applications.is_empty(), |row| row.child(Self::render_sent_applications(item, cx).mt_1()))
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
            .code_block_actions(code_block_actions)
            .on_link_click(move |url, _, window, cx| {
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
                } else if let Some(ix) = url
                    .strip_prefix("bootty-annotation:")
                    .and_then(|ix| ix.parse::<usize>().ok())
                {
                    _ = owner.update(cx, |this, cx| {
                        this.open_response_annotation(&id, ix, window, cx);
                    });
                } else {
                    // Files and URLs open the way the conversation's terminal opens them.
                    _ = owner.update(cx, |this, cx| {
                        this.command("open-link", vec![url.to_string()], window, cx);
                    });
                }
            })
    }

    /// A message the provider never confirmed says so, and its text can go back to the composer.
    fn undelivered_notice(
        item: &NativeTranscriptItem,
        owner: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> gpui_kit::Div {
        let id = item.id.clone();
        let text = item.text.clone();
        div()
            .debug_selector({
                let id = id.clone();
                move || format!("native-undelivered-{id}")
            })
            .h_8()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(cx.theme().danger)
            .child("Not delivered")
            .child(
                Button::new(gpui_kit::SharedString::from(format!("resend-message:{id}")))
                    .debug_selector(move || format!("native-resend-{id}"))
                    .ghost()
                    .xsmall()
                    .label("Edit and resend")
                    .on_click(move |_, window, cx| {
                        _ = owner.update(cx, |this, cx| {
                            this.composer.update(cx, |composer, cx| {
                                composer.set_value(text.clone(), window, cx);
                                composer.focus_handle(cx).focus(window, cx);
                            });
                        });
                    }),
            )
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
        for child in &related {
            if let Some(detail) = &child.subagent
                && self
                    .related
                    .iter()
                    .find(|old| old.target == child.target)
                    .is_none_or(|old| old.revision != child.revision)
            {
                self.accept_subagent_detail(detail.clone(), cx);
            }
        }
        // Every revision resends the list; only a changed one redraws. A child's detail follows
        // its revision.
        let unchanged = self.related.len() == related.len()
            && self.related.iter().zip(&related).all(|(old, new)| {
                old.target == new.target
                    && old.title == new.title
                    && old.model == new.model
                    && old.status == new.status
                    && old.revision == new.revision
                    && old.subagent.is_some() == new.subagent.is_some()
            });
        self.related = related;
        if !unchanged {
            cx.notify();
        }
    }

    fn render_related_conversations(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let related = self
            .related
            .iter()
            .filter(|related| related.subagent.is_none())
            .collect::<Vec<_>>();
        let open = self.working_indicator.side_chats_expanded;
        div().when(!related.is_empty(), |view| {
            view.child(
                Collapsible::new()
                    .open(open)
                    // Collapsible stretches its trigger and Button centers its content.
                    .child(
                        div().flex().child(
                            Button::new("side-chats-toggle")
                                .ghost()
                                .xsmall()
                                .icon(if open {
                                    gpui_kit::component::IconName::ChevronDown
                                } else {
                                    gpui_kit::component::IconName::ChevronRight
                                })
                                .text_color(cx.theme().muted_foreground)
                                .label(format!("Side chats · {}", related.len()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.working_indicator.side_chats_expanded ^= true;
                                    cx.notify();
                                })),
                        ),
                    )
                    .content(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(related.into_iter().map(|related| {
                                let target = related.target.clone();
                                let state = match related.status {
                                    NativeSessionStatus::Working => "Working…",
                                    NativeSessionStatus::Waiting => "Needs input",
                                    NativeSessionStatus::Error => "Failed",
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

    fn render_writer_callout(&self, cx: &Context<Self>) -> gpui_kit::Stateful<gpui_kit::Div> {
        div()
            .id("native-writer-callout")
            .debug_selector(|| "native-writer-callout".into())
            .w_full()
            .max_w(gpui_kit::rems(48.0))
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(cx.theme().border)
            .flex()
            .items_center()
            .gap_3()
            .child(crate::gpui::sized_icon(
                "lock",
                crate::gpui::IconSize::Small,
                cx.theme().muted_foreground,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("This is open in another app")
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child("Close it there to continue here."),
                    ),
            )
            .child(
                Button::new("native-writer-retry")
                    .debug_selector(|| "native-writer-retry".into())
                    .label("Retry")
                    .ghost()
                    .small()
                    .loading(self.resuming())
                    .disabled(!self.provider_enabled() || self.resuming())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.command("resume", vec![], window, cx);
                    })),
            )
            .child(
                Button::new("native-writer-fork")
                    .debug_selector(|| "native-writer-fork".into())
                    .label("Start side chat")
                    .ghost()
                    .small()
                    .disabled(!self.provider_enabled() || self.pending.contains("fork"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.command("fork", vec![], window, cx);
                    })),
            )
    }

    fn render_error_recovery(&self, cx: &Context<Self>) -> impl IntoElement {
        // The view writes its own messages for people; the provider's error passes the catalog.
        let message = self
            .error
            .clone()
            .filter(|_| !self.record.snapshot.provider_writer_blocked())
            .map(|message| (message, None))
            .or_else(|| {
                self.command_error
                    .clone()
                    .map(|(message, details)| (message, Some(details)))
            })
            .or_else(|| {
                (!self.record.snapshot.transport_lost
                    && !self.record.snapshot.provider_archived()
                    && !self.record.snapshot.provider_writer_blocked())
                .then_some(
                    self.record
                        .snapshot
                        .error
                        .as_deref()
                        .filter(|error| self.dismissed_error.as_deref() != Some(*error))
                        .map(|error| (diagnostic(error), Some(error.to_owned()))),
                )
                .flatten()
            });
        div().when_some(message, |body, (message, details)| {
            body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .debug_selector({
                                let message = message.clone();
                                move || format!("native-conversation-error:{message}")
                            })
                            .flex_1()
                            .text_color(cx.theme().danger)
                            .child(message),
                    )
                    .when_some(details, |row, details| {
                        row.child(
                            Button::new("copy-conversation-error-details")
                                .debug_selector(|| "copy-conversation-error-details".into())
                                .ghost()
                                .small()
                                .label("Copy details")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                        details.clone(),
                                    ));
                                }),
                        )
                    })
                    .child(
                        Button::new("conversation-recover-error")
                            .ghost()
                            .small()
                            .label(
                                if self.record.snapshot.status == NativeSessionStatus::Error {
                                    "Retry"
                                } else {
                                    "Dismiss"
                                },
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.error = None;
                                this.command_error = None;
                                this.dismissed_error.clone_from(&this.record.snapshot.error);
                                if this.record.snapshot.status == NativeSessionStatus::Error {
                                    this.command("resume", Vec::new(), window, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
        })
    }

    fn render_collapsed_composer(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let placeholder = format!("Message {}…", self.record.config.provider.display_name());
        div().flex().child(
            Button::new("expand-composer")
                .debug_selector(|| "native-collapsed-composer".into())
                .ghost()
                .w_full()
                .max_w(gpui_kit::rems(48.))
                .accessibility_label(placeholder.clone())
                .child(div().flex_1().min_w_0().truncate().child(placeholder))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.composer.read(cx).focus_handle(cx).focus(window, cx);
                })),
        )
    }

    fn render_composer(&self, window: &Window, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .px_4()
            .py_3()
            .child(
                div()
                    .w_full()
                    .max_w(gpui_kit::rems(48.))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.render_queued_messages(cx))
                    .child(
                        // An opaque base hides the drawer seam beneath the translucent editor.
                        div()
                            .relative()
                            .w_full()
                            .min_w_0()
                            .rounded_xl()
                            .bg(cx.theme().background)
                            .child(self.render_composer_body(window, cx)),
                    ),
            )
    }

    fn render_composer_body(&self, window: &Window, cx: &Context<Self>) -> gpui_kit::Div {
        if self.record.snapshot.provider_writer_blocked() {
            return div()
                .w_full()
                .min_w_0()
                .flex()
                .justify_center()
                .child(self.render_writer_callout(cx));
        }
        if self.presentation.collapse_composer_on_scroll
            && !self.list.is_following_tail()
            // Collapsing frees height; near the end that would reveal the end again and resume
            // following, so the composer would flicker back open.
            && self.distance_from_end() > f32::from(window.rem_size()) * 10.
            && self.composer.read(cx).value().is_empty()
            && !self.composer.read(cx).focus_handle(cx).is_focused(window)
            && self.attachments.is_empty()
            && self.attachment_imports.is_empty()
            && self.annotations.is_empty()
        {
            return self.render_collapsed_composer(cx);
        }
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
            .pb_2()
            .rounded_xl()
            .bg(cx.theme().muted.opacity(0.35))
            .border_1()
            .border_color(cx.theme().border)
            .when(
                !self.attachments.is_empty() || !self.attachment_imports.is_empty(),
                |body| body.child(self.render_attachments(cx)),
            )
            .when(!self.annotations.is_empty(), |body| {
                body.child(self.render_annotations(cx))
            })
            .child(self.render_composer_input(cx))
            .child(self.render_composer_controls(cx))
            .children(self.completion.clone());
        div().w_full().min_w_0().child(composer)
    }

    fn render_composer_input(&self, cx: &Context<Self>) -> Textarea {
        let owner = cx.weak_entity();
        let token_owner = owner.clone();
        let token_click_owner = owner.clone();
        Textarea::new(&self.composer)
            .token(move |context, _, cx| {
                token_owner.upgrade().map_or_else(
                    || div().into_any_element(),
                    |owner| owner.update(cx, |this, cx| this.render_prompt_token(context, cx)),
                )
            })
            .on_token_click(move |event, window, cx| {
                _ = token_click_owner.update(cx, |this, cx| {
                    this.open_attachment(event.token().id(), window, cx);
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
                self.record.config.provider.display_name()
            ))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One footer owns the adjacent composer controls"
    )]
    fn render_composer_controls(&self, cx: &Context<Self>) -> impl IntoElement {
        let send_pending = self.pending.contains("prompt") || self.pending.contains("queue");
        let send_label = if is_busy(self.record.snapshot.status) {
            if self.follow_up_behavior == bootty_config::config::FollowUpBehavior::Queue {
                "Queue message"
            } else {
                "Steer agent"
            }
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
        let resume_operation = if self.record.snapshot.provider_archived() {
            "unarchive"
        } else {
            "resume"
        };
        let options = div()
            .id("native-composer-options")
            .flex_1()
            .min_w_0()
            .overflow_x_scroll()
            .flex()
            .items_center()
            .gap_2()
            .when(self.record.config.provider != AgentKind::Pi, |row| {
                row.child(crate::gpui::with_composer_tooltip(
                    "native-permissions-tooltip",
                    div()
                        .track_focus(&self.permissions_focus)
                        .child(self.render_permission_control(cx)),
                    "Permissions",
                    vec![crate::gpui::composer_focus_action(
                        crate::gpui::ComposerControl::Permissions,
                        self.permissions_focus.clone(),
                    )],
                ))
            })
            .when(
                !self.provider_enabled() || self.record.snapshot.stopping(),
                |row| {
                    row.child(
                        div()
                            .debug_selector(|| "native-composer-notice".into())
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if self.provider_enabled() {
                                format!(
                                    "Stopping… Stop again to restart {}.",
                                    self.record.config.provider.display_name()
                                )
                            } else {
                                format!(
                                    "{} is disabled in Settings",
                                    self.record.config.provider.display_name()
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
            .child(
                crate::gpui_composer_completion::ComposerCompletion::add_button(
                    self.completion.as_ref(),
                    "native-attach",
                    inactive || !self.provider_enabled(),
                    self.record.config.plan_mode.unwrap_or(false),
                    None,
                    crate::gpui_composer_completion::ComposerFileActions::new(&self.composer, {
                        let owner = cx.weak_entity();
                        move |window, cx| {
                            _ = owner.update(cx, |this, cx| this.choose_attachments(window, cx));
                        }
                    }),
                ),
            )
            .child(options)
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(self.render_context_usage(cx))
                    .child(
                        div()
                            .id("native-model-control-scroll")
                            .min_w_0()
                            .flex_1()
                            .overflow_x_scroll()
                            .child(self.render_model_controls(cx)),
                    )
                    .child(self.speech.clone())
                    .when(interruptible, |row| {
                        // The turn can end under a resting pointer.
                        row.child(element_tooltip(
                            "native-interrupt-tooltip",
                            if self.record.snapshot.stopping() {
                                format!(
                                    "Restart {} to stop now",
                                    self.record.config.provider.display_name()
                                )
                            } else {
                                "Interrupt".to_owned()
                            },
                            Button::new("native-interrupt")
                                .debug_selector(|| "native-interrupt".into())
                                .accessibility_label("Interrupt")
                                .icon(gpui_kit::assets::IconName::Square)
                                .small()
                                .ghost()
                                .loading(interrupt_pending)
                                .disabled(interrupt_pending)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.command("interrupt", vec![], window, cx);
                                })),
                        ))
                    })
                    .when(resumable, |row| {
                        row.child(
                            Button::new("native-resume")
                                .debug_selector(|| "native-resume".into())
                                .label(if self.record.snapshot.provider_archived() {
                                    "Unarchive and resume"
                                } else {
                                    "Resume"
                                })
                                .small()
                                .primary()
                                .loading(self.pending.contains(resume_operation))
                                .disabled(
                                    !self.provider_enabled()
                                        || self.pending.contains(resume_operation),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.command(resume_operation, vec![], window, cx);
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
                                        || !self.attachment_imports.is_empty()
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

    fn render_context_usage(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        use gpui_kit::component::{progress::ProgressCircle, tooltip::Tooltip};
        let Some(usage) = self.record.snapshot.usage.as_ref() else {
            return div().into_any_element();
        };
        let Some((used, limit)) = context_tokens(usage) else {
            return div().into_any_element();
        };
        let percent = limit.map(|limit| {
            u16::try_from(
                used.saturating_mul(100)
                    .checked_div(limit)
                    .unwrap_or(0)
                    .min(100),
            )
            .unwrap_or(100)
        });
        let label = percent.map_or_else(
            || format!("{} tokens", compact_tokens(used)),
            |percent| format!("{percent}%"),
        );
        let details = limit.map_or_else(
            || format!("{} tokens used", compact_tokens(used)),
            |limit| {
                format!(
                    "{} / {} tokens used",
                    compact_tokens(used),
                    compact_tokens(limit)
                )
            },
        );
        let color = if percent.is_some_and(|percent| percent >= 90) {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        let accessible = percent.map_or_else(
            || details.clone(),
            |percent| {
                format!(
                    "Context window: {percent}% used, {}% left; {details}",
                    100u16.saturating_sub(percent)
                )
            },
        );
        div()
            .id("native-context-usage")
            .tooltip(move |window, cx| {
                let details = details.clone();
                Tooltip::element(move |_, cx| {
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .p_2()
                        .text_sm()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("Context window"),
                        )
                        .when_some(percent, |body, percent| {
                            body.child(format!(
                                "{percent}% used ({}% left)",
                                100u16.saturating_sub(percent)
                            ))
                        })
                        .child(details.clone())
                })
                .build(window, cx)
            })
            .child(
                Button::new("native-context-trigger")
                    .ghost()
                    .small()
                    .accessibility_label(accessible)
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
                            .when(percent.is_none(), |row| row.child(label)),
                    ),
            )
            .into_any_element()
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
    // Loaded provider pages remain in the view; live items replace their earlier snapshots.
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
        .cloned()
        .chain(page.with_live_transcript(live))
        .collect()
}
impl NativeAgentSessionView {
    /// How far the transcript is scrolled above its end, in pixels.
    fn distance_from_end(&self) -> f32 {
        let max = f32::from(self.list.max_offset_for_scrollbar().y);
        let top = -f32::from(self.list.scroll_px_offset_for_scrollbar().y);
        (max - top).max(0.)
    }

    fn needs_jump_to_latest(&self) -> bool {
        !self.list.is_following_tail()
            || self
                .history_page
                .as_ref()
                .is_some_and(|page| !page.at_latest)
    }

    fn render_jump_to_latest(cx: &Context<Self>) -> impl IntoElement {
        // Following new output hides this pill under a resting pointer.
        div().flex().justify_center().child(element_tooltip(
            "native-jump-latest-tooltip",
            "Jump to latest",
            Button::new("native-jump-latest")
                .debug_selector(|| "native-jump-latest".into())
                .icon(IconName::ChevronDown)
                .label("Latest")
                .outline()
                .rounded_full()
                .bg(cx.theme().background)
                .small()
                .accessibility_label("Jump to latest")
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
        ))
    }

    pub(crate) fn contains_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }
    fn load_history(&mut self, window: &Window, cx: &mut Context<Self>) {
        if matches!(
            self.record.snapshot.status,
            NativeSessionStatus::Idle | NativeSessionStatus::Working | NativeSessionStatus::Waiting
        ) && self.record.pending_initial_message.is_none()
            // A fresh Codex thread has no stored history until its first turn finishes.
            // Its live transcript already contains that turn; don't query an absent store.
            && (self.record.config.provider != AgentKind::Codex
                || self.record.snapshot.completed_turns > 0
                || self.record.snapshot.first_turn.as_ref().is_some_and(|turn| {
                    turn.outcome != bootty_agents::NativeTurnOutcome::Running
                })
                || (self.record.config.session_id.is_some()
                    && self.record.snapshot.first_turn.is_none()))
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
        let busy = self.record.snapshot.transport_lost
            || self.record.snapshot.status == NativeSessionStatus::Starting
            || is_busy(self.record.snapshot.status)
            || self.pending.contains("prompt");
        self.load_models(window, cx);
        self.load_history(window, cx);
        self.sync_activity_tick(window, cx);
        if busy && window.is_visible() && self.moving(cx) {
            crate::motion::request_frame(cx.entity_id(), cx);
        }
        let enabled = self.provider_enabled()
            && !matches!(
                self.record.snapshot.status,
                NativeSessionStatus::Starting
                    | NativeSessionStatus::Stopped
                    | NativeSessionStatus::Error
            );
        self.speech
            .update(cx, |speech, cx| speech.set_enabled(enabled, cx));
        let wide = self.wide;
        div()
            .id(gpui_kit::SharedString::from(self.record.id.clone()))
            .relative()
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .drag_over::<gpui_kit::ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().accent))
            .on_drop(
                cx.listener(|this, paths: &gpui_kit::ExternalPaths, window, cx| {
                    cx.stop_propagation();
                    for path in &paths.0 {
                        this.attach_path(path.clone(), window, cx);
                    }
                }),
            )
            .when(wide, |view| view.pr(gpui_kit::rems(20.)))
            .child(self.render_context_geometry(cx))
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
            .when(self.record.snapshot.provider_archived(), |body| {
                body.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("This conversation is archived in Codex. Unarchive it to continue."),
                )
            })
            .child(self.render_transcript(window, cx))
            .when(!self.record.snapshot.requests.is_empty(), |body| {
                body.child(self.requests.clone())
            })
            .when(busy || !self.subagents().is_empty(), |body| {
                body.child(self.render_activity(busy, window, cx))
            })
            .when(!wide, |body| {
                body.child(self.render_related_conversations(cx).px_4())
            })
            .child(self.render_error_recovery(cx))
            .child(self.render_composer(window, cx))
            .child(self.render_selection_toolbar(window, cx))
            .when(wide, |body| body.child(self.render_context_card(cx)))
    }
}

/// Token counts read at a glance: 950, 12k, 1.2M.
/// Give `control` a tooltip that ends with it. `Button::tooltip` shows through a window overlay
/// that hides only on hover-out, so a button that disappears under a still pointer leaves it up.
/// A fenced block's language and a copy button, in its corner.
fn code_block_actions(
    block: &gpui_kit::base::text::CodeBlock,
    _: &mut Window,
    cx: &mut App,
) -> gpui_kit::Div {
    let code = block.code();
    let id = block.span.map_or(0, |span| span.start);
    let lang = block.lang().unwrap_or_default();
    div()
        .flex()
        .items_center()
        .gap_1()
        .when_some(block.lang(), |row, lang| {
            row.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(lang),
            )
        })
        .child(element_tooltip(
            gpui_kit::SharedString::from(format!("copy-code-tooltip:{id}")),
            "Copy code",
            Button::new(gpui_kit::SharedString::from(format!("copy-code:{id}")))
                .debug_selector(move || format!("copy-code:{lang}"))
                .icon(IconName::Copy)
                .ghost()
                .xsmall()
                .accessibility_label("Copy code")
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(code.to_string()));
                }),
        ))
}

/// Long tool text scrolls inside its row instead of stretching the transcript.
fn capped_scroll(
    id: String,
    max_rems: f32,
    content: impl IntoElement,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(gpui_kit::SharedString::from(id.clone()))
        .debug_selector(move || id)
        .max_h(gpui_kit::rems(max_rems))
        .overflow_y_scroll()
        .child(content)
}

fn element_tooltip(
    id: impl Into<gpui_kit::ElementId>,
    label: impl Into<gpui_kit::SharedString>,
    control: impl IntoElement,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let label = label.into();
    div()
        .id(id)
        .flex_shrink_0()
        .child(control)
        .tooltip(move |window, cx| {
            let label = label.clone();
            gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                let selector = format!("tooltip:{label}");
                div().debug_selector(move || selector).child(label.clone())
            })
            .build(window, cx)
        })
}

/// Text from providers, commands, and lower layers, as people should read it. The view's own
/// messages are already written for people and skip this.
fn diagnostic(text: impl AsRef<str>) -> String {
    crate::error_catalog::ErrorNotice::from_text(text.as_ref()).to_string()
}

/// Keep a failed action's scope beside its original diagnostic.
fn command_failure(operation: &str, outcome: &CommandOutcome) -> (String, String) {
    let details = crate::commands::command_outcome_message(outcome)
        .unwrap_or_else(|| "Agent command failed".to_owned());
    let action = match operation {
        "history" => "Could not load conversation history",
        "prompt" | "queue" => "Could not send the message",
        "rename" => "Could not rename the conversation",
        "interrupt" => "Could not interrupt the agent",
        "fork" => "Could not start the side chat",
        "resume" => "Could not resume the conversation",
        _ => "Could not update the conversation",
    };
    (
        format!("{action}. {}", diagnostic(&details)),
        format!("{operation}: {details}"),
    )
}

/// A failed command's message as people should read it.
fn outcome_error(outcome: &CommandOutcome) -> Option<String> {
    crate::commands::command_outcome_message(outcome).map(diagnostic)
}

fn compact_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 1_000_000 {
        format!(
            "{}k",
            tokens.saturating_add(500).checked_div(1000).unwrap_or(0)
        )
    } else {
        let tenths = tokens
            .saturating_add(50_000)
            .checked_div(100_000)
            .unwrap_or(0);
        format!(
            "{}.{}M",
            tenths.checked_div(10).unwrap_or(0),
            tenths.checked_rem(10).unwrap_or(0)
        )
    }
}

fn context_tokens(usage: &Value) -> Option<(u64, Option<u64>)> {
    let counters = usage.get("last").unwrap_or(usage);
    let used = counters
        .get("totalTokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            let input = counters.get("input").and_then(Value::as_u64)?;
            Some(
                ["output", "cacheRead", "cacheWrite"]
                    .into_iter()
                    .fold(input, |tokens, field| {
                        tokens.saturating_add(
                            counters.get(field).and_then(Value::as_u64).unwrap_or(0),
                        )
                    }),
            )
        });
    let used = used?;
    let limit = usage
        .get("modelContextWindow")
        .and_then(Value::as_u64)
        .filter(|limit| *limit > 0);
    Some((used, limit))
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
/// Claude names MCP tools `mcp__<server>__<tool>`; Bootty's own server name carries a lease
/// suffix (`bootty_<hash>`), so only other servers are named in labels.
fn tool_parts(name: &str) -> (Option<&str>, &str) {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map_or((None, name), |(server, tool)| {
            (
                Some(server).filter(|server| !server.starts_with("bootty_")),
                tool,
            )
        })
}

fn tool_label(name: &str) -> std::borrow::Cow<'_, str> {
    let (server, name) = tool_parts(name);
    std::borrow::Cow::Borrowed(match name {
        "commandExecution" | "Bash" | "bash" | "exec_command" => "Run",
        "Read" | "read" | "read_file" => "Read",
        "fileChange" | "Edit" | "edit" | "Write" | "write" | "apply_patch" => "Edit",
        "Grep" | "grep" | "Glob" | "glob" | "search" | "web_search" | "webSearch" => "Search",
        "imageView" => "View image",
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
        _ => {
            return server.map_or(std::borrow::Cow::Borrowed(name), |server| {
                std::borrow::Cow::Owned(format!("{server} · {name}"))
            });
        }
    })
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
            .or_else(|| input.get("url").and_then(Value::as_str))
    });
    let code = input
        .as_ref()
        .and_then(|input| input.get("code"))
        .and_then(Value::as_str);
    let (_, name) = tool_parts(&tool.name);
    let icon = if command.is_some()
        || matches!(
            name,
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
    } else if matches!(name, "computer_snapshot" | "computer_input") {
        gpui_kit::assets::IconName::Monitor
    } else if name == "browser_snapshot" {
        gpui_kit::assets::IconName::Globe
    } else if matches!(
        name,
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
    } else if query.is_some() {
        gpui_kit::assets::IconName::Search
    } else if path.is_some() || name == "fileChange" {
        gpui_kit::assets::IconName::File
    } else {
        gpui_kit::assets::IconName::Wrench
    };
    // A search's pattern says more than the directory it ran in.
    let summary = command
        .or(query)
        .or(path)
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

/// The markdown a message shows: streamed replies reveal their visible part, and replies link the
/// file paths in their inline code.
fn streamed_markdown(
    item: &NativeTranscriptItem,
    streaming: bootty_config::config::ResponseStreaming,
) -> (String, crate::transcript_links::LinkMarks) {
    let text = transcript_markdown(item);
    let reply = matches!(item.role.as_str(), "assistant" | "plan") && item.tool.is_none();
    let visible = if item.role == "assistant" && item.tool.is_none() {
        streaming.visible_text(&text, item.complete)
    } else {
        text.as_str()
    };
    if reply {
        crate::transcript_links::link_code_paths(visible)
    } else {
        (
            visible.to_owned(),
            crate::transcript_links::LinkMarks::default(),
        )
    }
}

/// One changed file's row: its path relative to `cwd`, its +/− counts, and a diff view sized
/// to the patch.
fn tool_diff(
    path: &str,
    file: bootty_git::diff::FileDiff,
    cwd: &std::path::Path,
    window: &mut Window,
    cx: &mut App,
) -> ToolDiff {
    let counts = (!file.patch_unavailable).then(|| {
        file.hunks.iter().flat_map(|hunk| &hunk.lines).fold(
            (0_usize, 0_usize),
            |(added, removed), line| {
                (
                    added.saturating_add(usize::from(line.old_line.is_none())),
                    removed.saturating_add(usize::from(line.new_line.is_none())),
                )
            },
        )
    });
    let rows = file
        .hunks
        .iter()
        .map(|hunk| hunk.lines.len().saturating_add(1))
        .fold(0_usize, usize::saturating_add)
        .clamp(2, 14);
    let rows = num_traits::ToPrimitive::to_f32(&rows).unwrap_or(14.0);
    let view = cx.new(|cx| {
        let mut view = crate::gpui_git_panel::GitDiffPanel::new(window, cx);
        view.show_file_diff(file, window, cx);
        view
    });
    let path = std::path::Path::new(path);
    ToolDiff {
        path: path
            .strip_prefix(cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned(),
        view,
        rows,
        counts,
    }
}

/// Each changed file under its path, in a box sized to its patch.
fn tool_diff_files<'a>(
    files: &'a [ToolDiff],
    cx: &'a App,
) -> impl Iterator<Item = gpui_kit::Div> + 'a {
    files.iter().map(|file| {
        div()
            .debug_selector({
                let path = file.path.clone();
                move || format!("native-inline-diff:{path}")
            })
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
                    .child(file.path.clone()),
            )
            .child(
                div()
                    .h(cx.theme().mono_font_size.mul(file.rows.mul_add(1.5, 1.0)))
                    .min_w_0()
                    .child(file.view.clone()),
            )
    })
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

pub fn subagent_label(agent: &bootty_agents::NativeSubagent) -> String {
    let title = agent.title.trim();
    let label = if title.is_empty() || title.starts_with("/root/") || title == agent.id {
        agent
            .prompt
            .lines()
            .find(|line| !line.trim().is_empty())
            .map_or(title, str::trim)
    } else {
        title
    };
    if label.is_empty() {
        return "Subagent".into();
    }
    label.chars().take(80).collect()
}

pub fn subagent_avatar(agent: &bootty_agents::NativeSubagent, cx: &App) -> gpui_kit::Div {
    let index = agent
        .id
        .bytes()
        .fold(14_695_981_039_346_656_037_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        })
        .wrapping_rem(5);
    let color = match index {
        0 => cx.theme().chart_1,
        1 => cx.theme().chart_2,
        2 => cx.theme().chart_3,
        3 => cx.theme().chart_4,
        _ => cx.theme().chart_5,
    };
    let initials = subagent_label(agent)
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|word| word.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect::<String>();
    div()
        .size_6()
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(color.opacity(0.18))
        .text_color(color)
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .child(initials)
}

/// One status vocabulary for subagents wherever they are listed.
#[must_use]
pub const fn subagent_status_label(status: bootty_agents::NativeToolStatus) -> &'static str {
    match status {
        bootty_agents::NativeToolStatus::Running => "Working…",
        bootty_agents::NativeToolStatus::Completed => "Done",
        bootty_agents::NativeToolStatus::Failed => "Failed",
        bootty_agents::NativeToolStatus::Interrupted => "Interrupted",
        bootty_agents::NativeToolStatus::Declined => "Declined",
    }
}

fn subagent_header(agent: &bootty_agents::NativeSubagent, open: bool, cx: &App) -> gpui_kit::Div {
    let status = subagent_status_label(agent.status);
    let title = subagent_label(agent);
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
        .child(subagent_avatar(agent, cx))
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
