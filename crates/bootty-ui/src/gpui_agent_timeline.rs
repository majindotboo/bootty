//! Turn work folds share the transcript's virtual list; expansion never mounts the whole history.
use super::NativeAgentSessionView;
use bootty_agents::{NativeToolStatus, NativeTranscriptItem};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::{
    App, Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, prelude::*,
};
use std::collections::BTreeMap;
use std::ops::{Add as _, Sub as _};

#[derive(Clone)]
pub(super) enum TranscriptRow {
    History {
        direction: &'static str,
    },
    Message {
        index: usize,
        id: String,
    },
    Work {
        id: String,
        members: Vec<usize>,
        open: bool,
        live: bool,
    },
}
impl TranscriptRow {
    pub(super) fn key(&self) -> String {
        match self {
            Self::History { direction } => format!("history:{direction}"),
            Self::Message { id, .. } => format!("message:{id}"),
            Self::Work { id, .. } => format!("work:{id}"),
        }
    }
}

fn visible(item: &NativeTranscriptItem) -> bool {
    !item.display_text().trim().is_empty()
        || !item.images.is_empty()
        || !item.attachments.is_empty()
        || !item.citations.is_empty()
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
        if end == transcript.len() || transcript.get(end).is_some_and(|item| item.role == "user") {
            append_turn(
                &mut rows,
                transcript,
                start..end,
                expanded,
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
    live: bool,
) {
    let indices = range
        .filter(|ix| transcript.get(*ix).is_some_and(visible))
        .collect::<Vec<_>>();
    let final_message = indices.iter().rev().copied().find(|ix| {
        transcript
            .get(*ix)
            .is_some_and(|item| item.role == "assistant")
    });
    let members = indices
        .iter()
        .copied()
        .filter(|ix| {
            Some(*ix) != final_message
                && transcript.get(*ix).is_some_and(|item| {
                    matches!(
                        item.role.as_str(),
                        "assistant" | "thinking" | "reasoning" | "tool"
                    )
                })
        })
        .collect::<Vec<_>>();
    let has_work = members.iter().any(|ix| {
        transcript.get(*ix).is_some_and(|item| {
            matches!(item.role.as_str(), "thinking" | "reasoning" | "tool") || item.tool.is_some()
        })
    });
    if !has_work {
        rows.extend(indices.into_iter().filter_map(|index| {
            transcript.get(index).map(|item| TranscriptRow::Message {
                index,
                id: item.id.clone(),
            })
        }));
        return;
    }
    let Some(first) = members.first() else {
        return;
    };
    let Some(first) = transcript.get(*first) else {
        return;
    };
    let id = first.id.clone();
    let open = expanded.get(&id).copied().unwrap_or(live);
    let mut grouped = false;
    for index in indices {
        if members.binary_search(&index).is_ok() {
            if !grouped {
                rows.push(TranscriptRow::Work {
                    id: id.clone(),
                    members: members.clone(),
                    open,
                    live,
                });
                grouped = true;
            }
            if !open {
                continue;
            }
        }
        if let Some(item) = transcript.get(index) {
            rows.push(TranscriptRow::Message {
                index,
                id: item.id.clone(),
            });
        }
    }
}

impl NativeAgentSessionView {
    pub(super) fn refresh_timeline(&mut self) {
        let mut new = rows(
            &self.record.snapshot.transcript,
            &self.work_disclosures,
            super::is_busy(self.record.snapshot.status),
        );
        if let Some(page) = &self.history_page {
            if page.has_older {
                new.insert(0, TranscriptRow::History { direction: "older" });
            }
            if page.has_newer {
                new.push(TranscriptRow::History { direction: "newer" });
            }
        }
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
                let TranscriptRow::Message { index, id } = item else {
                    return None;
                };
                let message = self.record.snapshot.transcript.get(*index)?;
                (message.role == "user").then(|| {
                    let preview = previews.get(id.as_str()).map_or_else(
                        || {
                            SharedString::from(
                                message.display_text().chars().take(180).collect::<String>(),
                            )
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

    // Index user turns in the gutter; the virtual list remains the scroll owner.
    pub(super) fn render_history_gutter(
        &self,
        window: &gpui_kit::Window,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let top = self.list.logical_scroll_top().item_ix;
        let active = self
            .history_turns
            .iter()
            .rposition(|(row, _, _)| *row <= top);
        div()
            .w(gpui_kit::rems(if self.history_turns.len() > 1 {
                2.25
            } else {
                0.
            }))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .justify_center()
            .when(self.history_turns.len() > 1, |gutter| {
                gutter.child(
                    div()
                        .relative()
                        .flex()
                        .flex_col()
                        .child(self.history_navigation(false, active, cx))
                        .child(
                            div()
                                .id("native-history-rail")
                                .debug_selector(|| "native-history-rail".into())
                                .flex()
                                .flex_col()
                                .max_h(gpui_kit::rems(20.))
                                .overflow_y_scroll()
                                .track_scroll(&self.history_scroll)
                                .children(self.history_turns.iter().enumerate().map(
                                    |(ix, (_, id, preview))| {
                                        self.history_turn(ix, id, preview, active, cx)
                                    },
                                )),
                        )
                        .child(self.history_navigation(true, active, cx))
                        .when_some(
                            self.hovered_turn
                                .and_then(|ix| self.history_turns.get(ix).map(|turn| (ix, turn))),
                            |rail, (ix, (_, _, preview))| {
                                let offset = self.history_scroll.bounds_for_item(ix).map_or(
                                    gpui_kit::px(0.),
                                    |bounds| {
                                        bounds
                                            .top()
                                            .sub(self.history_scroll.bounds().top())
                                            .add(self.history_scroll.offset().y)
                                    },
                                );
                                rail.child(gpui_kit::deferred(
                                    div()
                                        .absolute()
                                        .left(gpui_kit::rems(2.25))
                                        .top(
                                            gpui_kit::rems(1.75)
                                                .to_pixels(window.rem_size())
                                                .add(offset),
                                        )
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

    fn history_turn(
        &self,
        ix: usize,
        id: &str,
        preview: &SharedString,
        active: Option<usize>,
        cx: &Context<Self>,
    ) -> Button {
        let highlighted = self.hovered_turn == Some(ix) || active == Some(ix);
        Button::new(SharedString::from(format!("history-turn:{id}")))
            .debug_selector({
                let id = id.to_owned();
                move || format!("history-turn:{id}")
            })
            .ghost()
            .small()
            .h(gpui_kit::rems(0.5))
            .min_h(gpui_kit::rems(0.5))
            .w(gpui_kit::rems(2.25))
            .p_0()
            .accessibility_label(format!(
                "Jump to turn {}: {}",
                ix.saturating_add(1),
                preview.split_whitespace().collect::<Vec<_>>().join(" ")
            ))
            .child(
                div()
                    .h(gpui_kit::px(2.))
                    .w(gpui_kit::px(if highlighted { 24. } else { 8. }))
                    .rounded_full()
                    .bg(cx
                        .theme()
                        .foreground
                        .opacity(if highlighted { 0.8 } else { 0.3 })),
            )
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                this.hovered_turn = hovering.then_some(ix);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.jump_to_turn(ix, cx);
            }))
    }

    fn history_navigation(&self, next: bool, active: Option<usize>, cx: &Context<Self>) -> Button {
        use gpui_kit::component::Disableable as _;
        use gpui_kit::component::IconName;
        let target = if next {
            active.map_or(Some(0), |ix| ix.checked_add(1))
        } else {
            active.and_then(|ix| ix.checked_sub(1))
        };
        let target = target.filter(|ix| *ix < self.history_turns.len());
        Button::new(if next {
            "history-next"
        } else {
            "history-previous"
        })
        .icon(if next {
            IconName::ChevronDown
        } else {
            IconName::ChevronUp
        })
        .ghost()
        .small()
        .disabled(target.is_none())
        .accessibility_label(if next { "Next turn" } else { "Previous turn" })
        .tooltip(if next { "Next turn" } else { "Previous turn" })
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(ix) = target {
                this.jump_to_turn(ix, cx);
            }
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
            Some(TranscriptRow::History { direction }) => {
                use gpui_kit::component::Disableable as _;
                let direction = *direction;
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .px_4()
                    .py_1()
                    .child(
                        Button::new(SharedString::from(format!("load-history-{direction}")))
                            .ghost()
                            .small()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .disabled(self.pending.contains("history"))
                            .label(if self.pending.contains("history") {
                                "Loading turns…"
                            } else if direction == "older" {
                                "Load earlier turns"
                            } else {
                                "Load later turns"
                            })
                            .on_click(move |_, window, cx| {
                                _ = owner.update(cx, |this, cx| {
                                    this.command("history", vec![direction.into()], window, cx);
                                });
                            }),
                    )
                    .into_any_element()
            }
            Some(TranscriptRow::Message { index, .. }) => {
                self.render_message(*index, owner, window, cx)
            }
            Some(TranscriptRow::Work {
                id,
                members,
                open,
                live,
            }) => self.render_work_group(id, members, *open, *live, owner, cx),
            None => div().into_any_element(),
        }
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
        let failed = tools
            .iter()
            .any(|tool| tool.status == NativeToolStatus::Failed);
        let label = if live {
            "Working"
        } else if tools.is_empty() {
            "Thought process"
        } else {
            "Worked"
        };
        let summary = if tools.is_empty() {
            label.to_owned()
        } else {
            format!(
                "{label} · {} {}{}",
                tools.len(),
                if tools.len() == 1 { "tool" } else { "tools" },
                if failed { " · Failed" } else { "" }
            )
        };
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
                "{} transcript",
                if open { "Collapse" } else { "Expand" }
            ))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(if failed {
                        cx.theme().danger
                    } else {
                        cx.theme().foreground.opacity(0.72)
                    })
                    .child(
                        gpui_kit::component::Icon::new(if open {
                            gpui_kit::component::IconName::ChevronDown
                        } else {
                            gpui_kit::component::IconName::ChevronRight
                        })
                        .small(),
                    )
                    .child(summary),
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
                    .max_w(gpui_kit::rems(48.))
                    .min_w_0()
                    .child(content),
            )
            .into_any_element()
    }
}
