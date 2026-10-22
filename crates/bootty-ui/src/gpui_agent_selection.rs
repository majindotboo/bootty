//! Word dragging extends Kit's normal pointer selection over rendered word boundaries.
use super::*;
use gpui_kit::base::TextSelection;
use gpui_kit::component::text::{RangeHighlight, RenderedText};
use gpui_kit::{InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Clone)]
struct Anchor {
    id: String,
    range: Range<usize>,
    text: RenderedText,
}

#[derive(Default)]
enum Gesture {
    #[default]
    None,
    Pressed(MouseDownEvent, Anchor),
    Dragging(Anchor),
}

struct WordRange {
    id: String,
    text: RenderedText,
    range: Range<usize>,
    native: Range<usize>,
}

#[derive(Default)]
pub(super) struct WordSelection {
    gesture: Gesture,
    /// Input is forwarded once to Kit, outside the original event dispatch.
    forwarding: bool,
    ranges: Vec<WordRange>,
}

impl WordSelection {
    pub(super) fn range_for(&self, id: &str) -> Option<Range<usize>> {
        self.ranges
            .iter()
            .find(|selected| selected.id == id)
            .map(|selected| selected.range.clone())
    }

    pub(super) const fn owns_selection(&self) -> bool {
        !matches!(self.gesture, Gesture::None) || !self.ranges.is_empty()
    }

    fn copy_text(&self) -> String {
        self.ranges
            .iter()
            .filter_map(|selected| selected.text.as_str().get(selected.range.clone()))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned()
    }
}

impl NativeAgentSessionView {
    pub(super) fn word_selection_focused(&self, window: &Window, cx: &App) -> bool {
        self.word_selection.ranges.iter().any(|selected| {
            self.transcript
                .get(&selected.id)
                .is_some_and(|state| state.read(cx).focus_handle().is_focused(window))
        })
    }

    pub(super) fn arm_word_selection(
        &mut self,
        event: &MouseDownEvent,
        window: &Window,
        cx: &Context<Self>,
    ) {
        if event.click_count != 2 || self.word_selection.forwarding {
            return;
        }
        let anchor = self.transcript.iter().find_map(|(id, state)| {
            let state = state.read(cx);
            if !state.bounds().contains(&event.position) || !state.focus_handle().is_focused(window)
            {
                return None;
            }
            let range = state
                .rendered_text()
                .range_for_source(state.selected_source_range()?)?;
            Some(Anchor {
                id: id.clone(),
                range,
                text: state.rendered_text(),
            })
        });
        if let Some(anchor) = anchor {
            self.word_selection.gesture = Gesture::Pressed(event.clone(), anchor);
        }
    }

