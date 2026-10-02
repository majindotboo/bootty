//! Shared tab appearance and close-affordance layout for dock and mux tabs.
use bootty_config::config::{TabAppearance, TabCloseButton, TabClosePosition, TabConfig};
use gpui_kit::base::{Tab, Tabs};
use gpui_kit::component::{
    ActiveTheme as _, Colorize as _, ElementExt as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::{
    AnyElement, App, Context, Div, FocusHandle, Hsla, MouseButton, ParentElement, Pixels,
    RenderOnce, ScrollHandle, SharedString, Styled, Task, TextRun, Window, div, prelude::*, px,
    relative,
};
use num_traits::ToPrimitive as _;
use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

pub fn tab_foreground(appearance: TabAppearance, selected: bool, accent: Hsla, cx: &App) -> Hsla {
    let theme = cx.theme();
    let background = if selected && appearance != TabAppearance::Underline {
        theme.tokens.tab_active.mix_oklab(accent, 0.88)
    } else {
        theme.tab_bar
    };
    let preferred = if selected {
        theme.foreground
    } else {
        theme.tab_foreground
    };
    let rgba = |value: Hsla| {
        let value = value.to_rgb();
        let [red, green, blue] = [value.r, value.g, value.b].map(|channel| {
            (channel.clamp(0.0, 1.0) * 255.0)
                .round()
                .to_u8()
                .unwrap_or_default()
        });
        super::Rgba::rgb(red, green, blue)
    };
    let foreground = super::theme::readable_color(rgba(background), rgba(preferred));
    gpui_kit::Rgba {
        r: f32::from(foreground.red) / 255.0,
        g: f32::from(foreground.green) / 255.0,
        b: f32::from(foreground.blue) / 255.0,
        a: 1.0,
    }
    .into()
}

/// Keep tab fills quiet without changing primary buttons or other accent controls.
pub fn tab(
    id: SharedString,
    appearance: TabAppearance,
    selected: bool,
    accent: Hsla,
    cx: &App,
) -> Tab {
    let theme = cx.theme();
    let fill = theme.tokens.tab_active.mix_oklab(accent, 0.88);
    let outline = accent.mix_oklab(theme.secondary, 0.6);
    let hover = theme.secondary_hover;
    let compact = matches!(appearance, TabAppearance::Pill | TabAppearance::Outline);
    let foreground = tab_foreground(appearance, selected, accent, cx);
    Tab::new(id)
        .selected(selected)
        .relative()
        .flex_none()
        .h_7()
        .line_height(relative(1.25))
        .whitespace_nowrap()
        .text_sm()
        .text_color(foreground)
        .when(compact, Styled::rounded_full)
        .when(appearance == TabAppearance::Segmented, |tab| {
            tab.rounded_sm()
        })
        .when(appearance == TabAppearance::Outline, |tab| {
            tab.border_1().border_color(theme.border)
        })
        .when(appearance == TabAppearance::Underline, |tab| {
            tab.border_b_2().border_color(theme.transparent)
        })
        .styles(|styles| {
            styles.selected(|style| {
                let style = style.text_color(foreground);
                match appearance {
                    TabAppearance::Underline => style.border_color(outline),
                    TabAppearance::Outline => style.border_color(outline).bg(fill),
                    _ => style.bg(fill),
                }
            })
        })
        .hover(move |style| if selected { style } else { style.bg(hover) })
}

pub fn content(
    content: AnyElement,
    close: Option<AnyElement>,
    hover_group: SharedString,
    config: TabConfig,
) -> Div {
    let close = close.filter(|_| config.close_button != TabCloseButton::Hidden);
    let close = close.map(|close| {
        div()
            .flex_none()
            .h_full()
            .flex()
            .items_center()
            .when(config.close_button == TabCloseButton::Hover, |button| {
                button
                    .invisible()
                    .group_hover(hover_group, gpui_kit::Styled::visible)
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(close)
    });
    let (left, right) = match config.close_position {
        TabClosePosition::Left => (close, None),
        TabClosePosition::Right => (None, close),
    };
    div()
        .h_full()
        .flex_1()
        .flex()
        .items_center()
        .min_w_0()
        .px_2()
        .gap_1()
        .when_some(left, ParentElement::child)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .child(content),
        )
        .when_some(right, ParentElement::child)
}

/// A single tab scroll owner, shared by mux and panel chrome.
#[derive(IntoElement)]
pub struct ScrollableTabBar {
    pub id: SharedString,
    pub config: TabConfig,
    pub background: Hsla,
    pub tabs: Vec<ScrollableTab>,
    pub selected: Option<usize>,
    pub end: AnyElement,
    pub notch: Option<NotchTabLayout>,
}

#[derive(Clone, Copy)]
pub struct NotchTabLayout {
    pub width: f32,
    pub inset: f32,
    pub height: f32,
    pub wrap: bool,
}

impl NotchTabLayout {
    fn first_row_tabs(self, widths: &[Pixels], gap: f32) -> (usize, f32) {
        let mut count: usize = 0;
        let mut used = 4.0;
        for width in widths {
            let width = f32::from(*width);
            if width.mul_add(0.5, used) > self.width {
                break;
            }
            used += width + gap;
            count = count.saturating_add(1);
        }
        (count, (used - gap + 4.0).max(self.width))
    }

    fn first_row(self, bar: Tabs) -> Div {
        div()
            .h(px(self.height))
            .w_full()
            .flex_none()
            .pl(px(self.inset))
            .child(
                div()
                    .w(px(self.width))
                    .h_full()
                    .pl_1()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .child(bar),
            )
    }
}

pub struct ScrollableTab {
    pub id: String,
    /// Terminal titles get bounded, delayed-shrink widths. Panel tabs keep intrinsic sizing.
    pub title: Option<SharedString>,
    pub focus: Option<FocusHandle>,
    pub tab: Tab,
}

#[derive(Default)]
struct TabScrollState {
    scroll: ScrollHandle,
    selected: Option<(String, usize)>,
    focused: Option<(String, usize)>,
    first_count: usize,
    widths: HashMap<String, TabWidth>,
    settle: Option<(Instant, Task<()>)>,
    edges: Rc<Cell<(bool, bool, bool)>>,
    viewport_width: Rc<Cell<Pixels>>,
}

struct TabWidth {
    title: SharedString,
    width: Pixels,
    desired: Pixels,
    shrink_at: Option<Instant>,
}

impl TabWidth {
    fn update(&mut self, title: SharedString, desired: Pixels, now: Instant) {
        if title != self.title || desired != self.desired {
            self.title = title;
            self.desired = desired;
            self.shrink_at = (desired < self.width)
                .then_some(now)
                .and_then(|now| now.checked_add(Duration::from_secs(1)));
        }
        if desired >= self.width || self.shrink_at.is_some_and(|deadline| now >= deadline) {
            self.width = desired;
            self.shrink_at = None;
        }
    }
}

impl TabScrollState {
    fn size_tabs(
        &mut self,
        tabs: Vec<ScrollableTab>,
        window: &Window,
        cx: &Context<Self>,
    ) -> (Vec<Tab>, Vec<Pixels>) {
        let now = cx.background_executor().now();
        let rem = f32::from(window.rem_size());
        self.widths
            .retain(|id, _| tabs.iter().any(|tab| &tab.id == id));
        let sized = tabs
            .into_iter()
            .map(|item| {
                let Some(title) = item.title else {
                    return (item.tab, px(0.0));
                };
                let text = window.text_system().shape_line(
                    title.clone(),
                    px(rem * 0.875),
                    &[TextRun {
                        len: title.len(),
                        font: window.text_style().font(),
                        color: window.text_style().color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                let desired = px(rem
                    .mul_add(3.5, f32::from(text.width))
                    .clamp(rem * 4.0, rem * 15.0));
                let width = self.widths.entry(item.id).or_insert_with(|| TabWidth {
                    title: title.clone(),
                    width: desired,
                    desired,
                    shrink_at: None,
                });
                width.update(title, desired, now);
                (item.tab.w(width.width), width.width)
            })
            .unzip();
        self.schedule_shrink(now, window, cx);
        sized
    }

    fn schedule_shrink(&mut self, now: Instant, window: &Window, cx: &Context<Self>) {
        let deadline = self
            .widths
            .values()
            .filter_map(|width| width.shrink_at)
            .min();
        if deadline == self.settle.as_ref().map(|(deadline, _)| *deadline) {
            return;
        }
        self.settle = deadline.map(|deadline| {
            let timer = cx
                .background_executor()
                .timer(deadline.saturating_duration_since(now));
            let task = cx.spawn_in(window, async move |state, cx| {
                timer.await;
                let _ = state.update_in(cx, |state, window, cx| {
                    state.settle = None;
                    window.refresh();
                    cx.notify();
                });
            });
            (deadline, task)
        });
    }

    fn reveal(
        &mut self,
        selected: Option<(String, usize)>,
        focused: Option<(String, usize)>,
        first_count: usize,
    ) -> Option<usize> {
        let target = focused
            .as_ref()
            .or(selected.as_ref())
            .and_then(|(_, ix)| ix.checked_sub(first_count));
        if self.selected != selected || self.focused != focused || self.first_count != first_count {
            if let Some(ix) = target {
                self.scroll.scroll_to_item(ix);
            }
            self.selected = selected;
            self.focused = focused;
            self.first_count = first_count;
        }
        target
    }
}

impl RenderOnce for ScrollableTabBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| TabScrollState::default());
        let selected = self
            .selected
            .and_then(|ix| self.tabs.get(ix).map(|tab| (tab.id.clone(), ix)));
        let focused = self.tabs.iter().enumerate().find_map(|(ix, tab)| {
            tab.focus
                .as_ref()
                .filter(|focus| focus.is_focused(window))
                .map(|_| (tab.id.clone(), ix))
        });
        let (mut tabs, widths) =
            state.update(cx, |state, cx| state.size_tabs(self.tabs, window, cx));
        let gap = tab_gap(self.config.appearance);
        let (first_count, first_width) = self
            .notch
            .filter(|notch| notch.wrap)
            .map_or((0, 0.0), |notch| notch.first_row_tabs(&widths, gap));
        let target = state.update(cx, |state, _| state.reveal(selected, focused, first_count));
        let (scroll, edges, viewport_width) = {
            let state = state.read(cx);
            (
                state.scroll.clone(),
                state.edges.clone(),
                state.viewport_width.clone(),
            )
        };
        let rem = f32::from(window.rem_size());
        let second_tabs = tabs.split_off(first_count);
        let first_row = self.notch.filter(|notch| notch.wrap).map(|notch| {
            let bar = make_bar(
                format!("{}-first", self.id).into(),
                self.config,
                self.background,
                tabs,
                self.selected.filter(|ix| *ix < first_count),
                None,
                gpui_kit::Empty.into_any_element(),
                rem,
            )
            .w(px(first_width))
            .flex_none();
            notch.first_row(bar)
        });
        let has_scrolling_row =
            self.notch.is_none_or(|notch| !notch.wrap) || !second_tabs.is_empty();
        let bar = make_bar(
            self.id.clone(),
            self.config,
            self.background,
            second_tabs,
            self.selected.and_then(|ix| ix.checked_sub(first_count)),
            Some(&scroll),
            self.end,
            rem,
        );
        let scroll_row = ScrollRow {
            id: self.id,
            bar,
            scroll,
            edges,
            viewport_width,
            target,
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .flex_none()
            .when(self.notch.is_none(), Styled::h_full)
            .children(first_row)
            .when(has_scrolling_row, |column| {
                column.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .when(self.notch.is_none(), Styled::h_full)
                        .when_some(self.notch, |row, notch| {
                            row.h(px(notch.height))
                                .when(!notch.wrap, |row| row.pl(px(notch.inset)))
                        })
                        .child(
                            div()
                                .h_full()
                                .min_w_0()
                                .when_some(self.notch.filter(|notch| !notch.wrap), |row, notch| {
                                    row.w(px(notch.width))
                                })
                                .child(scroll_row),
                        ),
                )
            })
    }
}

// Keep spacing stable across appearance changes; each tab is one logical scroll child.
const fn tab_gap(appearance: TabAppearance) -> f32 {
    match appearance {
        TabAppearance::Classic => 0.0,
        TabAppearance::Pill => 4.0,
        TabAppearance::Outline => 8.0,
        TabAppearance::Segmented => 2.0,
        TabAppearance::Underline => 12.0,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_bar(
    id: SharedString,
    config: TabConfig,
    background: Hsla,
    tabs: Vec<Tab>,
    selected: Option<usize>,
    scroll: Option<&ScrollHandle>,
    end: AnyElement,
    rem: f32,
) -> Tabs {
    let count = tabs.len();
    Tabs::new(id)
        .flex()
        .items_center()
        .w_full()
        .min_w_0()
        .h_full()
        .gap(px(tab_gap(config.appearance)))
        .bg(background)
        .overflow_x_scroll()
        .when_some(scroll, gpui_kit::StatefulInteractiveElement::track_scroll)
        .children(tabs.into_iter().enumerate().map(|(ix, tab)| {
            tab.id(ix)
                .set_position(ix.saturating_add(1), count)
                .min_w_0()
                .max_w(px(rem * 15.0))
                .overflow_hidden()
                .when_some(selected, |tab, selected| tab.selected(ix == selected))
        }))
        .child(end)
}

#[derive(IntoElement)]
struct ScrollRow {
    id: SharedString,
    bar: Tabs,
    scroll: ScrollHandle,
    edges: Rc<Cell<(bool, bool, bool)>>,
    viewport_width: Rc<Cell<Pixels>>,
    target: Option<usize>,
}

impl RenderOnce for ScrollRow {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let (overflow, left, right) = self.edges.get();
        let observe_scroll = self.scroll.clone();
        let wheel_scroll = self.scroll.clone();
        let id = self.id.clone();
        let rem = f32::from(window.rem_size());
        div()
            .id(SharedString::from(format!("{}-scroll-strip", self.id)))
            .debug_selector(move || format!("{id}-viewport"))
            .relative()
            .flex()
            .items_center()
            .w_full()
            .min_w_0()
            .h_full()
            .px_1()
            .overflow_hidden()
            .when(overflow, |row| {
                row.child(scroll_button(&self.id, &self.scroll, false, left))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(self.bar),
            )
            .when(overflow, |row| {
                row.child(scroll_button(&self.id, &self.scroll, true, right))
            })
            .on_scroll_wheel(move |event, window, cx| {
                let delta = event.delta.pixel_delta(px(rem));
                // Horizontal gestures are handled by Kit. Route a mouse wheel along the strip.
                if delta.x.abs() < delta.y.abs() {
                    move_scroll(&wheel_scroll, delta.y, window);
                }
                cx.stop_propagation();
            })
            .on_prepaint(move |bounds, window, _| {
                let width = observe_scroll.bounds().size.width;
                let maximum = observe_scroll.max_offset().x;
                let offset = observe_scroll.offset().x;
                // Compare with the full row so adding the chevrons cannot sustain overflow.
                let overflow = f32::from(width) + f32::from(maximum)
                    > rem.mul_add(-0.5, f32::from(bounds.size.width)) + 0.5;
                let next = (
                    overflow,
                    offset < px(-0.5),
                    f32::from(offset) + f32::from(maximum) > 0.5,
                );
                if self.edges.replace(next) != next {
                    window.refresh();
                }
                if self.viewport_width.replace(width) != width
                    && let Some(ix) = self.target
                {
                    observe_scroll.scroll_to_item(ix);
                    window.refresh();
                }
            })
    }
}

fn move_scroll(scroll: &ScrollHandle, delta: Pixels, window: &mut Window) {
    let mut offset = scroll.offset();
    offset.x =
        px((f32::from(offset.x) + f32::from(delta)).clamp(-f32::from(scroll.max_offset().x), 0.0));
    if offset != scroll.offset() {
        scroll.set_offset(offset);
        window.refresh();
    }
}

fn scroll_button(
    id: &SharedString,
    scroll: &ScrollHandle,
    right: bool,
    enabled: bool,
) -> impl IntoElement {
    let scroll = scroll.clone();
    let direction = if right { "right" } else { "left" };
    let selector = format!("{id}-scroll-{direction}");
    div()
        .w_5()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |slot| {
            slot.child(
                Button::new(SharedString::from(selector.clone()))
                    .debug_selector(move || selector)
                    .icon(if right {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronLeft
                    })
                    .ghost()
                    .xsmall()
                    .w_5()
                    .accessibility_label(format!("Scroll tabs {direction}"))
                    .tooltip(format!("Scroll tabs {direction}"))
                    .on_click(move |_, window, cx| {
                        let distance = f32::from(scroll.bounds().size.width) * 0.8;
                        move_scroll(
                            &scroll,
                            px(if right { -distance } else { distance }),
                            window,
                        );
                        cx.stop_propagation();
                    }),
            )
        })
}
