//! PR metadata and comment controls use captured PR identity.
use super::*;
use bootty_git::github::{
    CandidateKind, CandidatePage, GitHubCandidate, MetadataRequest, Reaction, ReactionGroup,
};
use gpui_kit::component::{
    command::{Command, CommandItem, CommandState},
    input::{InputState, TextareaState},
};
use gpui_kit::{FocusHandle, Focusable};

#[derive(Clone)]
pub(super) enum MetadataMode {
    PullRequest,
    Comment { id: String, body: String },
    Candidates(CandidateKind),
}

pub(super) struct MetadataSubmission {
    pub number: u32,
    pub request: MetadataRequest,
}
pub(super) struct MetadataEditor {
    context: GitPanelContext,
    number: u32,
    sender: BoundAppCommandSender,
    mode: MetadataMode,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    original_title: String,
    original_body: String,
    menu: Entity<CommandState>,
    candidates: Vec<GitHubCandidate>,
    selected: Vec<String>,
    next_page: Option<u32>,
    pending: bool,
    saving: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}
impl MetadataEditor {
    pub(super) fn new(
        context: GitPanelContext,
        number: u32,
        sender: BoundAppCommandSender,
        mode: MetadataMode,
        pr: &bootty_git::github::PullRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let original_body = match &mode {
            MetadataMode::Comment { body, .. } => body.clone(),
            _ => pr.body.clone().unwrap_or_default(),
        };
        let original_title = pr.title.clone();
        let title = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(original_title.clone(), window, cx);
            input
        });
        let body = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx).auto_grow(3, 10);
            input.set_value(original_body.clone(), window, cx);
            input
        });
        let selected = match mode {
            MetadataMode::Candidates(CandidateKind::Labels) => {
                pr.labels.iter().map(|label| label.name.clone()).collect()
            }
            MetadataMode::Candidates(CandidateKind::Reviewers) => pr
                .requested_reviewers
                .iter()
                .map(|actor| actor.login.clone())
                .collect(),
            MetadataMode::Candidates(CandidateKind::Teams) => pr
                .requested_teams
                .iter()
                .map(|team| team.slug.clone())
                .collect(),
            _ => Vec::new(),
        };
        let menu = cx.new(|cx| CommandState::new(window, cx));
        let subscriptions = vec![
            cx.observe(&title, |_, _, cx| cx.notify()),
            cx.observe(&body, |_, _, cx| cx.notify()),
        ];
        Self {
            context,
            number,
            sender,
            mode,
            title,
            body,
            original_title,
            original_body,
            menu,
            candidates: Vec::new(),
            selected,
            next_page: Some(1),
            pending: false,
            saving: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }
    pub(super) fn has_draft(&self, cx: &App) -> bool {
        self.pending
            || self.saving
            || match self.mode {
                MetadataMode::Candidates(_) => false,
                _ => {
                    self.title.read(cx).value().as_ref() != self.original_title
                        || self.body.read(cx).value().as_ref() != self.original_body
                }
            }
    }
    pub(super) const fn set_saving(&mut self, saving: bool) {
        self.saving = saving;
    }
    pub(super) fn load(&mut self, window: &Window, cx: &mut Context<Self>) {
        let MetadataMode::Candidates(kind) = self.mode else {
            return;
        };
        let Some(page) = self.next_page else {
            return;
        };
        if self.pending {
            return;
        }
        let mut invocation = CommandInvocation::new(
            "git.github.candidates",
            vec![
                self.context.directory.clone(),
                serde_json::to_value(kind)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                page.to_string(),
            ],
            Caller::Internal,
        );
        invocation.target = Some(self.context.target.clone());
        let Ok(receiver) = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) else {
            self.error = Some("The repository host is unavailable".into());
            cx.notify();
            return;
        };
        self.pending = true;
        self.error = None;
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = owner.update(cx, |this, cx| {
                this.pending = false;
                match result {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        match serde_json::from_value::<CandidatePage>(value) {
                            Ok(page) => {
                                this.next_page = page.next_page;
                                for candidate in page.candidates {
                                    if !this
                                        .candidates
                                        .iter()
                                        .any(|current| current.name == candidate.name)
                                    {
                                        this.candidates.push(candidate);
                                    }
                                }
                                if page.truncated {
                                    this.error =
                                        Some("More candidates are available on GitHub".into());
                                }
                            }
                            Err(error) => this.error = Some(error.to_string()),
                        }
                    }
                    Ok(outcome) => this.error = crate::commands::command_outcome_message(&outcome),
                    Err(_) => this.error = Some("The repository host disconnected".into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn submit(&self, cx: &mut Context<Self>) {
        let body = self.body.read(cx).value().to_string();
        let request = match &self.mode {
            MetadataMode::PullRequest => MetadataRequest::Edit {
                title: (self.title.read(cx).value().as_ref() != self.original_title)
                    .then(|| self.title.read(cx).value().to_string()),
                body: (body != self.original_body).then_some(body),
            },
            MetadataMode::Comment { id, .. } => MetadataRequest::EditComment {
                id: id.clone(),
                body,
            },
            MetadataMode::Candidates(_) => return,
        };
        cx.emit(MetadataSubmission {
            number: self.number,
            request,
        });
    }
    pub(super) fn accept(&mut self, request: &MetadataRequest, cx: &mut Context<Self>) {
        match request {
            MetadataRequest::Edit { title, body } => {
                if let Some(title) = title {
                    self.original_title.clone_from(title);
                }
                if let Some(body) = body {
                    self.original_body.clone_from(body);
                }
            }
            MetadataRequest::EditComment { id, body } if matches!(&self.mode,MetadataMode::Comment {id:current,..} if current==id) =>
            {
                self.original_body.clone_from(body);
            }
            MetadataRequest::Labels { names, applied } => {
                update_selected(&mut self.selected, names, *applied);
            }
            MetadataRequest::Reviewers {
                logins,
                teams,
                requested,
            } => {
                update_selected(&mut self.selected, logins, *requested);
                update_selected(&mut self.selected, teams, *requested);
            }
            _ => {}
        }
        cx.notify();
    }
    fn choose(&self, row: usize, cx: &mut Context<Self>) {
        let MetadataMode::Candidates(kind) = self.mode else {
            return;
        };
        let Some(candidate) = self.candidates.get(row) else {
            return;
        };
        let selected = self.selected.contains(&candidate.name);
        let request = match kind {
            CandidateKind::Labels => MetadataRequest::Labels {
                names: vec![candidate.name.clone()],
                applied: !selected,
            },
            CandidateKind::Reviewers => MetadataRequest::Reviewers {
                logins: vec![candidate.name.clone()],
                teams: Vec::new(),
                requested: !selected,
            },
            CandidateKind::Teams => MetadataRequest::Reviewers {
                logins: Vec::new(),
                teams: vec![candidate.name.clone()],
                requested: !selected,
            },
        };
        cx.emit(MetadataSubmission {
            number: self.number,
            request,
        });
    }
}
impl Focusable for MetadataEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.mode {
            MetadataMode::Candidates(_) => self.menu.focus_handle(cx),
            MetadataMode::PullRequest => self.title.focus_handle(cx),
            MetadataMode::Comment { .. } => self.body.focus_handle(cx),
        }
    }
}
impl EventEmitter<MetadataSubmission> for MetadataEditor {}
impl Render for MetadataEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background);
        if let MetadataMode::Candidates(kind) = self.mode {
            let owner = cx.weak_entity();
            let icon = match kind {
                CandidateKind::Labels => gpui_kit::assets::IconName::Tag,
                CandidateKind::Reviewers => gpui_kit::assets::IconName::User,
                CandidateKind::Teams => gpui_kit::assets::IconName::Users,
            };
            let items = self
                .candidates
                .iter()
                .map(|candidate| {
                    CommandItem::new()
                        .label(candidate.name.clone())
                        .keywords(candidate.description.clone())
                        .icon(if self.selected.contains(&candidate.name) {
                            gpui_kit::assets::IconName::Check
                        } else {
                            icon
                        })
                        .disabled(self.saving)
                })
                .collect::<Vec<_>>();
            return editor
                .child(
                    Command::new(&self.menu)
                        .placeholder("Search…")
                        .items(items)
                        .max_h(gpui_kit::rems(16.))
                        .on_confirm(move |index, _, cx| {
                            _ = owner.update(cx, |this, cx| this.choose(index.row, cx));
                        }),
                )
                .when_some(self.next_page, |view, _| {
                    view.child(
                        Button::new("load-candidates")
                            .label(if self.pending {
                                "Loading…"
                            } else {
                                "Load more"
                            })
                            .small()
                            .ghost()
                            .disabled(self.pending)
                            .on_click(cx.listener(|this, _, window, cx| this.load(window, cx))),
                    )
                })
                .when_some(self.error.as_ref(), |view, error| {
                    view.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(error.clone()),
                    )
                })
                .into_any_element();
        }
        editor
            .when(matches!(self.mode, MetadataMode::PullRequest), |view| {
                view.child(
                    Input::new(&self.title)
                        .aria_label("Pull request title")
                        .w_full(),
                )
            })
            .child(
                Textarea::new(&self.body)
                    .aria_label(if matches!(self.mode, MetadataMode::PullRequest) {
                        "Pull request description"
                    } else {
                        "Comment"
                    })
                    .w_full(),
            )
            .child(
                div().flex().justify_end().child(
                    Button::new("save-pr-metadata")
                        .label("Save")
                        .small()
                        .disabled(!self.has_draft(cx) || self.pending || self.saving)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
            )
            .into_any_element()
    }
}
fn update_selected(selected: &mut Vec<String>, names: &[String], applied: bool) {
    for name in names {
        if applied {
            if !selected.contains(name) {
                selected.push(name.clone());
            }
        } else {
            selected.retain(|current| current != name);
        }
    }
}

