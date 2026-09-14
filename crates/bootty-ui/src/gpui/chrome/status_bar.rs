use num_traits::ToPrimitive as _;
use std::{cmp::Ordering, rc::Rc, sync::OnceLock, time::Instant};

use gpui_kit::{
    Context, FocusHandle, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window,
    canvas, div, prelude::*, px, relative,
};

use super::{
    ChromeIntent, ChromePalette, ContextMenu, GpuiChrome, MenuRow, StatusAlignment,
    StatusBarSnapshot, StatusIntent, StatusItemSnapshot, StatusProgress, StatusSegmentSnapshot,
    StatusTabFocusMovement, TabBounds, TabContextAction, TabContextSnapshot, TabInsertionTarget,
    color, tab_insertion_target,
};
use crate::gpui::{IconSize, Rgba, sized_icon};
use gpui_kit::component::{
    ActiveTheme as _, ElementExt as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    menu::ContextMenuExt,
    tab::{Tab, TabBar},
};

#[derive(Clone)]
struct DraggedStatusItem {
    source: String,
    label: String,
    active: bool,
    colors: ChromePalette,
    width: f32,
}

const TAB_MAX_WIDTH: f32 = 240.0;

impl Render for DraggedStatusItem {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(28.0))
            .w(px(self.width))
            .debug_selector(|| "status-drag-preview".to_owned())
            .px(px(10.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .bg(color(self.colors.base))
            .border_1()
            .border_color(color(self.colors.border_variant))
            .text_sm()
            .text_color(color(if self.active {
                self.colors.text
            } else {
                self.colors.muted
            }))
            .child(self.label.clone())
    }
}

#[derive(Clone, Copy)]
struct ItemLayout {
    stretch: bool,
}

#[derive(Clone, Copy)]
struct StatusBarStyle<'a> {
    tab_config: bootty_config::config::TabConfig,
    keymap_context: &'a str,
    key: &'a str,
    background: Rgba,
    segmented: bool,
}

pub(super) struct RenderParams<'a> {
    pub(super) tab_config: bootty_config::config::TabConfig,
    pub(super) keymap_context: &'a str,
    pub(super) snapshot: &'a StatusBarSnapshot,
    pub(super) row_height: f32,
    pub(super) top_padding: f32,
    pub(super) compact: bool,
    pub(super) partition: Option<(f32, bool)>,
    pub(super) colors: ChromePalette,
    pub(super) tab_bounds: TabBounds,
    pub(super) insertion_target: Option<&'a TabInsertionTarget>,
    pub(super) tab_focus_handles: &'a std::collections::HashMap<String, FocusHandle>,
}

pub(super) fn render(params: RenderParams<'_>, cx: &Context<GpuiChrome>) -> gpui_kit::AnyElement {
    let RenderParams {
        tab_config,
        keymap_context,
        snapshot,
        row_height,
        top_padding,
        compact,
        partition,
        colors,
        tab_bounds,
        insertion_target,
        tab_focus_handles,
    } = params;
    let has_tabs = snapshot
        .segments
        .iter()
        .any(|segment| segment.surface == "windows");
    let row_height = if has_tabs {
        row_height.max(crate::gpui::UI_TAB_BAR_HEIGHT)
    } else {
        row_height
    };
    let mut left = Vec::new();
    let mut center = Vec::new();
    let mut right = Vec::new();
    let bar = StatusBarStyle {
        tab_config,
        keymap_context,
        key: &snapshot.key,
        background: snapshot.background,
        segmented: compact,
    };
    for segment in &snapshot.segments {
        let target = match segment.align {
            StatusAlignment::Left => &mut left,
            StatusAlignment::Center => &mut center,
            StatusAlignment::Right => &mut right,
        };
        target.extend(render_segment_items(
            segment,
            colors,
            bar,
            &tab_bounds,
            insertion_target,
            tab_focus_handles,
            cx,
        ));
    }
    if compact {
        let items = left
            .into_iter()
            .chain(center)
            .chain(right)
            .map(|item| {
                div()
                    .h(px(row_height - 8.0))
                    .rounded(cx.theme().radius)
                    .overflow_hidden()
                    .child(item)
                    .into_any_element()
            })
            .collect();
        return super::status_fit::StatusFit {
            id: SharedString::from(format!(
                "{}-status-fit-{}",
                bar.key,
                partition.is_some_and(|(_, dock)| dock)
            ))
            .into(),
            items,
            height: px(row_height),
            gap: px(4.0),
            partition: partition.map(|(width, side)| (px(width), side)),
        }
        .into_any_element();
    }
    let status_row = aligned_status_row(left, center, right, row_height, snapshot, cx);
    status_frame(snapshot, row_height, top_padding, has_tabs, status_row, cx)
}

