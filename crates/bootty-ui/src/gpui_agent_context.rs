//! Conversation context reuses the workspace's committed project and cached Git projection.
use super::*;
use crate::gpui::chrome::{SessionTarget, SidebarDiffSummary, SidebarPullRequest};
use gpui_kit::component::ElementExt as _;

#[derive(Clone, PartialEq, Eq)]
pub struct ConversationContext {
    pub project: Option<String>,
    pub branch: Option<String>,
    pub pull_request: Option<SidebarPullRequest>,
    /// The binding whose host runs Git for the conversation's checkout.
    pub binding: Option<CommandTarget>,
    pub diff: Option<SidebarDiffSummary>,
    pub target: Option<SessionTarget>,
    /// The session works in a linked worktree of its project.
    pub worktree: bool,
}

/// Where the context card shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardPlace {
    /// A narrow conversation, with the card drawn over the messages when opened.
    Overlay,
    /// A wide conversation, with the messages moved aside for the card.
    Aside,
    /// A wide conversation whose centered messages leave room for the card beside them.
    Beside,
}

impl CardPlace {
    pub(super) const fn wide(self) -> bool {
        matches!(self, Self::Aside | Self::Beside)
    }

    /// The place after measuring the conversation `wide` or not, with room for the card
    /// `beside` its messages or not. Visibility is a separate user choice.
    const fn measured(wide: bool, beside: bool) -> Self {
        match (wide, beside) {
            (true, true) => Self::Beside,
            (true, false) => Self::Aside,
            (false, _) => Self::Overlay,
        }
    }
}

/// Whether the last measured conversation was wide. A new conversation starts from it, since it
/// usually opens where the last one was, rather than painting narrow and then jumping.
pub(super) struct LastWide(pub(super) bool);
impl gpui_kit::Global for LastWide {}

impl NativeAgentSessionView {
    /// The project, branch and Git binding of the conversation's session, as the workspace sees
    /// them.
    pub fn set_conversation_context(
        &mut self,
        context: Option<ConversationContext>,
        cx: &mut Context<Self>,
    ) {
        if self.context_card != context {
            self.context_card = context;
            cx.notify();
        }
    }