    pub(super) fn clear_word_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.word_selection.owns_selection();
        self.word_selection.gesture = Gesture::None;
        for selected in self.word_selection.ranges.drain(..) {
            if let Some(state) = self.transcript.get(&selected.id) {
                state.update(cx, TextViewState::clear_range_highlights);
            }
        }
        changed
    }

    pub(super) fn dismiss_word_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.clear_word_selection(cx) {
            return false;
        }
        TextSelection::clear(window, cx);
        cx.notify();
        true
    }

    pub(super) fn copy_word_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.word_selection_focused(window, cx) {
            cx.propagate();
            return;
        }
        let current = self.word_selection.ranges.iter().all(|selected| {
            self.transcript.get(&selected.id).is_some_and(|state| {
                state
                    .read(cx)
                    .rendered_text()
                    .as_str()
                    .get(..selected.range.end)
                    == selected.text.as_str().get(..selected.range.end)
            })
        });
        if current {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                self.word_selection.copy_text(),
            ));
            cx.stop_propagation();
        } else {
            self.dismiss_word_selection(window, cx);
            cx.propagate();
        }
    }

    pub(super) fn word_selection_observed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Gesture::Dragging(anchor) = &self.word_selection.gesture {
            let anchor = anchor.clone();
            _ = TextSelection::selected_text(window, cx);
            self.refresh_word_selection(&anchor, cx);
        }
    }

    fn refresh_word_selection(&mut self, anchor: &Anchor, cx: &mut Context<Self>) {
        let current = self.transcript.get(&anchor.id).is_some_and(|state| {
            state
                .read(cx)
                .rendered_text()
                .as_str()
                .get(..anchor.range.end)
                == anchor.text.as_str().get(..anchor.range.end)
        });
        if !current {
            self.clear_word_selection(cx);
            return;
        }
        let mut ranges = Vec::new();
        for id in self.findable_messages() {
            let Some(state) = self.transcript.get(&id) else {
                continue;
            };
            let state = state.read(cx);
            let text = state.rendered_text();
            let native = state
                .selected_source_range()
                .and_then(|source| text.range_for_source(source));
            let mut range = native
                .as_ref()
                .map(|range| whole_words(text.as_str(), range.clone()));
            if id == anchor.id {
                range = Some(range.map_or_else(
                    || anchor.range.clone(),
                    |range| range.start.min(anchor.range.start)..range.end.max(anchor.range.end),
                ));
            }
            if let Some(range) = range.filter(|range| !range.is_empty()) {
                let native = native.unwrap_or(range.start..range.start);
                ranges.push(WordRange {
                    id,
                    text,
                    range,
                    native,
                });
            }
        }
        if self.word_selection.ranges.len() == ranges.len()
            && self
                .word_selection
                .ranges
                .iter()
                .zip(&ranges)
                .all(|(old, new)| {
                    old.id == new.id
                        && old.range == new.range
                        && old.native == new.native
                        && old.text == new.text
                })
        {
            return;
        }
        for selected in &self.word_selection.ranges {
            if let Some(state) = self.transcript.get(&selected.id) {
                state.update(cx, TextViewState::clear_range_highlights);
            }
        }
        self.word_selection.ranges = ranges;
        self.paint_word_selection(true, cx);
    }

    fn paint_word_selection(&self, held: bool, cx: &mut Context<Self>) {
        let color = cx.theme().selection;
        for selected in &self.word_selection.ranges {
            let Some(state) = self.transcript.get(&selected.id) else {
                continue;
            };
            let highlights = if held {
                // Kit paints the character selection; add only the missing word edges.
                vec![
                    selected.range.start..selected.native.start,
                    selected.native.end..selected.range.end,
                ]
            } else {
                vec![selected.range.clone()]
            };
            state.update(cx, |state, cx| {
                _ = state.set_range_highlights(
                    highlights
                        .into_iter()
                        .filter(|range| !range.is_empty())
                        .map(|range| RangeHighlight::new(range, color)),
                    cx,
                );
            });
        }
    }

    fn finish_word_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match std::mem::take(&mut self.word_selection.gesture) {
            Gesture::None => {}
            Gesture::Pressed(_, anchor) => {
                if self
                    .record
                    .snapshot
                    .transcript
                    .iter()
                    .any(|item| item.id == anchor.id && item.role == "assistant")
                    && let Some(state) = self.transcript.get(&anchor.id).cloned()
                {
                    self.capture_response_selection(&anchor.id, &state, window, cx);
                }
                cx.notify();
            }
            Gesture::Dragging(anchor) => {
                self.refresh_word_selection(&anchor, cx);
                TextSelection::clear(window, cx);
                self.paint_word_selection(false, cx);
                if let [selected] = self.word_selection.ranges.as_slice()
                    && let Some(item) = self
                        .record
                        .snapshot
                        .transcript
                        .iter()
                        .find(|item| item.id == selected.id && item.role == "assistant")
                    && let Some(source) =
                        copy_mode::source_range(&selected.text, selected.range.clone())
                {
                    let source = self.link_marks.get(&selected.id).map_or_else(
                        || source.clone(),
                        |marks| marks.source_range(source.clone()),
                    );
                    if let Some(quote) = item.display_text().get(source.clone()).map(str::to_owned)
                    {
                        self.selected_response = Some(ResponseSelection::new(
                            ResponseCitation {
                                message_id: selected.id.clone(),
                                source_range: source,
                                prompt_range: None,
                                quote,
                                comment: String::new(),
                            },
                            self.word_selection.copy_text(),
                            window.mouse_position(),
                        ));
                    }
                }
                cx.notify();
            }
        }
    }

    pub(super) fn render_word_selection_events(cx: &Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        gpui_kit::canvas(
            |_, _, _| {},
            move |_, (), window, _| {
                let down = owner.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                    if phase.capture() && event.button == MouseButton::Left {
                        _ = down.update(cx, |this, cx| {
                            if !this.word_selection.forwarding && this.clear_word_selection(cx) {
                                cx.notify();
                            }
                        });
                    }
                });
                let moving = owner.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.capture() || !event.dragging() {
                        return;
                    }
                    let active = moving
                        .update(cx, |this, _| {
                            !this.word_selection.forwarding
                                && !matches!(this.word_selection.gesture, Gesture::None)
                        })
                        .unwrap_or(false);
                    if !active {
                        return;
                    }
                    let moving = moving.clone();
                    let event = event.clone();
                    window.defer(cx, move |window, cx| {
                        let down = moving
                            .update(cx, |this, cx| {
                                if let Gesture::Pressed(down, anchor) = &this.word_selection.gesture
                                {
                                    let mut down = down.clone();
                                    let anchor = anchor.clone();
                                    down.click_count = 1;
                                    this.close_find(window, cx);
                                    this.leave_copy_mode(cx);
                                    this.selected_response = None;
                                    this.word_selection.gesture = Gesture::Dragging(anchor);
                                    this.word_selection.forwarding = true;
                                    Some(down)
                                } else {
                                    None
                                }
                            })
                            .ok()
                            .flatten();
                        if let Some(down) = down {
                            // Hand the held gesture to Kit once; it owns hit testing and scrolling.
                            TextSelection::clear(window, cx);
                            window.dispatch_event(down.to_platform_input(), cx);
                            window.dispatch_event(event.to_platform_input(), cx);
                            _ = moving.update(cx, |this, _| this.word_selection.forwarding = false);
                        }
                        _ = TextSelection::selected_text(window, cx);
                        _ = moving.update(cx, |this, cx| {
                            if let Gesture::Dragging(anchor) = &this.word_selection.gesture {
                                let anchor = anchor.clone();
                                this.refresh_word_selection(&anchor, cx);
                            }
                        });
                    });
                });
                let released = owner;
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if !phase.capture() || event.button != MouseButton::Left {
                        return;
                    }
                    let active = released
                        .update(cx, |this, _| {
                            !this.word_selection.forwarding
                                && !matches!(this.word_selection.gesture, Gesture::None)
                        })
                        .unwrap_or(false);
                    if !active {
                        return;
                    }
                    let released = released.clone();
                    window.defer(cx, move |window, cx| {
                        _ = TextSelection::selected_text(window, cx);
                        _ = released.update(cx, |this, cx| this.finish_word_selection(window, cx));
                    });
                });
            },
        )
        .absolute()
        .inset_0()
    }
}

fn whole_words(text: &str, range: Range<usize>) -> Range<usize> {
    let mut expanded = range.clone();
    for (start, word) in text.split_word_bound_indices() {
        let end = start.saturating_add(word.len());
        if start < range.start && range.start < end {
            expanded.start = start;
        }
        // The nearest caret can sit just before the glyph under the pointer.
        if start <= range.end && range.end < end && (start < range.end || !word.trim().is_empty()) {
            expanded.end = end;
        }
    }
    expanded
}