fn aligned_status_row(
    left: Vec<gpui_kit::AnyElement>,
    center: Vec<gpui_kit::AnyElement>,
    right: Vec<gpui_kit::AnyElement>,
    row_height: f32,
    snapshot: &StatusBarSnapshot,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::Div {
    div()
        .relative()
        .h(px(row_height))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .overflow_hidden()
        .bg(color(snapshot.background))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .overflow_hidden()
                .gap_1()
                .items_center()
                .children(left),
        )
        .child(
            div()
                .flex_initial()
                .min_w_0()
                .h_full()
                .flex()
                .overflow_hidden()
                .gap_1()
                .items_center()
                .justify_center()
                .children(center),
        )
        .child(
            div()
                .flex_initial()
                .min_w_0()
                .h_full()
                .flex()
                .overflow_hidden()
                .gap_1()
                .items_center()
                .justify_end()
                .children(right)
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "status-reorder-end-{}",
                            snapshot.key
                        )))
                        .h_full()
                        .w(px(8.0))
                        .flex_none()
                        .on_drop(cx.listener(|_, dragged: &DraggedStatusItem, _, cx| {
                            cx.emit(ChromeIntent::Status(StatusIntent::Reorder {
                                source: dragged.source.clone(),
                                before: None,
                            }));
                        })),
                ),
        )
}

fn status_frame(
    snapshot: &StatusBarSnapshot,
    row_height: f32,
    top_padding: f32,
    has_tabs: bool,
    status_row: gpui_kit::Div,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    div()
        .id(SharedString::from(format!("status-{}", snapshot.key)))
        .debug_selector({
            let key = snapshot.key.clone();
            move || format!("status-{key}")
        })
        .relative()
        .h(px(row_height + top_padding))
        .w_full()
        .flex_col()
        .overflow_hidden()
        // Keep the spacer so status item and tab backgrounds begin at the actual row instead of
        // filling the inset above it.
        .bg(color(snapshot.background))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseDownEvent, _, _| {
                this.window_drag.arm();
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                this.window_drag.cancel();
                if has_tabs && let Some(intent) = this.tab_drag.release() {
                    cx.emit(ChromeIntent::Status(intent));
                }
                cx.notify();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                this.window_drag.cancel();
                if has_tabs {
                    this.tab_drag.cancel();
                }
                cx.notify();
            }),
        )
        .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, _| {
            this.window_drag.cancel();
        }))
        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            update_tab_drag(this, event, has_tabs);
            if this.window_drag.take_on_motion() {
                cx.emit(ChromeIntent::StartWindowDrag);
            }
        }))
        .child(div().h(px(top_padding)).w_full().flex_none())
        .child(status_row)
        .into_any_element()
}

fn update_tab_drag(this: &mut GpuiChrome, event: &MouseMoveEvent, has_tabs: bool) {
    if has_tabs && event.dragging() {
        this.tab_drag
            .move_pointer(f32::from(event.position.x), f32::from(event.position.y));
        let (before, right_edge) = {
            let bounds = this.tab_bounds.borrow();
            let mut ordered = bounds.iter().collect::<Vec<_>>();
            ordered.sort_by(|(_, left), (_, right)| {
                left.origin
                    .x
                    .partial_cmp(&right.origin.x)
                    .unwrap_or(Ordering::Equal)
            });
            let before = ordered
                .iter()
                .enumerate()
                .find(|(_, (_, bounds))| bounds.contains(&event.position))
                .and_then(|(index, (_, bounds))| {
                    let anchors = ordered
                        .iter()
                        .map(|(anchor, _)| (*anchor).clone())
                        .collect::<Vec<_>>();
                    let source = this.tab_drag.source()?;
                    let right_half = f32::from(event.position.x)
                        >= f32::from(bounds.origin.x) + f32::from(bounds.size.width) / 2.0;
                    tab_insertion_target(&anchors, source, index, right_half).map(|target| {
                        match target {
                            TabInsertionTarget::Before(before) => Some(before),
                            TabInsertionTarget::End => None,
                        }
                    })
                });
            let right_edge = ordered
                .last()
                .map(|(_, bounds)| f32::from(bounds.origin.x) + f32::from(bounds.size.width));
            (before, right_edge)
        };
        if let Some(before) = before {
            this.tab_drag.hover_before(before.as_deref());
        } else if right_edge.is_some_and(|right| f32::from(event.position.x) > right) {
            this.tab_drag.hover_before(None);
        }
    }
}

