use std::{borrow::Cow, collections::HashMap, fmt::Write as _, time::Duration};

use bootty_ui::ThemePalette;
use eframe::egui::{self, CornerRadius, Pos2, Rect, Stroke, StrokeKind, TextureHandle};

use crate::{
    assets,
    config::{ChromeConfig, SegmentAlign},
    mux::{
        sidebar_meta::{DiffStat, SidebarMetadata},
        snapshot::MuxSession,
    },
    status_module::{ModuleCoord, ModulePrimitive},
    strings::{push_truncated_label, truncate_label},
    ui::{
        icons::{Icon, has_slug, paint_icon, paint_icon_slug},
        sidebar::{
            SidebarDisplay, SidebarItem, SidebarItemKind, SidebarTree, build_visible_sidebar_items,
        },
    },
};

#[derive(Clone, Debug)]
pub struct StatusBarModel<'a> {
    /// Ordered, resolved segments. Every segment is a Luau module's items; the app fills these in.
    pub segments: &'a [ResolvedSegment],
    /// Bar fill; set to the sidebar fullscreen background when the bar sits in the notch band.
    pub background: egui::Color32,
    /// Left edge inset before left-aligned segments; zero lets tab strips sit flush to adjacent chrome.
    pub left_padding: f32,
}

/// A status segment resolved for this frame: a module's items plus where the segment is aligned.
#[derive(Clone, Debug, Default)]
pub struct ResolvedSegment {
    pub align: SegmentAlign,
    pub items: Vec<ResolvedItem>,
}

/// One drawable element from a module. `action` (e.g. `activate-window:<id>`) is dispatched on click.
#[derive(Clone, Debug, Default)]
pub struct ResolvedItem {
    pub text: String,
    pub icon: Option<String>,
    pub fg: Option<egui::Color32>,
    pub bg: Option<egui::Color32>,
    pub stroke: Option<egui::Color32>,
    /// 0.0-1.0 fill drawn as a battery meter before the text.
    pub gauge: Option<f32>,
    pub primitives: Vec<ModulePrimitive>,
    pub pad_left: f32,
    pub pad_right: f32,
    /// Whether this item may visually connect its background to adjacent items. Defaults to true.
    pub join: Option<bool>,
    /// Whether to keep the normal inter-item gap before this item. Defaults to true.
    pub gap: Option<bool>,
    pub action: Option<String>,
}

#[derive(Clone)]
pub struct SidebarModel<'a> {
    pub sessions: &'a [MuxSession],
    pub selected_session: Option<&'a str>,
    pub metadata: &'a SidebarMetadata,
    pub title_visible: bool,
    pub reserve_titlebar_buttons: bool,
    pub title_icon: Option<&'a TextureHandle>,
    pub top_inset: f32,
    pub border_visible: bool,
    pub separator_visible: bool,
    pub focused: bool,
    pub hovered_session: Option<&'a str>,
    pub unfocused_dim: f32,
    /// Explicit color overrides from `[sidebar]`; each falls back to the theme-derived tint.
    pub hover_override: Option<egui::Color32>,
    pub current_override: Option<egui::Color32>,
    pub border_override: Option<egui::Color32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarEvent {
    ActivateSession(String),
    Reorder {
        source: String,
        before: Option<String>,
    },
}

pub const STATUS_EDGE_PAD: f32 = 12.0;
const STATUS_ITEM_GAP: f32 = 4.0;
const STATUS_ITEM_PAD: f32 = 8.0;
const STATUS_ICON_GAP: f32 = 6.0;
/// Square edge of a status-bar icon glyph, matched to the 12pt text.
const STATUS_ICON_SIZE: f32 = 14.0;
/// Battery meter dimensions for a `gauge` item (body width excludes the nub).
const STATUS_GAUGE_WIDTH: f32 = 22.0;
const STATUS_GAUGE_HEIGHT: f32 = 11.0;
/// Corner radius (logical px) for status-bar pills and the strip's outer ends.
const STATUS_PILL_RADIUS: u8 = 6;

/// Native replacement for the tmux status line. Flattens each alignment group's module items and
/// lays them out: left from the left edge, right anchored to the right edge, center centered. Items
/// with a `bg` render as pills; items with an `action` are clickable. Returns a clicked action.
pub fn show_status_bar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    model: StatusBarModel<'_>,
) -> Option<String> {
    let height = ui.available_height();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    ui.painter_at(rect).rect_filled(rect, 0.0, model.background);

    let font = egui::FontId::monospace(12.0);
    let segments_for = |align: SegmentAlign| {
        model
            .segments
            .iter()
            .filter(move |segment| segment.align == align && !segment.items.is_empty())
            .collect::<Vec<_>>()
    };
    let right = segments_for(SegmentAlign::Right);
    let center = segments_for(SegmentAlign::Center);
    let left = segments_for(SegmentAlign::Left);

    let right_start = rect.max.x - STATUS_EDGE_PAD - segments_width(ui, &right, &font);
    let mut clicked = None;
    let primary_press_pos = ui.input(|input| {
        input
            .pointer
            .button_pressed(egui::PointerButton::Primary)
            .then(|| input.pointer.interact_pos())
            .flatten()
    });
    let mut drag_blocked = false;

    draw_segments(
        ui,
        rect,
        right_start,
        rect.max.x - STATUS_EDGE_PAD,
        &right,
        palette,
        &font,
        &mut clicked,
        primary_press_pos,
        &mut drag_blocked,
    );
    let left_end = draw_segments(
        ui,
        rect,
        rect.min.x + model.left_padding,
        right_start - STATUS_ITEM_GAP,
        &left,
        palette,
        &font,
        &mut clicked,
        primary_press_pos,
        &mut drag_blocked,
    );
    if !center.is_empty() {
        let center_width = segments_width(ui, &center, &font);
        let center_start = rect.center().x - center_width / 2.0;
        let center_bound = right_start - STATUS_ITEM_GAP;
        if center_start >= left_end + STATUS_ITEM_GAP && center_start + center_width <= center_bound
        {
            draw_segments(
                ui,
                rect,
                center_start,
                center_bound,
                &center,
                palette,
                &font,
                &mut clicked,
                primary_press_pos,
                &mut drag_blocked,
            );
        }
    }
    if !drag_blocked {
        start_window_drag_on_primary_press(&response);
    }
    clicked
}

fn segment_width(ui: &egui::Ui, segment: &ResolvedSegment, font: &egui::FontId) -> f32 {
    let items = segment.items.iter().collect::<Vec<_>>();
    items_width(ui, &items, font)
}

fn segments_width(ui: &egui::Ui, segments: &[&ResolvedSegment], font: &egui::FontId) -> f32 {
    let mut total = 0.0;
    for segment in segments.iter().filter(|segment| !segment.items.is_empty()) {
        if total > 0.0 {
            total += STATUS_ITEM_GAP;
        }
        total += segment_width(ui, segment, font);
    }
    total
}

#[allow(clippy::too_many_arguments)]
fn draw_segments(
    ui: &mut egui::Ui,
    rect: Rect,
    start_x: f32,
    bound: f32,
    segments: &[&ResolvedSegment],
    palette: ThemePalette,
    font: &egui::FontId,
    clicked: &mut Option<String>,
    primary_press_pos: Option<Pos2>,
    drag_blocked: &mut bool,
) -> f32 {
    let mut x = start_x;
    let mut drawn = 0;
    for segment in segments {
        let width = segment_width(ui, segment, font);
        if width <= 0.0 {
            continue;
        }
        if drawn > 0 {
            x += STATUS_ITEM_GAP;
        }
        if x + width > bound {
            break;
        }
        let items = segment.items.iter().collect::<Vec<_>>();
        draw_items(
            ui,
            rect,
            x,
            x + width,
            &items,
            palette,
            font,
            clicked,
            primary_press_pos,
            drag_blocked,
        );
        drawn += 1;
        x += width;
    }
    x
}

fn text_width(ui: &egui::Ui, text: &str, font: &egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
        .size()
        .x
}

/// Whether the item draws an iconflow glyph for the requested slug.
fn item_icon(item: &ResolvedItem) -> Option<&str> {
    item.icon.as_deref().filter(|slug| has_slug(slug))
}

fn item_width(ui: &egui::Ui, item: &ResolvedItem, font: &egui::FontId) -> f32 {
    let mut inner = text_width(ui, &item.text, font);
    let lead = if item.gauge.is_some() {
        Some(STATUS_GAUGE_WIDTH)
    } else if item_icon(item).is_some() {
        Some(STATUS_ICON_SIZE)
    } else {
        None
    };
    if let Some(lead_width) = lead {
        inner += lead_width;
        if !item.text.is_empty() {
            inner += STATUS_ICON_GAP;
        }
    }
    inner + STATUS_ITEM_PAD * 2.0 + item.pad_left + item.pad_right
}

