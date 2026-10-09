//! Native pull request review. GitHub state and mutations belong to bootty-git.
mod creation;
mod merge;
mod metadata;
use creation::{CreationEditor, CreationSubmission};
use metadata::{MetadataEditor, MetadataMode};

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_git::{
    diff::FileDiff,
    github::{
        ActivityPage, CodeComment, MergeMethod, MetadataRequest, PullRequestAction,
        PullRequestActionRequest, PullRequestSnapshot, PullRequestSummary, ReviewRequest,
        ReviewVerdict, ThreadRequest, ViewedFilesPage,
    },
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    collapsible::Collapsible,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, Styled, Subscription,
    Window, div, prelude::*,
};

use super::{
    GitDiffPanel, GitPanelContext, OpenDiff,
    diff::{DraftCodeComment, PreparedReviewDiff},
};

struct PendingReviewComment {
    id: u64,
    comment: CodeComment,
    stale: bool,
}

struct SubmittedReview {
    body: String,
    comment_ids: Vec<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PullRequestPage {
    Search,
    Review,
}

pub(super) struct PullRequestView {
    context: GitPanelContext,
    sender: BoundAppCommandSender,
    diff: Entity<GitDiffPanel>,
    search: Entity<InputState>,
    body: Entity<TextareaState>,
    verdict: ReviewVerdict,
    results: Vec<PullRequestSummary>,
    snapshot: Option<PullRequestSnapshot>,
    page: PullRequestPage,
    activity: Option<ActivityPage>,
    workflows: Option<Vec<bootty_git::github::WorkflowApproval>>,
    stack: Option<bootty_git::github::PullRequestStack>,
    stack_loaded: bool,
    viewed: BTreeMap<String, bool>,
    viewed_cursor: Option<String>,
    show_files: bool,
    file_request: u64,
    selected_file: Option<String>,
    expanded_file: Option<bootty_git::github::PullRequestDiffRequest>,
    comments: Vec<PendingReviewComment>,
    next_comment_id: u64,
    submitted_review: Option<SubmittedReview>,
    replies: BTreeMap<String, Entity<TextareaState>>,
    pending: bool,
    merge: Option<merge::PendingMerge>,
    merge_poll: Option<gpui_kit::Task<()>>,
    expanded_threads: BTreeMap<String, bool>,
    pending_comment_page: Option<String>,
    pending_reply: Option<(String, String)>,
    submitted_metadata: Option<MetadataRequest>,
    metadata_editor: Option<Entity<MetadataEditor>>,
    metadata_subscription: Option<Subscription>,
    creation_editor: Option<Entity<CreationEditor>>,
    creation_subscription: Option<Subscription>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl PullRequestView {
    pub(super) fn new(
        context: GitPanelContext,
        sender: BoundAppCommandSender,
        diff: Entity<GitDiffPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Find pull requests…"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 4)
                .placeholder("Review comment…")
        });
        let search_subscription = cx.subscribe_in(&search, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.find(window, cx);
            }
        });
        let body_subscription = cx.observe(&body, |_, _, cx| cx.notify());
        let diff_subscription = cx.subscribe_in(
            &diff,
            window,
            |this, diff, event: &DraftCodeComment, window, cx| {
                let result = if event.target != this.context.target
                    || this.snapshot.as_ref().is_none_or(|snapshot| {
                        snapshot.pull_request.number != event.number
                            || snapshot.pull_request.head.sha != event.head
                    }) {
                    Err("The selected pull request changed. Refresh before commenting.".into())
                } else if this.comments.len() >= 100 {
                    Err("Submit this review before adding more comments.".into())
                } else if let Some(id) = this.next_comment_id.checked_add(1) {
                    this.next_comment_id = id;
                    this.comments.push(PendingReviewComment {
                        id,
                        comment: event.comment.clone(),
                        stale: false,
                    });
                    Ok(())
                } else {
                    Err("Unable to add another review comment.".into())
                };
                diff.update(cx, |diff, cx| diff.admit_comment(result, window, cx));
                cx.notify();
            },
        );
        Self {
            context,
            sender,
            diff,
            search,
            body,
            verdict: ReviewVerdict::Comment,
            results: Vec::new(),
            snapshot: None,
            page: PullRequestPage::Search,
            activity: None,
            workflows: None,
            stack: None,
            stack_loaded: false,
            viewed: BTreeMap::new(),
            viewed_cursor: None,
            show_files: false,
            file_request: 0,
            selected_file: None,
            expanded_file: None,
            comments: Vec::new(),
            next_comment_id: 0,
            submitted_review: None,
            replies: BTreeMap::new(),
            pending: false,
            merge: None,
            merge_poll: None,
            expanded_threads: BTreeMap::new(),
            pending_comment_page: None,
            pending_reply: None,
            submitted_metadata: None,
            metadata_editor: None,
            metadata_subscription: None,
            creation_editor: None,
            creation_subscription: None,
            error: None,
            _subscriptions: vec![search_subscription, body_subscription, diff_subscription],
        }
    }

    pub(super) fn has_draft(&self, cx: &App) -> bool {
        self.pending
            || self.merge.is_some()
            || self.creation_editor.is_some()
            || self
                .metadata_editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_draft(cx))
            || self.diff.read(cx).has_review_draft(&self.context.target)
            || !self.comments.is_empty()
            || !self.body.read(cx).value().trim().is_empty()
            || self
                .replies
                .values()
                .any(|reply| !reply.read(cx).value().trim().is_empty())
    }

    pub(super) fn find(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.page = PullRequestPage::Search;
        self.request(
            "git.github.search",
            vec![self.search.read(cx).value().to_string()],
            window,
            cx,
        );
    }

    pub(super) fn refresh(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.merge.is_some() {
            self.resume_merge(window, cx);
            return;
        }
        self.viewed.clear();
        self.viewed_cursor = None;
        if self.page == PullRequestPage::Review
            && let Some(number) = self
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.pull_request.number)
        {
            self.open(number, window, cx);
        } else {
            self.find(window, cx);
        }
    }

    fn open(&mut self, number: u32, window: &Window, cx: &mut Context<Self>) {
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.pull_request.number != number)
            && self.has_draft(cx)
        {
            self.error = Some(
                "Submit or discard the current review before opening another pull request.".into(),
            );
            cx.notify();
            return;
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|current| current.pull_request.number != number)
        {
            self.metadata_editor = None;
            self.metadata_subscription = None;
            self.activity = None;
            self.stack = None;
            self.stack_loaded = false;
            self.viewed.clear();
            self.viewed_cursor = None;
        }
        self.request("git.github.read", vec![number.to_string()], window, cx);
    }

    fn request(
        &mut self,
        command: &'static str,
        mut args: Vec<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending || (command == "git.github.action" && self.merge.is_some()) {
            return;
        }
        args.insert(0, self.context.directory.clone());
        let mut invocation = CommandInvocation::new(command, args, Caller::Internal);
        invocation.target = Some(self.context.target.clone());
        let Ok(receiver) = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_mins(2))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) else {
            self.error = Some("The repository host is unavailable".into());
            cx.notify();
            return;
        };
        self.pending = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                this.pending = false;
                if let Some(editor) = &this.metadata_editor {
                    editor.update(cx, |editor, _| editor.set_saving(false));
                }
                if let Some(editor) = &this.creation_editor {
                    editor.update(cx, |editor, _| editor.set_saving(false));
                }
                match result {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        if let Err(error) = this.receive(command, value, window, cx) {
                            this.error = Some(error);
                        }
                    }
                    Ok(outcome) => this.error = crate::commands::command_outcome_message(&outcome),
                    Err(_) => this.error = Some("The repository host disconnected".into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn receive(
        &mut self,
        command: &str,
        value: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if matches!(
            command,
            "git.github.creation-context" | "git.github.publish-branch" | "git.github.create"
        ) {
            return self.receive_creation(command, value, window, cx);
        }
        if command == "git.github.merge-status"
            || (command == "git.github.action" && value.get("status").is_some())
        {
            return self.receive_merge(command, value, window, cx);
        }
        match command {
            "git.github.checkout-context" => {
                let request: bootty_git::github::PullRequestCheckoutRequest =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                if self.snapshot.as_ref().is_none_or(|snapshot| {
                    snapshot.pull_request.number != request.number
                        || snapshot.pull_request.head.sha != request.expected_head
                        || snapshot.repository != request.repository
                }) {
                    return Err(
                        "The displayed pull request changed. Refresh before checking out.".into(),
                    );
                }
                self.request(
                    "git.github.checkout",
                    vec![serde_json::to_string(&request).map_err(|e| e.to_string())?],
                    window,
                    cx,
                );
            }
            "git.github.diff" => {
                let file: FileDiff = serde_json::from_value(value).map_err(|e| e.to_string())?;
                let request = self.expanded_file.take().ok_or("Missing file request")?;
                self.receive_diff(file, request, window, cx)?;
            }
            "git.github.search" => {
                self.results = serde_json::from_value(value).map_err(|e| e.to_string())?;
            }
            "git.github.read" => {
                self.receive_snapshot(value, window, cx)?;
            }
            "git.github.activity" => self.receive_activity(value, window, cx)?,
            "git.github.stack" => {
                self.stack = serde_json::from_value(value).map_err(|e| e.to_string())?;
                self.stack_loaded = true;
            }
            "git.github.workflows" => {
                self.workflows = Some(serde_json::from_value(value).map_err(|e| e.to_string())?);
            }
            "git.github.viewed" => {
                let page: ViewedFilesPage =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                for file in page.nodes {
                    self.viewed
                        .insert(file.path, file.viewer_viewed_state == "VIEWED");
                }
                self.viewed_cursor = page
                    .page_info
                    .end_cursor
                    .filter(|_| page.page_info.has_next_page);
                self.request_stack(window, cx);
            }
            "git.github.comments" => {
                let comments: bootty_git::github::CommentConnection =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                if let Some(id) = self.pending_comment_page.take()
                    && let Some(thread) = self.snapshot.as_mut().and_then(|snapshot| {
                        snapshot.threads.iter_mut().find(|thread| thread.id == id)
                    })
                {
                    for comment in comments.nodes {
                        if !thread
                            .comments
                            .nodes
                            .iter()
                            .any(|current| current.id == comment.id)
                        {
                            thread.comments.nodes.push(comment);
                        }
                    }
                    thread.comments.total_count = comments.total_count;
                    thread.comments.page_info = comments.page_info;
                }
            }
            _ => {
                self.accept_submission(command, window, cx);
                if let Some(number) = self
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.pull_request.number)
                {
                    self.open(number, window, cx);
                }
            }
        }
        Ok(())
    }

    fn accept_submission(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        if command == "git.github.metadata"
            && let Some(request) = self.submitted_metadata.take()
        {
            if let Some(editor) = &self.metadata_editor {
                editor.update(cx, |editor, cx| editor.accept(&request, cx));
            }
            if let MetadataRequest::Viewed { path, viewed, .. } = request {
                self.viewed.insert(path, viewed);
            }
        }
        if command == "git.github.review"
            && let Some(submitted) = self.submitted_review.take()
        {
            self.comments
                .retain(|comment| !submitted.comment_ids.contains(&comment.id));
            if self.body.read(cx).value().as_ref() == submitted.body {
                self.body
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
        }
        if command == "git.github.thread"
            && let Some((id, body)) = self.pending_reply.take()
            && let Some(reply) = self.replies.get(&id)
            && reply.read(cx).value().as_ref() == body
        {
            reply.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    fn receive_activity(
        &mut self,
        value: serde_json::Value,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let page: ActivityPage = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if let Some(activity) = &mut self.activity {
            for item in page.nodes {
                if !activity.nodes.iter().any(|current| current.id == item.id) {
                    activity.nodes.push(item);
                }
            }
            activity.page_info = page.page_info;
        } else {
            self.activity = Some(page);
        }
        if self.viewed.is_empty() {
            self.request(
                "git.github.viewed",
                vec![
                    self.snapshot
                        .as_ref()
                        .ok_or("Missing pull request")?
                        .pull_request
                        .number
                        .to_string(),
                    String::new(),
                ],
                window,
                cx,
            );
        } else {
            self.request_stack(window, cx);
        }
        Ok(())
    }

    fn receive_snapshot(
        &mut self,
        value: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let snapshot: PullRequestSnapshot =
            serde_json::from_value(value).map_err(|e| e.to_string())?;
        let same_revision = self.snapshot.as_ref().is_some_and(|previous| {
            previous.pull_request.head.sha == snapshot.pull_request.head.sha
                && previous.pull_request.base.sha == snapshot.pull_request.base.sha
        });
        if self.snapshot.as_ref().is_some_and(|previous| {
            previous.pull_request.head.sha != snapshot.pull_request.head.sha
        }) {
            self.viewed.clear();
            self.viewed_cursor = None;
        }
        for draft in &mut self.comments {
            if same_revision
                && snapshot
                    .files
                    .iter()
                    .any(|file| file.filename == draft.comment.anchor.path)
            {
                continue;
            }
            draft.stale = snapshot
                .files
                .iter()
                .find(|file| file.filename == draft.comment.anchor.path)
                .and_then(|file| {
                    FileDiff::parse(
                        file.filename.clone(),
                        file.previous_filename.clone(),
                        file.patch.as_deref(),
                    )
                    .ok()
                })
                .and_then(|file| file.quote(&draft.comment.anchor).ok())
                .is_none_or(|quote| quote != draft.comment.quote);
        }
        for thread in &snapshot.threads {
            self.expanded_threads
                .entry(thread.id.clone())
                .or_insert(!thread.is_resolved);
            self.replies.entry(thread.id.clone()).or_insert_with(|| {
                cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(1, 3)
                        .placeholder("Reply…")
                })
            });
        }
        self.workflows = None;
        self.stack = None;
        self.stack_loaded = false;
        self.snapshot = Some(snapshot);
        self.page = PullRequestPage::Review;
        self.activity = None;
        self.request(
            "git.github.activity",
            vec![
                self.snapshot
                    .as_ref()
                    .ok_or("Missing pull request")?
                    .pull_request
                    .number
                    .to_string(),
                String::new(),
            ],
            window,
            cx,
        );
        Ok(())
    }

    fn show_file(&mut self, path: &str, window: &Window, cx: &mut Context<Self>) {
        if self.diff.read(cx).has_review_draft(&self.context.target) {
            self.error = Some("Add or cancel the code comment before changing files.".into());
            cx.notify();
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let Some(file) = snapshot
            .files
            .iter()
            .find(|file| file.filename == path)
            .cloned()
        else {
            return;
        };
        let Some(request) = self.file_request.checked_add(1) else {
            return;
        };
        self.file_request = request;
        self.selected_file = Some(path.into());
        if file.patch.is_none()
            || FileDiff::parse(
                file.filename.clone(),
                file.previous_filename.clone(),
                file.patch.as_deref(),
            )
            .is_err()
        {
            self.expand_file(path, 3, window, cx);
            return;
        }
        let number = snapshot.pull_request.number;
        let head = snapshot.pull_request.head.sha.clone();
        let target = self.context.target.clone();
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { PreparedReviewDiff::prepare(file) })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if this.file_request != request
                    || this.context.target != target
                    || this.snapshot.as_ref().is_none_or(|snapshot| {
                        snapshot.pull_request.number != number
                            || snapshot.pull_request.head.sha != head
                    })
                {
                    return;
                }
                match result {
                    Ok(prepared) => {
                        if this.diff.read(cx).has_review_draft(&target) {
                            this.error = Some(
                                "Add or cancel the code comment before changing files.".into(),
                            );
                            cx.notify();
                            return;
                        }
                        this.diff.update(cx, |diff, cx| {
                            diff.show_review(target, number, head, prepared, window, cx);
                        });
                        cx.emit(OpenDiff);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn receive_diff(
        &self,
        file: FileDiff,
        request: bootty_git::github::PullRequestDiffRequest,
        window: &Window,
        cx: &Context<Self>,
    ) -> Result<(), String> {
        if file.path != request.path {
            return Err("GitHub returned a different file".into());
        }
        let snapshot = self.snapshot.as_ref().ok_or("Missing pull request")?;
        let number = snapshot.pull_request.number;
        let target = self.context.target.clone();
        let generation = self.file_request;
        cx.spawn_in(window, async move |owner, cx| {
            let prepared = cx
                .background_executor()
                .spawn(async move { PreparedReviewDiff::from_diff(file) })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if this.file_request != generation
                    || this.context.target != target
                    || this.selected_file.as_deref() != Some(request.path.as_str())
                    || this.snapshot.as_ref().is_none_or(|snapshot| {
                        snapshot.pull_request.number != number
                            || snapshot.pull_request.head.sha != request.head
                            || snapshot.pull_request.base.sha != request.base
                    })
                {
                    return;
                }
                if this.diff.read(cx).has_review_draft(&target) {
                    this.error =
                        Some("Add or cancel the code comment before expanding context.".into());
                    cx.notify();
                    return;
                }
                this.diff.update(cx, |diff, cx| {
                    diff.show_review(target, number, request.head, prepared, window, cx);
                });
                cx.emit(OpenDiff);
                cx.notify();
            });
        })
        .detach();
        Ok(())
    }

    fn expand_file(
        &mut self,
        path: &str,
        context_lines: u32,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending || self.diff.read(cx).has_review_draft(&self.context.target) {
            return;
        }
        let Some(generation) = self.file_request.checked_add(1) else {
            return;
        };
        self.file_request = generation;
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let request = bootty_git::github::PullRequestDiffRequest {
            head: snapshot.pull_request.head.sha.clone(),
            base: snapshot.pull_request.base.sha.clone(),
            path: path.into(),
            context_lines,
        };
        match serde_json::to_string(&request) {
            Ok(payload) => {
                let number = snapshot.pull_request.number;
                self.expanded_file = Some(request);
                self.request(
                    "git.github.diff",
                    vec![number.to_string(), payload],
                    window,
                    cx,
                );
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    fn submit_review(&mut self, verdict: ReviewVerdict, window: &Window, cx: &mut Context<Self>) {
        if self.pending || self.comments.iter().any(|draft| draft.stale) {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let request = ReviewRequest {
            expected_base: Some(snapshot.pull_request.base.sha.clone()),
            expected_head: snapshot.pull_request.head.sha.clone(),
            verdict,
            body: self.body.read(cx).value().to_string(),
            comments: self
                .comments
                .iter()
                .map(|draft| draft.comment.clone())
                .collect(),
        };
        match serde_json::to_string(&request) {
            Ok(payload) => {
                self.submitted_review = Some(SubmittedReview {
                    body: request.body,
                    comment_ids: self.comments.iter().map(|draft| draft.id).collect(),
                });
                self.request(
                    "git.github.review",
                    vec![snapshot.pull_request.number.to_string(), payload],
                    window,
                    cx,
                );
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    fn thread_request(&mut self, request: &ThreadRequest, window: &Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.pending_reply = match request {
            ThreadRequest::Reply { id, body } => Some((id.clone(), body.clone())),
            ThreadRequest::Resolve { .. } => None,
        };
        let Some(number) = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.pull_request.number)
        else {
            return;
        };
        if let Ok(request) = serde_json::to_string(&request) {
            self.request(
                "git.github.thread",
                vec![number.to_string(), request],
                window,
                cx,
            );
        }
    }

    fn more_comments(&mut self, id: &str, cursor: &str, window: &Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let Some(number) = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.pull_request.number)
        else {
            return;
        };
        self.pending_comment_page = Some(id.to_owned());
        self.request(
            "git.github.comments",
            vec![number.to_string(), id.to_owned(), cursor.to_owned()],
            window,
            cx,
        );
    }

    fn act(
        &mut self,
        action: PullRequestAction,
        method: Option<MergeMethod>,
        stack: Option<bootty_git::github::StackActionContext>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending || self.merge.is_some() {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let number = snapshot.pull_request.number;
        let request = PullRequestActionRequest {
            expected_head: snapshot.pull_request.head.sha.clone(),
            action,
            merge_method: method,
            stack,
        };
        let is_stack = request.stack.is_some();
        let Ok(payload) = serde_json::to_string(&request) else {
            return;
        };
        if is_stack
            || matches!(
                action,
                PullRequestAction::Merge
                    | PullRequestAction::Close
                    | PullRequestAction::EnableAutoMerge
                    | PullRequestAction::Revert
                    | PullRequestAction::ApproveWorkflows
            )
        {
            let label = match action {
                PullRequestAction::Merge if is_stack => "Merge stack through pull request",
                PullRequestAction::Merge => "Merge pull request",
                PullRequestAction::RebaseBranch if is_stack => "Rebase stack from pull request",
                PullRequestAction::Close => "Close pull request",
                PullRequestAction::Revert => "Create revert pull request",
                PullRequestAction::ApproveWorkflows => "Approve fork workflows",
                _ => "Enable auto-merge",
            };
            let answer = crate::gpui::prompt(
                &format!("{label} #{number}?"),
                None,
                &[
                    gpui_kit::PromptButton::Other(label.into()),
                    gpui_kit::PromptButton::Cancel("Cancel".into()),
                ],
                window,
                cx,
            );
            let target = self.context.target.clone();
            let expected_head = request.expected_head;
            cx.spawn_in(window, async move |owner, cx| {
                if answer.await == Ok(0) {
                    _ = owner.update_in(cx, |this, window, cx| {
                        if this.context.target != target
                            || this.snapshot.as_ref().is_none_or(|snapshot| {
                                snapshot.pull_request.number != number
                                    || snapshot.pull_request.head.sha != expected_head
                            })
                        {
                            this.error =
                                Some("The pull request changed while confirming the action".into());
                            cx.notify();
                            return;
                        }
                        this.request(
                            "git.github.action",
                            vec![number.to_string(), payload],
                            window,
                            cx,
                        );
                    });
                }
            })
            .detach();
        } else {
            self.request(
                "git.github.action",
                vec![number.to_string(), payload],
                window,
                cx,
            );
        }
    }

    fn request_stack(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.stack_loaded {
            return;
        }
        let Some(number) = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.pull_request.number)
        else {
            return;
        };
        self.stack_loaded = true;
        self.request("git.github.stack", vec![number.to_string()], window, cx);
    }

    fn render_stack(&self, snapshot: &PullRequestSnapshot, cx: &Context<Self>) -> gpui_kit::Div {
        let number = snapshot.pull_request.number;
        let Some(stack) = &self.stack else {
            return div();
        };
        let merge_context = stack_action_context(stack, Some(number));
        let rebase_context = stack_action_context(stack, None);
        div()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Stack #{} · {}", stack.number, stack.base)),
            )
            .children(stack.layers.iter().map(|layer| {
                let layer_number = layer.number;
                let label = format!("#{} {} · {}", layer.number, layer.title, layer.state);
                Button::new(format!("stack-layer:{layer_number}"))
                    .ghost()
                    .small()
                    .w_full()
                    .selected(layer_number == number)
                    .disabled(self.pending)
                    .accessibility_label(label.clone())
                    .tooltip(label.clone())
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open(layer_number, window, cx)),
                    )
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("merge-stack")
                            .ghost()
                            .small()
                            .label(if self.merge.is_some() {
                                "Merging stack…".to_owned()
                            } else {
                                format!("Merge through #{number}")
                            })
                            .disabled(
                                self.pending
                                    || self.merge.is_some()
                                    || !snapshot.permissions.can_write
                                    || snapshot.pull_request.state != "open"
                                    || stack
                                        .layers
                                        .iter()
                                        .take_while(|layer| layer.number != number)
                                        .any(|layer| layer.draft),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.act(
                                    PullRequestAction::Merge,
                                    None,
                                    Some(merge_context.clone()),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("rebase-stack")
                            .ghost()
                            .small()
                            .label("Rebase stack")
                            .disabled(
                                self.pending
                                    || self.merge.is_some()
                                    || stack
                                        .layers
                                        .last()
                                        .is_none_or(|layer| layer.number != number),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.act(
                                    PullRequestAction::RebaseBranch,
                                    None,
                                    Some(rebase_context.clone()),
                                    window,
                                    cx,
                                );
                            })),
                    ),
            )
    }

    fn render_search(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Input::new(&self.search)
                            .disabled(self.pending)
                            .aria_label("Search pull requests")
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(
                        Button::new("search-prs")
                            .icon(IconName::Search)
                            .ghost()
                            .small()
                            .disabled(self.pending)
                            .accessibility_label("Search pull requests")
                            .tooltip("Search pull requests")
                            .on_click(cx.listener(|this, _, window, cx| this.find(window, cx))),
                    ),
            )
            .child(
                div().flex().child(
                    Button::new("new-pull-request")
                        .icon(IconName::Plus)
                        .label("New pull request")
                        .small()
                        .ghost()
                        .disabled(self.pending || self.creation_editor.is_some())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.request("git.github.creation-context", Vec::new(), window, cx);
                        })),
                ),
            )
            .when_some(self.creation_editor.as_ref(), |view, editor| {
                view.child(editor.clone()).child(
                    Button::new("cancel-pr-creation")
                        .label("Cancel")
                        .small()
                        .ghost()
                        .disabled(self.pending)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.creation_editor = None;
                            this.creation_subscription = None;
                            cx.notify();
                        })),
                )
            })
            .children(self.results.iter().map(|pr| {
                let number = pr.number;
                let label = format!("#{number} {}", pr.title);
                Button::new(format!("pull-{number}"))
                    .icon(gpui_kit::assets::IconName::GitPullRequest)
                    .ghost()
                    .small()
                    .w_full()
                    .min_w_0()
                    .disabled(self.pending || self.creation_editor.is_some())
                    .accessibility_label(label.clone())
                    .tooltip(label.clone())
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .on_click(cx.listener(move |this, _, window, cx| this.open(number, window, cx)))
            }))
    }

    fn receive_creation(
        &mut self,
        command: &str,
        value: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match command {
            "git.github.creation-context" => self.start_creation(value, window, cx)?,
            "git.github.publish-branch" => {
                if let Some(editor) = &self.creation_editor {
                    editor.update(cx, CreationEditor::published);
                }
            }
            "git.github.create" => {
                let pr: bootty_git::github::PullRequest =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                self.creation_editor = None;
                self.creation_subscription = None;
                self.open(pr.number, window, cx);
            }
            _ => return Err("Unknown creation response".into()),
        }
        Ok(())
    }

    fn start_creation(
        &mut self,
        value: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let context = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let editor = cx.new(|cx| CreationEditor::new(context, window, cx));
        let subscription = cx.subscribe_in(
            &editor,
            window,
            |this, editor, submission: &CreationSubmission, window, cx| {
                if this.pending || this.creation_editor.as_ref() != Some(editor) {
                    return;
                }
                let (command, value) = match submission {
                    CreationSubmission::Publish(context) => {
                        ("git.github.publish-branch", serde_json::to_string(context))
                    }
                    CreationSubmission::Create(request) => {
                        ("git.github.create", serde_json::to_string(request))
                    }
                };
                match value {
                    Ok(value) => {
                        editor.update(cx, |editor, _| editor.set_saving(true));
                        this.request(command, vec![value], window, cx);
                    }
                    Err(error) => {
                        this.error = Some(error.to_string());
                        cx.notify();
                    }
                }
            },
        );
        self.creation_editor = Some(editor);
        self.creation_subscription = Some(subscription);
        Ok(())
    }

    fn render_review(&self, snapshot: &PullRequestSnapshot, cx: &Context<Self>) -> gpui_kit::Div {
        let pr = &snapshot.pull_request;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.render_actions(snapshot, cx))
            .child(self.render_stack(snapshot, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .child(format!("#{} {}", pr.number, pr.title)),
                    )
                    .child(
                        Button::new("refresh-pr")
                            .icon(IconName::RotateCw)
                            .ghost()
                            .small()
                            .disabled(self.pending)
                            .accessibility_label("Refresh pull request")
                            .tooltip("Refresh pull request")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(number) = this
                                    .snapshot
                                    .as_ref()
                                    .map(|snapshot| snapshot.pull_request.number)
                                {
                                    this.open(number, window, cx);
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {} → {}",
                        if pr.draft {
                            "Draft"
                        } else if pr.merged {
                            "Merged"
                        } else {
                            &pr.state
                        },
                        pr.head.branch,
                        pr.base.branch
                    )),
            )
            .child(self.render_metadata_controls(snapshot, cx))
            .when_some(self.metadata_editor.as_ref(), |view, editor| {
                view.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div().flex().justify_end().child(
                                Button::new("cancel-metadata-edit")
                                    .icon(IconName::Close)
                                    .small()
                                    .ghost()
                                    .accessibility_label("Cancel edit")
                                    .tooltip("Cancel edit")
                                    .disabled(self.pending)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.metadata_editor = None;
                                        this.metadata_subscription = None;
                                        cx.notify();
                                    })),
                            ),
                        )
                        .child(editor.clone()),
                )
            })
            .child(self.render_tabs(cx))
            .when(self.show_files, |view| {
                view.child(self.render_files(snapshot, cx))
            })
            .when(!self.show_files, |view| {
                view.child(self.render_overview(snapshot, cx))
            })
            .child(self.render_draft_comments(cx))
            .child(self.render_review_composer(snapshot, cx))
    }

    fn metadata_request(
        &mut self,
        request: &MetadataRequest,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending {
            return;
        }
        let Some(number) = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.pull_request.number)
        else {
            return;
        };
        match serde_json::to_string(request) {
            Ok(payload) => {
                self.submitted_metadata = Some(request.clone());
                if let Some(editor) = &self.metadata_editor {
                    editor.update(cx, |editor, _| editor.set_saving(true));
                }
                self.request(
                    "git.github.metadata",
                    vec![number.to_string(), payload],
                    window,
                    cx,
                );
                if !self.pending
                    && let Some(editor) = &self.metadata_editor
                {
                    editor.update(cx, |editor, _| editor.set_saving(false));
                }
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    fn render_tabs(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .gap_1()
            .child(
                Button::new("pr-overview")
                    .label("Overview")
                    .icon(gpui_kit::assets::IconName::MessageSquare)
                    .ghost()
                    .small()
                    .selected(!self.show_files)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_files = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("pr-files")
                    .label("Files")
                    .icon(gpui_kit::assets::IconName::Files)
                    .ghost()
                    .small()
                    .selected(self.show_files)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_files = true;
                        cx.notify();
                    })),
            )
    }

    fn render_context(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div().when_some(self.selected_file.clone(), |view, path| {
            let owner = cx.entity().downgrade();
            view.child(
                Button::new("diff-context")
                    .label("Context")
                    .small()
                    .ghost()
                    .disabled(self.pending)
                    .dropdown_menu_with_anchor(
                        gpui_kit::Anchor::BottomRight,
                        move |mut menu, _, _| {
                            for (label, lines) in
                                [("3 lines", 3), ("20 lines", 20), ("Full file", 100_000)]
                            {
                                let owner = owner.clone();
                                let path = path.clone();
                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                    move |_, window, cx| {
                                        _ = owner.update(cx, |this, cx| {
                                            this.expand_file(&path, lines, window, cx);
                                        });
                                    },
                                ));
                            }
                            menu
                        },
                    ),
            )
        })
    }

    fn render_file_row(
        &self,
        file: &bootty_git::github::PullRequestFile,
        head: &str,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let path = file.filename.clone();
        let viewed = self.viewed.get(&path).copied();
        let request = MetadataRequest::Viewed {
            expected_head: head.to_owned(),
            path: path.clone(),
            viewed: viewed != Some(true),
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new(format!("pr-file:{path}"))
                    .disabled(self.pending)
                    .selected(self.selected_file.as_deref() == Some(path.as_str()))
                    .ghost()
                    .flex_1()
                    .min_w_0()
                    .justify_start()
                    .child(crate::gpui::sized_icon(
                        crate::gpui_prompt_attachments::icon(&path),
                        crate::gpui::IconSize::Small,
                        cx.theme().muted_foreground,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .text_sm()
                            .child(path.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().success)
                            .child(format!("+{}", file.additions)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(format!("−{}", file.deletions)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_file(&path, window, cx);
                    })),
            )
            .child(
                Button::new(format!("viewed:{}", file.filename))
                    .icon(if viewed == Some(true) {
                        IconName::Check
                    } else {
                        IconName::Eye
                    })
                    .ghost()
                    .small()
                    .selected(viewed == Some(true))
                    .disabled(self.pending || viewed.is_none())
                    .accessibility_label(if viewed.is_none() {
                        "Viewed status has not loaded"
                    } else if viewed == Some(true) {
                        "Mark file unread"
                    } else {
                        "Mark file viewed"
                    })
                    .tooltip(if viewed.is_none() {
                        "Viewed status has not loaded"
                    } else if viewed == Some(true) {
                        "Mark file unread"
                    } else {
                        "Mark file viewed"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.metadata_request(&request, window, cx);
                    })),
            )
    }

    fn render_files(&self, snapshot: &PullRequestSnapshot, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.render_context(cx))
            .children(
                snapshot
                    .files
                    .iter()
                    .map(|file| self.render_file_row(file, &snapshot.pull_request.head.sha, cx)),
            )
            .when(snapshot.files_truncated, |view| {
                view.child(
                    "GitHub returned its file limit. Remaining files are available on GitHub.",
                )
            })
            .when_some(self.viewed_cursor.as_ref(), |view, cursor| {
                let cursor = cursor.clone();
                let number = snapshot.pull_request.number;
                view.child(
                    Button::new("more-viewed-files")
                        .label("Load more viewed files")
                        .ghost()
                        .small()
                        .disabled(self.pending)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.request(
                                "git.github.viewed",
                                vec![number.to_string(), cursor.clone()],
                                window,
                                cx,
                            );
                        })),
                )
            })
    }

    fn render_activity(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div().flex().flex_col().gap_3().children(
            self.activity
                .as_ref()
                .into_iter()
                .flat_map(|activity| &activity.nodes)
                .map(|item| {
                    let actor = item
                        .author
                        .as_ref()
                        .or(item.actor.as_ref())
                        .map_or("", |actor| actor.login.as_str());
                    let label = item.commit.as_ref().map_or_else(
                        || activity_label(&item.kind, item.state.as_deref()).to_owned(),
                        |commit| commit.message_headline.clone(),
                    );
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{actor} · {label}")),
                        )
                        .child(self.render_reactions(&item.id, &item.reaction_groups, cx))
                        .when(
                            item.viewer_can_update && item.kind == "IssueComment",
                            |view| {
                                let mode = MetadataMode::Comment {
                                    id: item.id.clone(),
                                    body: item.body.clone().unwrap_or_default(),
                                };
                                view.child(
                                    Button::new(format!("edit-activity:{}", item.id))
                                        .icon(gpui_kit::assets::IconName::Pencil)
                                        .ghost()
                                        .small()
                                        .accessibility_label("Edit comment")
                                        .tooltip("Edit comment")
                                        .disabled(self.pending)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_metadata(mode.clone(), window, cx);
                                        })),
                                )
                            },
                        )
                        .when_some(
                            item.body.as_ref().filter(|body| !body.trim().is_empty()),
                            |view, body| {
                                view.child(
                                    gpui_kit::component::text::TextView::markdown(
                                        gpui_kit::SharedString::from(format!(
                                            "activity:{}",
                                            item.id
                                        )),
                                        body.clone(),
                                    )
                                    .selectable(true)
                                    .text_sm(),
                                )
                            },
                        )
                }),
        )
    }

    fn render_overview(&self, snapshot: &PullRequestSnapshot, cx: &Context<Self>) -> gpui_kit::Div {
        let description = snapshot.pull_request.body.clone().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .when(!description.trim().is_empty(), |view| {
                view.child(
                    gpui_kit::component::text::TextView::markdown(
                        "pull-request-description",
                        description,
                    )
                    .selectable(true)
                    .text_sm(),
                )
            })
            .child(self.render_reactions(
                &snapshot.pull_request.node_id,
                &snapshot.reaction_groups,
                cx,
            ))
            .child(Self::render_checks(snapshot, cx))
            .child(self.render_workflows(snapshot, cx))
            .child(self.render_activity(cx))
            .when_some(
                self.activity.as_ref().and_then(|activity| {
                    activity
                        .page_info
                        .end_cursor
                        .as_ref()
                        .filter(|_| activity.page_info.has_next_page)
                }),
                |view, cursor| {
                    let cursor = cursor.clone();
                    let number = snapshot.pull_request.number;
                    view.child(
                        Button::new("more-pr-activity")
                            .label("Load more activity")
                            .ghost()
                            .small()
                            .disabled(self.pending)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request(
                                    "git.github.activity",
                                    vec![number.to_string(), cursor.clone()],
                                    window,
                                    cx,
                                );
                            })),
                    )
                },
            )
    }

    fn render_checks(snapshot: &PullRequestSnapshot, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                snapshot
                    .checks
                    .get("nodes")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|check| {
                        let name = check
                            .get("name")
                            .or_else(|| check.get("context"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Check");
                        let state = check
                            .get("conclusion")
                            .filter(|value| !value.is_null())
                            .or_else(|| check.get("status"))
                            .or_else(|| check.get("state"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Unknown");
                        let (icon, color) = match state {
                            "SUCCESS" | "NEUTRAL" | "SKIPPED" => ("check", cx.theme().success),
                            "FAILURE" | "ERROR" | "TIMED_OUT" => ("x", cx.theme().danger),
                            _ => ("clock", cx.theme().muted_foreground),
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(crate::gpui::sized_icon(
                                icon,
                                crate::gpui::IconSize::XSmall,
                                color,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .text_xs()
                                    .child(name.to_owned()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(color)
                                    .child(state.to_lowercase().replace('_', " ")),
                            )
                    }),
            )
            .when(
                snapshot.checks.pointer("/pageInfo/hasNextPage")
                    == Some(&serde_json::Value::Bool(true)),
                |view| view.child("More checks are available on GitHub."),
            )
    }

    fn render_draft_comments(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(self.comments.iter().map(|draft| {
                let id = draft.id;
                let comment = &draft.comment;
                div()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{}:{}–{}",
                                        comment.anchor.path,
                                        comment.anchor.start_line,
                                        comment.anchor.line
                                    )),
                            )
                            .child(
                                Button::new(format!("remove-comment-{id}"))
                                    .icon(gpui_kit::assets::IconName::X)
                                    .ghost()
                                    .small()
                                    .disabled(self.pending)
                                    .accessibility_label("Remove draft comment")
                                    .tooltip("Remove draft comment")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.comments.retain(|draft| draft.id != id);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(div().text_sm().child(comment.body.clone()))
                    .when(draft.stale, |view| {
                        view.child(
                            div().text_xs().text_color(cx.theme().danger).child(
                                "Code changed. Remove this draft and select the current code.",
                            ),
                        )
                    })
            }))
    }

    fn render_review_composer(
        &self,
        snapshot: &PullRequestSnapshot,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let has_comment =
            !self.comments.is_empty() || !self.body.read(cx).value().trim().is_empty();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                Textarea::new(&self.body)
                    .aria_label("Pull request review")
                    .w_full(),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .child(
                        Button::new("discard-review")
                            .label("Discard draft")
                            .ghost()
                            .small()
                            .disabled(self.pending || !has_comment)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.comments.clear();
                                this.body
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                                cx.notify();
                            })),
                    )
                    .child(self.render_review_button(snapshot, has_comment, cx)),
            )
    }

    fn render_review_button(
        &self,
        snapshot: &PullRequestSnapshot,
        has_comment: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let owner = cx.weak_entity();
        let choices = [
            ("Comment", ReviewVerdict::Comment, has_comment),
            (
                "Approve",
                ReviewVerdict::Approve,
                !snapshot.permissions.did_author,
            ),
            (
                "Request changes",
                ReviewVerdict::RequestChanges,
                has_comment && !snapshot.permissions.did_author,
            ),
        ];
        let owns = snapshot.permissions.did_author;
        let verdict = self.verdict;
        let label = match verdict {
            ReviewVerdict::Comment => "Comment",
            ReviewVerdict::Approve => "Approve",
            ReviewVerdict::RequestChanges => "Request changes",
        };
        let allowed = choices
            .iter()
            .any(|(_, choice, allowed)| *choice == verdict && *allowed);
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new("review-verdict")
                    .label(label)
                    .icon(IconName::ChevronDown)
                    .small()
                    .ghost()
                    .disabled(self.pending)
                    .dropdown_menu_with_anchor(
                        gpui_kit::Anchor::BottomRight,
                        move |mut menu, _, _| {
                            for (label, choice, _) in choices {
                                let owner = owner.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(label)
                                        .checked(choice == verdict)
                                        .disabled(owns && choice != ReviewVerdict::Comment)
                                        .on_click(move |_, _, cx| {
                                            _ = owner.update(cx, |this, cx| {
                                                this.verdict = choice;
                                                cx.notify();
                                            });
                                        }),
                                );
                            }
                            menu
                        },
                    ),
            )
            .child(
                Button::new("submit-review")
                    .label("Submit review")
                    .small()
                    .disabled(
                        self.pending || !allowed || self.comments.iter().any(|draft| draft.stale),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.submit_review(verdict, window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_actions(&self, snapshot: &PullRequestSnapshot, cx: &Context<Self>) -> gpui_kit::Div {
        let owner = cx.weak_entity();
        let permissions = &snapshot.permissions;
        let pr = &snapshot.pull_request;
        let open = pr.state == "open" && !pr.merged;
        let number = pr.number;
        let head = pr.head.sha.clone();
        let mut actions = Vec::new();
        if permissions.can_write && open && !pr.draft {
            for method in &snapshot.merge_methods {
                actions.push((
                    format!("{}…", merge_method_label(*method)),
                    PullRequestAction::Merge,
                    Some(*method),
                ));
                if pr.auto_merge.is_none() {
                    actions.push((
                        format!("Auto-merge ({})…", merge_method_label(*method)),
                        PullRequestAction::EnableAutoMerge,
                        Some(*method),
                    ));
                }
            }
        }
        for (label, action, allowed) in [
            (
                "Mark ready",
                PullRequestAction::Ready,
                permissions.can_update && open && pr.draft,
            ),
            (
                "Convert to draft",
                PullRequestAction::Draft,
                permissions.can_update && open && !pr.draft,
            ),
            (
                "Close…",
                PullRequestAction::Close,
                permissions.can_update && open,
            ),
            (
                "Reopen",
                PullRequestAction::Reopen,
                permissions.can_update && !open && !pr.merged,
            ),
            (
                "Update branch",
                PullRequestAction::UpdateBranch,
                permissions.can_update_branch && open,
            ),
            (
                "Rebase branch",
                PullRequestAction::RebaseBranch,
                permissions.can_update_branch && open,
            ),
            (
                "Approve workflows…",
                PullRequestAction::ApproveWorkflows,
                permissions.can_write
                    && self.workflows.as_ref().is_some_and(|runs| !runs.is_empty()),
            ),
            (
                "Disable auto-merge",
                PullRequestAction::DisableAutoMerge,
                permissions.can_write && pr.auto_merge.is_some(),
            ),
            (
                "Revert…",
                PullRequestAction::Revert,
                permissions.can_write && pr.merged,
            ),
        ] {
            if allowed {
                actions.push((label.into(), action, None));
            }
        }
        div().flex().justify_end().child(
            Button::new("pr-actions")
                .label("Actions")
                .ghost()
                .small()
                .disabled(self.pending || self.merge.is_some())
                .dropdown_menu_with_anchor(gpui_kit::Anchor::BottomRight, move |mut menu, _, _| {
                    menu = menu.item(Self::checkout_menu_item(number, &head, owner.clone()));
                    for (label, action, method) in actions.iter().cloned() {
                        let owner = owner.clone();
                        menu =
                            menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                _ = owner.update(cx, |this, cx| {
                                    this.act(action, method, None, window, cx);
                                });
                            }));
                    }
                    menu
                }),
        )
    }

    fn checkout_menu_item(
        number: u32,
        head: &str,
        owner: gpui_kit::WeakEntity<Self>,
    ) -> PopupMenuItem {
        let head = head.to_owned();
        PopupMenuItem::new("Check out revision")
            .icon(gpui_kit::assets::IconName::GitFork)
            .on_click(move |_, window, cx| {
                _ = owner.update(cx, |this, cx| {
                    this.request(
                        "git.github.checkout-context",
                        vec![number.to_string(), head.clone()],
                        window,
                        cx,
                    );
                });
            })
    }

    fn render_workflows(
        &self,
        snapshot: &PullRequestSnapshot,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let pr = &snapshot.pull_request;
        let fork = pr.head.repo.as_ref().is_some_and(|head| {
            head.full_name != format!("{}/{}", snapshot.repository.owner, snapshot.repository.name)
        });
        let number = pr.number;
        let head = pr.head.sha.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .when(fork && pr.state == "open", |view| {
                view.child(
                    Button::new("workflow-approvals")
                        .label("Check workflow approvals")
                        .ghost()
                        .small()
                        .disabled(self.pending)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.request(
                                "git.github.workflows",
                                vec![number.to_string(), head.clone()],
                                window,
                                cx,
                            );
                        })),
                )
                .when_some(self.workflows.as_ref(), |view, runs| {
                    view.child(if runs.is_empty() {
                        "No workflows waiting for approval".into()
                    } else {
                        format!("{} workflows waiting for approval", runs.len())
                    })
                })
            })
    }

    fn render_threads(&self, snapshot: &PullRequestSnapshot, cx: &Context<Self>) -> gpui_kit::Div {
        let mut view = div().flex().flex_col().gap_2();
        for thread in &snapshot.threads {
            let Some(reply) = self.replies.get(&thread.id) else {
                continue;
            };
            let reply_state = reply.clone();
            let id = thread.id.clone();
            let resolution_id = id.clone();
            let resolved = thread.is_resolved;
            let header = Self::render_thread_header(thread, cx);
            let reply_button = Button::new(format!("reply:{id}"))
                .label("Reply")
                .ghost()
                .small()
                .disabled(self.pending)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.thread_request(
                        &ThreadRequest::Reply {
                            id: id.clone(),
                            body: reply_state.read(cx).value().to_string(),
                        },
                        window,
                        cx,
                    );
                }));
            let resolve_button = Button::new(format!("resolve:{resolution_id}"))
                .label(if resolved { "Reopen" } else { "Resolve" })
                .ghost()
                .small()
                .disabled(self.pending || !snapshot.permissions.resolve)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.thread_request(
                        &ThreadRequest::Resolve {
                            id: resolution_id.clone(),
                            resolved: !resolved,
                        },
                        window,
                        cx,
                    );
                }));
            let page_id = thread.id.clone();
            let cursor = thread.comments.page_info.end_cursor.clone();
            let content = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(self.render_thread_comment_text(thread, cx))
                .when_some(
                    cursor.filter(|_| thread.comments.page_info.has_next_page),
                    |view, cursor| {
                        view.child(
                            Button::new(format!("more-comments:{page_id}"))
                                .label("Load more comments")
                                .ghost()
                                .small()
                                .disabled(self.pending)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.more_comments(&page_id, &cursor, window, cx);
                                })),
                        )
                    },
                )
                .child(
                    Textarea::new(reply)
                        .aria_label("Reply to review thread")
                        .w_full(),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_1()
                        .child(reply_button)
                        .child(resolve_button),
                );
            view = view.child(
                Collapsible::new()
                    .open(
                        self.expanded_threads
                            .get(&thread.id)
                            .copied()
                            .unwrap_or(!resolved),
                    )
                    .child(header)
                    .content(content),
            );
        }
        view
    }
    fn render_thread_header(
        thread: &bootty_git::github::ReviewThread,
        cx: &Context<Self>,
    ) -> Button {
        let resolved = thread.is_resolved;
        let toggle_id = thread.id.clone();
        let label = format!(
            "{}{}{}",
            thread.path,
            thread
                .line
                .map_or_else(String::new, |line| format!(":{line}")),
            if resolved {
                " · Resolved"
            } else if thread.is_outdated {
                " · Outdated"
            } else {
                ""
            }
        );
        Button::new(format!("review-thread:{}", thread.id))
            .ghost()
            .w_full()
            .accessibility_label(label.clone())
            .tooltip(label.clone())
            .child(div().flex_1().min_w_0().truncate().child(label))
            .on_click(cx.listener(move |this, _, _, cx| {
                let open = this
                    .expanded_threads
                    .entry(toggle_id.clone())
                    .or_insert(!resolved);
                *open = !*open;
                cx.notify();
            }))
    }

    fn render_thread_comment_text(
        &self,
        thread: &bootty_git::github::ReviewThread,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(thread.comments.nodes.iter().map(|comment| {
                div()
                    .flex()
                    .flex_col()
                    .group("github-comment")
                    .gap_1()
                    .p_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                comment
                                    .author
                                    .as_ref()
                                    .map_or("Deleted account", |author| author.login.as_str())
                                    .to_owned(),
                            )
                            .child(
                                div()
                                    .opacity(0.)
                                    .group_hover("github-comment", |stamp| stamp.opacity(1.))
                                    .text_xs()
                                    .child(comment.created_at.clone()),
                            ),
                    )
                    .child(
                        gpui_kit::component::text::TextView::markdown(
                            gpui_kit::SharedString::from(format!("review-comment:{}", comment.id)),
                            comment.body.clone(),
                        )
                        .selectable(true)
                        .text_sm(),
                    )
                    .child(self.render_reactions(&comment.id, &comment.reaction_groups, cx))
                    .when(comment.viewer_can_update, |view| {
                        let mode = MetadataMode::Comment {
                            id: comment.id.clone(),
                            body: comment.body.clone(),
                        };
                        view.child(
                            div()
                                .opacity(0.)
                                .group_hover("github-comment", |view| view.opacity(1.))
                                .child(
                                    Button::new(format!("edit-comment:{}", comment.id))
                                        .icon(gpui_kit::assets::IconName::Pencil)
                                        .xsmall()
                                        .ghost()
                                        .accessibility_label("Edit comment")
                                        .tooltip("Edit comment")
                                        .disabled(self.pending)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_metadata(mode.clone(), window, cx);
                                        })),
                                ),
                        )
                    })
            }))
    }
}

