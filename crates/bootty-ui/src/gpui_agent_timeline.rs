//! Turn work folds share the transcript's virtual list; expansion never mounts the whole history.
use super::NativeAgentSessionView;
use bootty_agents::{NativeToolStatus, NativeTranscriptItem};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::{
    App, Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, Window, div,
    list, prelude::*,
};
use std::collections::BTreeMap;

#[derive(Clone)]
pub(super) enum TranscriptRow {
    Message {
        index: usize,
        id: String,
        /// Timestamp and actions belong to user messages and a settled turn's final reply,
        /// not to the commentary between tools.
        meta: bool,
    },
    Work {
        id: String,
        members: Vec<usize>,
        open: bool,
        live: bool,
    },
    /// The provider's report that the last turn failed, shown where the turn ended.
    Failure { error: String },
}
impl TranscriptRow {
    pub(super) fn key(&self) -> String {
        match self {
            // A reply gains its metadata when its turn settles; the new key remeasures it.
            Self::Message { id, meta, .. } => format!("message:{id}:{meta}"),
            Self::Work { id, .. } => format!("work:{id}"),
            Self::Failure { error } => format!("failure:{error}"),
        }
    }
}

/// Whether a row has anything to draw. Image references aren't drawn on their own; the prompt
/// text links the annotation they came from.
fn visible(item: &NativeTranscriptItem) -> bool {
    !item.display_text().trim().is_empty()
        || !item.attachments.is_empty()
        || !item.citations.is_empty()
        || !item.questions.is_empty()
        || item.tool.is_some()
        || item.subagent.is_some()
}

pub(super) fn rows(
    transcript: &[NativeTranscriptItem],
    expanded: &BTreeMap<String, bool>,
    working: bool,
) -> Vec<TranscriptRow> {
    let mut rows = Vec::new();
    let mut start = 0;
    for end in 0..=transcript.len() {
        // A steering message stays inside the turn it steered.
        if end == transcript.len()
            || transcript
                .get(end)
                .is_some_and(|item| item.role == "user" && !item.steered)
        {
            append_turn(
                &mut rows,
                transcript,
                start..end,
                expanded,
                end == transcript.len(),
                working
                    && (end == transcript.len()
                        || transcript.get(start..end).is_some_and(|turn| {
                            turn.iter().any(|item| {
                                item.tool
                                    .as_ref()
                                    .is_some_and(|tool| tool.status == NativeToolStatus::Running)
                            })
                        })),
            );
            if let Some(item) = transcript.get(end)
                && visible(item)
            {
                rows.push(TranscriptRow::Message {
                    index: end,
                    id: item.id.clone(),
                    meta: true,
                });
            }
            start = end.saturating_add(1);
        }
    }
    rows
}

fn append_turn(
    rows: &mut Vec<TranscriptRow>,
    transcript: &[NativeTranscriptItem],
    range: std::ops::Range<usize>,
    expanded: &BTreeMap<String, bool>,
    current_turn: bool,
    live: bool,
) {
    let indices = range
        .filter(|ix| {
            transcript
                .get(*ix)
                .is_some_and(|item| visible(item) && !(current_turn && item.subagent.is_some()))
        })
        .collect::<Vec<_>>();
    let final_message = indices.iter().rev().copied().find(|ix| {
        !live
            && transcript
                .get(*ix)
                .is_some_and(|item| matches!(item.role.as_str(), "assistant" | "plan"))
    });
    // Progress messages and file changes stay readable when a turn settles.
    // Only the runs of thinking and command output between them fold.
    let mut run = Vec::new();
    for index in indices {
        let Some(item) = transcript.get(index) else {
            continue;
        };
        if is_work(item) && item.compaction_status().is_none() {
            run.push(index);
            continue;
        }
        // Work a steering message arrived during can still be running.
        let running = live && running_tool(transcript, &run);
        push_work_run(
            rows,
            transcript,
            std::mem::take(&mut run),
            expanded,
            running,
        );
        rows.push(TranscriptRow::Message {
            index,
            id: item.id.clone(),
            // A steering message keeps its actions where it sits inside the turn.
            meta: Some(index) == final_message || item.role == "user",
        });
    }
    // Only the trailing run can still be working.
    push_work_run(rows, transcript, run, expanded, live);
}