/// Adjacent items that both carry a background render as one connected strip (no
/// gap), like the tmux/mux segmented bar, unless a module opts either item out.
fn connected(prev: Option<&ResolvedItem>, cur: &ResolvedItem) -> bool {
    prev.is_some_and(|prev| {
        item_join(prev) && item_join(cur) && prev.bg.is_some() && prev.stroke.is_none()
    }) && cur.bg.is_some()
        && cur.stroke.is_none()
}

fn item_join(item: &ResolvedItem) -> bool {
    item.join.unwrap_or(true)
}

fn item_gap_before(item: &ResolvedItem) -> bool {
    item.gap.unwrap_or(true)
}

fn items_width(ui: &egui::Ui, items: &[&ResolvedItem], font: &egui::FontId) -> f32 {
    let mut total = 0.0;
    let mut prev: Option<&ResolvedItem> = None;
    for item in items {
        if prev.is_some() && item_gap_before(item) && !connected(prev, item) {
            total += STATUS_ITEM_GAP;
        }
        total += item_width(ui, item, font);
        prev = Some(item);
    }
    total
}

/// A battery meter (rounded body + terminal nub) filled to `ratio`, tinted `color`.
fn paint_battery_gauge(
    painter: &egui::Painter,
    left: f32,
    center_y: f32,
    ratio: f32,
    color: egui::Color32,
) {
    let body_w = STATUS_GAUGE_WIDTH - 3.0;
    let body = Rect::from_min_size(
        Pos2::new(left, center_y - STATUS_GAUGE_HEIGHT / 2.0),
        egui::vec2(body_w, STATUS_GAUGE_HEIGHT),
    );
    painter.rect_stroke(body, 2.0, Stroke::new(1.0, color), StrokeKind::Inside);
    let nub = Rect::from_min_size(
        Pos2::new(body.max.x + 1.0, center_y - STATUS_GAUGE_HEIGHT * 0.22),
        egui::vec2(2.0, STATUS_GAUGE_HEIGHT * 0.44),
    );
    painter.rect_filled(nub, 0.0, color);
    let inset = 2.0;
    let fill_w = (body.width() - inset * 2.0) * ratio.clamp(0.0, 1.0);
    if fill_w > 0.5 {
        let fill = Rect::from_min_size(
            Pos2::new(body.min.x + inset, body.min.y + inset),
            egui::vec2(fill_w, body.height() - inset * 2.0),
        );
        painter.rect_filled(fill, 1.0, color);
    }
}

fn coord_x(rect: Rect, coord: ModuleCoord) -> f32 {
    rect.min.x + rect.width() * coord.frac + coord.px
}

fn coord_y(rect: Rect, coord: ModuleCoord) -> f32 {
    rect.min.y + rect.height() * coord.frac + coord.px
}

fn coord_w(rect: Rect, coord: ModuleCoord) -> f32 {
    rect.width() * coord.frac + coord.px
}

fn coord_h(rect: Rect, coord: ModuleCoord) -> f32 {
    rect.height() * coord.frac + coord.px
}

fn paint_item_primitives(painter: &egui::Painter, item_rect: Rect, item: &ResolvedItem) {
    for primitive in &item.primitives {
        match primitive {
            ModulePrimitive::Rect {
                fill,
                stroke,
                x,
                y,
                w,
                h,
                radius,
            } => {
                let rect = Rect::from_min_size(
                    Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)),
                    egui::vec2(coord_w(item_rect, *w), coord_h(item_rect, *h)),
                );
                if let Some(fill) = fill {
                    painter.rect_filled(rect, *radius, *fill);
                }
                if let Some(stroke) = stroke {
                    painter.rect_stroke(
                        rect,
                        *radius,
                        Stroke::new(1.0, *stroke),
                        StrokeKind::Inside,
                    );
                }
            }
            ModulePrimitive::Polygon {
                fill,
                stroke,
                points,
            } => {
                let points = points
                    .iter()
                    .map(|(x, y)| Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)))
                    .collect::<Vec<_>>();
                if points.len() >= 3 {
                    if let Some(fill) = fill {
                        painter.add(egui::Shape::convex_polygon(
                            points.clone(),
                            *fill,
                            Stroke::new(0.0, egui::Color32::TRANSPARENT),
                        ));
                    }
                    if let Some(stroke) = stroke {
                        painter.add(egui::Shape::closed_line(points, Stroke::new(1.0, *stroke)));
                    }
                }
            }
        }
    }
}