    pub(super) fn render_context_card(&self, cx: &Context<Self>) -> impl IntoElement {
        let context = self.context_card.as_ref();
        let project = context
            .and_then(|context| context.project.clone())
            .unwrap_or_else(|| "No project".into());
        let sources = self.render_context_sources(cx);
        let card = div()
            .id("native-context-card")
            .debug_selector(|| "native-context-card".into())
            .w_full()
            .max_h_full()
            .overflow_y_scroll()
            .occlude()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_xl()
            .border_1()
            .border_color(cx.theme().border)
            // Opened over a narrow conversation, it covers the messages, so it's opaque there.
            .bg(if self.card.wide() {
                cx.theme().muted.opacity(0.5)
            } else {
                cx.theme().popover
            })
            .shadow_sm()
            .on_mouse_down_out(
                cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, cx| {
                    if this
                        .card_button_bounds
                        .get()
                        .is_none_or(|bounds| !bounds.contains(&event.position))
                    {
                        this.close_context_card(cx);
                    }
                }),
            )
            .child(
                card_row()
                    .text_color(cx.theme().muted_foreground)
                    .child(project),
            )
            .child(
                self.render_context_directory(context.is_some_and(|context| context.worktree), cx),
            )
            .when_some(
                context.and_then(|context| context.branch.clone()),
                |card, branch| {
                    card.child(
                        card_row()
                            .child(crate::gpui::sized_icon(
                                "git-branch",
                                crate::gpui::IconSize::Small,
                                cx.theme().muted_foreground,
                            ))
                            .child(div().min_w_0().truncate().child(branch)),
                    )
                },
            )
            .children(
                context
                    .and_then(|context| context.pull_request.clone())
                    .map(|pull_request| Self::render_context_pull_request(pull_request, cx)),
            )
            .child(self.render_context_model(cx))
            .when_some(
                context.and_then(|context| context.target.clone()),
                |card, target| card.child(self.render_context_changes(target, cx)),
            )
            .when(!self.subagents().is_empty(), |card| {
                card.child(self.render_context_subagents(cx))
            })
            // A narrow conversation shows its side chats above the composer instead.
            .when(
                self.card.wide()
                    && self
                        .related
                        .iter()
                        .any(|related| related.subagent.is_none()),
                |card| card.child(card_section(cx).child(self.render_related_conversations(cx))),
            )
            .when(!sources.is_empty(), |card| {
                card.child(
                    card_section(cx)
                        .child(
                            card_row()
                                .debug_selector(|| "native-context-sources".into())
                                .text_color(cx.theme().muted_foreground)
                                .child("Sources"),
                        )
                        .children(sources),
                )
            });
        div()
            .absolute()
            .top_4()
            .right_4()
            .bottom_4()
            .w(gpui_kit::rems(18.))
            .flex()
            .flex_col()
            .child(card)
    }

    /// The same details toggle at every conversation width.
    #[must_use]
    pub fn render_context_card_button(&self, cx: &Context<Self>) -> Option<gpui_kit::AnyElement> {
        self.context_card.as_ref()?;
        let button = Button::new("native-context-card-button")
            .debug_selector(|| "native-context-card-button".into())
            .ghost()
            .w(crate::gpui::UI_HEADER_CONTROL_SIZE)
            .h(crate::gpui::UI_HEADER_CONTROL_SIZE)
            .icon(IconName::Info)
            .accessibility_label("Conversation details")
            .tooltip("Conversation details")
            .selected(self.context_card_visible())
            .on_click(cx.listener(|this, _, _, cx| {
                this.card_open = Some(!this.context_card_visible());
                cx.notify();
            }));
        let bounds = self.card_button_bounds.clone();
        Some(
            div()
                .flex()
                .flex_none()
                .on_prepaint(move |current, _, _| {
                    bounds.set(Some(current));
                })
                .child(button)
                .into_any_element(),
        )
    }

    pub(super) fn context_card_visible(&self) -> bool {
        self.card_open.unwrap_or_else(|| self.card.wide())
    }

    /// Dismisses an overlay; the wide card stays until its toggle is used.
    pub(super) fn close_context_card(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.card == CardPlace::Overlay && self.context_card_visible();
        if open {
            self.card_open = Some(false);
            cx.notify();
        }
        open
    }

    fn render_context_subagents(&self, cx: &Context<Self>) -> impl IntoElement {
        card_section(cx)
            .child(
                card_row()
                    .text_color(cx.theme().muted_foreground)
                    .child("Subagents"),
            )
            .children(
                self.subagents()
                    .into_iter()
                    .map(|agent| self.render_subagent_entry(agent, cx)),
            )
    }

    /// Where the conversation works, to copy, and whether that's a linked worktree.
    fn render_context_directory(&self, worktree: bool, cx: &Context<Self>) -> gpui_kit::Div {
        let path = self.record.config.cwd.to_string_lossy().into_owned();
        let shown = readable_path(&self.record.config.cwd, self.record.config.remote.is_some());
        card_row()
            .debug_selector(|| "native-context-directory".into())
            .child(crate::gpui::sized_icon(
                if worktree { "git-fork" } else { "folder" },
                crate::gpui::IconSize::Small,
                cx.theme().muted_foreground,
            ))
            .child(div().flex_1().min_w_0().truncate().child(shown))
            .when(worktree, |row| {
                row.child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().muted)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("worktree"),
                )
            })
            .child(
                Button::new("native-context-copy-path")
                    .debug_selector(|| "native-context-copy-path".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .accessibility_label("Copy path")
                    .tooltip(path.clone())
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(path.clone()));
                    }),
            )
    }

    /// Who answers: the provider, its model and how hard it thinks.
    fn render_context_model(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let config = &self.record.config;
        let model = config.model.as_deref().map(|id| {
            self.models
                .iter()
                .flatten()
                .find(|model| model.id == id)
                .map_or(id, |model| model.display_name.as_str())
        });
        let effort = config
            .reasoning_effort
            .as_deref()
            .map(super::models::reasoning_label);
        let words = [
            Some(config.provider.display_name()),
            model,
            effort.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        card_row()
            .debug_selector(|| "native-context-model".into())
            .child(crate::gpui::sized_icon(
                config.provider.icon(),
                crate::gpui::IconSize::Small,
                crate::gpui::provider_color(&config.provider.to_string(), cx),
            ))
            .child(div().min_w_0().truncate().child(words))
    }

    /// The branch's pull request: its number in the color of where it stands and its title,
    /// which opens it, then where it stands in words.
    fn render_context_pull_request(
        pull_request: SidebarPullRequest,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let tint = gpui_kit::Hsla::from(pull_request.color);
        let url = pull_request.url.clone();
        div()
            .id("native-context-pull-request")
            .debug_selector(|| "native-context-pull-request".into())
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .rounded(cx.theme().radius)
            .text_sm()
            .when(!url.is_empty(), |row| {
                row.cursor_pointer()
                    .hover(|row| row.bg(cx.theme().muted.opacity(0.5)))
                    .on_click(move |_, _, cx| cx.open_url(&url))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(crate::gpui::sized_icon(
                        "git-pull-request",
                        crate::gpui::IconSize::Small,
                        tint,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .text_color(tint)
                            .child(format!("#{}", pull_request.number)),
                    )
                    .child(div().min_w_0().truncate().child(pull_request.title)),
            )
            .child(
                div()
                    .pl_5()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(pull_request.status),
            )
    }

    fn render_context_changes(
        &self,
        target: SessionTarget,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let diff = self.context_card.as_ref().and_then(|context| context.diff);
        Button::new("context-changes")
            .ghost()
            .small()
            .w_full()
            .icon(IconName::File)
            .accessibility_label("Open Git changes")
            .child(div().flex_1().child("Changes"))
            .when_some(diff, |button, diff| {
                button.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .text_color(cx.theme().success)
                                .child(format!("+{}", diff.added)),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("−{}", diff.removed)),
                        ),
                )
            })
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(OpenNativeSession::Changes {
                    target: target.clone(),
                });
            }))
    }

    /// Wide mode for a host that knows the conversation's width before it paints, so the first
    /// frame already has the right layout.
    pub(crate) fn fit_width(
        &mut self,
        width: gpui_kit::Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let wide = width >= gpui_kit::rems(76.).to_pixels(window.rem_size());
        let card = CardPlace::measured(wide, self.card_fits_beside(width, window.rem_size()));
        if self.card != card {
            self.card = card;
            cx.notify();
        }
    }

    fn card_fits_beside(&self, width: gpui_kit::Pixels, rem: gpui_kit::Pixels) -> bool {
        card_fits_beside(self.presentation.width.rems(), width, rem)
    }

    /// Wide mode follows the measured width, which arrives after the first paint, for hosts that
    /// don't pass it in.
    pub(super) fn render_context_geometry(&self, cx: &Context<Self>) -> impl IntoElement {
        let geometry_owner = cx.weak_entity();
        let card = self.card;
        let column = self.presentation.width.rems();
        gpui_kit::canvas(
            move |bounds, window, cx| {
                let rem = window.rem_size();
                let wide = bounds.size.width >= gpui_kit::rems(76.).to_pixels(rem);
                let beside = card_fits_beside(column, bounds.size.width, rem);
                if CardPlace::measured(wide, beside) != card {
                    let owner = geometry_owner;
                    cx.defer(move |cx| {
                        cx.set_global(LastWide(wide));
                        _ = owner.update(cx, |this, cx| {
                            let next = CardPlace::measured(wide, beside);
                            if this.card != next {
                                this.card = next;
                                cx.notify();
                            }
                        });
                    });
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
    }

    /// Files the conversation's messages carry, each opening its message. Images already show
    /// in their messages.
    fn render_context_sources(&self, cx: &Context<Self>) -> Vec<gpui_kit::AnyElement> {
        self.record
            .attachments
            .iter()
            .filter(|attachment| attachment.kind != bootty_agents::NativeAttachmentKind::Image)
            .filter_map(|attachment| {
                let message = self
                    .record
                    .snapshot
                    .transcript
                    .iter()
                    .find(|item| {
                        item.attachments
                            .iter()
                            .any(|source| source.id == attachment.id)
                    })?
                    .id
                    .clone();
                let id = format!("context-source:{}", attachment.id);
                Some(
                    Button::new(gpui_kit::SharedString::from(id.clone()))
                        .debug_selector(move || id)
                        .ghost()
                        .small()
                        .w_full()
                        .accessibility_label(attachment.name.clone())
                        .tooltip(attachment.name.clone())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(attachment.name.clone()),
                        )
                        .child(crate::gpui::sized_icon(
                            "paperclip",
                            crate::gpui::IconSize::Small,
                            cx.theme().muted_foreground,
                        ))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.reveal_response(&message, cx)),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// A card row, laid out like the card's small buttons so icons and words line up with theirs.
/// A local path under the home directory reads from `~`; a remote host's path stays whole.
fn readable_path(path: &std::path::Path, remote: bool) -> String {
    (!remote)
        .then(crate::strings::home_dir)
        .flatten()
        .and_then(|home| {
            path.strip_prefix(home)
                .ok()
                .map(std::path::Path::to_path_buf)
        })
        .map_or_else(
            || path.display().to_string(),
            |rest| {
                if rest.as_os_str().is_empty() {
                    "~".to_owned()
                } else {
                    format!("~/{}", rest.display())
                }
            },
        )
}

fn card_row() -> gpui_kit::Div {
    div().h_6().px_2().flex().items_center().gap_1().text_sm()
}

/// A card section below a rule.
fn card_section(cx: &App) -> gpui_kit::Div {
    div()
        .border_t_1()
        .border_color(cx.theme().border)
        .pt_2()
        .flex()
        .flex_col()
        .gap_1()
}

/// Whether the context card fits in the margin beside a centered message column `column` rems
/// wide, so the column needn't move aside for it. A full-width column leaves no margin.
fn card_fits_beside(column: Option<f32>, width: gpui_kit::Pixels, rem: gpui_kit::Pixels) -> bool {
    // The card is 18rem wide, 1rem from the edge and 1rem from the column: 20rem each side.
    column.is_some_and(|column| width >= gpui_kit::rems(column + 40.).to_pixels(rem))
}