pub(super) fn tab_ids(snapshot: &StatusBarSnapshot) -> Vec<String> {
    snapshot
        .segments
        .iter()
        .filter(|segment| segment.surface == "windows")
        .flat_map(|segment| tab_ids_for_segment(&snapshot.key, segment))
        .collect()
}

fn tab_id(bar_key: &str, source_slot: usize, key: &str) -> String {
    format!("status-tab-{bar_key}-{source_slot}-{key}")
}

fn tab_groups(segment: &StatusSegmentSnapshot) -> impl Iterator<Item = &[StatusItemSnapshot]> {
    segment.items.chunk_by(|left, right| {
        left.reorder_anchor.is_some() && left.reorder_anchor == right.reorder_anchor
    })
}

fn tab_ids_for_segment(bar_key: &str, segment: &StatusSegmentSnapshot) -> Vec<String> {
    tab_groups(segment)
        .filter_map(|items| items.first())
        .map(|item| tab_id(bar_key, segment.source_slot, &item.key))
        .collect()
}

fn render_segment_items(
    segment: &StatusSegmentSnapshot,
    colors: ChromePalette,
    bar: StatusBarStyle<'_>,
    tab_bounds: &TabBounds,
    insertion_target: Option<&TabInsertionTarget>,
    tab_focus_handles: &std::collections::HashMap<String, FocusHandle>,
    cx: &Context<GpuiChrome>,
) -> Vec<gpui_kit::AnyElement> {
    if segment.surface != "windows" {
        return segment
            .items
            .iter()
            .map(|item| {
                render_item(
                    segment,
                    item,
                    bar,
                    colors,
                    None,
                    ItemLayout { stretch: false },
                    cx,
                )
            })
            .collect();
    }
    let groups = tab_groups(segment).collect::<Vec<_>>();
    let active_index = groups
        .iter()
        .position(|items| items.iter().any(tab_is_active));
    let tab_ids: Rc<[String]> = tab_ids_for_segment(bar.key, segment).into();
    let tabs = groups
        .into_iter()
        .map(|items| {
            render_tab(
                segment,
                items,
                bar,
                colors,
                tab_bounds.clone(),
                insertion_target,
                tab_focus_handles,
                &tab_ids,
                cx,
            )
        })
        .collect::<Vec<_>>();
    let end_target = tab_end_target(bar, segment.source_slot, insertion_target, colors, cx);
    vec![
        div()
            .relative()
            .min_w_0()
            .h_full()
            .flex_1()
            .overflow_hidden()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "status-tabs-{}-{}",
                        bar.key, segment.source_slot
                    )))
                    .h_full()
                    .flex()
                    .items_center()
                    // Preserve the custom mux drag gesture and horizontal scrolling.
                    .overflow_x_scroll()
                    .child(crate::gpui::tabs::blend_bar(
                        TabBar::new(SharedString::from(format!(
                            "mux-tabs-{}-{}",
                            bar.key, segment.source_slot
                        )))
                        .with_variant(crate::gpui::tabs::variant(bar.tab_config.appearance))
                        .with_size(crate::gpui::tabs::size(bar.tab_config.appearance))
                        .max_width(px(TAB_MAX_WIDTH))
                        .min_w_full()
                        .flex_shrink_0()
                        .when_some(
                            active_index,
                            gpui_kit::component::tab::TabBar::selected_index,
                        )
                        .children(tabs)
                        .suffix(gpui_kit::Empty)
                        .last_empty_space(end_target),
                        bar.tab_config.appearance,
                        color(bar.background),
                    )),
            )
            .into_any_element(),
    ]
}

