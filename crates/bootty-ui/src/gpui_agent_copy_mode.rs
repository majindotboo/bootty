//! Keyboard copy mode for a conversation: a cursor over its messages that moves the way vim's
//! does, selections by character, line or column, and copy, comment or react on what is
//! selected. The terminal's copy mode keys enter it.
use super::*;
use crate::presentation::transcript_copy::{self as engine, CopyCursor, CopyMotion, CopySelection};
use gpui_kit::component::text::{RangeHighlight, RenderedText};
use std::ops::Range;

/// Lines Ctrl-D and Ctrl-U move.
const HALF_PAGE: usize = 12;

pub(super) struct CopyMode {
    /// The shown messages' text, one after another.
    text: String,
    /// Each message's place in `text`, its trailing line break left out.
    messages: Vec<(String, Range<usize>)>,
    cursor: CopyCursor,
    /// Where the selection started, and how it grows.
    selection: Option<(usize, CopySelection)>,
    count: Option<usize>,
    /// A key waiting for the next: `g` for `gg`, or `f`, `F`, `t` or `T` for their character.
    pending: Option<char>,
    last_find: Option<CopyMotion>,
}

impl CopyMode {
    /// Adds a typed digit to the count before a motion. A 0 that starts no count moves to the
    /// line's start instead.
    fn add_count_digit(&mut self, typed: Option<char>) -> bool {
        let Some(digit) = typed
            .and_then(|typed| typed.to_digit(10))
            .filter(|digit| (*digit > 0 || self.count.is_some()) && self.pending.is_none())
        else {
            return false;
        };
        let digit = usize::try_from(digit).unwrap_or(0);
        self.count = Some(
            self.count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(digit)
                .min(9999),
        );
        true
    }

    /// The motion a waiting key and the one after it make: `gg`, or a find of the character.
    const fn finish_pending(&mut self, pending: char, typed: Option<char>) -> Option<CopyMotion> {
        let motion = match (pending, typed) {
            ('g', Some('g')) => CopyMotion::TextStart,
            ('f' | 'F' | 't' | 'T', Some(target)) => CopyMotion::Find {
                target,
                forward: pending.is_lowercase(),
                till: matches!(pending, 't' | 'T'),
            },
            _ => return None,
        };
        if matches!(motion, CopyMotion::Find { .. }) {
            self.last_find = Some(motion);
        }
        Some(motion)
    }
}