fn paint_status_item_background(
    painter: &egui::Painter,
    item_rect: Rect,
    bg: Option<egui::Color32>,
    stroke: Option<egui::Color32>,
    corners: CornerRadius,
) {
    if let Some(bg) = bg {
        painter.rect_filled(item_rect, corners, bg);
    }
    if let Some(stroke) = stroke {
        painter.rect_stroke(
            item_rect,
            corners,
            Stroke::new(1.0, stroke),
            StrokeKind::Inside,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_items(
    ui: &mut egui::Ui,
    rect: Rect,
    start_x: f32,
    bound: f32,
    items: &[&ResolvedItem],
    palette: ThemePalette,
    font: &egui::FontId,
    clicked: &mut Option<String>,
    primary_press_pos: Option<Pos2>,
    drag_blocked: &mut bool,
) {
    let mut x = start_x;
    for index in 0..items.len() {
        let item = items[index];
        let prev = (index > 0).then(|| items[index - 1]);
        let next = items.get(index + 1).copied();
        if prev.is_some() && item_gap_before(item) && !connected(prev, item) {
            x += STATUS_ITEM_GAP;
        }
        let width = item_width(ui, item, font);
        if x + width > bound {
            break;
        }
        let item_rect = Rect::from_min_size(
            Pos2::new(x, rect.min.y + 3.0),
            egui::vec2(width, rect.height() - 6.0),
        );

        let response = item.action.as_ref().map(|action| {
            let id = ui.make_persistent_id(("status-item", action.as_str(), x as i32));
            (ui.interact(item_rect, id, egui::Sense::click()), action)
        });
        if item.action.is_some() && primary_press_pos.is_some_and(|pos| item_rect.contains(pos)) {
            *drag_blocked = true;
        }
        let hovered = response.as_ref().is_some_and(|(resp, _)| resp.hovered());

        let painter = ui.painter_at(rect);
        if item.bg.is_some() || item.stroke.is_some() {
            let r = STATUS_PILL_RADIUS;
            let left_join = connected(prev, item);
            let right_join = next.is_some_and(|next| connected(Some(item), next));
            let corners = CornerRadius {
                nw: if left_join { 0 } else { r },
                sw: if left_join { 0 } else { r },
                ne: if right_join { 0 } else { r },
                se: if right_join { 0 } else { r },
            };
            paint_status_item_background(&painter, item_rect, item.bg, item.stroke, corners);
        } else if hovered {
            painter.rect_filled(item_rect, STATUS_PILL_RADIUS, palette.hover);
        }
        paint_item_primitives(&painter, item_rect, item);
        let color = item.fg.unwrap_or(palette.subtext);
        let mut text_x = x + STATUS_ITEM_PAD + item.pad_left;
        if let Some(ratio) = item.gauge {
            paint_battery_gauge(&painter, text_x, rect.center().y, ratio, color);
            text_x += STATUS_GAUGE_WIDTH;
            if !item.text.is_empty() {
                text_x += STATUS_ICON_GAP;
            }
        } else if let Some(slug) = item_icon(item) {
            let center = Pos2::new(text_x + STATUS_ICON_SIZE / 2.0, rect.center().y);
            paint_icon_slug(&painter, slug, center, STATUS_ICON_SIZE, color);
            text_x += STATUS_ICON_SIZE;
            if !item.text.is_empty() {
                text_x += STATUS_ICON_GAP;
            }
        }
        if !item.text.is_empty() {
            painter.text(
                Pos2::new(text_x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &item.text,
                font.clone(),
                color,
            );
        }

        if let Some((resp, action)) = response
            && resp.clicked()
        {
            *clicked = Some(action.clone());
        }
        x += width;
    }
}

const SIDEBAR_HEADER_HEIGHT: f32 = 44.0;
const SIDEBAR_FOOTER_BASE_HEIGHT: f32 = 44.0;
const SIDEBAR_MAX_USAGE_BARS: usize = 4;
const SIDEBAR_ROW_HEIGHT: f32 = 24.0;
const SIDEBAR_PAD_X: f32 = 14.0;
pub(crate) const MACOS_TITLEBAR_BUTTON_SAFE_WIDTH: f32 = 72.0;
const AGENT_DETAIL_MAX_CHARS: usize = 18;
const MACOS_TITLEBAR_BUTTON_CENTER_Y: f32 = 16.0;

fn start_window_drag_on_primary_press(response: &egui::Response) {
    let primary_press_pos = response.ctx.input(|input| {
        input
            .pointer
            .button_pressed(egui::PointerButton::Primary)
            .then(|| input.pointer.interact_pos())
            .flatten()
    });
    if primary_press_pos.is_some_and(|pos| response.rect.contains(pos)) {
        response
            .ctx
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

fn sidebar_title_drag_rect(rect: Rect, reserve_titlebar_buttons: bool) -> Rect {
    let reserved = if reserve_titlebar_buttons {
        MACOS_TITLEBAR_BUTTON_SAFE_WIDTH
    } else {
        0.0
    };
    Rect::from_min_max(
        Pos2::new((rect.min.x + reserved).min(rect.max.x), rect.min.y),
        rect.max,
    )
}

pub fn show_sidebar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    height: f32,
    model: SidebarModel<'_>,
) -> Option<SidebarEvent> {
    // `palette` arrives with `base`/`foreground` already overridden, so the derived tints below
    // mix from the resolved colors; explicit `[sidebar]` overrides win outright.
    let hover_color = model
        .hover_override
        .unwrap_or_else(|| sidebar_hover_color(palette));
    let current_color = model
        .current_override
        .unwrap_or_else(|| sidebar_current_color(palette));
    let border_color = model
        .border_override
        .unwrap_or_else(|| subtle_border(palette));
    let width = ui.max_rect().width().max(0.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, palette.base);
    if model.border_visible {
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, border_color),
            egui::StrokeKind::Inside,
        );
    }

    let header_h = sidebar_header_height(model.title_visible);
    let content_top = rect.min.y + model.top_inset;
    let title_rect = Rect::from_min_max(
        Pos2::new(rect.min.x, content_top),
        Pos2::new(rect.max.x, (content_top + header_h).min(rect.max.y)),
    );
    if model.title_visible {
        paint_sidebar_title(ui, title_rect, palette, &model);
        let drag_rect = sidebar_title_drag_rect(title_rect, model.reserve_titlebar_buttons);
        let response = ui.interact(
            drag_rect,
            ui.id().with("sidebar-titlebar-drag"),
            egui::Sense::click_and_drag(),
        );
        start_window_drag_on_primary_press(&response);
    }

    let list_top = content_top + header_h;
    let usage_bars = parse_usage_bars(model.metadata.usage_lines());
    let footer_h = sidebar_footer_height(usage_bars.len());
    if model.sessions.is_empty() {
        painter.text(
            Pos2::new(rect.center().x, list_top + 42.0),
            egui::Align2::CENTER_CENTER,
            "no mux sessions",
            egui::FontId::monospace(13.0),
            palette.muted,
        );
    }

    let max_rows = visible_sidebar_row_capacity(height, model.top_inset, header_h, footer_h);
    let items = build_visible_sidebar_items(
        model.sessions,
        model.selected_session,
        model.metadata,
        max_rows,
    );
    let preview_labels = sidebar_drag_preview_labels(&items);
    let drag_id = egui::Id::new("mux-sidebar-drag-anchor");
    let mut dragged = ui
        .ctx()
        .data_mut(|data| data.get_persisted::<SidebarDragState>(drag_id));
    let pointer_pos = ui.input(|input| {
        input
            .pointer
            .latest_pos()
            .or_else(|| input.pointer.hover_pos())
    });
    let primary_down = ui.input(|input| input.pointer.primary_down());
    let pointer_hovered_session = pointer_pos
        .and_then(|pos| sidebar_hovered_row(pos, rect.min.x, list_top, width, max_rows))
        .and_then(|index| items.get(index))
        .and_then(|item| item.session_id);
    let suppress_click = dragged.is_some();

    let mut event = None;
    for (index, item) in items.iter().enumerate() {
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.min.x, list_top + index as f32 * SIDEBAR_ROW_HEIGHT),
            egui::vec2(width, SIDEBAR_ROW_HEIGHT),
        );
        let hovered = item.session_id.is_some_and(|session_id| {
            Some(session_id) == pointer_hovered_session
                || model.focused && Some(session_id) == model.hovered_session
        });
        let response = sidebar_item_row(
            ui,
            row_rect,
            item,
            hovered,
            palette,
            hover_color,
            current_color,
        );
        if response.drag_started()
            && let Some(anchor) = item.reorder_anchor
        {
            let state = SidebarDragState {
                anchor: anchor.to_owned(),
                preview: preview_labels
                    .get(anchor)
                    .cloned()
                    .unwrap_or_else(|| anchor.to_owned()),
            };
            ui.ctx()
                .data_mut(|data| data.insert_persisted(drag_id, state.clone()));
            dragged = Some(state);
            ui.ctx().request_repaint();
        }

        if event.is_none()
            && !suppress_click
            && response.clicked_by(egui::PointerButton::Primary)
            && let Some(session_id) = &item.session_id
        {
            event = Some(SidebarEvent::ActivateSession((*session_id).to_owned()));
        }
    }

    let drop = dragged.as_ref().and_then(|drag| {
        sidebar_drop_target(
            &items,
            pointer_pos,
            rect.min.x,
            list_top,
            width,
            &drag.anchor,
        )
    });
    if let Some((_, indicator_y)) = drop {
        painter.line_segment(
            [
                Pos2::new(rect.min.x, indicator_y),
                Pos2::new(rect.max.x, indicator_y),
            ],
            Stroke::new(2.0, palette.primary),
        );
    }

    if let Some(drag) = dragged.as_ref() {
        paint_sidebar_drag_preview(ui, pointer_pos, &drag.preview, palette);
        if primary_down {
            ui.ctx().request_repaint();
        } else {
            if event.is_none() {
                event = sidebar_reorder_event(dragged.as_ref(), drop);
            }
            ui.ctx()
                .data_mut(|data| data.remove::<SidebarDragState>(drag_id));
        }
    }

    paint_sidebar_footer(
        ui,
        rect,
        footer_h,
        &usage_bars,
        model.separator_visible,
        palette,
        border_color,
    );
    if !model.focused {
        painter.rect_filled(rect, 0.0, dim_overlay_color(model.unfocused_dim));
    }
    event
}

pub(crate) fn sidebar_metadata_session_budget(
    height: f32,
    top_inset: f32,
    title_visible: bool,
    usage_lines: &[String],
) -> usize {
    let header_h = sidebar_header_height(title_visible);
    let usage_bars = parse_usage_bars(usage_lines);
    let footer_h = sidebar_footer_height(usage_bars.len());
    visible_sidebar_row_capacity(height, top_inset, header_h, footer_h)
}

fn visible_sidebar_row_capacity(
    height: f32,
    top_inset: f32,
    header_h: f32,
    footer_h: f32,
) -> usize {
    let list_top = top_inset + header_h;
    let list_bottom = (height - footer_h).max(list_top);
    ((list_bottom - list_top) / SIDEBAR_ROW_HEIGHT)
        .floor()
        .max(0.0) as usize
}

fn sidebar_hover_color(palette: ThemePalette) -> egui::Color32 {
    mix_color(palette.base, palette.text, 0.045)
}

fn sidebar_current_color(palette: ThemePalette) -> egui::Color32 {
    mix_color(palette.base, palette.text, 0.065)
}

fn sidebar_hovered_row(
    pos: Pos2,
    left: f32,
    top: f32,
    width: f32,
    max_rows: usize,
) -> Option<usize> {
    let list_rect = Rect::from_min_size(
        Pos2::new(left, top),
        egui::vec2(width, max_rows as f32 * SIDEBAR_ROW_HEIGHT),
    );
    if !list_rect.contains(pos) {
        return None;
    }
    let row = ((pos.y - top) / SIDEBAR_ROW_HEIGHT).floor() as usize;
    (row < max_rows).then_some(row)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SidebarBlock<'a> {
    anchor: &'a str,
    start_row: usize,
    end_row: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SidebarDragState {
    anchor: String,
    preview: String,
}

fn sidebar_drag_preview_labels<'a>(items: &'a [SidebarItem<'a>]) -> HashMap<&'a str, String> {
    let mut labels = HashMap::new();
    for item in items {
        let Some(anchor) = item.reorder_anchor else {
            continue;
        };
        labels
            .entry(anchor)
            .or_insert_with(|| sidebar_drag_label(item));
    }
    labels
}

fn sidebar_drag_label(item: &SidebarItem<'_>) -> String {
    match item.display {
        SidebarDisplay::Text(text) => text.to_owned(),
        SidebarDisplay::Numbered { label, .. } => label.to_owned(),
        SidebarDisplay::Progress(pct) => format!("{pct}%"),
    }
}

fn paint_sidebar_drag_preview(
    ui: &egui::Ui,
    pointer_pos: Option<Pos2>,
    preview: &str,
    palette: ThemePalette,
) {
    let Some(pointer_pos) = pointer_pos else {
        return;
    };
    let preview = truncate_label(preview, 24);
    let font = egui::FontId::monospace(13.0);
    let width = preview.chars().count() as f32 * 7.4 + 18.0;
    let rect = Rect::from_min_size(
        pointer_pos + egui::vec2(14.0, 14.0),
        egui::vec2(width.max(48.0), SIDEBAR_ROW_HEIGHT - 2.0),
    );
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("mux-sidebar-drag-preview"),
    ));
    painter.rect_filled(rect, 6.0, mix_color(palette.base, palette.text, 0.12));
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, palette.primary),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.left_center() + egui::vec2(9.0, 0.0),
        egui::Align2::LEFT_CENTER,
        preview,
        font,
        palette.text,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarDropTarget<'a> {
    Before(&'a str),
    End,
}

fn sidebar_drop_target<'a>(
    items: &'a [SidebarItem<'a>],
    pos: Option<Pos2>,
    left: f32,
    top: f32,
    width: f32,
    dragged_anchor: &str,
) -> Option<(SidebarDropTarget<'a>, f32)> {
    let pos = pos?;
    let row = sidebar_hovered_row(pos, left, top, width, items.len())?;
    let blocks = sidebar_blocks(items);
    let source_index = blocks
        .iter()
        .position(|block| block.anchor == dragged_anchor)?;
    let block_index = blocks
        .iter()
        .position(|block| block.start_row <= row && row <= block.end_row)?;
    let block = blocks[block_index];
    let block_top = top + block.start_row as f32 * SIDEBAR_ROW_HEIGHT;
    let block_bottom = top + (block.end_row + 1) as f32 * SIDEBAR_ROW_HEIGHT;
    let midpoint = (block_top + block_bottom) * 0.5;

    let (target, target_index, indicator_y) = if pos.y < midpoint {
        (
            SidebarDropTarget::Before(block.anchor),
            Some(block_index),
            block_top,
        )
    } else if let Some(next_block) = blocks.get(block_index + 1) {
        (
            SidebarDropTarget::Before(next_block.anchor),
            Some(block_index + 1),
            top + next_block.start_row as f32 * SIDEBAR_ROW_HEIGHT,
        )
    } else {
        (SidebarDropTarget::End, None, block_bottom)
    };

    if sidebar_drop_is_noop(source_index, target_index, blocks.len()) {
        return None;
    }

    Some((target, indicator_y))
}