fn tab_end_target(
    bar: StatusBarStyle<'_>,
    source_slot: usize,
    insertion_target: Option<&TabInsertionTarget>,
    colors: ChromePalette,
    cx: &Context<GpuiChrome>,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!(
            "status-tab-drop-end-{}-{}",
            bar.key, source_slot
        )))
        .min_w(px(24.0))
        .h_full()
        .flex_grow(1.0)
        .child("")
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
            if event.dragging() {
                this.tab_drag.hover_before(None);
                cx.notify();
            }
        }))
        .when(
            matches!(insertion_target, Some(TabInsertionTarget::End)),
            |element| element.border_l_2().border_color(color(colors.accent)),
        )
        .drag_over::<DraggedStatusItem>(move |element, _, _, _| {
            element.border_l_2().border_color(color(colors.accent))
        })
        .on_drop(cx.listener(move |_, dragged: &DraggedStatusItem, _, cx| {
            cx.emit(ChromeIntent::Status(StatusIntent::Reorder {
                source: dragged.source.clone(),
                before: None,
            }));
        }))
}

#[allow(clippy::too_many_arguments)]
fn render_tab(
    segment: &StatusSegmentSnapshot,
    items: &[StatusItemSnapshot],
    bar: StatusBarStyle<'_>,
    colors: ChromePalette,
    tab_bounds: TabBounds,
    insertion_target: Option<&TabInsertionTarget>,
    tab_focus_handles: &std::collections::HashMap<String, FocusHandle>,
    tab_ids: &Rc<[String]>,
    cx: &Context<GpuiChrome>,
) -> Tab {
    let active = items.iter().any(tab_is_active);
    let key = items.first().map_or("empty", |item| item.key.as_str());
    let label = items
        .iter()
        .map(|item| item.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let tab_context = items.iter().find_map(|item| item.tab_context.clone());
    let activation = tab_activation(items, tab_context.as_ref());
    let source = items.first().and_then(|item| item.reorder_anchor.clone());
    let insertion_here = source.as_ref().is_some_and(|source| {
        matches!(insertion_target, Some(TabInsertionTarget::Before(before)) if before == source)
    });
    let group = SharedString::from(format!(
        "status-tab-hover-{}-{}-{key}",
        bar.key, segment.source_slot
    ));
    let focus_id = tab_id(bar.key, segment.source_slot, key);
    let tab_focus = tab_focus_handles.get(&focus_id).cloned();
    let focus_id_for_key = focus_id;
    let navigation_ids_for_key = tab_ids.clone();
    let activation_for_key = activation.clone();
    let items = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            render_item(
                segment,
                item,
                bar,
                colors,
                Some(active),
                ItemLayout {
                    stretch: index.saturating_add(1) == items.len(),
                },
                cx,
            )
        })
        .collect::<Vec<_>>();

    let close = tab_close_button(
        tab_context.as_ref(),
        format!("status-tab-close-{}-{}-{key}", bar.key, segment.source_slot),
        &label,
        cx,
    );

    let content = crate::gpui::tabs::content(
        div()
            .h_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .children(items)
            .into_any_element(),
        close,
        group.clone(),
        bar.tab_config,
        (active && bar.tab_config.appearance == bootty_config::config::TabAppearance::Segmented)
            .then_some(cx.theme().secondary_active),
    );
    let content = tab_content_gestures(content, source.as_ref(), tab_context.as_ref(), cx);
    let tab = Tab::new()
        .debug_selector({
            let source_slot = segment.source_slot;
            let key = key.to_owned();
            let bar_key = bar.key.to_owned();
            move || format!("status-tab-{bar_key}-{source_slot}-{key}")
        })
        .group(group)
        .selected(active)
        .aria_label(label)
        .when(insertion_here, |element| {
            element.border_l_2().border_color(color(colors.accent))
        })
        .when_some(tab_focus, |element, focus| {
            element.track_focus(&focus).focusable().tab_index(0_isize)
        })
        .cursor_pointer()
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            status_tab_key(
                this,
                event,
                &navigation_ids_for_key,
                &focus_id_for_key,
                activation_for_key.as_ref(),
                window,
                cx,
            );
        }))
        .when_some(activation, |element, intent| {
            element.on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(ChromeIntent::Status(intent.clone()));
            }))
        })
        .map(|tab| tab_middle_close(tab, tab_context, cx))
        .child(content);
    measured_tab(tab, source, tab_bounds, cx)
}