impl NativeAgentSessionView {
    /// Enters copy mode, adopting a selection made with the pointer; otherwise the cursor starts
    /// at the latest message.
    pub fn enter_copy_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_find(window, cx);
        let mut text = String::new();
        let mut messages = Vec::new();
        let mut adopted = None;
        for id in self.findable_messages() {
            let Some(state) = self.transcript.get(&id) else {
                continue;
            };
            let state = state.read(cx);
            let rendered = state.rendered_text();
            let shown = rendered.as_str().trim_end_matches('\n');
            let start = text.len();
            if adopted.is_none()
                && !state.selected_text().is_empty()
                && let Some(range) = state
                    .selected_source_range()
                    .and_then(|source| rendered.range_for_source(source))
            {
                adopted = Some(start.saturating_add(range.start)..start.saturating_add(range.end));
            }
            text.push_str(shown);
            messages.push((id, start..text.len()));
            text.push('\n');
        }
        let Some((_, latest)) = messages.last() else {
            return;
        };
        let (cursor, selection) = adopted.map_or((latest.start, None), |range| {
            let end = text
                .get(..range.end)
                .and_then(|before| before.chars().next_back())
                .map_or(range.end, |c| range.end.saturating_sub(c.len_utf8()));
            (end, Some((range.start, CopySelection::Character)))
        });
        self.copy_mode = Some(CopyMode {
            cursor: engine::cursor_at(&text, cursor),
            text,
            messages,
            selection,
            count: None,
            pending: None,
            last_find: None,
        });
        self.copy_focus.focus(window, cx);
        self.paint_copy_mode(cx);
        self.reveal_copy_cursor(cx);
        cx.notify();
    }

    /// Leaves copy mode and returns to the composer.
    fn exit_copy_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.leave_copy_mode(cx) {
            self.composer.focus_handle(cx).focus(window, cx);
        }
    }

    /// Leaves copy mode and clears its highlights. Returns whether it was on.
    pub(super) fn leave_copy_mode(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(copy) = self.copy_mode.take() else {
            return false;
        };
        for (id, _) in &copy.messages {
            if let Some(state) = self.transcript.get(id) {
                state.update(cx, TextViewState::clear_range_highlights);
            }
        }
        cx.notify();
        true
    }

    /// The keys copy mode takes while it is on.
    pub(super) fn copy_mode_key(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(copy) = &mut self.copy_mode else {
            return;
        };
        let keystroke = &event.keystroke;
        let typed = keystroke
            .key_char
            .as_deref()
            .filter(|_| !keystroke.modifiers.control && !keystroke.modifiers.platform)
            .and_then(|typed| typed.chars().next());
        if copy.add_count_digit(typed) {
            cx.stop_propagation();
            return;
        }
        let count = copy.count.take().unwrap_or(1);
        let handled = if let Some(pending) = copy.pending.take() {
            if let Some(motion) = copy.finish_pending(pending, typed) {
                self.move_copy_cursor(motion, count, cx);
            }
            true
        } else if keystroke.modifiers.control {
            self.copy_mode_control_key(&keystroke.key, count, window, cx)
        } else if let Some((motion, repeat)) = plain_motion(&keystroke.key, typed) {
            self.move_copy_cursor(motion, repeat.unwrap_or(count), cx);
            true
        } else {
            self.copy_mode_command(&keystroke.key, typed, count, window, cx)
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn copy_mode_control_key(
        &mut self,
        key: &str,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match key {
            "d" => self.move_copy_cursor(CopyMotion::Down, HALF_PAGE.saturating_mul(count), cx),
            "u" => self.move_copy_cursor(CopyMotion::Up, HALF_PAGE.saturating_mul(count), cx),
            "v" => self.toggle_copy_selection(CopySelection::Block, cx),
            "c" | "g" => self.exit_copy_mode(window, cx),
            _ => return false,
        }
        true
    }

    /// The keys that select, copy, quote or leave, rather than move.
    fn copy_mode_command(
        &mut self,
        key: &str,
        typed: Option<char>,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(copy) = &mut self.copy_mode else {
            return false;
        };
        match (key, typed) {
            (_, Some(key @ ('g' | 'f' | 'F' | 't' | 'T'))) => {
                copy.pending = Some(key);
                if count > 1 {
                    copy.count = Some(count);
                }
            }
            (_, Some(key @ (';' | ','))) => {
                if let Some(CopyMotion::Find {
                    target,
                    forward,
                    till,
                }) = copy.last_find
                {
                    let forward = if key == ';' { forward } else { !forward };
                    self.move_copy_cursor(
                        CopyMotion::Find {
                            target,
                            forward,
                            till,
                        },
                        count,
                        cx,
                    );
                }
            }
            (_, Some('v')) => self.toggle_copy_selection(CopySelection::Character, cx),
            (_, Some('V')) => self.toggle_copy_selection(CopySelection::Line, cx),
            (_, Some('o')) => {
                if let Some((anchor, kind)) = copy.selection {
                    let cursor = copy.cursor.offset;
                    copy.selection = Some((cursor, kind));
                    copy.cursor = engine::cursor_at(&copy.text, anchor);
                    self.paint_copy_mode(cx);
                    self.reveal_copy_cursor(cx);
                }
            }
            ("enter", _) | (_, Some('y')) => self.copy_copy_selection(window, cx),
            (_, Some(key @ ('c' | 'r'))) => self.quote_copy_selection(key == 'r', window, cx),
            ("escape", _) => {
                if copy.selection.take().is_some() {
                    self.paint_copy_mode(cx);
                } else {
                    self.exit_copy_mode(window, cx);
                }
            }
            (_, Some('q')) => self.exit_copy_mode(window, cx),
            _ => {
                if count > 1 {
                    copy.count = Some(count);
                }
                return false;
            }
        }
        true
    }

    fn move_copy_cursor(&mut self, motion: CopyMotion, count: usize, cx: &mut Context<Self>) {
        let Some(copy) = &mut self.copy_mode else {
            return;
        };
        copy.cursor = engine::apply_motion(&copy.text, copy.cursor, motion, count);
        self.paint_copy_mode(cx);
        self.reveal_copy_cursor(cx);
    }

    /// Starts a selection of `kind` at the cursor, switches an open one to `kind`, or ends one of
    /// that kind.
    fn toggle_copy_selection(&mut self, kind: CopySelection, cx: &mut Context<Self>) {
        let Some(copy) = &mut self.copy_mode else {
            return;
        };
        copy.selection = match copy.selection {
            Some((_, current)) if current == kind => None,
            Some((anchor, _)) => Some((anchor, kind)),
            None => Some((copy.cursor.offset, kind)),
        };
        self.paint_copy_mode(cx);
    }

    /// The selection's ranges of the whole text, or the cursor's line when nothing is selected.
    fn copy_ranges(copy: &CopyMode) -> Vec<Range<usize>> {
        let (anchor, kind) = copy
            .selection
            .unwrap_or((copy.cursor.offset, CopySelection::Line));
        engine::selected_ranges(&copy.text, anchor, copy.cursor.offset, kind)
    }

    /// Copies the selection, or the cursor's line, and leaves copy mode.
    fn copy_copy_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(copy) = &self.copy_mode else {
            return;
        };
        let text = engine::selected_text(&copy.text, &Self::copy_ranges(copy));
        self.exit_copy_mode(window, cx);
        self.copy("copy-mode".to_owned(), text, window, cx);
    }

    /// Comments on the selection, or reacts to it, when it lies within one reply.
    fn quote_copy_selection(&mut self, react: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(copy) = &self.copy_mode else {
            return;
        };
        if copy.selection.is_none() {
            return;
        }
        let ranges = Self::copy_ranges(copy);
        let (Some(first), Some(last)) = (ranges.first(), ranges.last()) else {
            return;
        };
        let range = first.start..last.end;
        let Some((id, span)) = copy
            .messages
            .iter()
            .find(|(_, span)| span.start <= range.start && range.end <= span.end)
        else {
            return;
        };
        let local = range.start.saturating_sub(span.start)..range.end.saturating_sub(span.start);
        let copy_text = engine::selected_text(&copy.text, &ranges);
        let Some(item) = self
            .record
            .snapshot
            .transcript
            .iter()
            .find(|item| item.id == *id && item.role == "assistant")
        else {
            return;
        };
        let Some(state) = self.transcript.get(id) else {
            return;
        };
        let Some(source) = source_range(&state.read(cx).rendered_text(), local) else {
            return;
        };
        let source = self.link_marks.get(id).map_or_else(
            || source.clone(),
            |marks| marks.source_range(source.clone()),
        );
        let Some(quote) = item.display_text().get(source.clone()).map(str::to_owned) else {
            return;
        };
        let viewport = window.viewport_size();
        let citation = ResponseCitation {
            message_id: id.clone(),
            source_range: source,
            prompt_range: None,
            quote,
            comment: String::new(),
        };
        self.exit_copy_mode(window, cx);
        self.selected_response = Some(ResponseSelection::new(
            citation,
            copy_text,
            gpui_kit::point(
                gpui_kit::px(f32::from(viewport.width) / 2.),
                gpui_kit::px(f32::from(viewport.height) / 2.),
            ),
        ));
        if react {
            self.open_reaction_menu(window, cx);
        } else {
            self.comment_response(None, window, cx);
        }
    }

    /// Paints the selection and the cursor over the messages they cover.
    fn paint_copy_mode(&self, cx: &mut Context<Self>) {
        let Some(copy) = &self.copy_mode else {
            return;
        };
        let selected = copy.selection.map_or_else(Vec::new, |(anchor, kind)| {
            engine::selected_ranges(&copy.text, anchor, copy.cursor.offset, kind)
        });
        let cursor = copy.cursor.offset
            ..copy
                .text
                .get(copy.cursor.offset..)
                .and_then(|after| after.chars().next())
                .map_or(copy.cursor.offset, |c| {
                    copy.cursor.offset.saturating_add(c.len_utf8())
                });
        let (selection_color, cursor_color) =
            (cx.theme().selection, cx.theme().primary.opacity(0.45));
        for (id, span) in &copy.messages {
            let Some(state) = self.transcript.get(id) else {
                continue;
            };
            let local = |range: &Range<usize>| {
                let start = range.start.max(span.start);
                let end = range.end.min(span.end);
                (start < end)
                    .then(|| start.saturating_sub(span.start)..end.saturating_sub(span.start))
            };
            let highlights = selected
                .iter()
                .filter_map(|range| {
                    local(range).map(|range| RangeHighlight::new(range, selection_color))
                })
                .chain(local(&cursor).map(|range| RangeHighlight::new(range, cursor_color)))
                .collect::<Vec<_>>();
            state.update(cx, |state, cx| {
                _ = state.set_range_highlights(highlights, cx);
            });
        }
    }

    /// Scrolls the cursor's message, and the cursor within it, into view.
    fn reveal_copy_cursor(&self, cx: &mut Context<Self>) {
        let Some(copy) = &self.copy_mode else {
            return;
        };
        let offset = copy.cursor.offset;
        let Some((id, span)) = copy
            .messages
            .iter()
            .find(|(_, span)| span.start <= offset && offset <= span.end)
            .cloned()
        else {
            return;
        };
        if let Some(row) = self.timeline.iter().position(
            |row| matches!(row, TranscriptRow::Message { id: message, .. } if *message == id),
        ) {
            self.list.scroll_to_reveal_item(row);
        }
        let local = offset.saturating_sub(span.start);
        if let Some(state) = self.transcript.get(&id) {
            state.update(cx, |state, cx| {
                _ = state.reveal_range(local..local, cx);
            });
        }
    }

    /// A badge over the transcript's corner: the mode copy mode is in, or that it copied.
    pub(super) fn render_copy_mode_badge(&self, cx: &Context<Self>) -> Option<gpui_kit::Div> {
        let label = match self.copy_mode.as_ref().map(|copy| copy.selection) {
            Some(None) => "Copy mode",
            Some(Some((_, CopySelection::Character))) => "Visual",
            Some(Some((_, CopySelection::Line))) => "Visual line",
            Some(Some((_, CopySelection::Block))) => "Visual block",
            None if self.copied_message.as_deref() == Some("copy-mode") => "Copied",
            None => return None,
        };
        Some(
            div().absolute().bottom_2().right_4().child(
                div()
                    .debug_selector(move || format!("conversation-copy-mode:{label}"))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .shadow_sm()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            ),
        )
    }
}