fn running_tool(transcript: &[NativeTranscriptItem], members: &[usize]) -> bool {
    members.iter().any(|index| {
        transcript
            .get(*index)
            .and_then(|item| item.tool.as_ref())
            .is_some_and(|tool| tool.status == NativeToolStatus::Running)
    })
}

fn is_work(item: &NativeTranscriptItem) -> bool {
    item.tool
        .as_ref()
        .is_none_or(|tool| !matches!(tool.name.as_str(), "fileChange" | "contextCompaction"))
        && (matches!(item.role.as_str(), "thinking" | "reasoning" | "tool") || item.tool.is_some())
}

fn push_work_run(
    rows: &mut Vec<TranscriptRow>,
    transcript: &[NativeTranscriptItem],
    members: Vec<usize>,
    expanded: &BTreeMap<String, bool>,
    live: bool,
) {
    let Some(id) = members
        .first()
        .and_then(|ix| transcript.get(*ix))
        .map(|item| item.id.clone())
    else {
        return;
    };
    let open = expanded.get(&id).copied().unwrap_or(false);
    rows.push(TranscriptRow::Work {
        id,
        members: members.clone(),
        open,
        live,
    });
    if open {
        rows.extend(members.into_iter().filter_map(|index| {
            transcript.get(index).map(|item| TranscriptRow::Message {
                index,
                id: item.id.clone(),
                meta: false,
            })
        }));
    }
}