fn tab_content_gestures(
    content: gpui_kit::Div,
    source: Option<&String>,
    tab_context: Option<&TabContextSnapshot>,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    // Kit stops left presses at the tab. Start the mux gesture on its content,
    // whose padding spans the tab, so the close control can stop it first.
    let pressed_source = source.cloned();
    let content = content.on_mouse_down(
        MouseButton::Left,
        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
            this.window_drag.cancel();
            if let Some(source) = &pressed_source {
                this.tab_drag.begin(source);
                cx.notify();
            }
        }),
    );
    if let Some(tab_context) = tab_context.cloned() {
        let owner = cx.weak_entity();
        content
            .context_menu(move |menu, _, _| {
                super::popup_menu(menu, &ContextMenu::Tab(tab_context.clone()), &owner)
            })
            .into_any_element()
    } else {
        content.into_any_element()
    }
}

fn tab_middle_close(
    tab: Tab,
    tab_context: Option<TabContextSnapshot>,
    cx: &Context<GpuiChrome>,
) -> Tab {
    tab.when_some(tab_context, |element, tab_context| {
        let close_context = tab_context;
        element.when(close_context.can_close_pane, |element| {
            element.on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |_, _, _, cx| {
                    cx.emit(ChromeIntent::Status(StatusIntent::Context {
                        session_id: close_context.session_id.clone(),
                        window_id: close_context.window_id.clone(),
                        action: TabContextAction::ClosePane,
                    }));
                    cx.stop_propagation();
                }),
            )
        })
    })
}

fn tab_activation(
    items: &[StatusItemSnapshot],
    tab_context: Option<&TabContextSnapshot>,
) -> Option<StatusIntent> {
    items
        .iter()
        .find_map(|item| item.action.clone())
        .map(StatusIntent::Action)
        .or_else(|| {
            tab_context
                .filter(|tab_context| tab_context.can_activate)
                .map(|tab_context| StatusIntent::Context {
                    session_id: tab_context.session_id.clone(),
                    window_id: tab_context.window_id.clone(),
                    action: TabContextAction::Activate,
                })
        })
}

fn tab_close_button(
    tab_context: Option<&TabContextSnapshot>,
    id: String,
    label: &str,
    cx: &Context<GpuiChrome>,
) -> Option<gpui_kit::AnyElement> {
    tab_context.and_then(|tab_context| {
        tab_context.can_close_pane.then(|| {
            let session_id = tab_context.session_id.clone();
            let window_id = tab_context.window_id.clone();
            Button::new(SharedString::from(id.clone()))
                .icon(gpui_kit::component::IconName::Close)
                .ghost()
                .xsmall()
                .size_4()
                .debug_selector(move || id)
                .accessibility_label(format!("Close pane in {label}"))
                .tooltip(format!("Close pane in {label}"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(ChromeIntent::Status(StatusIntent::Context {
                        session_id: session_id.clone(),
                        window_id: window_id.clone(),
                        action: TabContextAction::ClosePane,
                    }));
                }))
                .into_any_element()
        })
    })
}

fn status_tab_key(
    this: &GpuiChrome,
    event: &KeyDownEvent,
    navigation_ids: &[String],
    focus_id: &str,
    activation: Option<&StatusIntent>,
    window: &mut Window,
    cx: &mut Context<GpuiChrome>,
) {
    match event.keystroke.key.as_str() {
        "enter" | "space" => {
            if let Some(intent) = activation.cloned() {
                cx.emit(ChromeIntent::Status(intent));
            }
        }
        "left" | "up" => this.focus_status_tab(
            navigation_ids,
            focus_id,
            StatusTabFocusMovement::Previous,
            window,
            cx,
        ),
        "right" | "down" => this.focus_status_tab(
            navigation_ids,
            focus_id,
            StatusTabFocusMovement::Next,
            window,
            cx,
        ),
        "home" => this.focus_status_tab(
            navigation_ids,
            focus_id,
            StatusTabFocusMovement::First,
            window,
            cx,
        ),
        "end" => this.focus_status_tab(
            navigation_ids,
            focus_id,
            StatusTabFocusMovement::Last,
            window,
            cx,
        ),
        _ => return,
    }
    cx.stop_propagation();
}