const fn merge_method_label(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "Merge commit",
        MergeMethod::Squash => "Squash",
        MergeMethod::Rebase => "Rebase",
    }
}
fn activity_label(kind: &str, state: Option<&str>) -> &'static str {
    match kind {
        "IssueComment" => "Commented",
        "PullRequestReview" => match state {
            Some("APPROVED") => "Approved",
            Some("CHANGES_REQUESTED") => "Requested changes",
            _ => "Reviewed",
        },
        "MergedEvent" => "Merged",
        "ClosedEvent" => "Closed",
        "ReopenedEvent" => "Reopened",
        "ReadyForReviewEvent" => "Ready for review",
        "ConvertToDraftEvent" => "Converted to draft",
        _ => "Activity",
    }
}

impl EventEmitter<OpenDiff> for PullRequestView {}

impl Render for PullRequestView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(if self.page == PullRequestPage::Search {
                "pull-request-search"
            } else {
                "pull-request-review"
            })
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap_3()
            .overflow_y_scroll()
            .when(self.page == PullRequestPage::Review, |view| {
                view.child(
                    div().flex().child(
                        Button::new("back-to-pull-requests")
                            .icon(IconName::ArrowLeft)
                            .label("All pull requests")
                            .ghost()
                            .small()
                            .disabled(self.pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.page = PullRequestPage::Search;
                                cx.notify();
                            })),
                    ),
                )
            })
            .when(self.pending, |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Loading…"),
                )
            })
            .when_some(self.error.as_ref(), |view, error| {
                view.child(gpui_kit::component::alert::Alert::error(
                    "github-error",
                    error.clone(),
                ))
            })
            .when(self.page == PullRequestPage::Search, |view| {
                view.child(self.render_search(cx))
            })
            .when_some(
                self.snapshot
                    .as_ref()
                    .filter(|_| self.page == PullRequestPage::Review),
                |view, snapshot| {
                    view.child(self.render_review(snapshot, cx))
                        .when(!self.show_files, |view| {
                            view.child(self.render_threads(snapshot, cx))
                        })
                },
            )
    }
}

fn stack_action_context(
    stack: &bootty_git::github::PullRequestStack,
    through: Option<u32>,
) -> bootty_git::github::StackActionContext {
    let end = through
        .and_then(|number| stack.layers.iter().position(|layer| layer.number == number))
        .map_or(stack.layers.len(), |index| index.saturating_add(1));
    bootty_git::github::StackActionContext {
        number: stack.number,
        heads: stack
            .layers
            .iter()
            .take(end)
            .filter(|layer| layer.state != "merged")
            .map(|layer| bootty_git::github::StackHead {
                number: layer.number,
                head: layer.head.clone(),
            })
            .collect(),
    }
}
