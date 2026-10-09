use std::{collections::VecDeque, path::Path};

use bootty_browser::{Annotation, AnnotationEvent, AnnotationStore, apply_annotation_event};

use super::*;

pub(super) struct BrowserAnnotations {
    store: AnnotationStore,
    records: Vec<Annotation>,
    loaded: bool,
    writing: bool,
    queue: VecDeque<AnnotationMutation>,
    status: Option<String>,
    error: Option<String>,
    selection: Option<(u64, String)>,
    editor: Option<u64>,
    recipient: Option<bootty_control::CommandTarget>,
    capture: Option<AnnotationCaptureIntent>,
}

#[derive(Clone)]
pub(super) struct AnnotationCaptureIntent {
    pub id: u64,
    pub record: Annotation,
    pub recipient: bootty_control::CommandTarget,
    pub window: bootty_control::CommandTarget,
    #[cfg(target_os = "macos")]
    pub store: AnnotationStore,
}

enum AnnotationMutation {
    Page {
        page: u64,
        address: String,
        event: AnnotationEvent,
        conversation: Option<bootty_control::CommandTarget>,
    },
    Detach {
        target: bootty_control::CommandTarget,
        records: Vec<Annotation>,
    },
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "Image association awaits an exact-window capture backend on this host"
        )
    )]
    AttachImage {
        intent: Box<AnnotationCaptureIntent>,
        image: bootty_browser::AnnotationImage,
        response: async_channel::Sender<Result<(), String>>,
    },
}

impl BrowserAnnotations {
    pub(super) fn new(directory: &Path) -> Self {
        Self {
            store: AnnotationStore::new(directory),
            records: Vec::new(),
            loaded: false,
            writing: false,
            queue: VecDeque::new(),
            status: None,
            error: None,
            selection: None,
            editor: None,
            recipient: None,
            capture: None,
        }
    }
}

