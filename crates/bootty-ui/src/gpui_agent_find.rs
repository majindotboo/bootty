//! Find in a conversation: a query's matches across its messages, highlighted, and stepped
//! through one at a time.
use super::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::text::{RangeHighlight, RenderedText};
use std::ops::Range;

/// Matches highlighted at most; the count still says how many there are.
// Raise it with highlights limited to the rows on screen if conversations need more.
const MAX_HIGHLIGHTS: usize = 1000;

/// The find bar's query and what it found.
pub(super) struct TranscriptFind {
    input: Entity<InputState>,
    /// Each searched message's rendered text and the query's matches in it.
    found: BTreeMap<String, (RenderedText, Vec<Range<usize>>)>,
    /// The match shown: its message and its place among that message's matches.
    current: Option<(String, usize)>,
    _subscription: Subscription,
}

impl NativeAgentSessionView {
    /// Opens the find bar, or returns to its query when it is open.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_word_selection(cx);
        if let Some(find) = &self.find {
            find.input.focus_handle(cx).focus(window, cx);
            return;
        }
        // Find reads every row's text, so folded rows take theirs first.
        let folded = self.folded_text.iter().cloned().collect::<Vec<_>>();
        self.unfold_text(folded.iter().map(String::as_str), cx);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in conversation"));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.search_find(cx);
            }
        });
        input.focus_handle(cx).focus(window, cx);
        self.find = Some(TranscriptFind {
            input,
            found: BTreeMap::new(),
            current: None,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Closes the find bar and clears its highlights, returning to the composer.
    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.take() else {
            return;
        };
        for id in find.found.keys() {
            if let Some(state) = self.transcript.get(id) {
                state.update(cx, TextViewState::clear_range_highlights);
            }
        }
        self.composer.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// The terminal's find actions, taken by the focused conversation.
    pub fn find_action(
        &mut self,
        action: &crate::app_actions::TerminalFindAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::app_actions::TerminalFindAction as Find;
        match action {
            Find::Prompt => self.open_find(window, cx),
            Find::Next => self.step_find(true, window, cx),
            Find::Previous => self.step_find(false, window, cx),
            Find::Close => self.close_find(window, cx),
            _ => {}
        }
    }

    /// The messages find searches, in the order they show.
    pub(super) fn findable_messages(&self) -> Vec<String> {
        self.timeline
            .iter()
            .filter_map(|row| match row {
                TranscriptRow::Message { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    /// Searches every shown message again for the query.
    fn search_find(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &self.find else {
            return;
        };
        let query = find.input.read(cx).value().to_string();
        let previous = find.found.keys().cloned().collect::<Vec<_>>();
        let mut found = BTreeMap::new();
        for id in self.findable_messages() {
            if let Some(state) = self.transcript.get(&id) {
                let text = state.read(cx).rendered_text();
                let ranges = matches(text.as_str(), &query);
                found.insert(id, (text, ranges));
            }
        }
        for id in previous {
            if !found.contains_key(&id)
                && let Some(state) = self.transcript.get(&id)
            {
                state.update(cx, TextViewState::clear_range_highlights);
            }
        }
        let order = self.findable_messages();
        if let Some(find) = &mut self.find {
            find.found = found;
            find.current = order.iter().find_map(|id| {
                find.found
                    .get(id)
                    .filter(|(_, ranges)| !ranges.is_empty())
                    .map(|_| (id.clone(), 0))
            });
        }
        self.paint_find(cx);
        cx.notify();
    }

    /// Searches one message again when its text changed while the find bar is open.
    pub(super) fn find_observed(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(find) = &self.find else {
            return;
        };
        let Some(state) = self.transcript.get(id) else {
            return;
        };
        let text = state.read(cx).rendered_text();
        if find
            .found
            .get(id)
            .is_some_and(|(searched, _)| *searched == text)
            || !self.findable_messages().iter().any(|shown| shown == id)
        {
            return;
        }
        let query = find.input.read(cx).value().to_string();
        let ranges = matches(text.as_str(), &query);
        if let Some(find) = &mut self.find {
            if find
                .current
                .as_ref()
                .is_some_and(|(current, index)| current == id && *index >= ranges.len())
            {
                find.current = None;
            }
            if find.current.is_none() && !ranges.is_empty() {
                find.current = Some((id.to_owned(), 0));
            }
            find.found.insert(id.to_owned(), (text, ranges));
        }
        self.paint_find(cx);
        cx.notify();
    }

    /// Every match in order, as its message and place there.
    fn find_order(&self) -> Vec<(String, usize)> {
        let Some(find) = &self.find else {
            return Vec::new();
        };
        self.findable_messages()
            .into_iter()
            .flat_map(|id| {
                let count = find.found.get(&id).map_or(0, |(_, ranges)| ranges.len());
                (0..count).map(move |index| (id.clone(), index))
            })
            .collect()
    }

    /// Moves to the next match, or the previous one, wrapping around, and shows it.
    fn step_find(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.find_order();
        let Some(find) = &mut self.find else {
            return;
        };
        if order.is_empty() {
            return;
        }
        let at = find
            .current
            .as_ref()
            .and_then(|current| order.iter().position(|entry| entry == current));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => order.len().saturating_sub(1),
            (Some(at), true) => at.saturating_add(1).checked_rem(order.len()).unwrap_or(0),
            (Some(at), false) => at
                .checked_sub(1)
                .unwrap_or_else(|| order.len().saturating_sub(1)),
        };
        find.current = order.get(next).cloned();
        self.paint_find(cx);
        self.reveal_find(window, cx);
        cx.notify();
    }

    /// Scrolls the current match into view, unfolding a long prompt that hides it.
    fn reveal_find(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some((id, range)) = self.find.as_ref().and_then(|find| {
            let (id, index) = find.current.as_ref()?;
            let range = find.found.get(id)?.1.get(*index)?.clone();
            Some((id.clone(), range))
        }) else {
            return;
        };
        let row = self.timeline.iter().position(
            |row| matches!(row, TranscriptRow::Message { id: message, .. } if *message == id),
        );
        if self
            .record
            .snapshot
            .transcript
            .iter()
            .any(|item| item.id == id && collapsible_prompt(item))
            && self.expanded_messages.insert(id.clone())
            && let Some(row) = row
        {
            self.list.remeasure_items(row..row.saturating_add(1));
        }
        if let Some(row) = row {
            self.list.scroll_to_reveal_item(row);
        }
        if let Some(state) = self.transcript.get(&id) {
            state.update(cx, |state, cx| {
                _ = state.reveal_range(range, cx);
            });
        }
    }

    /// Highlights the matches, the current one stronger.
    fn paint_find(&self, cx: &mut Context<Self>) {
        let Some(find) = &self.find else {
            return;
        };
        let (match_color, current_color) = (
            cx.theme().warning.opacity(0.25),
            cx.theme().warning.opacity(0.6),
        );
        let mut budget = MAX_HIGHLIGHTS;
        for (id, (_, ranges)) in &find.found {
            let Some(state) = self.transcript.get(id) else {
                continue;
            };
            let shown = ranges.len().min(budget);
            budget = budget.saturating_sub(shown);
            let highlights = ranges
                .iter()
                .take(shown)
                .enumerate()
                .map(|(index, range)| {
                    let current = find
                        .current
                        .as_ref()
                        .is_some_and(|(current, at)| current == id && *at == index);
                    RangeHighlight::new(
                        range.clone(),
                        if current { current_color } else { match_color },
                    )
                })
                .collect::<Vec<_>>();
            state.update(cx, |state, cx| {
                _ = state.set_range_highlights(highlights, cx);
            });
        }
    }

    pub(super) fn render_find(&self, cx: &Context<Self>) -> Option<gpui_kit::Div> {
        let find = self.find.as_ref()?;
        let order = self.find_order();
        let position = find
            .current
            .as_ref()
            .and_then(|current| order.iter().position(|entry| entry == current));
        let count = if find.input.read(cx).value().is_empty() {
            String::new()
        } else if order.is_empty() {
            "No results".to_owned()
        } else {
            format!(
                "{} of {}",
                position.map_or(0, |at| at.saturating_add(1)),
                order.len()
            )
        };
        let step = |id: &'static str, icon: IconName, label: &'static str, forward: bool| {
            Button::new(id)
                .icon(icon)
                .ghost()
                .xsmall()
                .disabled(order.is_empty())
                .accessibility_label(label)
                .tooltip(label)
                .debug_selector(move || id.into())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.step_find(forward, window, cx);
                }))
        };
        Some(
            div()
                .debug_selector(|| "conversation-find".into())
                .flex()
                .items_center()
                .gap_1()
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .shadow_md()
                .capture_action(cx.listener(
                    |this, _: &gpui_kit::component::input::Escape, window, cx| {
                        this.close_find(window, cx);
                        cx.stop_propagation();
                    },
                ))
                // Enter goes to the next match and Shift-Enter to the previous one.
                .capture_action(cx.listener(
                    |this, enter: &gpui_kit::component::input::Enter, window, cx| {
                        this.step_find(!enter.shift, window, cx);
                        cx.stop_propagation();
                    },
                ))
                .child(
                    div().w(gpui_kit::rems(14.)).child(
                        Input::new(&find.input)
                            .context_menu(crate::gpui::text_menu(&find.input))
                            .xsmall()
                            .aria_label("Find in conversation"),
                    ),
                )
                .child(
                    div()
                        .debug_selector({
                            let count = count.clone();
                            move || format!("conversation-find-count:{count}")
                        })
                        .min_w(gpui_kit::rems(4.))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(count),
                )
                .child(step(
                    "conversation-find-previous",
                    IconName::ChevronUp,
                    "Previous match",
                    false,
                ))
                .child(step(
                    "conversation-find-next",
                    IconName::ChevronDown,
                    "Next match",
                    true,
                ))
                .child(
                    Button::new("conversation-find-close")
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .accessibility_label("Close find")
                        .tooltip("Close find")
                        .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
                ),
        )
    }
}

/// Where `query` occurs in `text`, ignoring the case of ASCII letters, without overlaps.
fn matches(text: &str, query: &str) -> Vec<Range<usize>> {
    let (haystack, needle) = (text.as_bytes(), query.as_bytes());
    let mut found = Vec::new();
    if needle.is_empty() {
        return found;
    }
    let mut start = 0;
    while let Some(at) = haystack.get(start..).and_then(|rest| {
        rest.windows(needle.len())
            .position(|window| window.eq_ignore_ascii_case(needle))
    }) {
        let begin = start.saturating_add(at);
        let end = begin.saturating_add(needle.len());
        if text.is_char_boundary(begin) && text.is_char_boundary(end) {
            found.push(begin..end);
            start = end;
        } else {
            start = begin.saturating_add(1);
        }
    }
    found
}
