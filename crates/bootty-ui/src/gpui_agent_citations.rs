//! Composer quotes are view-owned drafts; their context is persisted with the sent native prompt.
pub(super) use bootty_agents::NativeResponseCitation as ResponseCitation;
use bootty_agents::{NATIVE_CITATION_TEXT_LIMIT as CITATION_TEXT_LIMIT, NativeTranscriptItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, ThemeStyled as _,
    button::{Button, ButtonVariants as _},
    input::{InlineToken, InlineTokenContext, Textarea},
    menu::{PopupMenu, PopupMenuItem},
    popover::Popover,
    text::TextViewState,
};
use gpui_kit::{
    App, Context, Entity, Focusable as _, Half as _, IntoElement as _, ParentElement as _, Pixels,
    Point, SharedString, Styled as _, Window, div, prelude::*,
};

use super::NativeAgentSessionView;

// Pi copy-mode annotations use reactions as shortcuts to a comment, not a second state.
const REACTIONS: [(&str, &str); 7] = [
    ("👍", "Looks good"),
    ("🚫", "Rejected"),
    ("✅", "Approved"),
    ("❓", "Clarify"),
    ("🧬", "Match existing patterns"),
    ("🔄", "Consider alternatives"),
    ("🔍", "Verify"),
];

#[derive(Clone, PartialEq, Eq, gpui_kit::Action)]
#[action(namespace = bootty, no_json)]
struct SelectReaction(usize);

pub fn init_reaction_keys(cx: &mut App) {
    cx.bind_keys(
        ["1", "2", "3", "4", "5", "6", "7"]
            .into_iter()
            .enumerate()
            .map(|(index, key)| {
                gpui_kit::KeyBinding::new(
                    key,
                    SelectReaction(index),
                    Some("BoottyResponseReactions"),
                )
            }),
    );
}

pub(super) fn citation_key(citation: &ResponseCitation) -> String {
    format!(
        "{}:{}:{}",
        citation.message_id, citation.source_range.start, citation.source_range.end
    )
}