fn measured_tab(
    tab: Tab,
    source: Option<String>,
    tab_bounds: TabBounds,
    cx: &Context<GpuiChrome>,
) -> Tab {
    match source {
        Some(source) => {
            let measured_source = source.clone();
            let measured_bounds = tab_bounds;
            tab.cursor_move()
                .on_prepaint(move |bounds, _, _| {
                    measured_bounds
                        .borrow_mut()
                        .insert(measured_source.clone(), bounds);
                })
                .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                    if event.dragging() {
                        this.tab_drag.hover_before(Some(&source));
                        cx.notify();
                    }
                }))
        }
        None => tab,
    }
}

fn render_item(
    segment: &StatusSegmentSnapshot,
    item: &StatusItemSnapshot,
    bar: StatusBarStyle<'_>,
    colors: ChromePalette,
    tab_active: Option<bool>,
    layout: ItemLayout,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    let tab = tab_active.is_some();
    let action = item.action.clone();
    if let Some(super::NativeChromeAction::TogglePanel(kind)) = &item.action {
        return panel_button(*kind, item, bar, colors, cx);
    }
    let reorder_anchor = (!tab).then(|| item.reorder_anchor.clone()).flatten();
    let active = tab_active.unwrap_or(false);
    let foreground = if tab {
        if active { colors.text } else { colors.muted }
    } else {
        item.foreground.unwrap_or(colors.text)
    };
    let background = if tab {
        if active { colors.base } else { colors.tab_bar }
    } else {
        item.background.unwrap_or(bar.background)
    };
    let gauge = item.gauge.map(|value| status_gauge(value, foreground));
    let progress = item.progress.map(|progress| {
        status_progress(
            progress,
            format!("status-progress-{}-{}", segment.source_slot, item.key),
        )
    });
    let element = div()
        .id(SharedString::from(format!(
            "status-item-{}-{}-{}",
            bar.key, segment.source_slot, item.key
        )))
        .debug_selector({
            let source_slot = segment.source_slot;
            let key = item.key.clone();
            move || format!("status-item-{source_slot}-{key}")
        })
        .relative()
        .when(layout.stretch, |element| element.flex_1().min_w_0())
        .when(!layout.stretch, gpui_kit::Styled::flex_none)
        .h_full()
        .when(!tab, |element| {
            element
                .pl(px(8.0 + item.pad_left.max(0.0)))
                .pr(px(8.0 + item.pad_right.max(0.0)))
        })
        .flex()
        .items_center()
        .gap_1()
        .overflow_hidden()
        .text_xs()
        .when(!tab, |element| element.text_color(color(foreground)))
        .when(!tab, |element| {
            element.bg(if bar.segmented && item.background.is_none() {
                cx.theme().tab_active
            } else {
                color(background)
            })
        })
        .when(bar.segmented, |element| element.rounded(cx.theme().radius))
        .when(!tab && action.is_none(), |element| {
            element.on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
        })
        .when(!tab && item.active, |element| {
            element.border_b_1().border_color(color(colors.accent))
        })
        .when_some(item.icon.clone(), |element, icon| {
            element.child(sized_icon(&icon, IconSize::Small, color(foreground)))
        })
        .when_some(gauge, ParentElement::child)
        .child(
            div()
                .debug_selector({
                    let id = format!("status-label-{}-{}", segment.source_slot, item.key);
                    move || id
                })
                .min_w_0()
                .truncate()
                .child(item.text.clone()),
        )
        .when(tab, |element| {
            element.when_some(progress, ParentElement::child)
        })
        .when_some(reorder_anchor, |element, source| {
            reorderable_item(element, source, item, colors, cx)
        });
    finish_item(element, segment, item, bar, tab, layout, cx)
}