fn sidebar_reorder_event(
    dragged: Option<&SidebarDragState>,
    drop: Option<(SidebarDropTarget<'_>, f32)>,
) -> Option<SidebarEvent> {
    let (drag, (drop_target, _)) = (dragged?, drop?);
    Some(SidebarEvent::Reorder {
        source: drag.anchor.clone(),
        before: match drop_target {
            SidebarDropTarget::Before(target) => Some(target.to_owned()),
            SidebarDropTarget::End => None,
        },
    })
}

fn sidebar_drop_is_noop(
    source_index: usize,
    target_index: Option<usize>,
    block_count: usize,
) -> bool {
    match target_index {
        Some(target_index) if source_index < target_index => source_index + 1 == target_index,
        Some(target_index) => source_index == target_index,
        None => source_index + 1 == block_count,
    }
}

fn sidebar_blocks<'a>(items: &'a [SidebarItem<'a>]) -> Vec<SidebarBlock<'a>> {
    let mut blocks: Vec<SidebarBlock<'a>> = Vec::new();
    for (row, item) in items.iter().enumerate() {
        let Some(anchor) = item.reorder_anchor else {
            continue;
        };
        if let Some(block) = blocks.last_mut()
            && block.anchor == anchor
        {
            block.end_row = row;
            continue;
        }
        blocks.push(SidebarBlock {
            anchor,
            start_row: row,
            end_row: row,
        });
    }
    blocks
}

fn subtle_border(palette: ThemePalette) -> egui::Color32 {
    mix_color(palette.base, palette.text, 0.09)
}

fn dim_overlay_color(amount: f32) -> egui::Color32 {
    let alpha = (amount.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_black_alpha(alpha)
}

fn mix_color(a: egui::Color32, b: egui::Color32, amount: f32) -> egui::Color32 {
    let amount = amount.clamp(0.0, 1.0);
    let inv = 1.0 - amount;
    egui::Color32::from_rgb(
        (f32::from(a.r()) * inv + f32::from(b.r()) * amount).round() as u8,
        (f32::from(a.g()) * inv + f32::from(b.g()) * amount).round() as u8,
        (f32::from(a.b()) * inv + f32::from(b.b()) * amount).round() as u8,
    )
}
pub fn load_app_icon_texture(
    ctx: &egui::Context,
    texture: &mut Option<TextureHandle>,
) -> TextureHandle {
    texture
        .get_or_insert_with(|| {
            ctx.load_texture(
                "bootty-app-icon",
                assets::title_icon_color_image(),
                egui::TextureOptions::LINEAR,
            )
        })
        .clone()
}

fn paint_sidebar_title(ui: &egui::Ui, rect: Rect, palette: ThemePalette, model: &SidebarModel<'_>) {
    let painter = ui.painter_at(rect);
    let layout = sidebar_title_layout(rect, model.reserve_titlebar_buttons);
    if let Some(icon) = model.title_icon {
        painter.image(
            icon.id(),
            layout.icon_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        painter.circle_filled(layout.icon_rect.center(), 8.0, palette.primary);
    }
    painter.text(
        layout.title_pos,
        egui::Align2::LEFT_CENTER,
        "Bootty",
        egui::FontId::proportional(15.0),
        palette.text,
    );
    painter.text(
        Pos2::new(rect.max.x - SIDEBAR_PAD_X, layout.title_pos.y),
        egui::Align2::RIGHT_CENTER,
        model.sessions.len().to_string(),
        egui::FontId::monospace(13.0),
        palette.muted,
    );
}

fn sidebar_header_height(title_visible: bool) -> f32 {
    if title_visible {
        SIDEBAR_HEADER_HEIGHT
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SidebarTitleLayout {
    icon_rect: Rect,
    title_pos: Pos2,
}

fn sidebar_title_layout(rect: Rect, reserve_titlebar_buttons: bool) -> SidebarTitleLayout {
    let (reserved, center_y) = if reserve_titlebar_buttons {
        (
            MACOS_TITLEBAR_BUTTON_SAFE_WIDTH,
            rect.min.y + MACOS_TITLEBAR_BUTTON_CENTER_Y,
        )
    } else {
        (0.0, rect.min.y + SIDEBAR_HEADER_HEIGHT * 0.5)
    };
    let icon_size = 18.0;
    let left = rect.min.x + SIDEBAR_PAD_X + reserved;
    let icon_rect = Rect::from_min_size(
        Pos2::new(left, center_y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    SidebarTitleLayout {
        icon_rect,
        title_pos: Pos2::new(icon_rect.max.x + 10.0, center_y),
    }
}

pub fn sidebar_rect(rect: Rect, chrome: &ChromeConfig) -> Rect {
    let width = if chrome.sidebar {
        chrome.sidebar_width
    } else {
        0.0
    };
    Rect::from_min_max(
        rect.min,
        Pos2::new((rect.min.x + width).min(rect.max.x), rect.max.y),
    )
}

pub fn selected_session_name<'a>(
    sessions: &'a [MuxSession],
    selected_session: Option<&str>,
) -> Option<&'a str> {
    let selected = selected_session?;
    sessions
        .iter()
        .find(|session| session.id == selected || session.name == selected)
        .map(|session| session.name.as_str())
}

fn sidebar_item_row(
    ui: &mut egui::Ui,
    rect: Rect,
    item: &SidebarItem<'_>,
    hovered_session: bool,
    palette: ThemePalette,
    hover_color: egui::Color32,
    current_color: egui::Color32,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("mux-sidebar-item", &item.id)),
        if item.reorder_anchor.is_some() {
            egui::Sense::click_and_drag()
        } else if item.session_id.is_some() || item.selectable {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let bg = if hovered_session {
            hover_color
        } else if item.current {
            current_color
        } else {
            palette.base
        };
        painter.rect_filled(rect, 0.0, bg);

        if item.current {
            let bar = Rect::from_min_max(rect.min, Pos2::new(rect.min.x + 4.0, rect.max.y));
            painter.rect_filled(bar, 0.0, item.color);
        }

        paint_tree_guide(&painter, rect, item);

        match &item.kind {
            SidebarItemKind::Group => paint_group_item(&painter, rect, item, palette),
            SidebarItemKind::Session {
                active,
                process,
                diff,
            } => paint_session_item(&painter, rect, item, *active, *process, *diff, palette),
            SidebarItemKind::Process {
                name,
                cpu_pct,
                mem_bytes,
            } => paint_process_item(&painter, rect, item, name, *cpu_pct, *mem_bytes, palette),
            SidebarItemKind::Agent { text } => {
                let active = is_agent_active(text);
                if active {
                    ui.ctx().request_repaint_after(Duration::from_millis(180));
                }
                let time = ui.input(|input| input.time);
                paint_agent_item(&painter, rect, item, text, active, time, palette)
            }
            SidebarItemKind::Branch { name } => {
                paint_detail_item(&painter, rect, item, "", name, palette)
            }
            SidebarItemKind::Status { text } => {
                paint_detail_item(&painter, rect, item, "status", text, palette)
            }
            SidebarItemKind::Progress { pct } => {
                paint_progress_item(&painter, rect, item, *pct, palette)
            }
        }
    }
    response
}
const SIDEBAR_INDENT_PX: f32 = 7.0;

fn item_text_x(rect: Rect, item: &SidebarItem<'_>) -> f32 {
    rect.min.x + 12.0 + f32::from(item.indent) * SIDEBAR_INDENT_PX
}

fn paint_tree_guide(painter: &egui::Painter, rect: Rect, item: &SidebarItem<'_>) {
    let x = rect.min.x + 15.5;
    let cy = rect.center().y;
    let stroke = Stroke::new(1.0, item.dim_color.gamma_multiply(0.8));
    match item.tree {
        SidebarTree::None | SidebarTree::Blank => {}
        SidebarTree::Middle => {
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], stroke);
            painter.line_segment([Pos2::new(x, cy), Pos2::new(x + 5.0, cy)], stroke);
        }
        SidebarTree::Last => {
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, cy)], stroke);
            painter.line_segment([Pos2::new(x, cy), Pos2::new(x + 5.0, cy)], stroke);
        }
        SidebarTree::Pipe => {
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], stroke);
        }
    }
}