impl NativeAgentSessionView {
    pub(super) fn render_compaction(
        &self,
        item: &NativeTranscriptItem,
        status: bootty_agents::NativeToolStatus,
        window: &Window,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        use bootty_agents::NativeToolStatus;
        use gpui_kit::component::{Icon, IconName};
        let active = status == NativeToolStatus::Running
            && self.record.snapshot.status == bootty_agents::NativeSessionStatus::Working;
        let label = match status {
            NativeToolStatus::Completed => "Context compacted",
            NativeToolStatus::Running if active => "Compacting context",
            NativeToolStatus::Failed => "Compaction failed",
            _ if self.record.snapshot.status == bootty_agents::NativeSessionStatus::Error => {
                "Compaction failed"
            }
            _ => "Compaction interrupted",
        };
        let elapsed = active
            .then_some(item.created_at)
            .flatten()
            .and_then(|started| {
                u64::try_from(
                    chrono::Utc::now()
                        .timestamp_millis()
                        .saturating_sub(started),
                )
                .ok()
                .map(std::time::Duration::from_millis)
                .map(crate::clock::format_working_duration)
            });
        div()
            .w_full()
            .min_w_0()
            .px_4()
            .py_2()
            .flex()
            .justify_center()
            .child(
                div()
                    .debug_selector({
                        let id = item.id.clone();
                        move || format!("native-compaction-{id}")
                    })
                    .w_full()
                    .max_w(self.column_width())
                    .min_w_0()
                    .p_3()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted.opacity(0.3))
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if active && window.is_visible() && self.moving(cx) {
                        crate::motion::spinner(cx.theme().muted_foreground).into_any_element()
                    } else {
                        Icon::new(if status == NativeToolStatus::Completed {
                            IconName::Check
                        } else if active {
                            IconName::LoaderCircle
                        } else {
                            IconName::Square
                        })
                        .small()
                        .into_any_element()
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .debug_selector(move || {
                                        format!("native-compaction-status:{label}")
                                    })
                                    .child(label),
                            )
                            .when_some(elapsed, ParentElement::child)
                            .children(compacted_tokens(item)),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn refresh_timeline(&mut self) {
        let mut new = rows(
            &self.record.snapshot.transcript,
            &self.work_disclosures,
            super::is_busy(self.record.snapshot.status),
        );
        new.extend(
            self.turn_failure()
                .map(|error| TranscriptRow::Failure { error }),
        );
        let old_keys = self
            .timeline
            .iter()
            .map(TranscriptRow::key)
            .collect::<Vec<_>>();
        let new_keys = new.iter().map(TranscriptRow::key).collect::<Vec<_>>();
        let anchor = (!self.list.is_following_tail())
            .then(|| self.list.logical_scroll_top())
            .and_then(|offset| {
                old_keys
                    .get(offset.item_ix)
                    .map(|key| (key.clone(), offset))
            });
        let prefix = old_keys
            .iter()
            .zip(&new_keys)
            .take_while(|(old, new)| old == new)
            .count();
        let suffix = old_keys
            .iter()
            .skip(prefix)
            .rev()
            .zip(new_keys.iter().skip(prefix).rev())
            .take_while(|(old, new)| old == new)
            .count();
        let old_end = old_keys.len().saturating_sub(suffix);
        let new_end = new_keys.len().saturating_sub(suffix);
        if prefix < old_end || prefix < new_end {
            self.list
                .splice(prefix..old_end, new_end.saturating_sub(prefix));
        }
        self.timeline = new;
        let previews = self
            .history_turns
            .iter()
            .map(|(_, id, preview)| (id.as_str(), preview))
            .collect::<std::collections::HashMap<_, _>>();
        self.history_turns = self
            .timeline
            .iter()
            .enumerate()
            .filter_map(|(row, item)| {
                let TranscriptRow::Message { index, id, .. } = item else {
                    return None;
                };
                let message = self.record.snapshot.transcript.get(*index)?;
                (message.role == "user" && !message.steered).then(|| {
                    let preview = previews.get(id.as_str()).map_or_else(
                        || {
                            SharedString::from(crate::strings::clipped(
                                &message.display_text(),
                                180,
                            ))
                        },
                        |preview| (*preview).clone(),
                    );
                    (row, id.clone(), preview)
                })
            })
            .collect();
        self.hovered_turn = self
            .hovered_turn
            .filter(|ix| *ix < self.history_turns.len());
        if let Some((key, mut offset)) = anchor
            && let Some(ix) = new_keys.iter().position(|candidate| *candidate == key)
        {
            offset.item_ix = ix;
            self.list.scroll_to(offset);
        }
    }

    // All loaded turns fit one strip; the transcript remains the only scroll owner. The gutter is
    // reserved for any conversation with messages, so a second turn doesn't move the column.
    pub(super) fn render_history_gutter(
        &self,
        window: &gpui_kit::Window,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        use num_traits::ToPrimitive as _;
        let top = self.list.logical_scroll_top().item_ix;
        let active = self
            .history_turns
            .iter()
            .rposition(|(row, _, _)| *row <= top);
        let count = self.history_turns.len().to_f32().unwrap_or(1.);
        let previewed = self
            .hovered_turn
            .or_else(|| active.filter(|_| self.history_rail_focus.contains_focused(window, cx)));
        div()
            .w(gpui_kit::rems(if self.timeline.is_empty() {
                0.
            } else {
                2.25
            }))
            .flex_shrink_0()
            .min_h_0()
            .flex()
            .flex_col()
            .justify_center()
            .when(self.history_turns.len() > 1, |gutter| {
                gutter.child(
                    div()
                        .id("native-history-rail")
                        .debug_selector(|| "native-history-rail".into())
                        .track_focus(&self.history_rail_focus)
                        .relative()
                        .flex()
                        .flex_col()
                        .h(gpui_kit::rems((count * 0.5).min(20.)))
                        .max_h_full()
                        .min_h_0()
                        .on_key_down(cx.listener(
                            move |this, event: &gpui_kit::KeyDownEvent, _, cx| {
                                if this.history_rail_key(&event.keystroke.key, active, cx) {
                                    cx.stop_propagation();
                                }
                            },
                        ))
                        .children(self.history_turns.iter().enumerate().map(
                            |(ix, (_, id, preview))| {
                                Self::history_turn(ix, id, preview, active, previewed, cx)
                            },
                        ))
                        .when_some(
                            previewed
                                .and_then(|ix| self.history_turns.get(ix).map(|turn| (ix, turn))),
                            |rail, (ix, (_, _, preview))| {
                                rail.child(gpui_kit::deferred(
                                    div()
                                        .debug_selector(move || format!("history-preview:{ix}"))
                                        .absolute()
                                        .left(gpui_kit::rems(2.25))
                                        .top(gpui_kit::relative(ix.to_f32().unwrap_or(0.) / count))
                                        .w(gpui_kit::rems(16.25))
                                        .p_3()
                                        .rounded_lg()
                                        .bg(cx.theme().popover)
                                        .text_color(cx.theme().popover_foreground)
                                        .shadow_md()
                                        .text_sm()
                                        .line_clamp(3)
                                        .text_ellipsis()
                                        .child(
                                            preview
                                                .split_whitespace()
                                                .collect::<Vec<_>>()
                                                .join(" "),
                                        ),
                                ))
                            },
                        ),
                )
            })
    }

    /// Arrow keys pick a turn on the focused rail, Enter jumps to it, and Escape leaves it.
    fn history_rail_key(
        &mut self,
        key: &str,
        active: Option<usize>,
        cx: &mut Context<Self>,
    ) -> bool {
        let last = self.history_turns.len().saturating_sub(1);
        let current = self.hovered_turn.or(active).unwrap_or(0);
        match key {
            "up" => self.hovered_turn = Some(current.saturating_sub(1)),
            "down" => self.hovered_turn = Some(current.saturating_add(1).min(last)),
            "enter" => self.jump_to_turn(current, cx),
            "escape" => self.hovered_turn = None,
            _ => return false,
        }
        cx.notify();
        true
    }

    fn history_turn(
        ix: usize,
        id: &str,
        preview: &SharedString,
        active: Option<usize>,
        previewed: Option<usize>,
        cx: &Context<Self>,
    ) -> Button {
        let highlighted = previewed == Some(ix) || active == Some(ix);
        Button::new(SharedString::from(format!("history-turn:{id}")))
            .debug_selector({
                let id = id.to_owned();
                move || format!("history-turn:{id}")
            })
            .ghost()
            .small()
            .flex_1()
            .min_h_0()
            .border_0()
            .w(gpui_kit::rems(2.25))
            .p_0()
            .tab_stop(false)
            .accessibility_label(format!(
                "Jump to turn {}: {}",
                ix.saturating_add(1),
                preview.split_whitespace().collect::<Vec<_>>().join(" ")
            ))
            .child(
                div()
                    .h(gpui_kit::rems(0.1875))
                    .w(gpui_kit::rems(if highlighted { 1.5 } else { 0.75 }))
                    .rounded_full()
                    .bg(cx
                        .theme()
                        .foreground
                        .opacity(if highlighted { 0.8 } else { 0.35 })),
            )
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                this.hovered_turn = hovering.then_some(ix);
                cx.notify();
            }))
            // The rail keeps focus, so the arrow keys go on from the turn picked.
            .on_click(cx.listener(move |this, _, window, cx| {
                this.jump_to_turn(ix, cx);
                this.history_rail_focus.focus(window, cx);
            }))
    }

    fn jump_to_turn(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some((row, _, _)) = self.history_turns.get(ix) {
            self.list.scroll_to(gpui_kit::ListOffset {
                item_ix: *row,
                offset_in_item: gpui_kit::px(0.),
            });
            self.hovered_turn = None;
            cx.notify();
        }
    }

    pub(super) fn reveal_response(&mut self, id: &str, cx: &mut Context<Self>) {
        let group = self.timeline.iter().find_map(|row| match row {
            TranscriptRow::Work {
                id: group, members, ..
            } if members.iter().any(|ix| {
                self.record
                    .snapshot
                    .transcript
                    .get(*ix)
                    .is_some_and(|item| item.id == id)
            }) =>
            {
                Some(group.clone())
            }
            _ => None,
        });
        if let Some(group) = group {
            self.work_disclosures.insert(group, true);
            self.refresh_timeline();
        }
        if let Some(ix) = self.timeline.iter().position(
            |row| matches!(row, TranscriptRow::Message { id: message, .. } if message == id),
        ) {
            self.list.scroll_to_reveal_item(ix);
            cx.notify();
        }
    }

    pub(super) fn render_timeline_row(
        &self,
        ix: usize,
        owner: gpui_kit::WeakEntity<Self>,
        window: &gpui_kit::Window,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        match self.timeline.get(ix) {
            Some(TranscriptRow::Message { index, meta, .. }) => {
                self.render_message(*index, *meta, owner, window, cx)
            }
            Some(TranscriptRow::Work {
                id,
                members,
                open,
                live,
            }) => self.render_work_group(id, members, *open, *live, owner, cx),
            Some(TranscriptRow::Failure { error }) => self.render_turn_failure(error, owner, cx),
            None => div().into_any_element(),
        }
    }

    pub(super) fn render_transcript(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let owner = cx.weak_entity();
        let empty = self.timeline.is_empty()
            && self.record.snapshot.status == bootty_agents::NativeSessionStatus::Idle
            && self.record.pending_initial_message.is_none()
            && !self.pending.contains("history");
        div()
            .track_focus(&self.copy_focus)
            .on_key_down(cx.listener(Self::copy_mode_key))
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .child(self.render_history_gutter(window, cx))
            .when(empty, |view| {
                view.child(
                    div()
                        .debug_selector(|| "native-empty-conversation".into())
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No messages"),
                )
            })
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
            ))
            .children(
                self.render_find(cx)
                    .map(|bar| div().absolute().top_2().right_4().child(bar)),
            )
            .children(self.render_copy_mode_badge(cx))
            // Floats over the list so showing it never resizes the transcript.
            .when(self.needs_jump_to_latest(), |view| {
                view.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .left_0()
                        .right_0()
                        .child(Self::render_jump_to_latest(cx)),
                )
            })
    }

    fn work_group_summary(&self, members: &[usize], live: bool) -> String {
        let tools = members
            .iter()
            .filter_map(|ix| {
                self.record
                    .snapshot
                    .transcript
                    .get(*ix)
                    .and_then(|item| item.tool.as_ref())
            })
            .collect::<Vec<_>>();
        let active = tools
            .iter()
            .rev()
            .find(|tool| tool.status == NativeToolStatus::Running);
        if live && let Some(tool) = active {
            return running_label(tool);
        }
        let summary = if tools.is_empty() {
            "Thought process".to_owned()
        } else {
            activity_summary(&tools)
        };
        // A turn can fold several runs; its duration reads once, on the last.
        match members
            .first()
            .and_then(|first| self.turn_duration(*first))
            .filter(|_| !live && self.last_fold_of_turn(members))
        {
            Some(duration) if duration.as_secs() > 0 => format!(
                "{summary} · {}",
                crate::clock::format_working_duration(duration)
            ),
            _ => summary,
        }
    }

    /// Whether no work follows `members` before the next prompt.
    fn last_fold_of_turn(&self, members: &[usize]) -> bool {
        let transcript = &self.record.snapshot.transcript;
        members
            .last()
            .and_then(|last| transcript.get(last.saturating_add(1)..))
            .is_none_or(|rest| {
                !rest
                    .iter()
                    .take_while(|item| item.role != "user")
                    .any(|item| visible(item) && is_work(item))
            })
    }

    /// How long a settled turn took: from its prompt to the last item the turn reported.
    fn turn_duration(&self, first: usize) -> Option<std::time::Duration> {
        let transcript = &self.record.snapshot.transcript;
        let start = transcript
            .get(..first)?
            .iter()
            .rev()
            .find(|item| item.role == "user")?
            .created_at?;
        let end = transcript
            .get(first..)?
            .iter()
            .take_while(|item| item.role != "user")
            .filter_map(|item| item.updated_at.or(item.created_at))
            .max()?;
        let millis = u64::try_from(end.checked_sub(start)?).ok()?;
        Some(std::time::Duration::from_millis(millis))
    }

    /// A reply carries elapsed time when no work fold already carries it.
    fn unfolded_turn_duration(&self, item: &NativeTranscriptItem) -> Option<std::time::Duration> {
        if !matches!(item.role.as_str(), "assistant" | "plan") {
            return None;
        }
        let transcript = &self.record.snapshot.transcript;
        let index = transcript.iter().position(|entry| entry.id == item.id)?;
        let prompt = transcript
            .get(..index)?
            .iter()
            .rposition(|entry| entry.role == "user")?;
        if transcript
            .get(prompt.saturating_add(1)..)?
            .iter()
            .take_while(|entry| entry.role != "user")
            .any(|entry| visible(entry) && is_work(entry))
        {
            return None;
        }
        self.turn_duration(index)
            .filter(|duration| duration.as_secs() > 0)
    }

    pub(super) fn render_unfolded_turn_duration(
        &self,
        item: &NativeTranscriptItem,
        cx: &App,
    ) -> Option<gpui_kit::Div> {
        let duration = crate::clock::format_working_duration(self.unfolded_turn_duration(item)?);
        Some(
            div()
                .mr_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .debug_selector({
                    let id = item.id.clone();
                    let duration = duration.clone();
                    move || format!("native-turn-duration:{id}:{duration}")
                })
                .child(duration),
        )
    }

    fn render_work_group(
        &self,
        id: &str,
        members: &[usize],
        open: bool,
        live: bool,
        owner: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        let summary = self.work_group_summary(members, live);
        let key = id.to_owned();
        let content = Button::new(SharedString::from(format!("work-group:{id}")))
            .debug_selector({
                let id = id.to_owned();
                move || format!("native-work-group-{id}")
            })
            .small()
            .ghost()
            .px_0()
            .w_full()
            .accessibility_label(format!(
                "{} work: {summary}",
                if open { "Hide" } else { "Show" }
            ))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        gpui_kit::component::Icon::new(if open {
                            gpui_kit::component::IconName::ChevronDown
                        } else {
                            gpui_kit::component::IconName::ChevronRight
                        })
                        .small(),
                    )
                    .child(
                        div()
                            .debug_selector({
                                let summary = summary.clone();
                                move || format!("native-work-summary:{summary}")
                            })
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(
                                live && self.record.snapshot.status
                                    == bootty_agents::NativeSessionStatus::Working
                                    && self.moving(cx),
                                |summary_row| summary_row.opacity(crate::motion::pulse()),
                            )
                            .child(summary),
                    ),
            )
            .on_click(move |_, _, cx| {
                _ = owner.update(cx, |this, cx| {
                    this.work_disclosures.insert(key.clone(), !open);
                    this.refresh_timeline();
                    cx.notify();
                });
            });
        div()
            .w_full()
            .min_w_0()
            .px_4()
            .py_1()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(self.column_width())
                    .min_w_0()
                    .child(content),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum ToolActivity {
    Command,
    Read,
    Edit,
    Search,
    Other,
}