fn citation_label(citation: &ResponseCitation) -> String {
    let text = if citation.comment.trim().is_empty() {
        &citation.quote
    } else {
        &citation.comment
    };
    let preview = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut label = preview.chars().take(64).collect::<String>();
    if preview.chars().count() > 64 {
        label.push('…');
    }
    label
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ResponseSelection {
    pub citation: ResponseCitation,
    copy_text: String,
    position: Point<Pixels>,
}

impl NativeAgentSessionView {
    pub(super) fn observe_response_selection(
        message_id: String,
        state: &Entity<TextViewState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Subscription {
        cx.observe_in(state, window, move |this, state, window, cx| {
            if state.read(cx).is_selecting() {
                this.capture_response_selection(&message_id, &state, window, cx);
            } else {
                Self::capture_response_after_frame(message_id.clone(), state, None, window, cx);
            }
        })
    }

    fn capture_response_after_frame(
        message_id: String,
        state: Entity<TextViewState>,
        key: Option<gpui_kit::KeyDownEvent>,
        window: &mut Window,
        cx: &Context<Self>,
    ) {
        // Kit publishes source ranges while painting, after the first frame callback.
        cx.on_next_frame(window, move |_, window, cx| {
            cx.notify();
            cx.on_next_frame(window, move |this, window, cx| {
                if key.is_some() && !state.read(cx).focus_handle().is_focused(window) {
                    return;
                }
                this.capture_response_selection(&message_id, &state, window, cx);
                if let Some(key) = key
                    && this
                        .selected_response
                        .as_ref()
                        .is_some_and(|selected| selected.citation.message_id == message_id)
                {
                    this.handle_selection_key(&key, window, cx);
                }
            });
        });
    }

    pub(super) fn capture_response_selection(
        &mut self,
        message_id: &str,
        state: &Entity<TextViewState>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing_citation.is_some() {
            return;
        }
        let state = state.read(cx);
        if !state.focus_handle().is_focused(window) {
            return;
        }
        // Kit owns selection across rich-text views. A quote is admitted only after release.
        if state.is_selecting() {
            if self.selected_response.take().is_some() {
                cx.notify();
            }
            return;
        }
        let rendered_quote = state.selected_text();
        let selection = state.selected_source_range().and_then(|source_range| {
            let item = self
                .record
                .snapshot
                .transcript
                .iter()
                .find(|item| item.id == message_id)?;
            let quote = item.display_text().get(source_range.clone())?.to_owned();
            (!rendered_quote.trim().is_empty() && !source_range.is_empty()).then(|| {
                ResponseSelection {
                    citation: ResponseCitation {
                        message_id: message_id.to_owned(),
                        source_range,
                        prompt_range: None,
                        quote,
                        comment: String::new(),
                    },
                    copy_text: rendered_quote.clone(),
                    position: window.mouse_position(),
                }
            })
        });
        // Window selection clears on pointer down before a toolbar click; the toolbar
        // owns this captured quote until explicit dismissal or a new selection.
        if selection.is_none() {
            return;
        }
        if self
            .selected_response
            .as_ref()
            .zip(selection.as_ref())
            .is_some_and(|(current, selected)| current.citation == selected.citation)
        {
            return;
        }
        if self.selected_response != selection {
            self.selected_response = selection;
            self.reaction_menu = None;
            cx.notify();
        }
    }

    fn copy_response_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = &self.selected_response else {
            return;
        };
        let text = selection.copy_text.clone();
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
        self.dismiss_citation_controls(window, cx);
    }

    pub(super) fn handle_selection_key(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = event.keystroke.modifiers;
        if modifiers.control
            || modifiers.platform
            || modifiers.alt
            || modifiers.shift
            || self.editing_citation.is_some()
            || self.reaction_menu.is_some()
            || !matches!(event.keystroke.key.as_str(), "c" | "r" | "y")
        {
            return false;
        }
        let Some(selection) = &self.selected_response else {
            return self.defer_response_key(event, window, cx);
        };
        if !self
            .transcript
            .get(&selection.citation.message_id)
            .is_some_and(|state| state.read(cx).focus_handle().is_focused(window))
        {
            return self.defer_response_key(event, window, cx);
        }
        let key = event.keystroke.key.as_str();
        match key {
            "c" => self.comment_response(None, window, cx),
            "r" => self.open_reaction_menu(window, cx),
            "y" => self.copy_response_selection(window, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    fn defer_response_key(
        &self,
        event: &gpui_kit::KeyDownEvent,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> bool {
        let focused = self.record.snapshot.transcript.iter().find_map(|item| {
            if item.role != "assistant" {
                return None;
            }
            let state = self.transcript.get(&item.id)?;
            let view = state.read(cx);
            view.focus_handle()
                .is_focused(window)
                .then(|| (item.id.clone(), state.clone()))
        });
        let Some((message_id, state)) = focused else {
            return false;
        };
        Self::capture_response_after_frame(message_id, state, Some(event.clone()), window, cx);
        true
    }

    fn open_reaction_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = &self.selected_response else {
            return;
        };
        if self.reaction_menu.is_some() || selection.citation.quote.len() > CITATION_TEXT_LIMIT {
            return;
        }
        let focus = self
            .transcript
            .get(&selection.citation.message_id)
            .map(|state| state.read(cx).focus_handle().clone());
        let owner = cx.entity().downgrade();
        let menu_width = gpui_kit::rems(14.).to_pixels(window.rem_size());
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            let mut menu = menu
                .min_w(menu_width)
                .when_some(focus, PopupMenu::action_context);
            for (index, (emoji, meaning)) in REACTIONS.into_iter().enumerate() {
                let owner = owner.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!("{emoji} {meaning}"))
                        .action(Box::new(SelectReaction(index)))
                        .on_click(move |_, window, cx| {
                            _ = owner.update(cx, |this, cx| {
                                this.select_reaction(index, window, cx);
                            });
                        }),
                );
            }
            menu
        });
        let subscription = cx.subscribe_in(
            &menu,
            window,
            |this, _, _: &gpui_kit::DismissEvent, window, cx| {
                this.dismiss_reaction_menu(window, cx);
            },
        );
        menu.focus_handle(cx).focus(window, cx);
        self.reaction_menu = Some((menu, subscription));
        cx.notify();
    }

    fn select_reaction(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.reaction_menu.is_some()
            && let Some((emoji, meaning)) = REACTIONS.get(index)
        {
            self.comment_response(Some(&format!("{emoji} {meaning}")), window, cx);
        }
    }

    fn dismiss_reaction_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reaction_menu.take().is_none() {
            return;
        }
        if let Some(selection) = &self.selected_response
            && let Some(state) = self.transcript.get(&selection.citation.message_id)
        {
            window.focus(&state.read(cx).focus_handle().clone(), cx);
        }
        cx.notify();
    }

    fn comment_response(
        &mut self,
        reaction: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = self.selected_response.take() else {
            return;
        };
        if selection.citation.quote.len() > CITATION_TEXT_LIMIT {
            self.selected_response = Some(selection);
            return;
        }
        self.reaction_menu = None;
        let key = citation_key(&selection.citation);
        if !self
            .citations
            .iter()
            .any(|citation| citation_key(citation) == key)
        {
            self.citations.push(selection.citation.clone());
        }
        if let Some(reaction) = reaction
            && let Some(citation) = self
                .citations
                .iter_mut()
                .find(|citation| citation_key(citation) == key)
        {
            reaction.clone_into(&mut citation.comment);
        }
        self.draft_revision = self.draft_revision.wrapping_add(1);
        gpui_kit::base::TextSelection::clear(window, cx);
        if let Some(state) = self.transcript.get(&selection.citation.message_id) {
            state.update(cx, TextViewState::clear_selection);
        }
        if reaction.is_none() {
            self.edit_citation(&key, selection.position, window, cx);
        } else {
            self.insert_citation_token(&key, window, cx);
            window.focus(&self.composer.focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn insert_citation_token(&self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(citation) = self
            .citations
            .iter()
            .find(|citation| citation_key(citation) == key)
        else {
            return;
        };
        let token_id = format!("citation:{key}");
        if !self
            .composer
            .read(cx)
            .tokens()
            .iter()
            .any(|span| span.token().id().as_ref() == token_id)
        {
            let label = citation
                .quote
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let label = label.chars().take(40).collect::<String>();
            self.composer.update(cx, |composer, cx| {
                _ = composer.replace_with_token(
                    InlineToken::new(token_id, "[quote]").with_label(label),
                    window,
                    cx,
                );
            });
        }
    }

    pub(super) fn active_citations(&self, cx: &App) -> Vec<ResponseCitation> {
        self.citations
            .iter()
            .filter_map(|citation| {
                let id = format!("citation:{}", citation_key(citation));
                self.composer
                    .read(cx)
                    .tokens()
                    .iter()
                    .find(|span| span.token().id().as_ref() == id)
                    .map(|span| {
                        let mut citation = citation.clone();
                        citation.prompt_range = Some(span.range());
                        citation
                    })
            })
            .collect()
    }

    pub(super) fn render_prompt_token(
        &self,
        context: &InlineTokenContext,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui_kit::AnyElement {
        let Some(key) = context.token().id().strip_prefix("citation:") else {
            return crate::gpui_prompt_attachments::render(context, window, cx);
        };
        let label = self
            .citations
            .iter()
            .find(|citation| citation_key(citation) == key)
            .map_or_else(|| context.token().label().to_string(), citation_label);
        div()
            .debug_selector(|| "response-quote-token".into())
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .h(context.line_height())
            .max_w(context.available_width())
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(if context.is_selected() {
                cx.theme().selection
            } else {
                cx.theme().muted.opacity(0.4)
            })
            .text_color(cx.theme().foreground)
            .child("❝")
            .child(
                div()
                    .min_w_0()
                    .max_w(gpui_kit::rems(16.))
                    .text_ellipsis()
                    .child(label),
            )
            .child(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Pencil).xsmall())
            .into_any_element()
    }

    pub(super) fn open_citation_token(
        &mut self,
        id: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = id.strip_prefix("citation:") {
            self.edit_citation(key, position, window, cx);
        }
    }

    pub(super) fn edit_citation(
        &mut self,
        key: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(citation) = self
            .citations
            .iter()
            .find(|citation| citation_key(citation) == key)
        else {
            return;
        };
        let comment = citation.comment.clone();
        self.selected_response = None;
        self.editing_citation = Some((key.to_owned(), position));
        self.citation_editor.update(cx, |editor, cx| {
            editor.set_value(comment, window, cx);
            editor.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn save_citation_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let comment = self.citation_editor.read(cx).value().to_string();
        if comment.len() > CITATION_TEXT_LIMIT {
            return;
        }
        if let Some((key, _)) = &self.editing_citation
            && let Some(citation) = self
                .citations
                .iter_mut()
                .find(|citation| citation_key(citation) == *key)
            && citation.comment != comment
        {
            citation.comment = comment;
            self.draft_revision = self.draft_revision.wrapping_add(1);
        }
        if let Some((key, _)) = self.editing_citation.clone() {
            self.insert_citation_token(&key, window, cx);
        }
        self.dismiss_citation_controls(window, cx);
    }

    pub(super) fn dismiss_citation_controls(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.reaction_menu.is_some() {
            self.dismiss_reaction_menu(window, cx);
            return true;
        }
        if self.editing_citation.take().is_some() {
            window.focus(&self.composer.focus_handle(cx), cx);
            cx.notify();
            return true;
        }
        if let Some(selection) = self.selected_response.take() {
            self.reaction_menu = None;
            gpui_kit::base::TextSelection::clear(window, cx);
            if let Some(state) = self.transcript.get(&selection.citation.message_id) {
                state.update(cx, TextViewState::clear_selection);
                let focus = state.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
            cx.notify();
            return true;
        }
        false
    }

    pub(super) fn clear_submitted_citations(
        &mut self,
        submitted: &[ResponseCitation],
        cx: &mut Context<Self>,
    ) {
        let active = self.active_citations(cx);
        self.citations.retain(|citation| {
            !submitted.iter().any(|sent| {
                sent.message_id == citation.message_id
                    && sent.source_range == citation.source_range
                    && sent.quote == citation.quote
                    && sent.comment == citation.comment
            }) || active
                .iter()
                .any(|token| citation_key(token) == citation_key(citation))
        });
        if self.editing_citation.as_ref().is_some_and(|(key, _)| {
            !self
                .citations
                .iter()
                .any(|citation| citation_key(citation) == *key)
        }) {
            self.editing_citation = None;
        }
        cx.notify();
    }

    pub(super) fn render_sent_citations(
        item: &NativeTranscriptItem,
        owner: &gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .text_color(cx.theme().foreground)
            .children(
                item.citations
                    .iter()
                    .filter(|citation| citation.prompt_range.is_none())
                    .map(|citation| {
                        let message_id = citation.message_id.clone();
                        let owner = owner.clone();
                        Button::new(SharedString::from(format!(
                            "sent-citation:{}:{}",
                            item.id,
                            citation_key(citation)
                        )))
                        .label(format!("❝ {}", citation_label(citation)))
                        .small()
                        .outline()
                        .tooltip(format!("{}\nView quoted response", citation.quote))
                        .on_click(move |_, _, cx| {
                            _ = owner.update(cx, |this, cx| {
                                this.reveal_response(&message_id, cx);
                            });
                        })
                    }),
            )
    }

    fn render_reaction_menu(&self, too_long: bool, window: &Window, cx: &Context<Self>) -> Popover {
        let anchor =
            if self.selected_response.as_ref().is_some_and(|selection| {
                selection.position.y > window.viewport_size().height.half()
            }) {
                gpui_kit::Anchor::BottomLeft
            } else {
                gpui_kit::Anchor::TopLeft
            };
        Popover::new("response-reactions")
            .appearance(false)
            .overlay_closable(false)
            .anchor(anchor)
            .open(self.reaction_menu.is_some())
            .trigger(
                Button::new("response-selection-react")
                    .debug_selector(|| "response-selection-react".into())
                    .label("React")
                    .icon(gpui_kit::assets::IconName::ThumbsUp)
                    .small()
                    .ghost()
                    .disabled(too_long)
                    .accessibility_label("React to selection")
                    .child(selection_key_hint("r"))
                    .child(
                        gpui_kit::component::Icon::new(gpui_kit::assets::IconName::ChevronDown)
                            .small(),
                    ),
            )
            .on_open_change(cx.listener(|this, open: &bool, window, cx| {
                if *open {
                    this.open_reaction_menu(window, cx);
                } else {
                    this.dismiss_reaction_menu(window, cx);
                }
            }))
            .when_some(self.reaction_menu.as_ref(), |popover, (menu, _)| {
                popover.track_focus(&menu.focus_handle(cx)).child(
                    div()
                        .debug_selector(|| "response-reaction-menu".into())
                        .key_context("BoottyResponseReactions")
                        .on_action(cx.listener(|this, action: &SelectReaction, window, cx| {
                            this.select_reaction(action.0, window, cx);
                        }))
                        .child(menu.clone()),
                )
            })
    }

    pub(super) fn render_selection_toolbar(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        if let Some((key, position)) = &self.editing_citation
            && let Some(citation) = self
                .citations
                .iter()
                .find(|citation| citation_key(citation) == *key)
        {
            return self.render_citation_editor(citation, *position, cx);
        }
        let Some(selection) = &self.selected_response else {
            return div().into_any_element();
        };
        if self.reaction_menu.is_none()
            && !self
                .transcript
                .get(&selection.citation.message_id)
                .is_some_and(|state| state.read(cx).focus_handle().is_focused(window))
        {
            return div().into_any_element();
        }
        let too_long = selection.citation.quote.len() > CITATION_TEXT_LIMIT;
        let surface = div()
            .id("response-selection-toolbar")
            .debug_selector(|| "response-selection-toolbar".into())
            .popover_style(cx)
            .p_1()
            .flex()
            .items_center()
            .gap_1()
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, cx| {
                // The toolbar uses the captured source quote even if window selection clears.
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                if this.reaction_menu.is_none() {
                    this.dismiss_citation_controls(window, cx);
                }
            }))
            .child(
                Button::new("response-selection-comment")
                    .debug_selector(|| "response-selection-comment".into())
                    .label(if too_long {
                        "Shorten selection"
                    } else {
                        "Comment"
                    })
                    .icon(gpui_kit::assets::IconName::MessageCircle)
                    .small()
                    .ghost()
                    .disabled(too_long)
                    .accessibility_label("Comment on selection")
                    .child(selection_key_hint("c"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.comment_response(None, window, cx)),
                    ),
            )
            .child(self.render_reaction_menu(too_long, window, cx))
            .child(
                Button::new("response-selection-copy")
                    .debug_selector(|| "response-selection-copy".into())
                    .label("Copy")
                    .icon(gpui_kit::assets::IconName::Copy)
                    .small()
                    .ghost()
                    .child(selection_key_hint("y"))
                    .accessibility_label("Copy selection")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.copy_response_selection(window, cx);
                    })),
            );
        gpui_kit::deferred(
            gpui_kit::base::Positioner::corner(gpui_kit::Anchor::TopLeft, selection.position)
                .occlude()
                .child(surface),
        )
        .into_any_element()
    }

    fn render_citation_editor(
        &self,
        _citation: &ResponseCitation,
        position: Point<Pixels>,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let too_long = self.citation_editor.read(cx).value().len() > CITATION_TEXT_LIMIT;
        let surface = div()
            .id("response-quote-editor")
            .debug_selector(|| "response-quote-editor".into())
            .popover_style(cx)
            .w(gpui_kit::rems(18.))
            .max_w_full()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.dismiss_citation_controls(window, cx);
            }))
            .child(
                Textarea::new(&self.citation_editor)
                    .appearance(false)
                    .bordered(false)
                    .aria_label("Comment on selected text"),
            )
            .when(too_long, |surface| {
                surface.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child("Comment is too long"),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        Button::new("citation-comment-cancel")
                            .label("Cancel")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss_citation_controls(window, cx);
                            })),
                    )
                    .child(
                        Button::new("citation-comment-save")
                            .label("Save")
                            .xsmall()
                            .ghost()
                            .disabled(too_long)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_citation_comment(window, cx);
                            })),
                    ),
            );
        gpui_kit::deferred(
            gpui_kit::base::Positioner::corner(gpui_kit::Anchor::TopLeft, position)
                .occlude()
                .child(surface),
        )
        .into_any_element()
    }
}

fn selection_key_hint(key: &str) -> gpui_kit::AnyElement {
    gpui_kit::Keystroke::parse(key).map_or_else(
        |_| div().into_any_element(),
        |key| gpui_kit::component::kbd::Kbd::new(key).into_any_element(),
    )
}