fn paint_group_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    palette: ThemePalette,
) {
    let SidebarDisplay::Text(text) = item.display else {
        return;
    };
    painter.text(
        Pos2::new(item_text_x(rect, item), rect.center().y),
        egui::Align2::LEFT_CENTER,
        truncate_label(text, 28),
        egui::FontId::monospace(12.0),
        palette.border,
    );
}

fn paint_session_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    active: bool,
    process: Option<&str>,
    diff: Option<DiffStat>,
    palette: ThemePalette,
) {
    let label_color = if active { item.color } else { item.dim_color };
    let x = item_text_x(rect, item);
    let cy = rect.center().y;
    let (number, name) = match item.display {
        SidebarDisplay::Numbered { number, label } => (Some(number), label),
        SidebarDisplay::Text(text) => (None, text),
        SidebarDisplay::Progress(_) => (None, ""),
    };
    let mut text_x = x;
    if let Some(number) = number {
        let badge = Rect::from_center_size(Pos2::new(x + 7.0, cy), egui::vec2(14.0, 14.0));
        if active {
            painter.rect_filled(badge, 3.0, item.color);
        } else {
            painter.rect_stroke(
                badge,
                3.0,
                Stroke::new(1.0, item.dim_color),
                egui::StrokeKind::Inside,
            );
        }
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            (number % 100).to_string(),
            egui::FontId::monospace(10.0),
            if active { palette.base } else { item.dim_color },
        );
        text_x = badge.max.x + 6.0;
    }
    painter.text(
        Pos2::new(text_x, cy),
        egui::Align2::LEFT_CENTER,
        truncate_label(name, 20),
        egui::FontId::monospace(13.0),
        label_color,
    );

    if let Some(diff) = diff
        && (diff.added > 0 || diff.removed > 0)
    {
        paint_diff_stat(painter, rect, diff, palette);
    } else if let Some(process) = process {
        painter.text(
            Pos2::new(rect.max.x - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            truncate_label(process, 10),
            egui::FontId::monospace(12.0),
            palette.border,
        );
    }
}

fn paint_diff_stat(painter: &egui::Painter, rect: Rect, diff: DiffStat, palette: ThemePalette) {
    painter.text(
        Pos2::new(rect.max.x - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("-{}", diff.removed),
        egui::FontId::monospace(12.0),
        palette.destructive,
    );
    painter.text(
        Pos2::new(rect.max.x - 46.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("+{}", diff.added),
        egui::FontId::monospace(12.0),
        palette.success,
    );
}

fn paint_process_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    name: &str,
    cpu_pct: Option<f32>,
    mem_bytes: Option<u64>,
    palette: ThemePalette,
) {
    let color = process_color(name, palette);
    let x = item_text_x(rect, item);
    let cy = rect.center().y;
    paint_icon(
        painter,
        process_icon(name),
        Pos2::new(x + 6.0, cy),
        12.0,
        color,
    );
    let mut label = String::new();
    push_truncated_label(&mut label, name, 16);
    painter.text(
        Pos2::new(x + 16.0, cy),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(11.0),
        color,
    );

    let mut metrics = String::new();
    if let Some(cpu_pct) = cpu_pct {
        let _ = write!(metrics, "{cpu_pct:.1}%");
    }
    if let Some(mem_bytes) = mem_bytes.filter(|bytes| *bytes > 0) {
        if !metrics.is_empty() {
            metrics.push(' ');
        }
        push_formatted_bytes(&mut metrics, mem_bytes);
    }
    if !metrics.is_empty() {
        painter.text(
            Pos2::new(rect.max.x - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            metrics,
            egui::FontId::monospace(11.0),
            palette.border,
        );
    }
}

fn process_color(name: &str, palette: ThemePalette) -> egui::Color32 {
    match name {
        "node" | "bun" | "deno" => palette.success,
        "nvim" | "vim" => palette.accent,
        "fish" | "zsh" | "bash" | "sh" => palette.subtext,
        "cargo" | "rustc" | "rust-analyzer" | "python" | "python3" => palette.warning,
        "git" => palette.destructive,
        _ => palette.muted,
    }
}

fn process_icon(name: &str) -> Icon {
    match name {
        "nvim" | "vim" => Icon::Editor,
        "git" => Icon::GitBranch,
        "node" | "bun" | "deno" | "cargo" | "rustc" | "rust-analyzer" | "python" | "python3" => {
            Icon::Package
        }
        _ => Icon::Terminal,
    }
}

fn push_formatted_bytes(out: &mut String, bytes: u64) {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        let _ = write!(out, "{:.1}g", bytes as f64 / GIB as f64);
    } else {
        let _ = write!(out, "{}m", bytes.div_ceil(MIB));
    }
}

fn paint_agent_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    text: &str,
    active: bool,
    time: f64,
    palette: ThemePalette,
) {
    let (name, detail) = text.split_once(' ').unwrap_or((text, ""));
    let color = agent_color(name, palette);
    let x = item_text_x(rect, item);
    let cy = rect.center().y;
    let icon = if name == "claude" {
        Icon::Sparkles
    } else {
        Icon::Bot
    };
    paint_icon(painter, icon, Pos2::new(x + 6.0, cy), 12.0, color);
    painter.text(
        Pos2::new(x + 16.0, cy),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::monospace(11.0),
        color,
    );
    painter.text(
        Pos2::new(rect.max.x - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        agent_detail_label(detail, active, time),
        egui::FontId::monospace(11.0),
        if detail.contains("asking") {
            palette.warning
        } else {
            palette.subtext
        },
    );
}

fn is_agent_active(text: &str) -> bool {
    text.contains('…') || text.contains("Working")
}

fn agent_detail_label(detail: &str, active: bool, time: f64) -> String {
    let pulse_len = if active {
        ((time * 5.0) as usize % 4) + 1
    } else {
        0
    };
    let mut label = String::with_capacity(detail.len().min(AGENT_DETAIL_MAX_CHARS * 4) + pulse_len);
    let mut chars = detail.chars().chain(std::iter::repeat_n('.', pulse_len));
    for _ in 0..AGENT_DETAIL_MAX_CHARS {
        let Some(ch) = chars.next() else {
            return label;
        };
        label.push(ch);
    }

    if chars.next().is_some() {
        label.pop();
        label.push('…');
    }
    label
}

fn agent_color(name: &str, palette: ThemePalette) -> egui::Color32 {
    match name {
        "claude" => palette.warning,
        "codex" => egui::Color32::from_rgb(0x74, 0xc7, 0xec),
        "opencode" => egui::Color32::from_rgb(0x9a, 0x8f, 0xbf),
        _ => palette.subtext,
    }
}

fn paint_detail_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    kind: &str,
    text: &str,
    palette: ThemePalette,
) {
    let x = item_text_x(rect, item);
    let cy = rect.center().y;
    let mut display = String::new();
    let text_x = if kind.is_empty() {
        paint_icon(
            painter,
            Icon::GitBranch,
            Pos2::new(x + 6.0, cy),
            11.0,
            palette.muted,
        );
        push_truncated_label(&mut display, text, 24);
        x + 16.0
    } else {
        display.push_str(kind);
        display.push(' ');
        push_truncated_label(&mut display, text, 22);
        x
    };
    painter.text(
        Pos2::new(text_x, cy),
        egui::Align2::LEFT_CENTER,
        display,
        egui::FontId::monospace(11.0),
        palette.muted,
    );
}