fn tool_activity(tool: &bootty_agents::NativeToolCall) -> ToolActivity {
    let (_, name) = super::tool_parts(&tool.name);
    let input = serde_json::from_str::<serde_json::Value>(&tool.input).ok();
    let has = |key: &str| input.as_ref().is_some_and(|input| input.get(key).is_some());
    if matches!(
        name,
        "Edit"
            | "MultiEdit"
            | "Write"
            | "NotebookEdit"
            | "edit"
            | "write"
            | "apply_patch"
            | "fileChange"
    ) {
        ToolActivity::Edit
    } else if has("command")
        || has("cmd")
        || matches!(
            name,
            "Bash" | "bash" | "exec_command" | "commandExecution" | "shell"
        )
    {
        ToolActivity::Command
    } else if has("pattern")
        || has("query")
        || matches!(
            name,
            "Grep" | "Glob" | "grep" | "find" | "WebSearch" | "web_search" | "webSearch"
        )
    {
        ToolActivity::Search
    } else if has("file_path") || has("filePath") || has("path") || matches!(name, "Read" | "read")
    {
        ToolActivity::Read
    } else {
        ToolActivity::Other
    }
}

/// "Ran 3 commands, read 2 files, 1 failed": what a run of tools did, in a fixed order.
fn activity_summary(tools: &[&bootty_agents::NativeToolCall]) -> String {
    let mut counts = BTreeMap::new();
    for tool in tools {
        let count = counts.entry(tool_activity(tool)).or_insert(0_usize);
        *count = count.saturating_add(1);
    }
    let phrases = counts
        .into_iter()
        .map(|(activity, count)| {
            let plural = count != 1;
            match activity {
                ToolActivity::Command => {
                    format!("ran {count} command{}", if plural { "s" } else { "" })
                }
                ToolActivity::Read => format!("read {count} file{}", if plural { "s" } else { "" }),
                ToolActivity::Edit => {
                    format!("edited {count} file{}", if plural { "s" } else { "" })
                }
                ToolActivity::Search => {
                    format!("searched {count} time{}", if plural { "s" } else { "" })
                }
                ToolActivity::Other => {
                    format!("used {count} tool{}", if plural { "s" } else { "" })
                }
            }
        })
        .chain(
            Some(
                tools
                    .iter()
                    .filter(|tool| tool.status == NativeToolStatus::Failed)
                    .count(),
            )
            .filter(|failed| *failed > 0)
            .map(|failed| format!("{failed} failed")),
        )
        .collect::<Vec<_>>()
        .join(", ");
    let mut chars = phrases.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A running command reads "Running cargo"; the full command stays in the expanded row.
fn running_label(tool: &bootty_agents::NativeToolCall) -> String {
    if tool_activity(tool) == ToolActivity::Command
        && let Some(program) = command_program(&tool.input)
    {
        return format!("Running {program}");
    }
    let (_, detail) = super::tool_input_summary(tool);
    format!("{} {}", super::tool_label(&tool.name), detail)
        .trim()
        .to_owned()
}

/// The program a shell line runs, past `cd` steps and environment assignments.
fn command_program(input: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(input).ok()?;
    let command = value
        .get("command")
        .or_else(|| value.get("cmd"))
        .and_then(|command| {
            command.as_str().map(str::to_owned).or_else(|| {
                // Codex sends argv; its shell wrapper carries the line last.
                command.as_array()?.last()?.as_str().map(str::to_owned)
            })
        })?;
    command
        .split(['&', ';', '|', '\n'])
        .map(str::trim)
        .filter(|step| !step.is_empty() && !step.starts_with("cd ") && *step != "cd")
        .find_map(|step| {
            step.split_whitespace()
                .find(|word| !word.contains('='))
                .map(|word| word.rsplit('/').next().unwrap_or(word).to_owned())
        })
}

/// How far a compaction shrank the context, when the provider said: "180k → 12k tokens".
fn compacted_tokens(item: &NativeTranscriptItem) -> Option<String> {
    let input = serde_json::from_str::<serde_json::Value>(&item.tool.as_ref()?.input).ok()?;
    let before = input.get("preTokens")?.as_u64()?;
    let after = input.get("postTokens")?.as_u64()?;
    Some(format!(
        "{} → {} tokens",
        super::compact_tokens(before),
        super::compact_tokens(after)
    ))
}