impl PullRequestView {
    pub(super) fn open_metadata(
        &mut self,
        mode: MetadataMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending
            || self
                .metadata_editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_draft(cx))
        {
            self.error = Some("Save or cancel the current edit first.".into());
            cx.notify();
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let context = self.context.clone();
        let sender = self.sender.clone();
        let number = snapshot.pull_request.number;
        let editor = cx.new(|cx| {
            MetadataEditor::new(
                context,
                number,
                sender,
                mode,
                &snapshot.pull_request,
                window,
                cx,
            )
        });
        editor.update(cx, |editor, cx| editor.load(window, cx));
        editor.focus_handle(cx).focus(window, cx);
        self.metadata_subscription = Some(cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &MetadataSubmission, window, cx| {
                if this
                    .snapshot
                    .as_ref()
                    .is_none_or(|snapshot| snapshot.pull_request.number != event.number)
                {
                    this.error = Some("The pull request changed".into());
                    cx.notify();
                    return;
                }
                this.metadata_request(&event.request, window, cx);
            },
        ));
        self.metadata_editor = Some(editor);
        cx.notify();
    }
    pub(super) fn render_metadata_controls(
        &self,
        snapshot: &PullRequestSnapshot,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .child(
                Button::new("edit-pull-request")
                    .icon(gpui_kit::assets::IconName::Pencil)
                    .label("Edit")
                    .small()
                    .ghost()
                    .disabled(self.pending || !snapshot.permissions.can_update)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_metadata(MetadataMode::PullRequest, window, cx);
                    })),
            )
            .child(
                Button::new("pull-request-labels")
                    .icon(gpui_kit::assets::IconName::Tag)
                    .label("Labels")
                    .small()
                    .ghost()
                    .disabled(self.pending || !snapshot.permissions.labels)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_metadata(
                            MetadataMode::Candidates(CandidateKind::Labels),
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                Button::new("pull-request-reviewers")
                    .icon(gpui_kit::assets::IconName::Users)
                    .label("Reviewers")
                    .small()
                    .ghost()
                    .disabled(self.pending || !snapshot.permissions.can_write)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_metadata(
                            MetadataMode::Candidates(CandidateKind::Reviewers),
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                Button::new("pull-request-teams")
                    .icon(gpui_kit::assets::IconName::Users)
                    .label("Teams")
                    .small()
                    .ghost()
                    .disabled(self.pending || !snapshot.permissions.can_write)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_metadata(
                            MetadataMode::Candidates(CandidateKind::Teams),
                            window,
                            cx,
                        );
                    })),
            )
            .children(snapshot.pull_request.labels.iter().map(|label| {
                div()
                    .text_xs()
                    .px_1()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .child(label.name.clone())
            }))
            .children(
                snapshot
                    .pull_request
                    .requested_reviewers
                    .iter()
                    .map(|actor| {
                        div()
                            .text_xs()
                            .px_1()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("@{}", actor.login))
                    }),
            )
    }
    pub(super) fn render_reactions(
        &self,
        id: &str,
        groups: &[ReactionGroup],
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let owner = cx.weak_entity();
        let subject = id.to_owned();
        let reactions = groups.to_vec();
        div()
            .flex()
            .items_center()
            .gap_1()
            .children(
                groups
                    .iter()
                    .filter(|group| group.users.total_count > 0)
                    .map(|group| {
                        let request = MetadataRequest::React {
                            id: Some(id.to_owned()),
                            content: group.content,
                            reacted: !group.viewer_has_reacted,
                        };
                        Button::new(format!("reaction:{id}:{:?}", group.content))
                            .label(format!(
                                "{} {}",
                                reaction_emoji(group.content),
                                group.users.total_count
                            ))
                            .xsmall()
                            .ghost()
                            .selected(group.viewer_has_reacted)
                            .disabled(self.pending)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.metadata_request(&request, window, cx);
                            }))
                    }),
            )
            .child(
                Button::new(format!("add-reaction:{id}"))
                    .icon(gpui_kit::assets::IconName::ThumbsUp)
                    .xsmall()
                    .ghost()
                    .accessibility_label("React")
                    .tooltip("Add reaction")
                    .disabled(self.pending)
                    .dropdown_menu(move |mut menu, _, _| {
                        for content in [
                            Reaction::ThumbsUp,
                            Reaction::ThumbsDown,
                            Reaction::Laugh,
                            Reaction::Hooray,
                            Reaction::Confused,
                            Reaction::Heart,
                            Reaction::Rocket,
                            Reaction::Eyes,
                        ] {
                            let request = MetadataRequest::React {
                                id: Some(subject.clone()),
                                content,
                                reacted: !reactions.iter().any(|group| {
                                    group.content == content && group.viewer_has_reacted
                                }),
                            };
                            let owner = owner.clone();
                            menu = menu.item(PopupMenuItem::new(reaction_emoji(content)).on_click(
                                move |_, window, cx| {
                                    _ = owner.update(cx, |this, cx| {
                                        this.metadata_request(&request, window, cx);
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
    }
}
const fn reaction_emoji(reaction: Reaction) -> &'static str {
    match reaction {
        Reaction::ThumbsUp => "👍",
        Reaction::ThumbsDown => "👎",
        Reaction::Laugh => "😄",
        Reaction::Hooray => "🎉",
        Reaction::Confused => "😕",
        Reaction::Heart => "❤️",
        Reaction::Rocket => "🚀",
        Reaction::Eyes => "👀",
    }
}