fn paint_progress_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    pct: u8,
    palette: ThemePalette,
) {
    let x = item_text_x(rect, item);
    let cy = rect.center().y;
    let color = if pct >= 100 {
        palette.success
    } else {
        item.dim_color
    };
    let track = Rect::from_min_size(
        Pos2::new(x, cy - 2.0),
        egui::vec2((rect.max.x - 52.0 - x).max(20.0), 4.0),
    );
    painter.rect_filled(track, 2.0, palette.surface);
    let fill_w = track.width() * f32::from(pct.min(100)) / 100.0;
    if fill_w > 0.0 {
        painter.rect_filled(
            Rect::from_min_size(track.min, egui::vec2(fill_w, 4.0)),
            2.0,
            color,
        );
    }
    painter.text(
        Pos2::new(track.max.x + 8.0, cy),
        egui::Align2::LEFT_CENTER,
        format!("{pct}%"),
        egui::FontId::monospace(11.0),
        color,
    );
}

fn sidebar_footer_height(usage_bar_count: usize) -> f32 {
    let usage_count = usage_bar_count.min(SIDEBAR_MAX_USAGE_BARS);
    SIDEBAR_FOOTER_BASE_HEIGHT + usage_count as f32 * 30.0
}

fn paint_sidebar_footer(
    ui: &egui::Ui,
    rect: Rect,
    footer_h: f32,
    usage_bars: &UsageBars,
    separator_visible: bool,
    palette: ThemePalette,
    border_color: egui::Color32,
) {
    let painter = ui.painter_at(rect);
    let y = rect.max.y - footer_h;
    if separator_visible {
        painter.line_segment(
            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
            Stroke::new(1.0, border_color),
        );
    }

    let mut row_y = y + 18.0;
    for bar in usage_bars.iter() {
        paint_usage_bar(
            &painter,
            Rect::from_min_size(
                Pos2::new(rect.min.x + 14.0, row_y - 10.0),
                egui::vec2(rect.width() - 28.0, 26.0),
            ),
            bar,
            palette,
        );
        row_y += 30.0;
    }

    painter.text(
        Pos2::new(rect.min.x + 14.0, rect.max.y - 18.0),
        egui::Align2::LEFT_CENTER,
        crate::platform::sidebar_shortcut_hint(),
        egui::FontId::monospace(11.0),
        palette.muted,
    );
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct UsageMarker {
    pos: f32,
    color: egui::Color32,
}

#[derive(Clone, Debug)]
struct UsageBar<'a> {
    label: Cow<'a, str>,
    pct: u8,
    pace: Cow<'a, str>,
    marker: Option<UsageMarker>,
    color: egui::Color32,
}

#[derive(Clone, Debug)]
struct UsageBars<'a> {
    bars: [Option<UsageBar<'a>>; SIDEBAR_MAX_USAGE_BARS],
    len: usize,
}

impl<'a> UsageBars<'a> {
    fn new() -> Self {
        Self {
            bars: std::array::from_fn(|_| None),
            len: 0,
        }
    }

    fn push(&mut self, bar: UsageBar<'a>) -> bool {
        if self.len >= SIDEBAR_MAX_USAGE_BARS {
            return false;
        }
        self.bars[self.len] = Some(bar);
        self.len += 1;
        true
    }

    fn len(&self) -> usize {
        self.len
    }

    fn iter(&self) -> impl Iterator<Item = &UsageBar<'a>> {
        self.bars[..self.len]
            .iter()
            .filter_map(std::option::Option::as_ref)
    }
}

impl<'a> std::ops::Index<usize> for UsageBars<'a> {
    type Output = UsageBar<'a>;

    fn index(&self, index: usize) -> &Self::Output {
        self.bars[index].as_ref().expect("usage bar index in range")
    }
}
fn paint_usage_bar(painter: &egui::Painter, rect: Rect, bar: &UsageBar<'_>, palette: ThemePalette) {
    painter.text(
        Pos2::new(rect.min.x, rect.min.y + 6.0),
        egui::Align2::LEFT_CENTER,
        truncate_label(&bar.label, 22),
        egui::FontId::monospace(11.0),
        palette.subtext,
    );
    paint_usage_right_text(painter, rect, bar, palette);

    let track = Rect::from_min_size(
        Pos2::new(rect.min.x, rect.min.y + 17.0),
        egui::vec2(rect.width(), 4.0),
    );
    let fill_x = track.left() + track.width() * f32::from(bar.pct) / 100.0;
    painter.rect_filled(track, 2.0, palette.surface);
    painter.line_segment(
        [
            Pos2::new(track.left(), track.center().y),
            Pos2::new(fill_x, track.center().y),
        ],
        Stroke::new(2.0, bar.color),
    );
    if let Some(marker) = bar.marker {
        let marker_x = track.left() + track.width() * marker.pos;
        painter.line_segment(
            [
                Pos2::new(marker_x, track.top() - 3.0),
                Pos2::new(marker_x, track.bottom() + 3.0),
            ],
            Stroke::new(2.0, marker.color),
        );
    }
}

fn paint_usage_right_text(
    painter: &egui::Painter,
    rect: Rect,
    bar: &UsageBar<'_>,
    palette: ThemePalette,
) {
    if bar.pace.is_empty() {
        painter.text(
            Pos2::new(rect.max.x, rect.min.y + 6.0),
            egui::Align2::RIGHT_CENTER,
            format!("{}%", bar.pct),
            egui::FontId::monospace(11.0),
            bar.color,
        );
        return;
    }

    painter.text(
        Pos2::new(rect.max.x, rect.min.y + 6.0),
        egui::Align2::RIGHT_CENTER,
        &bar.pace,
        egui::FontId::monospace(11.0),
        palette.muted,
    );
    let pace_width = bar.pace.chars().count() as f32 * 7.0 + 8.0;
    painter.text(
        Pos2::new(rect.max.x - pace_width, rect.min.y + 6.0),
        egui::Align2::RIGHT_CENTER,
        format!("{}%", bar.pct),
        egui::FontId::monospace(11.0),
        bar.color,
    );
}
fn parse_usage_bars(lines: &[String]) -> UsageBars<'_> {
    let mut bars = UsageBars::new();
    let mut index = 0;
    while index < lines.len() {
        let line = &lines[index];
        index += 1;
        let (pct, label, pace) = match strip_ansi(line) {
            Cow::Borrowed(text) => {
                let Some(pct) = parse_percent(text) else {
                    continue;
                };
                (pct, usage_label(text, true), usage_pace(text, true))
            }
            Cow::Owned(text) => {
                let Some(pct) = parse_percent(&text) else {
                    continue;
                };
                (
                    pct,
                    Cow::Owned(usage_label(&text, true).into_owned()),
                    Cow::Owned(usage_pace(&text, true).into_owned()),
                )
            }
        };
        // A label line may be followed by its painted bar line, which carries
        // the pace marker (`│`) position and over/under-pace color.
        let marker = match lines.get(index) {
            Some(next) if is_bar_line(next) => {
                index += 1;
                bar_line_marker(next)
            }
            _ => None,
        };
        if !bars.push(UsageBar {
            label: if label.is_empty() {
                Cow::Borrowed("usage")
            } else {
                label
            },
            pct,
            pace,
            marker,
            color: first_ansi_color(line).unwrap_or(egui::Color32::from_rgb(0x89, 0xb4, 0xfa)),
        }) {
            break;
        }
    }
    bars
}