/// The motion a key makes on its own, and how often when not as often as its count says.
fn plain_motion(key: &str, typed: Option<char>) -> Option<(CopyMotion, Option<usize>)> {
    Some(match (key, typed) {
        ("left", _) | (_, Some('h')) => (CopyMotion::Left, None),
        ("right", _) | (_, Some('l')) => (CopyMotion::Right, None),
        ("down", _) | (_, Some('j')) => (CopyMotion::Down, None),
        ("up", _) | (_, Some('k')) => (CopyMotion::Up, None),
        ("pagedown", _) => (CopyMotion::Down, Some(HALF_PAGE.saturating_mul(2))),
        ("pageup", _) => (CopyMotion::Up, Some(HALF_PAGE.saturating_mul(2))),
        ("home", _) | (_, Some('0')) => (CopyMotion::LineStart, Some(1)),
        ("end", _) | (_, Some('$')) => (CopyMotion::LineEnd, Some(1)),
        (_, Some('^')) => (CopyMotion::FirstNonBlank, Some(1)),
        (_, Some('w')) => (CopyMotion::WordStart, None),
        (_, Some('b')) => (CopyMotion::WordBack, None),
        (_, Some('e')) => (CopyMotion::WordEnd, None),
        (_, Some('G')) => (CopyMotion::TextEnd, Some(1)),
        _ => return None,
    })
}

/// The source of `rendered` that renders `range`: from just past what renders before it to
/// just past its last character.
fn source_range(rendered: &RenderedText, range: Range<usize>) -> Option<Range<usize>> {
    let source = rendered.source();
    let boundaries = source
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(source.len()))
        .collect::<Vec<_>>();
    let rendered_through = |end: &usize| {
        if *end == 0 {
            0
        } else {
            rendered
                .range_for_source(0..*end)
                .map_or(0, |range| range.end)
        }
    };
    let start = boundaries
        .partition_point(|end| rendered_through(end) <= range.start)
        .checked_sub(1)
        .and_then(|index| boundaries.get(index).copied())?;
    let end = boundaries
        .get(boundaries.partition_point(|end| rendered_through(end) < range.end))
        .copied()?;
    (start < end).then_some(start..end)
}