fn finish_item(
    element: gpui_kit::Stateful<gpui_kit::Div>,
    segment: &StatusSegmentSnapshot,
    item: &StatusItemSnapshot,
    bar: StatusBarStyle<'_>,
    tab: bool,
    layout: ItemLayout,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    let context = (!tab).then(|| item.tab_context.clone()).flatten();
    let action = item.action.clone();
    let accessible_label = if item.text.trim().is_empty() {
        item.key.clone()
    } else {
        item.text.clone()
    };
    let element = if let Some(context) = context {
        let owner = cx.weak_entity();
        element
            .context_menu(move |menu, _, _| {
                super::popup_menu(menu, &ContextMenu::Tab(context.clone()), &owner)
            })
            .into_any_element()
    } else {
        element.into_any_element()
    };
    if !tab && let Some(action) = action {
        status_action(
            format!(
                "status-action-{}-{}-{}",
                bar.key, segment.source_slot, item.key
            ),
            element,
            layout,
            accessible_label,
            action,
            cx,
        )
    } else {
        element
    }
}

fn reorderable_item(
    element: gpui_kit::Stateful<gpui_kit::Div>,
    source: String,
    item: &StatusItemSnapshot,
    colors: ChromePalette,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let dragged = DraggedStatusItem {
        source: source.clone(),
        label: item.text.clone(),
        active: item.active,
        colors,
        width: 96.0,
    };
    element
        .cursor_move()
        .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
        .on_drop(cx.listener(move |_, dragged: &DraggedStatusItem, _, cx| {
            if dragged.source != source {
                cx.emit(ChromeIntent::Status(StatusIntent::Reorder {
                    source: dragged.source.clone(),
                    before: Some(source.clone()),
                }));
            }
        }))
}

fn panel_button(
    kind: bootty_config::config::PanelKind,
    item: &StatusItemSnapshot,
    bar: StatusBarStyle<'_>,
    colors: ChromePalette,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    let action = crate::commands::DockAction::TogglePanel(kind);
    let command = action.command();
    let invocation = bootty_control::CommandInvocation::from_action(
        command.action(),
        bootty_control::Caller::Internal,
    );
    Button::new(SharedString::from(format!(
        "panel-button-{}-{}",
        bar.key, item.key
    )))
    .ghost()
    .small()
    .selected(item.active)
    .child(sized_icon(
        command.icon(),
        IconSize::Small,
        color(colors.text),
    ))
    .tooltip_with_action(
        command.title(),
        &crate::gpui_actions::dock_binding_action(action),
        Some(bar.keymap_context),
    )
    .accessibility_label(command.title())
    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChromeIntent::Command(invocation.clone()))))
    .into_any_element()
}

fn status_action(
    id: String,
    element: gpui_kit::AnyElement,
    layout: ItemLayout,
    accessible_label: String,
    action: super::NativeChromeAction,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    let owner = cx.weak_entity();
    let button = Button::new(SharedString::from(id))
        .ghost()
        .p_0()
        .when(layout.stretch, gpui_kit::Styled::w_full)
        .h_full()
        .tab_index(0_isize)
        .accessibility_label(accessible_label)
        .child(element);
    let activated = super::button::activated_button(
        div()
            .h_full()
            .when(layout.stretch, gpui_kit::Styled::w_full),
        button,
        move |_, app| {
            _ = owner.update(app, |_, cx| {
                cx.emit(ChromeIntent::Status(StatusIntent::Action(action.clone())));
            });
        },
    );
    div()
        .flex_none()
        .when(layout.stretch, |element| element.flex_1().min_w_0())
        .h_full()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(activated)
        .into_any_element()
}