impl BrowserPanel {
    pub(super) fn clear_annotation_scopes(&mut self) {
        self.annotations.selection = None;
        self.annotations.editor = None;
        self.annotations.recipient = None;
    }
    pub(super) fn load_annotations(&self, window: &Window, cx: &Context<Self>) {
        let store = self.annotations.store.clone();
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { store.load() })
                .await;
            _ = owner.update(cx, |this, cx| {
                match result {
                    Ok(records) => {
                        this.annotations.records = records;
                        this.annotations.loaded = true;
                    }
                    Err(error) => this.annotations.error = Some(error.to_string()),
                }
                cx.emit(BrowserAnnotationsChanged);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn receive_annotation(
        &mut self,
        page: u64,
        event: AnnotationEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(&event, AnnotationEvent::Save { .. }) && self.annotations.capture.is_some() {
            return;
        }
        let current = self
            .tabs
            .iter()
            .find(|tab| tab.id == page)
            .and_then(|tab| tab.view.as_ref())
            .and_then(|view| view.current_address().ok());
        if current.as_deref() != Some(event.address()) {
            return;
        }
        match &event {
            AnnotationEvent::CancelPick { .. } => {
                if !self
                    .annotations
                    .selection
                    .as_ref()
                    .is_some_and(|(target, address)| *target == page && address == event.address())
                {
                    return;
                }
                self.annotations.selection = None;
                return;
            }
            AnnotationEvent::Pick { address, .. } => {
                if self.annotations.selection.as_ref() != Some(&(page, address.clone())) {
                    return;
                }
                self.annotations.selection = None;
            }
            AnnotationEvent::Draft { id, .. }
            | AnnotationEvent::Save { id, .. }
            | AnnotationEvent::Cancel { id, .. } => {
                let Ok(id) = id.parse::<u64>() else {
                    return;
                };
                if self.annotations.editor != Some(id)
                    || !self.annotations.records.iter().any(|record| {
                        record.id == id && record.page == page && record.address == event.address()
                    })
                {
                    return;
                }
                if matches!(&event, AnnotationEvent::Cancel { .. }) {
                    self.annotations.editor = None;
                }
            }
        }
        let conversation = if matches!(&event, AnnotationEvent::Save { .. }) {
            let Some(target) = self
                .annotations
                .recipient
                .clone()
                .filter(|target| self.conversation_target.as_ref() == Some(target))
            else {
                if let AnnotationEvent::Save { id, .. } = &event
                    && let Ok(id) = id.parse::<u64>()
                    && let Some(view) = self.selected_tab().and_then(|tab| tab.view.as_ref())
                {
                    _ = view.annotation_edit_result(id, Some("Selected agent changed"));
                }
                return;
            };
            Some(target)
        } else if matches!(&event, AnnotationEvent::Pick { .. }) {
            self.annotations.recipient.clone()
        } else {
            None
        };
        let address = event.address().to_owned();
        self.queue_annotation(
            AnnotationMutation::Page {
                page,
                address,
                event,
                conversation,
            },
            window,
            cx,
        );
    }

    fn queue_annotation(
        &mut self,
        mutation: AnnotationMutation,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !self.annotations.loaded {
            return;
        }
        if self.annotations.queue.len() >= 64 {
            let error = "Annotations are still saving. Try again.";
            self.annotation_failure(&mutation, error);
            self.annotations.error = Some(error.into());
            cx.emit(BrowserAnnotationsChanged);
            cx.notify();
            return;
        }
        self.annotations.queue.push_back(mutation);
        self.persist_annotation(window, cx);
    }

    fn persist_annotation(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.annotations.writing {
            return;
        }
        let Some(mutation) = self.annotations.queue.pop_front() else {
            return;
        };
        if let AnnotationMutation::AttachImage { intent, .. } = &mutation
            && self.annotation_capture_intent(intent.id).is_none()
        {
            self.annotation_failure(&mutation, "Agent changed");
            self.persist_annotation(window, cx);
            return;
        }
        if let AnnotationMutation::Page {
            conversation: Some(target),
            event: AnnotationEvent::Save { .. },
            ..
        } = &mutation
            && self.conversation_target.as_ref() != Some(target)
        {
            self.annotation_failure(&mutation, "Agent changed");
            self.persist_annotation(window, cx);
            return;
        }
        let result = annotation_candidate(&self.annotations.records, &mutation);
        let candidate = match result {
            Ok(candidate) => candidate,
            Err(error) => {
                self.annotation_failure(&mutation, &error.to_string());
                self.annotations.error = Some(error.to_string());
                cx.emit(BrowserAnnotationsChanged);
                self.persist_annotation(window, cx);
                cx.notify();
                return;
            }
        };
        if candidate == self.annotations.records {
            if let AnnotationMutation::Page {
                page,
                address,
                event: AnnotationEvent::Save { id, .. },
                conversation,
            } = &mutation
                && let Ok(id) = id.parse::<u64>()
            {
                self.begin_annotation_capture(*page, address, id, conversation.clone(), window, cx);
            }
            self.persist_annotation(window, cx);
            return;
        }
        self.commit_annotation(mutation, candidate, window, cx);
    }

    fn commit_annotation(
        &mut self,
        mutation: AnnotationMutation,
        candidate: Vec<Annotation>,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let saved = match &mutation {
            AnnotationMutation::Page {
                page,
                address,
                event: AnnotationEvent::Save { id, .. },
                ..
            } => id
                .parse::<u64>()
                .ok()
                .map(|id| (*page, address.clone(), id)),
            _ => None,
        };
        let attaching = match &mutation {
            AnnotationMutation::AttachImage {
                intent, response, ..
            } => Some((intent.clone(), response.clone())),
            _ => None,
        };
        let recipient = match &mutation {
            AnnotationMutation::Page {
                event: AnnotationEvent::Save { .. },
                conversation,
                ..
            } => conversation.clone(),
            _ => None,
        };
        let editor = match mutation {
            AnnotationMutation::Page {
                event: AnnotationEvent::Pick { .. },
                conversation,
                ..
            } => candidate
                .last()
                .cloned()
                .map(|record| (record, conversation)),
            _ => None,
        };
        self.annotations.writing = true;
        let generation = self.interaction_generation;
        let store = self.annotations.store.clone();
        let expected = self.annotations.records.clone();
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    store
                        .commit(&expected, &candidate)
                        .map(|outcome| (candidate, outcome))
                })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if let Some((intent, response)) = &attaching
                    && this.restore_stale_attachment_if_needed(
                        intent.as_ref(),
                        response,
                        &result,
                        window,
                        cx,
                    )
                {
                    return;
                }
                this.annotations.writing = false;
                match result {
                    Ok((records, outcome)) => {
                        this.annotations.records = records;
                        if let bootty_write::CommitOutcome::CommittedWithDurabilityWarning(error) =
                            outcome
                        {
                            this.annotations.status = Some(format!(
                                "Saved, but disk durability was not confirmed: {error}"
                            ));
                        }
                        this.annotations.error = None;
                        if let Some((editor, recipient)) = editor
                            && this.interaction_generation == generation
                        {
                            this.open_annotation_editor(&editor, recipient, cx);
                        }
                    }
                    Err(error) => this.annotations.error = Some(error.to_string()),
                }
                if let Some((intent, response)) = attaching {
                    this.finish_annotation_attachment(&intent, &response);
                } else if let Some((page, address, id)) = saved {
                    if this.annotations.error.is_none() {
                        this.begin_annotation_capture(page, &address, id, recipient, window, cx);
                    } else {
                        this.finish_annotation_save(Some((page, address, id)));
                    }
                }
                cx.emit(BrowserAnnotationsChanged);
                this.persist_annotation(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_annotation_attachment(
        &mut self,
        intent: &AnnotationCaptureIntent,
        response: &async_channel::Sender<Result<(), String>>,
    ) {
        let result = self.annotations.error.clone().map_or(Ok(()), Err);
        self.finish_annotation_capture_ui(intent, result.as_ref().err().map(String::as_str));
        _ = response.try_send(result);
    }

    fn restore_stale_attachment_if_needed(
        &self,
        intent: &AnnotationCaptureIntent,
        response: &async_channel::Sender<Result<(), String>>,
        result: &Result<
            (Vec<Annotation>, bootty_write::CommitOutcome),
            bootty_browser::AnnotationError,
        >,
        window: &Window,
        cx: &Context<Self>,
    ) -> bool {
        let Ok((records, _)) = result else {
            return false;
        };
        if self.annotation_capture_intent(intent.id).is_some() {
            return false;
        }
        let Some(attached) = records
            .iter()
            .find(|record| record.id == intent.record.id)
            .cloned()
        else {
            return false;
        };
        self.restore_cancelled_attachment(intent.clone(), attached, response.clone(), window, cx);
        true
    }

    fn restore_cancelled_attachment(
        &self,
        intent: AnnotationCaptureIntent,
        attached: Annotation,
        response: async_channel::Sender<Result<(), String>>,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let store = self.annotations.store.clone();
        let prior = intent.record.clone();
        cx.spawn_in(window, async move |owner, cx| {
            let (result, latest) = cx
                .background_executor()
                .spawn(async move {
                    match store.restore_attachment_if_unchanged(&attached, &prior) {
                        Ok(records) => (Ok(records), None),
                        Err(error) => (Err(error.to_string()), store.load().ok()),
                    }
                })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                this.annotations.writing = false;
                let failure = match result {
                    Ok(records) => {
                        this.annotations.records = records;
                        None
                    }
                    Err(error) => {
                        if let Some(records) = latest {
                            this.annotations.records = records;
                        }
                        Some(format!(
                            "Could not restore the cancelled annotation: {error}"
                        ))
                    }
                };
                this.finish_annotation_capture_ui(&intent, failure.as_deref());
                _ =
                    response
                        .try_send(Err(failure
                            .unwrap_or_else(|| "Annotation capture was cancelled".to_owned())));
                cx.emit(BrowserAnnotationsChanged);
                this.persist_annotation(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_annotation_save(&mut self, saved: Option<(u64, String, u64)>) {
        let Some((page, address, id)) = saved else {
            return;
        };
        let error = self.annotations.error.as_deref();
        if let Some(view) = self
            .tabs
            .iter()
            .find(|tab| tab.id == page && tab.address == address)
            .and_then(|tab| tab.view.as_ref())
        {
            _ = view.annotation_edit_result(id, error);
        }
        if error.is_none() && self.annotations.editor == Some(id) {
            self.annotations.editor = None;
            self.annotations.recipient = None;
        }
    }

    pub(super) fn annotation_recipient_available(&self) -> bool {
        self.annotations.recipient.is_some()
            && self.annotations.recipient == self.conversation_target
    }

    fn annotation_failure(&mut self, mutation: &AnnotationMutation, error: &str) {
        if let AnnotationMutation::AttachImage {
            intent, response, ..
        } = mutation
        {
            self.finish_annotation_capture_ui(intent, Some(error));
            _ = response.try_send(Err(error.to_owned()));
        }
        if let AnnotationMutation::Page {
            page,
            address,
            event: AnnotationEvent::Save { id, .. },
            ..
        } = mutation
            && let Ok(id) = id.parse::<u64>()
            && let Some(view) = self
                .tabs
                .iter()
                .find(|tab| tab.id == *page)
                .and_then(|tab| tab.view.as_ref())
            && view.current_address().ok().as_deref() == Some(address.as_str())
        {
            _ = view.annotation_edit_result(id, Some(error));
        }
    }

    fn open_annotation_editor(
        &mut self,
        record: &Annotation,
        recipient: Option<bootty_control::CommandTarget>,
        cx: &mut Context<Self>,
    ) {
        let page = record.page;
        if page != self.selected || !self.active || !self.host_visible {
            return;
        }
        let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.id == page && tab.address == record.address)
        else {
            return;
        };
        let Some(view) = &tab.view else {
            return;
        };
        if let Err(error) = view.edit_annotation(
            record,
            cx.theme().is_dark(),
            recipient.is_some() && recipient == self.conversation_target,
        ) {
            self.annotations.error = Some(error.to_string());
            return;
        }
        self.annotations.editor = Some(record.id);
        self.annotations.recipient = recipient;
        self.interaction = BrowserInteraction::RestoringPageFocus;
        self.sync_visibility(cx);
    }

    pub(super) fn annotation_navigation(&mut self, page: u64) {
        if self
            .annotations
            .selection
            .as_ref()
            .is_some_and(|(selected, _)| *selected == page)
        {
            self.annotations.selection = None;
        }
        if self.annotations.editor.is_some_and(|id| {
            self.annotations
                .records
                .iter()
                .any(|record| record.id == id && record.page == page)
        }) {
            self.annotations.editor = None;
        }
    }

    fn pick_annotation(&mut self, cx: &mut Context<Self>) {
        self.record_interaction();
        if let Some(tab) = self.selected_tab() {
            self.annotations.selection = Some((tab.id, tab.address.clone()));
            self.annotations.editor = None;
            self.annotations.recipient = self.conversation_target.clone();
        }
        if let Some(view) = self.selected_tab().and_then(|tab| tab.view.as_ref())
            && let Err(error) = view.pick_annotation(cx.theme().is_dark())
        {
            self.annotations.error = Some(error.to_string());
            cx.notify();
        }
    }

    pub(super) fn render_annotation_actions(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div().flex_shrink_0().child(
            Button::new("browser-annotate")
                .icon(Icon::new(gpui_kit::assets::IconName::MousePointerClick))
                .ghost()
                .small()
                .size_6()
                .flex_shrink_0()
                .accessibility_label("Annotate")
                .disabled(
                    !self.annotations.loaded
                        || self.selected_tab().is_none_or(|tab| tab.view.is_none()),
                )
                .tooltip("Annotate elements, regions, and drawings")
                .on_click(cx.listener(|this, _, _, cx| this.pick_annotation(cx))),
        )
    }

    pub(crate) fn annotation_status(&self) -> Option<&str> {
        self.annotations
            .error
            .as_deref()
            .or(self.annotations.status.as_deref())
    }

    pub(crate) fn annotation_records(&self) -> &[Annotation] {
        &self.annotations.records
    }

    pub(crate) fn detach_annotations(
        &mut self,
        target: bootty_control::CommandTarget,
        records: Vec<Annotation>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if target.kind != bootty_control::ResourceKind::Session
            || records
                .iter()
                .any(|record| !record.is_attached_to(&target.handle))
        {
            return;
        }
        self.queue_annotation(AnnotationMutation::Detach { target, records }, window, cx);
    }

    fn begin_annotation_capture(
        &mut self,
        page: u64,
        address: &str,
        id: u64,
        recipient: Option<bootty_control::CommandTarget>,
        window: &Window,
        cx: &Context<Self>,
    ) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(1);
        if self.annotations.capture.is_some() {
            return;
        }
        let Some(record) = self
            .annotations
            .records
            .iter()
            .find(|record| record.id == id && record.page == page && record.address == address)
            .cloned()
        else {
            return;
        };
        let Some(recipient) =
            recipient.filter(|target| self.conversation_target.as_ref() == Some(target))
        else {
            self.annotations.error = Some("Agent changed".into());
            self.finish_annotation_save(Some((page, address.to_owned(), id)));
            return;
        };
        let Some(target) = self.window_target.clone() else {
            self.annotations.error = Some("Window unavailable".into());
            self.finish_annotation_save(Some((page, address.to_owned(), id)));
            return;
        };
        let Ok(capture_id) =
            NEXT_CAPTURE.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        else {
            self.annotations.error = Some("Capture unavailable".into());
            self.finish_annotation_save(Some((page, address.to_owned(), id)));
            return;
        };
        let intent = AnnotationCaptureIntent {
            id: capture_id,
            record,
            recipient,
            window: target.clone(),
            #[cfg(target_os = "macos")]
            store: self.annotations.store.clone(),
        };
        self.annotations.error = None;
        self.annotations.capture = Some(intent.clone());
        let mut invocation = bootty_control::CommandInvocation::new(
            "browser.capture",
            vec![page.to_string(), capture_id.to_string()],
            bootty_control::Caller::Internal,
        );
        invocation.target = Some(target);
        let now = std::time::Instant::now();
        let receiver = match self.sender.submit(
            invocation,
            now.checked_add(std::time::Duration::from_secs(30))
                .unwrap_or(now),
            bootty_control::CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.finish_annotation_capture_ui(
                    &intent,
                    Some(&format!("Capture unavailable: {error:?}")),
                );
                return;
            }
        };
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = owner.update(cx, |this, cx| {
                let error = result.map_or_else(
                    |error| Some(error.to_string()),
                    |outcome| crate::commands::command_outcome_message(&outcome),
                );
                if this
                    .annotations
                    .capture
                    .as_ref()
                    .is_some_and(|pending| pending.id == intent.id)
                {
                    this.finish_annotation_capture_ui(&intent, error.as_deref());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn annotation_capture_intent(&self, id: u64) -> Option<AnnotationCaptureIntent> {
        self.annotations
            .capture
            .as_ref()
            .filter(|intent| {
                intent.id == id
                    && self.annotations.editor == Some(intent.record.id)
                    && self.conversation_target.as_ref() == Some(&intent.recipient)
                    && self.window_target.as_ref() == Some(&intent.window)
                    && self.annotations.records.contains(&intent.record)
            })
            .cloned()
    }

    #[cfg(target_os = "macos")]
    pub(super) fn annotation_capture_committed(
        &mut self,
        intent: AnnotationCaptureIntent,
        result: Result<
            (
                bootty_browser::AnnotationImage,
                Vec<bootty_write::CommitOutcome>,
            ),
            String,
        >,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> async_channel::Receiver<Result<(), String>> {
        let (response, receiver) = async_channel::bounded(1);
        let result = if self.annotation_capture_intent(intent.id).is_some() {
            result
        } else {
            Err("Agent or page changed".into())
        };
        match result {
            Ok((image, outcomes)) => {
                for outcome in outcomes {
                    if let bootty_write::CommitOutcome::CommittedWithDurabilityWarning(error) =
                        outcome
                    {
                        self.annotations.status =
                            Some(format!("Saved; disk durability unconfirmed: {error}"));
                    }
                }
                self.queue_annotation(
                    AnnotationMutation::AttachImage {
                        intent: Box::new(intent),
                        image,
                        response,
                    },
                    window,
                    cx,
                );
            }
            Err(error) => {
                self.finish_annotation_capture_ui(&intent, Some(&error));
                _ = response.try_send(Err(error));
            }
        }
        receiver
    }

    pub(super) fn finish_annotation_capture_ui(
        &mut self,
        intent: &AnnotationCaptureIntent,
        error: Option<&str>,
    ) {
        if self
            .annotations
            .capture
            .as_ref()
            .is_none_or(|pending| pending.id != intent.id)
        {
            return;
        }
        if let Some(view) = self
            .tabs
            .iter()
            .find(|tab| tab.id == intent.record.page)
            .and_then(|tab| tab.view.as_ref())
        {
            _ = view.finish_annotation_capture(intent.record.id, intent.id);
            _ = view.annotation_edit_result(intent.record.id, error);
        }
        if self
            .annotations
            .capture
            .as_ref()
            .is_some_and(|pending| pending.id == intent.id)
        {
            self.annotations.capture = None;
            self.annotations.error = error.map(str::to_owned);
            if error.is_none() {
                self.annotations.editor = None;
                self.annotations.recipient = None;
            }
        }
    }
}

pub struct BrowserAnnotationsChanged;
impl EventEmitter<BrowserAnnotationsChanged> for BrowserPanel {}

fn annotation_candidate(
    records: &[Annotation],
    mutation: &AnnotationMutation,
) -> Result<Vec<Annotation>, bootty_browser::AnnotationError> {
    match mutation {
        AnnotationMutation::Page {
            page,
            address,
            event,
            conversation,
        } => {
            let saving = if let AnnotationEvent::Save { address, id, note } = event {
                AnnotationEvent::Draft {
                    address: address.clone(),
                    id: id.clone(),
                    note: note.clone(),
                }
            } else {
                event.clone()
            };
            let mut candidate = apply_annotation_event(records, *page, address, saving)?;
            if let AnnotationEvent::Save { id, note, .. } = event {
                let target = conversation
                    .as_ref()
                    .ok_or(bootty_browser::AnnotationError::Invalid)?;
                if target.kind != bootty_control::ResourceKind::Session {
                    return Err(bootty_browser::AnnotationError::Invalid);
                }
                let id = id
                    .parse::<u64>()
                    .map_err(|_| bootty_browser::AnnotationError::Invalid)?;
                candidate
                    .iter_mut()
                    .find(|record| record.id == id)
                    .ok_or(bootty_browser::AnnotationError::Missing)?
                    .prepare_attachment(note.clone(), &target.handle)?;
            }
            Ok(candidate)
        }
        AnnotationMutation::Detach {
            target,
            records: submitted,
        } => {
            let mut candidate = records.to_vec();
            for snapshot in submitted {
                if !snapshot.is_attached_to(&target.handle) {
                    return Err(bootty_browser::AnnotationError::Invalid);
                }
                if let Some(record) = candidate.iter_mut().find(|record| record.id == snapshot.id) {
                    record.detach_if_unchanged(snapshot)?;
                }
            }
            Ok(candidate)
        }
        AnnotationMutation::AttachImage { intent, image, .. } => {
            let mut candidate = records.to_vec();
            candidate
                .iter_mut()
                .find(|record| record.id == intent.record.id)
                .ok_or(bootty_browser::AnnotationError::Missing)?
                .finish_attachment(&intent.record, image.clone())?;
            Ok(candidate)
        }
    }
}