fn usage_label<'a>(text: &'a str, borrow_single_part: bool) -> Cow<'a, str> {
    let mut label = None;
    for part in text
        .split_whitespace()
        .filter(|part| !part.contains('%'))
        .filter(|part| !part.starts_with('+') && !part.contains(':') && !part.contains('↺'))
        .filter(|part| part.chars().any(|ch| ch.is_ascii_alphanumeric()))
        .take(2)
    {
        let Some(first) = label else {
            label = Some(Cow::Borrowed(part));
            continue;
        };
        let mut joined = String::with_capacity(first.len() + 1 + part.len());
        joined.push_str(&first);
        joined.push(' ');
        joined.push_str(part);
        return Cow::Owned(joined);
    }
    match label {
        Some(label) if borrow_single_part => label,
        Some(label) => Cow::Owned(label.into_owned()),
        None => Cow::Borrowed(""),
    }
}

fn usage_pace<'a>(text: &'a str, borrow_single_part: bool) -> Cow<'a, str> {
    let mut seen_pct = false;
    let mut first = None;
    let mut joined = None::<String>;
    let mut count = 0usize;
    for part in text.split_whitespace() {
        if part.ends_with('%') {
            seen_pct = true;
            continue;
        }
        if !seen_pct || !(part.starts_with('+') || part.contains(':') || part.contains('↺')) {
            continue;
        }
        count += 1;
        if count == 1 {
            first = Some(part);
        } else {
            let joined = joined.get_or_insert_with(|| {
                let first = first.unwrap_or_default();
                let mut joined = String::with_capacity(first.len() + 1 + part.len());
                joined.push_str(first);
                joined
            });
            joined.push(' ');
            joined.push_str(part);
        }
        if count == 3 {
            break;
        }
    }
    match (joined, first) {
        (Some(pace), _) => Cow::Owned(pace),
        (None, Some(pace)) if borrow_single_part => Cow::Borrowed(pace),
        (None, Some(pace)) => Cow::Owned(pace.to_owned()),
        (None, None) => Cow::Borrowed(""),
    }
}

fn is_bar_line(line: &str) -> bool {
    line.contains(['█', '▓', '▒', '░']) && parse_percent(&strip_ansi(line)).is_none()
}

/// Extract the pace marker (`│` cell) from a painted usage bar line:
/// fractional position across the bar cells and its ANSI color.
fn bar_line_marker(line: &str) -> Option<UsageMarker> {
    let mut color = None;
    let mut cells = 0usize;
    let mut marker = None;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut code = String::new();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    if c == 'm' {
                        color = sgr_color(&code);
                    }
                    break;
                }
                code.push(c);
            }
        } else if matches!(ch, '█' | '▓' | '▒' | '░') {
            cells += 1;
        } else if ch == '│' {
            marker = Some((cells, color));
            cells += 1;
        }
    }
    let (index, marker_color) = marker?;
    (cells > 1).then(|| UsageMarker {
        pos: (index as f32 + 0.5) / cells as f32,
        color: marker_color.unwrap_or(egui::Color32::from_rgb(0xa6, 0xe3, 0xa1)),
    })
}

fn parse_percent(text: &str) -> Option<u8> {
    text.split_whitespace().find_map(|part| {
        part.strip_suffix('%')
            .and_then(|value| value.parse::<u8>().ok())
            .map(|pct| pct.min(100))
    })
}
fn strip_ansi(line: &str) -> Cow<'_, str> {
    if !line.contains('\x1b') && !line.chars().any(|ch| matches!(ch, '█' | '░' | '▒' | '▓'))
    {
        return Cow::Borrowed(line.trim());
    }

    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if !matches!(ch, '█' | '░' | '▒' | '▓') {
            out.push(ch);
        }
    }
    Cow::Owned(out.trim().to_owned())
}

fn first_ansi_color(line: &str) -> Option<egui::Color32> {
    if !line.contains('\x1b') {
        return None;
    }

    let mut offset = 0;
    while let Some(start) = line[offset..].find("\x1b[") {
        let code_start = offset + start + 2;
        let code = &line[code_start..];
        let Some(command_offset) = code.bytes().position(|byte| byte.is_ascii_alphabetic()) else {
            break;
        };
        let command = code.as_bytes()[command_offset];
        if command == b'm' {
            let code = &code[..command_offset];
            if let Some(color) = sgr_color(code) {
                return Some(color);
            }
        }
        offset = code_start + command_offset + 1;
    }
    None
}