/// Paint the native tab progress edge. Indeterminate progress sweeps continuously so a pending
/// native operation remains visibly alive without changing the tab's measured content geometry.
fn status_progress(progress: StatusProgress, id: String) -> gpui_kit::AnyElement {
    const HEIGHT: f32 = 2.0;
    const SWEEP_PERIOD: f64 = 1.5;
    static STARTED: OnceLock<Instant> = OnceLock::new();
    let started = *STARTED.get_or_init(Instant::now);

    let bar = canvas(
        move |bounds, _, _| bounds,
        move |bounds, _, window, _| {
            let width = f32::from(bounds.size.width).max(0.0);
            let (fill_x, fill_width) = progress.value.map_or_else(
                || {
                    if window.is_window_active() {
                        window.request_animation_frame();
                    }
                    let phase = (started.elapsed().as_secs_f64() % SWEEP_PERIOD) / SWEEP_PERIOD;
                    let phase = if phase < 0.5 {
                        phase * 2.0
                    } else {
                        phase.mul_add(-2.0, 2.0)
                    };
                    let phase = phase.to_f32().unwrap_or_default();
                    let width = width * 0.35;
                    (width * 0.65 * phase, width)
                },
                |value| (0.0, width * (f32::from(value.min(100)) / 100.0)),
            );
            window.paint_quad(gpui_kit::quad(
                gpui_kit::Bounds {
                    origin: gpui_kit::point(
                        px(f32::from(bounds.origin.x) + fill_x),
                        bounds.origin.y,
                    ),
                    size: gpui_kit::size(px(fill_width), px(HEIGHT)),
                },
                gpui_kit::Corners {
                    top_left: px(0.0),
                    top_right: px(0.0),
                    bottom_right: px(0.0),
                    bottom_left: px(0.0),
                },
                color(progress.color),
                px(0.0),
                gpui_kit::transparent_black(),
                gpui_kit::BorderStyle::Solid,
            ));
        },
    )
    .size_full();
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(HEIGHT))
        .debug_selector(move || id)
        .child(bar)
        .into_any_element()
}

/// Paint an inline battery gauge without affecting the native tab progress edge.
fn status_gauge(value: f32, foreground: Rgba) -> gpui_kit::AnyElement {
    const WIDTH: f32 = 22.0;
    const HEIGHT: f32 = 11.0;
    const INSET: f32 = 2.0;

    div()
        .relative()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .flex_none()
        .rounded(px(2.0))
        .border_1()
        .border_color(color(foreground))
        .child(
            div()
                .absolute()
                .left(px(INSET))
                .right(px(INSET))
                .top(px(INSET))
                .bottom(px(INSET))
                .overflow_hidden()
                .rounded(px(1.0))
                .child(
                    div()
                        .h_full()
                        .w(relative(value.clamp(0.0, 1.0)))
                        .bg(color(foreground)),
                ),
        )
        .into_any_element()
}

fn tab_is_active(item: &StatusItemSnapshot) -> bool {
    item.active
        || item
            .tab_context
            .as_ref()
            .is_some_and(|context| !context.can_activate)
}

pub(super) fn tab_menu(tab: TabContextSnapshot) -> Vec<MenuRow> {
    use TabContextAction as A;
    let row = |label: &str, enabled: bool, destructive: bool, starts_group: bool, action| MenuRow {
        label: label.to_owned(),
        enabled,
        destructive,
        starts_group,
        intent: ChromeIntent::Status(StatusIntent::Context {
            session_id: tab.session_id.clone(),
            window_id: tab.window_id.clone(),
            action,
        }),
    };
    let mut rows = vec![
        row("Activate Tab", tab.can_activate, false, false, A::Activate),
        row("New Tab", true, false, true, A::NewTab),
        row(
            "Previous Tab",
            tab.can_navigate,
            false,
            false,
            A::PreviousTab,
        ),
        row("Next Tab", tab.can_navigate, false, false, A::NextTab),
        row("Last Tab", tab.can_navigate, false, false, A::LastTab),
        row("Rename Tab", true, false, true, A::Rename),
        row(
            "Move Tab Left",
            tab.can_move_left,
            false,
            false,
            A::MoveLeft,
        ),
        row(
            "Move Tab Right",
            tab.can_move_right,
            false,
            false,
            A::MoveRight,
        ),
        row("Close Pane", tab.can_close_pane, true, true, A::ClosePane),
    ];
    rows.extend(
        tab.pane_actions
            .into_iter()
            .enumerate()
            .map(|(index, (label, invocation))| MenuRow {
                label,
                enabled: true,
                destructive: false,
                starts_group: index == 0,
                intent: ChromeIntent::Command(invocation),
            }),
    );
    rows
}