fn sgr_color(code: &str) -> Option<egui::Color32> {
    if code == "0" || code == "39" {
        return None;
    }

    let mut parts = code.split(';');
    if parts.next()? != "38" || parts.next()? != "2" {
        return None;
    }
    let r = parts.next()?.parse::<u8>().ok()?;
    let g = parts.next()?.parse::<u8>().ok()?;
    let b = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(egui::Color32::from_rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_rect_uses_configured_width_and_can_be_disabled() {
        let rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(500.0, 300.0));
        let mut chrome = ChromeConfig {
            sidebar_width: 240.0,
            ..Default::default()
        };

        assert_eq!(sidebar_rect(rect, &chrome).width(), 240.0);

        chrome.sidebar = false;
        assert_eq!(sidebar_rect(rect, &chrome).width(), 0.0);
    }

    #[test]
    fn usage_lines_parse_to_native_bar_data() {
        let lines = vec![
            "\x1b[38;2;116;199;236m  5h  90% +38m\x1b[0m".to_owned(),
            "\x1b[38;2;116;199;236m████████░░\x1b[0m".to_owned(),
        ];

        let bars = parse_usage_bars(&lines);

        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].pct, 90);
        assert!(bars[0].label.contains("5h"));
        assert_eq!(bars[0].label, "5h");
        assert_eq!(bars[0].pace, "+38m");
        assert_eq!(bars[0].color, egui::Color32::from_rgb(116, 199, 236));
        assert_eq!(bars[0].marker, None);
    }

    #[test]
    fn usage_lines_parse_only_painted_bars() {
        let lines = vec![
            "not a usage line".to_owned(),
            "first 10% +1m".to_owned(),
            "second 20% +2m".to_owned(),
            "third 30% +3m".to_owned(),
            "fourth 40% +4m".to_owned(),
        ];

        let bars = parse_usage_bars(&lines);

        assert_eq!(bars.len(), SIDEBAR_MAX_USAGE_BARS);
        assert_eq!(bars[0].label, "first");
        assert_eq!(bars[1].label, "second");
        assert_eq!(bars[2].label, "third");
        assert_eq!(bars[3].label, "fourth");
    }

    #[test]
    fn usage_marker_comes_from_bar_line_position_and_color() {
        let cell = |color: &str, ch: &str| format!("\x1b[38;2;{color}m{ch}\x1b[39m");
        let mut bar_line = String::new();
        bar_line.push_str(&cell("232;150;103", "\u{2593}"));
        bar_line.push_str(&cell("232;150;103", "\u{2593}"));
        bar_line.push_str(&cell("58;61;78", "\u{2591}"));
        bar_line.push_str(&cell("239;68;68", "\u{2502}"));
        for _ in 0..6 {
            bar_line.push_str(&cell("58;61;78", "\u{2591}"));
        }
        let lines = vec![
            "\x1b[38;2;116;199;236m 5h 78% +3h03 \u{21ba}50m\x1b[0m".to_owned(),
            bar_line,
        ];

        let bars = parse_usage_bars(&lines);

        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].pace, "+3h03 \u{21ba}50m");
        let marker = bars[0].marker.unwrap();
        assert!((marker.pos - 0.35).abs() < 0.001);
        assert_eq!(marker.color, egui::Color32::from_rgb(239, 68, 68));
    }

    #[test]
    fn bar_line_without_marker_yields_no_marker() {
        let lines = vec![
            "\x1b[38;2;116;199;236m 5h 90% +38m\x1b[0m".to_owned(),
            "\x1b[38;2;116;199;236m\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\x1b[0m".to_owned(),
            "\x1b[38;2;116;199;236m 7d 10% +1h\x1b[0m".to_owned(),
        ];

        let bars = parse_usage_bars(&lines);

        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].marker, None);
        assert_eq!(bars[1].label, "7d");
    }

    #[test]
    fn agent_detail_label_formats_active_pulse_without_intermediate_text() {
        assert_eq!(agent_detail_label("Working", false, 0.0), "Working");
        assert_eq!(agent_detail_label("Working", true, 0.0), "Working.");
        assert_eq!(agent_detail_label("Working", true, 0.6), "Working....");
        assert_eq!(
            agent_detail_label("abcdefghijklmnop", true, 0.6),
            "abcdefghijklmnop.…"
        );
        assert_eq!(
            agent_detail_label("abcdefghijklmnopqrst", true, 0.0),
            "abcdefghijklmnopq…"
        );
    }

    #[test]
    fn sidebar_title_layout_reserves_macos_titlebar_button_area() {
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(286.0, 200.0));

        let normal = sidebar_title_layout(rect, false);
        let reserved = sidebar_title_layout(rect, true);

        assert_eq!(normal.icon_rect.min.x, SIDEBAR_PAD_X);
        assert_eq!(
            reserved.icon_rect.min.x,
            SIDEBAR_PAD_X + MACOS_TITLEBAR_BUTTON_SAFE_WIDTH
        );
        assert_eq!(normal.title_pos.y, SIDEBAR_HEADER_HEIGHT * 0.5);
        assert_eq!(reserved.title_pos.y, MACOS_TITLEBAR_BUTTON_CENTER_Y);
        assert!(reserved.title_pos.x > reserved.icon_rect.max.x);
    }

    #[test]
    fn sidebar_title_drag_rect_reserves_macos_titlebar_button_area() {
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(286.0, SIDEBAR_HEADER_HEIGHT));

        assert_eq!(sidebar_title_drag_rect(rect, false), rect);
        assert_eq!(
            sidebar_title_drag_rect(rect, true).min.x,
            MACOS_TITLEBAR_BUTTON_SAFE_WIDTH
        );
    }

    #[test]
    fn sidebar_hovered_row_maps_pointer_to_visible_rows() {
        assert_eq!(
            sidebar_hovered_row(Pos2::new(20.0, 10.0), 10.0, 10.0, 100.0, 3),
            Some(0)
        );
        assert_eq!(
            sidebar_hovered_row(
                Pos2::new(20.0, 10.0 + SIDEBAR_ROW_HEIGHT * 2.0 + 1.0),
                10.0,
                10.0,
                100.0,
                3
            ),
            Some(2)
        );
        assert_eq!(
            sidebar_hovered_row(
                Pos2::new(20.0, 10.0 + SIDEBAR_ROW_HEIGHT * 3.0),
                10.0,
                10.0,
                100.0,
                3
            ),
            None
        );
        assert_eq!(
            sidebar_hovered_row(Pos2::new(9.0, 10.0), 10.0, 10.0, 100.0, 3),
            None
        );
    }

    #[test]
    fn sidebar_header_collapses_when_title_is_hidden() {
        assert_eq!(sidebar_header_height(true), SIDEBAR_HEADER_HEIGHT);
        assert_eq!(sidebar_header_height(false), 0.0);
    }

    #[test]
    fn status_segments_width_ignores_empty_segments() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 300.0));
        let empty = ResolvedSegment {
            align: SegmentAlign::Left,
            items: Vec::new(),
        };
        let non_empty = ResolvedSegment {
            align: SegmentAlign::Left,
            items: vec![ResolvedItem {
                text: "1".to_owned(),
                ..ResolvedItem::default()
            }],
        };
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| {
                let font = egui::FontId::monospace(12.0);
                let with_empty = segments_width(ui, &[&empty, &non_empty], &font);
                let without_empty = segments_width(ui, &[&non_empty], &font);
                assert_eq!(with_empty, without_empty);
            },
        );
        assert!(output.viewport_output.contains_key(&egui::ViewportId::ROOT));
    }

    #[test]
    fn status_bar_primary_press_starts_window_drag() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 300.0));
        let show = |ui: &mut egui::Ui| {
            show_status_bar(
                ui,
                ThemePalette::default(),
                StatusBarModel {
                    segments: &[],
                    background: ThemePalette::default().base,
                    left_padding: STATUS_EDGE_PAD,
                },
            );
        };

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerMoved(Pos2::new(20.0, 15.0))],
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| show(ui));
            },
        );

        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: Pos2::new(20.0, 15.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| show(ui));
            },
        );

        let root_output = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("root viewport output");
        assert!(
            root_output
                .commands
                .contains(&egui::ViewportCommand::StartDrag)
        );
    }

    #[test]
    fn status_action_primary_press_does_not_start_window_drag() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 300.0));
        let segments = [ResolvedSegment {
            align: SegmentAlign::Left,
            items: vec![ResolvedItem {
                text: "wake".to_owned(),
                action: Some("toggle-caffeinate".to_owned()),
                ..ResolvedItem::default()
            }],
        }];
        let show = |ui: &mut egui::Ui| {
            show_status_bar(
                ui,
                ThemePalette::default(),
                StatusBarModel {
                    segments: &segments,
                    background: ThemePalette::default().base,
                    left_padding: STATUS_EDGE_PAD,
                },
            );
        };

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerMoved(Pos2::new(20.0, 15.0))],
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| show(ui));
            },
        );

        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: Pos2::new(20.0, 15.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| show(ui));
            },
        );

        let root_output = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("root viewport output");
        assert!(
            !root_output
                .commands
                .contains(&egui::ViewportCommand::StartDrag)
        );
    }

    fn test_session(id: &str, name: &str, active: bool) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active,
            anchor: crate::mux::snapshot::MuxPaneAnchor {
                session_id: id.to_owned(),
                ..Default::default()
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    fn show_test_sidebar(ui: &mut egui::Ui, sessions: &[MuxSession]) -> Option<SidebarEvent> {
        show_sidebar(
            ui,
            ThemePalette::default(),
            300.0,
            SidebarModel {
                sessions,
                selected_session: Some("s1"),
                metadata: &SidebarMetadata::default(),
                title_visible: false,
                reserve_titlebar_buttons: false,
                title_icon: None,
                top_inset: 0.0,
                border_visible: false,
                separator_visible: false,
                focused: false,
                hovered_session: None,
                unfocused_dim: 0.0,
                hover_override: None,
                current_override: None,
                border_override: None,
            },
        )
    }

    #[test]
    fn sidebar_rows_ignore_keyboard_activation_keys() {
        for key in [egui::Key::Enter, egui::Key::Space] {
            let context = egui::Context::default();
            let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(286.0, 300.0));
            let sessions = vec![
                test_session("s1", "alpha", true),
                test_session("s2", "beta", false),
            ];

            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        let item_id = crate::ui::sidebar::SidebarItemId::Session("s2");
                        let id = ui.make_persistent_id(("mux-sidebar-item", &item_id));
                        ui.memory_mut(|memory| memory.request_focus(id));
                        show_test_sidebar(ui, &sessions);
                    });
                },
            );

            let mut event = None;
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        event = show_test_sidebar(ui, &sessions);
                    });
                },
            );

            assert_eq!(event, None);
        }
    }

    #[test]
    fn sidebar_drop_target_uses_group_midpoint_for_after_group_moves() {
        let sessions = vec![
            test_session("s1", "agents", true),
            test_session("s2", "arc/migrations", false),
            test_session("s3", "arc/readiness", false),
            test_session("s4", "bootty", false),
        ];
        let metadata = SidebarMetadata::default();
        let items = build_visible_sidebar_items(&sessions, Some("s1"), &metadata, 32);

        assert_eq!(
            sidebar_drop_target(
                &items,
                Some(Pos2::new(20.0, SIDEBAR_ROW_HEIGHT * 3.5)),
                0.0,
                0.0,
                240.0,
                "agents",
            ),
            Some((
                SidebarDropTarget::Before("bootty"),
                SIDEBAR_ROW_HEIGHT * 4.0,
            ))
        );
    }

    #[test]
    fn sidebar_reorder_event_commits_drop_target() {
        let drag = SidebarDragState {
            anchor: String::from("agents"),
            preview: String::from("agents"),
        };

        assert_eq!(
            sidebar_reorder_event(
                Some(&drag),
                Some((
                    SidebarDropTarget::Before("bootty"),
                    SIDEBAR_ROW_HEIGHT * 4.0
                )),
            ),
            Some(SidebarEvent::Reorder {
                source: String::from("agents"),
                before: Some(String::from("bootty")),
            })
        );
        assert_eq!(sidebar_reorder_event(Some(&drag), None), None);
        assert_eq!(
            sidebar_reorder_event(Some(&drag), Some((SidebarDropTarget::End, 0.0))),
            Some(SidebarEvent::Reorder {
                source: String::from("agents"),
                before: None,
            })
        );
    }

    #[test]
    fn sidebar_metadata_budget_tracks_visible_row_capacity() {
        let plain = sidebar_metadata_session_budget(900.0, 0.0, true, &[]);
        let with_usage = sidebar_metadata_session_budget(
            900.0,
            0.0,
            true,
            &[
                "terminal 5h 90% +38m".to_owned(),
                "agent 7d 73% +1d06:20".to_owned(),
            ],
        );

        assert!(with_usage < plain);
        assert_eq!(plain, 33);
    }
}
