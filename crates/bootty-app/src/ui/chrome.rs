use std::ops::Range;

use bootty_ui::{ThemePalette, readable_color};
use eframe::egui::{self, CornerRadius, Pos2, Rect, Stroke, StrokeKind};

use crate::{
    config::SegmentAlign,
    extensions::{ModuleCoord, ModulePrimitive},
    ui::icons::{has_slug, paint_icon_slug},
};

const STATUS_WINDOWS_MODULE: &str = "windows";

#[derive(Clone, Debug)]
pub struct StatusBarModel<'a> {
    /// Ordered, resolved segments. Every segment is a Luau module's items; the app fills these in.
    pub segments: &'a [ResolvedSegment],
    /// Bar fill; set to the sidebar fullscreen background when the bar sits in the notch band.
    pub background: egui::Color32,
    /// Left edge inset before left-aligned segments; zero lets tab strips sit flush to adjacent chrome.
    pub left_padding: f32,
    /// Height of the drawable status row. When the allocated bar is taller, items are bottom-aligned
    /// so extra notch-clearance space appears above them instead of stretching the row.
    pub row_height: f32,
    /// Active fullscreen notch x-range in the same coordinate space as the status bar.
    pub notch_x: Option<Range<f32>>,
    /// Number of tab rows to reserve for the windows segment. Other status modules stay on the
    /// bottom row.
    pub tab_rows: usize,
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
    /// Drag-to-reorder anchor; contiguous items sharing one anchor form a draggable block.
    pub reorder_anchor: Option<String>,
    /// The module that produced this item, so a reorder routes back to its `on_reorder`.
    pub module: String,
}

mod sidebar_panel;

pub(crate) use sidebar_panel::MACOS_TITLEBAR_BUTTON_SAFE_WIDTH;
pub use sidebar_panel::{
    SidebarEvent, SidebarModel, load_app_icon_texture, selected_session_name, show_sidebar,
    sidebar_rect,
};

/// The outcome of a status-bar frame: an item was clicked, or a draggable item was reordered
/// (routed to `module`'s `on_reorder`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusBarEvent {
    Action(String),
    Reorder {
        module: String,
        source: String,
        before: Option<String>,
    },
}

pub const STATUS_EDGE_PAD: f32 = 12.0;
const STATUS_ITEM_GAP: f32 = 4.0;
const STATUS_ITEM_PAD: f32 = 10.0;
const STATUS_ICON_GAP: f32 = 6.0;
/// Square edge of a status-bar icon glyph, matched to the 12pt text.
const STATUS_ICON_SIZE: f32 = 14.0;
/// Battery meter dimensions for a `gauge` item (body width excludes the nub).
const STATUS_GAUGE_WIDTH: f32 = 22.0;
const STATUS_GAUGE_HEIGHT: f32 = 11.0;
/// Corner radius (logical px) for status-bar pills and the strip's outer ends.
const STATUS_PILL_RADIUS: u8 = 6;
const STATUS_DIAGONAL_JOIN_WIDTH: f32 = 8.0;

pub fn status_bar_windows_intersect_x_range(
    ui: &egui::Ui,
    bar_rect: Rect,
    segments: &[ResolvedSegment],
    left_padding: f32,
    x_range: (f32, f32),
) -> bool {
    let font = egui::FontId::monospace(12.0);
    let right = segments
        .iter()
        .filter(|segment| segment.align == SegmentAlign::Right && !segment.items.is_empty())
        .collect::<Vec<_>>();
    let right_start = bar_rect.max.x - STATUS_EDGE_PAD - segments_width(ui, &right, &font);
    let bound = right_start - STATUS_ITEM_GAP;
    let mut x = bar_rect.min.x + left_padding;
    let mut drawn = 0;

    for segment in segments
        .iter()
        .filter(|segment| segment.align == SegmentAlign::Left && !segment.items.is_empty())
    {
        let width = segment_width(ui, segment, &font);
        if width <= 0.0 {
            continue;
        }
        if drawn > 0 {
            x += STATUS_ITEM_GAP;
        }
        let window_segment = segment_contains_module(segment, STATUS_WINDOWS_MODULE);
        let visible_end = (x + width).min(bound);
        if window_segment && ranges_intersect((x, visible_end), x_range) {
            return true;
        }
        if x + width > bound {
            break;
        }
        drawn += 1;
        x += width;
    }

    false
}

fn ranges_intersect(a: (f32, f32), b: (f32, f32)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

pub fn status_bar_window_tab_row_count(
    ui: &egui::Ui,
    bar_rect: Rect,
    segments: &[ResolvedSegment],
    left_padding: f32,
    notch_x: Option<(f32, f32)>,
) -> usize {
    let font = egui::FontId::monospace(12.0);
    let right = segments
        .iter()
        .filter(|segment| segment.align == SegmentAlign::Right && !segment.items.is_empty())
        .collect::<Vec<_>>();
    let left = segments
        .iter()
        .filter(|segment| segment.align == SegmentAlign::Left && !segment.items.is_empty())
        .collect::<Vec<_>>();
    let bottom_bound =
        bar_rect.max.x - STATUS_EDGE_PAD - segments_width(ui, &right, &font) - STATUS_ITEM_GAP;
    let top_bound = notch_x
        .map(|(left, _)| left - STATUS_ITEM_GAP)
        .unwrap_or(bar_rect.max.x - STATUS_EDGE_PAD);
    let notch_collision = notch_x.is_some_and(|range| {
        status_bar_windows_intersect_x_range(ui, bar_rect, segments, left_padding, range)
    });

    let mut row_count = if notch_collision { 2 } else { 1 };
    loop {
        let bounds = status_tab_row_bounds(bar_rect, row_count, top_bound, bottom_bound);
        let rows = split_left_segments_for_tab_rows(
            ui,
            &left,
            &font,
            bar_rect.min.x + left_padding,
            &bounds,
            false,
        );
        if rows.len() <= row_count || row_count >= status_window_group_count(&left) {
            return row_count.max(1);
        }
        row_count += 1;
    }
}

/// Native replacement for the tmux status line. Flattens each alignment group's module items and
/// lays them out: left from the left edge, right anchored to the right edge, center centered. Items
/// with a `bg` render as pills; items with an `action` are clickable. Returns a clicked action.
pub fn show_status_bar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    model: StatusBarModel<'_>,
) -> Option<StatusBarEvent> {
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

    let drag_id = egui::Id::new("status-bar-drag");
    let mut dragging = ui
        .ctx()
        .data_mut(|data| data.get_persisted::<StatusDragState>(drag_id));
    let primary_press_pos = ui.input(|input| {
        input
            .pointer
            .button_pressed(egui::PointerButton::Primary)
            .then(|| input.pointer.interact_pos())
            .flatten()
    });
    let primary_down = ui.input(|input| input.pointer.primary_down());
    let pointer_pos = ui.input(|input| {
        input
            .pointer
            .latest_pos()
            .or_else(|| input.pointer.hover_pos())
    });
    let mut input = StatusInput {
        rect,
        palette,
        font: font.clone(),
        clicked: None,
        primary_press_pos,
        drag_blocked: false,
        suppress_click: dragging.is_some(),
        started: None,
        blocks: Vec::new(),
    };

    if model.tab_rows > 1 {
        draw_status_bar_tab_rows(ui, rect, &model, &left, &center, &right, &mut input);
    } else {
        draw_status_bar_row(
            ui,
            bottom_status_row(rect, model.row_height),
            model.left_padding,
            &left,
            &center,
            &right,
            &mut input,
        );
    }

    if !input.drag_blocked {
        start_window_drag_on_primary_press(&response);
    }

    // A press that crosses the drag threshold begins a reorder; persist it so the gesture spans
    // frames (egui per-widget drag tracking would lapse as the bar reflows).
    if let Some(started) = input.started.take() {
        ui.ctx()
            .data_mut(|data| data.insert_persisted(drag_id, started.clone()));
        dragging = Some(started);
        ui.ctx().request_repaint();
    }

    let mut event = input.clicked.take().map(StatusBarEvent::Action);
    if let Some(drag) = dragging.as_ref() {
        let drop = pointer_pos
            .and_then(|pos| status_drop_target(&input.blocks, &drag.module, &drag.anchor, pos));
        if let Some((_, indicator_x)) = drop.as_ref() {
            ui.painter_at(rect).line_segment(
                [
                    Pos2::new(*indicator_x, rect.min.y + 2.0),
                    Pos2::new(*indicator_x, rect.max.y - 2.0),
                ],
                Stroke::new(2.0, palette.primary),
            );
        }
        if primary_down {
            ui.ctx().request_repaint();
            event = None;
        } else {
            event = drop.map(|(before, _)| StatusBarEvent::Reorder {
                module: drag.module.clone(),
                source: drag.anchor.clone(),
                before,
            });
            ui.ctx()
                .data_mut(|data| data.remove::<StatusDragState>(drag_id));
        }
    }
    event
}

/// Reorder gesture for the status bar, persisted across frames while the pointer is held.
#[derive(Clone)]
struct StatusDragState {
    module: String,
    anchor: String,
}

/// A contiguous run of items sharing a `reorder_anchor`, with its drawn horizontal extent.
struct StatusBlock {
    module: String,
    anchor: String,
    start_x: f32,
    end_x: f32,
    start_y: f32,
    end_y: f32,
}

/// Layout context plus per-frame interaction accumulators, threaded through the status-bar draw
/// pass. Carrying `rect`/`palette`/`font` here keeps the draw fns to a few arguments.
struct StatusInput {
    rect: Rect,
    palette: ThemePalette,
    font: egui::FontId,
    clicked: Option<String>,
    primary_press_pos: Option<Pos2>,
    drag_blocked: bool,
    suppress_click: bool,
    started: Option<StatusDragState>,
    blocks: Vec<StatusBlock>,
}

/// Picks the insertion slot for a horizontal drag: scans same-module blocks left to right and
/// drops before the first whose midpoint is past the pointer (or at the end). Returns the anchor
/// to insert before (`None` = end) and the indicator x, or `None` when the drop is a no-op.
fn status_drop_target(
    blocks: &[StatusBlock],
    module: &str,
    anchor: &str,
    pointer: Pos2,
) -> Option<(Option<String>, f32)> {
    let module_blocks: Vec<&StatusBlock> = blocks
        .iter()
        .filter(|block| block.module == module)
        .collect();
    let source_index = module_blocks
        .iter()
        .position(|block| block.anchor == anchor)?;
    let row_blocks = module_blocks
        .iter()
        .copied()
        .filter(|block| pointer.y >= block.start_y && pointer.y <= block.end_y)
        .collect::<Vec<_>>();
    let target_blocks = if row_blocks.is_empty() {
        module_blocks.as_slice()
    } else {
        row_blocks.as_slice()
    };
    let mut target_anchor = None;
    for block in target_blocks {
        if pointer.x < (block.start_x + block.end_x) * 0.5 {
            target_anchor = Some(block.anchor.as_str());
            break;
        }
    }
    let mut target_index = module_blocks.len();
    if let Some(anchor) = target_anchor {
        for (index, block) in module_blocks.iter().enumerate() {
            if block.anchor == anchor {
                target_index = index;
                break;
            }
        }
    } else if let Some(last) = target_blocks.last()
        && let Some(index) = module_blocks
            .iter()
            .position(|block| block.anchor == last.anchor)
    {
        target_index = index + 1;
    }
    if target_blocks.is_empty() {
        for (index, block) in module_blocks.iter().enumerate() {
            if pointer.x < (block.start_x + block.end_x) * 0.5 {
                target_index = index;
                break;
            }
        }
    }
    if target_index == source_index || target_index == source_index + 1 {
        return None;
    }
    let before = module_blocks
        .get(target_index)
        .map(|block| block.anchor.clone());
    let indicator_x = match module_blocks.get(target_index) {
        Some(block) => block.start_x,
        None => module_blocks.last().map_or(pointer.x, |block| block.end_x),
    };
    Some((before, indicator_x))
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

fn clamped_status_row_height(rect: Rect, row_height: f32) -> f32 {
    row_height.max(0.0).min(rect.height())
}

fn bottom_status_row(rect: Rect, row_height: f32) -> Rect {
    let row_height = clamped_status_row_height(rect, row_height);
    Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - row_height), rect.max)
}

fn draw_status_bar_row(
    ui: &mut egui::Ui,
    row_rect: Rect,
    left_padding: f32,
    left: &[&ResolvedSegment],
    center: &[&ResolvedSegment],
    right: &[&ResolvedSegment],
    input: &mut StatusInput,
) -> f32 {
    input.rect = row_rect;
    let font = input.font.clone();
    let right_start = row_rect.max.x - STATUS_EDGE_PAD - segments_width(ui, right, &font);
    draw_segments(
        ui,
        right_start,
        row_rect.max.x - STATUS_EDGE_PAD,
        right,
        input,
    );
    let left_end = draw_segments(
        ui,
        row_rect.min.x + left_padding,
        right_start - STATUS_ITEM_GAP,
        left,
        input,
    );
    if !center.is_empty() {
        let center_width = segments_width(ui, center, &font);
        let center_start = row_rect.center().x - center_width / 2.0;
        let center_bound = right_start - STATUS_ITEM_GAP;
        if center_start >= left_end + STATUS_ITEM_GAP && center_start + center_width <= center_bound
        {
            draw_segments(ui, center_start, center_bound, center, input);
        }
    }
    left_end
}

fn draw_status_bar_tab_rows(
    ui: &mut egui::Ui,
    rect: Rect,
    model: &StatusBarModel<'_>,
    left: &[&ResolvedSegment],
    center: &[&ResolvedSegment],
    right: &[&ResolvedSegment],
    input: &mut StatusInput,
) {
    let row_count = model.tab_rows.max(1);
    let bottom_rect = bottom_status_row(rect, model.row_height);
    let font = input.font.clone();
    let bottom_bound =
        bottom_rect.max.x - STATUS_EDGE_PAD - segments_width(ui, right, &font) - STATUS_ITEM_GAP;
    let top_bound = model
        .notch_x
        .as_ref()
        .map_or(rect.max.x - STATUS_EDGE_PAD, |range| {
            range.start - STATUS_ITEM_GAP
        });
    let bounds = status_tab_row_bounds(rect, row_count, top_bound, bottom_bound);
    let row_segments = split_left_segments_for_tab_rows(
        ui,
        left,
        &font,
        rect.min.x + model.left_padding,
        &bounds,
        true,
    );

    for (row_index, segments) in row_segments.iter().take(row_count - 1).enumerate() {
        if segments.is_empty() {
            continue;
        }
        let row_rect = status_row_from_top(rect, model.row_height, row_index);
        input.rect = row_rect;
        let row_refs = segments.iter().collect::<Vec<_>>();
        draw_segments(
            ui,
            row_rect.min.x + model.left_padding,
            bounds[row_index],
            &row_refs,
            input,
        );
    }

    let bottom_left = row_segments
        .get(row_count - 1)
        .map(|segments| segments.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    draw_status_bar_row(
        ui,
        bottom_rect,
        model.left_padding,
        &bottom_left,
        center,
        right,
        input,
    );
}

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

fn status_tab_row_bounds(
    rect: Rect,
    row_count: usize,
    first_top_bound: f32,
    bottom_bound: f32,
) -> Vec<f32> {
    (0..row_count)
        .map(|index| {
            if index + 1 == row_count {
                bottom_bound
            } else if index == 0 {
                first_top_bound
            } else {
                rect.max.x - STATUS_EDGE_PAD
            }
        })
        .collect()
}

fn status_row_from_top(rect: Rect, row_height: f32, row_index: usize) -> Rect {
    let row_height = clamped_status_row_height(rect, row_height);
    let y = rect.min.y + row_height * row_index as f32;
    Rect::from_min_max(
        Pos2::new(rect.min.x, y),
        Pos2::new(rect.max.x, y + row_height),
    )
}

fn split_left_segments_for_tab_rows(
    ui: &egui::Ui,
    left: &[&ResolvedSegment],
    font: &egui::FontId,
    left_start: f32,
    bounds: &[f32],
    force_last_row: bool,
) -> Vec<Vec<ResolvedSegment>> {
    let Some(window_index) = left
        .iter()
        .position(|segment| segment_contains_module(segment, STATUS_WINDOWS_MODULE))
    else {
        return vec![left.iter().map(|segment| (*segment).clone()).collect()];
    };
    if bounds.is_empty() {
        return Vec::new();
    }

    let window_segment = left[window_index];
    let items = window_segment.items.iter().collect::<Vec<_>>();
    let mut rows = vec![Vec::new(); bounds.len()];
    let mut item_start = 0;
    let bottom_index = bounds.len() - 1;
    let bottom_start = status_bottom_window_start(ui, left, font, left_start, window_index);

    for row_index in 0..bounds.len() {
        if item_start >= items.len() {
            break;
        }
        let row_start = if row_index == bottom_index {
            bottom_start
        } else {
            left_start
        };
        let split = status_items_split_index_before_x(
            ui,
            &items[item_start..],
            font,
            row_start,
            bounds[row_index],
        );
        let item_end = if split == 0 {
            if force_last_row && row_index == bottom_index {
                status_item_group_end(&items, item_start)
            } else {
                continue;
            }
        } else {
            item_start + split
        };
        let mut row_items = window_segment.items[item_start..item_end].to_vec();
        round_window_row_end(&mut row_items);
        rows[row_index].push(ResolvedSegment {
            align: window_segment.align,
            items: row_items,
        });
        item_start = item_end;
    }
    if item_start < items.len() {
        let mut row_items = window_segment.items[item_start..].to_vec();
        round_window_row_end(&mut row_items);
        rows.push(vec![ResolvedSegment {
            align: window_segment.align,
            items: row_items,
        }]);
    }

    let bottom_windows = rows[bottom_index].clone();
    let mut bottom = Vec::new();
    for (index, segment) in left.iter().enumerate() {
        if index == window_index {
            bottom.extend(bottom_windows.clone());
        } else {
            bottom.push((*segment).clone());
        }
    }
    rows[bottom_index] = bottom;
    rows
}

fn round_window_row_end(items: &mut [ResolvedItem]) {
    let Some(item) = items
        .last_mut()
        .filter(|item| item.module == STATUS_WINDOWS_MODULE && item.reorder_anchor.is_some())
    else {
        return;
    };

    for primitive in &mut item.primitives {
        if let ModulePrimitive::Rect { radius, .. } = primitive {
            radius.ne = STATUS_PILL_RADIUS;
            radius.se = STATUS_PILL_RADIUS;
        }
    }
}

fn status_bottom_window_start(
    ui: &egui::Ui,
    left: &[&ResolvedSegment],
    font: &egui::FontId,
    left_start: f32,
    window_index: usize,
) -> f32 {
    let mut x = left_start;
    let mut drawn = 0;
    for segment in left.iter().take(window_index) {
        let width = segment_width(ui, segment, font);
        if width <= 0.0 {
            continue;
        }
        if drawn > 0 {
            x += STATUS_ITEM_GAP;
        }
        x += width;
        drawn += 1;
    }
    if drawn > 0 {
        x += STATUS_ITEM_GAP;
    }
    x
}

fn status_window_group_count(left: &[&ResolvedSegment]) -> usize {
    let Some(segment) = left
        .iter()
        .find(|segment| segment_contains_module(segment, STATUS_WINDOWS_MODULE))
    else {
        return 1;
    };
    let items = segment.items.iter().collect::<Vec<_>>();
    let mut count = 0;
    let mut index = 0;
    while index < items.len() {
        count += 1;
        index = status_item_group_end(&items, index);
    }
    count.max(1)
}

fn segment_contains_module(segment: &ResolvedSegment, module: &str) -> bool {
    segment.items.iter().any(|item| item.module == module)
}

fn status_items_split_index_before_x(
    ui: &egui::Ui,
    items: &[&ResolvedItem],
    font: &egui::FontId,
    start_x: f32,
    bound: f32,
) -> usize {
    let mut x = start_x;
    let mut index = 0;
    while index < items.len() {
        let end = status_item_group_end(items, index);
        let width = status_item_group_width(ui, items, font, index, end);
        let gap = if index > 0
            && item_gap_before(items[index])
            && !connected(Some(items[index - 1]), items[index])
        {
            STATUS_ITEM_GAP
        } else {
            0.0
        };
        if x + gap + width > bound {
            return index;
        }
        x += gap + width;
        index = end;
    }
    items.len()
}

fn status_item_group_end(items: &[&ResolvedItem], start: usize) -> usize {
    let anchor = items[start].reorder_anchor.as_deref();
    let mut end = start + 1;
    while end < items.len() && anchor.is_some() && items[end].reorder_anchor.as_deref() == anchor {
        end += 1;
    }
    end
}

fn status_item_group_width(
    ui: &egui::Ui,
    items: &[&ResolvedItem],
    font: &egui::FontId,
    start: usize,
    end: usize,
) -> f32 {
    let mut width = 0.0;
    for index in start..end {
        if index > start
            && item_gap_before(items[index])
            && !connected(Some(items[index - 1]), items[index])
        {
            width += STATUS_ITEM_GAP;
        }
        width += item_width(ui, items[index], font);
    }
    width
}

fn draw_segments(
    ui: &mut egui::Ui,
    start_x: f32,
    bound: f32,
    segments: &[&ResolvedSegment],
    input: &mut StatusInput,
) -> f32 {
    let font = input.font.clone();
    let mut x = start_x;
    let mut drawn = 0;
    for segment in segments {
        let width = segment_width(ui, segment, &font);
        if width <= 0.0 {
            continue;
        }
        if drawn > 0 {
            x += STATUS_ITEM_GAP;
        }
        let items = segment.items.iter().collect::<Vec<_>>();
        if x + width > bound {
            if segment_contains_module(segment, STATUS_WINDOWS_MODULE) && x < bound {
                draw_items(ui, x, bound, &items, input);
            }
            break;
        }
        draw_items(ui, x, x + width, &items, input);
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
    let mut lead = 0.0;
    if item.gauge.is_some() {
        lead += STATUS_GAUGE_WIDTH;
    }
    if item_icon(item).is_some() {
        if lead > 0.0 {
            lead += STATUS_ICON_GAP;
        }
        lead += STATUS_ICON_SIZE;
    }
    if lead > 0.0 {
        inner += lead;
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

fn hovered_reorder_anchor(
    ui: &egui::Ui,
    rect: Rect,
    start_x: f32,
    bound: f32,
    items: &[&ResolvedItem],
    font: &egui::FontId,
) -> Option<(String, String)> {
    let hover_pos = ui.input(|input| input.pointer.hover_pos())?;
    let mut x = start_x;
    for index in 0..items.len() {
        let item = items[index];
        let prev = (index > 0).then(|| items[index - 1]);
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
        if item_rect.contains(hover_pos)
            && let Some(anchor) = item.reorder_anchor.as_deref()
        {
            return Some((item.module.clone(), anchor.to_owned()));
        }
        x += width;
    }
    None
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

fn blend_toward(color: egui::Color32, background: egui::Color32, keep: f32) -> egui::Color32 {
    if keep >= 1.0 {
        return color;
    }
    if keep <= 0.0 {
        return background;
    }
    let mix = |fg: u8, bg: u8| (bg as f32 + (fg as f32 - bg as f32) * keep).round() as u8;
    egui::Color32::from_rgb(
        mix(color.r(), background.r()),
        mix(color.g(), background.g()),
        mix(color.b(), background.b()),
    )
}

fn paint_item_primitives(
    painter: &egui::Painter,
    item_rect: Rect,
    primitives: &[ModulePrimitive],
    default_color: egui::Color32,
    background: egui::Color32,
    // Sidebar session rows pick intentionally dim, hue-tinted colors; honor them verbatim instead of
    // running them through readable_color, whose AAA contrast gate flattens dim tints to white. The
    // status bar and footer keep the gate so module colors stay legible on varied backgrounds.
    respect_color: bool,
    // Fraction of each color to keep before blending the rest toward the background. 1.0 paints the
    // color as-is; unfocused session rows pass < 1.0 so every element dims in its own hue.
    keep: f32,
) {
    let dim = |color: egui::Color32| blend_toward(color, background, keep);
    let resolve = |color: &Option<egui::Color32>| {
        let value = color.unwrap_or(default_color);
        let value = if respect_color {
            value
        } else {
            readable_color(background, value)
        };
        dim(value)
    };
    for primitive in primitives {
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
                    painter.rect_filled(rect, *radius, dim(*fill));
                }
                if let Some(stroke) = stroke {
                    painter.rect_stroke(
                        rect,
                        *radius,
                        Stroke::new(1.0, dim(*stroke)),
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
                            dim(*fill),
                            Stroke::new(0.0, egui::Color32::TRANSPARENT),
                        ));
                    }
                    if let Some(stroke) = stroke {
                        painter.add(egui::Shape::closed_line(
                            points,
                            Stroke::new(1.0, dim(*stroke)),
                        ));
                    }
                }
            }
            ModulePrimitive::Text {
                text,
                color,
                x,
                y,
                size,
                align,
                min_width,
            } => {
                if min_width.is_some_and(|min_width| item_rect.width() < min_width) {
                    continue;
                }
                painter.text(
                    Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)),
                    primitive_align(align),
                    text,
                    egui::FontId::monospace(*size),
                    resolve(color),
                );
            }
            ModulePrimitive::Icon {
                icon,
                color,
                x,
                y,
                size,
                min_width,
            } => {
                if min_width.is_some_and(|min_width| item_rect.width() < min_width) {
                    continue;
                }
                paint_icon_slug(
                    painter,
                    icon,
                    Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)),
                    *size,
                    resolve(color),
                );
            }
        }
    }
}

fn paint_item_hover_overlay(
    painter: &egui::Painter,
    item_rect: Rect,
    primitives: &[ModulePrimitive],
    color: egui::Color32,
) {
    for primitive in primitives {
        match primitive {
            ModulePrimitive::Rect {
                fill: Some(_),
                x,
                y,
                w,
                h,
                radius,
                ..
            } => {
                let rect = Rect::from_min_size(
                    Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)),
                    egui::vec2(coord_w(item_rect, *w), coord_h(item_rect, *h)),
                );
                painter.rect_filled(rect, *radius, color);
            }
            ModulePrimitive::Polygon {
                fill: Some(_),
                points,
                ..
            } => {
                let points = points
                    .iter()
                    .map(|(x, y)| Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)))
                    .collect::<Vec<_>>();
                if points.len() >= 3 {
                    painter.add(egui::Shape::convex_polygon(
                        points,
                        color,
                        Stroke::new(0.0, egui::Color32::TRANSPARENT),
                    ));
                }
            }
            ModulePrimitive::Rect { fill: None, .. }
            | ModulePrimitive::Polygon { fill: None, .. }
            | ModulePrimitive::Text { .. }
            | ModulePrimitive::Icon { .. } => {}
        }
    }
}

fn primitive_align(value: &str) -> egui::Align2 {
    match value {
        "left_top" => egui::Align2::LEFT_TOP,
        "left_center" => egui::Align2::LEFT_CENTER,
        "left_bottom" => egui::Align2::LEFT_BOTTOM,
        "center_top" => egui::Align2::CENTER_TOP,
        "center_center" | "center" => egui::Align2::CENTER_CENTER,
        "center_bottom" => egui::Align2::CENTER_BOTTOM,
        "right_top" => egui::Align2::RIGHT_TOP,
        "right_center" => egui::Align2::RIGHT_CENTER,
        "right_bottom" => egui::Align2::RIGHT_BOTTOM,
        _ => egui::Align2::LEFT_CENTER,
    }
}

fn primitive_background(primitives: &[ModulePrimitive]) -> Option<egui::Color32> {
    primitives
        .iter()
        .rev()
        .find_map(|primitive| match primitive {
            ModulePrimitive::Rect { fill, .. } => *fill,
            ModulePrimitive::Polygon { .. }
            | ModulePrimitive::Text { .. }
            | ModulePrimitive::Icon { .. } => None,
        })
        .or_else(|| {
            primitives
                .iter()
                .rev()
                .find_map(|primitive| match primitive {
                    ModulePrimitive::Polygon { fill, .. } => *fill,
                    ModulePrimitive::Rect { .. }
                    | ModulePrimitive::Text { .. }
                    | ModulePrimitive::Icon { .. } => None,
                })
        })
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

fn paint_status_diagonal_join(painter: &egui::Painter, item_rect: Rect, color: egui::Color32) {
    let width = STATUS_DIAGONAL_JOIN_WIDTH.min(item_rect.width() / 2.0);
    if width <= 0.5 {
        return;
    }
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(item_rect.min.x - width, item_rect.min.y),
            item_rect.left_top(),
            item_rect.left_bottom(),
        ],
        color,
        Stroke::new(0.0, egui::Color32::TRANSPARENT),
    ));
}

fn draw_items(
    ui: &mut egui::Ui,
    start_x: f32,
    bound: f32,
    items: &[&ResolvedItem],
    input: &mut StatusInput,
) {
    let rect = input.rect;
    let palette = input.palette;
    let font = input.font.clone();
    let hovered_anchor = hovered_reorder_anchor(ui, rect, start_x, bound, items, &font);
    let mut x = start_x;
    for index in 0..items.len() {
        let item = items[index];
        let prev = (index > 0).then(|| items[index - 1]);
        let next = items.get(index + 1).copied();
        if prev.is_some() && item_gap_before(item) && !connected(prev, item) {
            x += STATUS_ITEM_GAP;
        }
        let width = item_width(ui, item, &font);
        if x + width > bound {
            break;
        }
        let item_rect = Rect::from_min_size(
            Pos2::new(x, rect.min.y + 3.0),
            egui::vec2(width, rect.height() - 6.0),
        );

        // An anchored item drags; otherwise an action item just clicks. Key the id on the
        // anchor/action (not position) so the press is recognized as the same widget.
        let interactive = item.reorder_anchor.as_deref().or(item.action.as_deref());
        let response = interactive.map(|key| {
            let id = ui.make_persistent_id(("status-item", key, x as i32));
            let sense = if item.reorder_anchor.is_some() {
                egui::Sense::click_and_drag()
            } else {
                egui::Sense::click()
            };
            ui.interact(item_rect, id, sense)
        });
        if interactive.is_some()
            && input
                .primary_press_pos
                .is_some_and(|pos| item_rect.contains(pos))
        {
            input.drag_blocked = true;
        }
        if let Some(anchor) = item.reorder_anchor.as_deref() {
            match input.blocks.last_mut() {
                Some(block) if block.module == item.module && block.anchor == anchor => {
                    block.end_x = x + width;
                    block.start_y = block.start_y.min(item_rect.min.y);
                    block.end_y = block.end_y.max(item_rect.max.y);
                }
                _ => input.blocks.push(StatusBlock {
                    module: item.module.clone(),
                    anchor: anchor.to_owned(),
                    start_x: x,
                    end_x: x + width,
                    start_y: item_rect.min.y,
                    end_y: item_rect.max.y,
                }),
            }
        }
        if let Some(anchor) = item.reorder_anchor.as_deref()
            && response.as_ref().is_some_and(egui::Response::drag_started)
        {
            input.started = Some(StatusDragState {
                module: item.module.clone(),
                anchor: anchor.to_owned(),
            });
        }
        let hovered = response.as_ref().is_some_and(egui::Response::hovered)
            || item.reorder_anchor.as_deref().is_some_and(|anchor| {
                hovered_anchor
                    .as_ref()
                    .is_some_and(|(module, hovered)| module == &item.module && hovered == anchor)
            });

        let painter = ui.painter_at(rect);
        let primitive_bg = primitive_background(&item.primitives);
        let hover_background = hovered.then_some(palette.hover);
        let text_background = hover_background
            .or(item.bg)
            .or(primitive_bg)
            .unwrap_or(palette.base);
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
            if let Some(bg) = item.bg
                && left_join
                && prev.and_then(|prev| prev.bg) != Some(bg)
            {
                paint_status_diagonal_join(&painter, item_rect, bg);
            }
            if let Some(hover_background) = hover_background {
                paint_status_item_background(
                    &painter,
                    item_rect,
                    Some(hover_background),
                    None,
                    corners,
                );
                if left_join {
                    paint_status_diagonal_join(&painter, item_rect, hover_background);
                }
            }
        } else if let Some(hover_background) = hover_background
            && primitive_bg.is_none()
        {
            painter.rect_filled(item_rect, STATUS_PILL_RADIUS, hover_background);
        }
        paint_item_primitives(
            &painter,
            item_rect,
            &item.primitives,
            palette.subtext,
            text_background,
            false,
            1.0,
        );
        if hovered && primitive_bg.is_some() {
            paint_item_hover_overlay(&painter, item_rect, &item.primitives, palette.hover);
        }
        let color = readable_color(text_background, item.fg.unwrap_or(palette.subtext));
        let mut text_x = x + STATUS_ITEM_PAD + item.pad_left;
        if let Some(slug) = item_icon(item) {
            let center = Pos2::new(text_x + STATUS_ICON_SIZE / 2.0, rect.center().y);
            paint_icon_slug(&painter, slug, center, STATUS_ICON_SIZE, color);
            text_x += STATUS_ICON_SIZE;
        }
        if let Some(ratio) = item.gauge {
            if item_icon(item).is_some() {
                text_x += STATUS_ICON_GAP;
            }
            paint_battery_gauge(&painter, text_x, rect.center().y, ratio, color);
            text_x += STATUS_GAUGE_WIDTH;
        }
        if (item.gauge.is_some() || item_icon(item).is_some()) && !item.text.is_empty() {
            text_x += STATUS_ICON_GAP;
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

        if let (Some(resp), Some(action)) = (response.as_ref(), item.action.as_deref())
            && resp.clicked()
            && !input.suppress_click
        {
            input.clicked = Some(action.to_owned());
        }
        x += width;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_background_prefers_cell_rect_over_chevron_polygon() {
        let rect_fill = egui::Color32::from_rgb(0xee, 0xee, 0xee);
        let chevron_fill = egui::Color32::from_rgb(0x4c, 0x7d, 0xd9);
        let primitives = [
            ModulePrimitive::Rect {
                fill: Some(rect_fill),
                stroke: None,
                x: ModuleCoord::default(),
                y: ModuleCoord::default(),
                w: ModuleCoord { frac: 1.0, px: 0.0 },
                h: ModuleCoord { frac: 1.0, px: 0.0 },
                radius: egui::CornerRadius::ZERO,
            },
            ModulePrimitive::Polygon {
                fill: Some(chevron_fill),
                stroke: None,
                points: Vec::new(),
            },
        ];

        assert_eq!(primitive_background(&primitives), Some(rect_fill));
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
    fn status_bar_detects_window_tabs_crossing_notch_span() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 300.0));
        let segments = [ResolvedSegment {
            align: SegmentAlign::Left,
            items: vec![ResolvedItem {
                text: "1 alpha-with-long-name".to_owned(),
                module: STATUS_WINDOWS_MODULE.to_owned(),
                ..ResolvedItem::default()
            }],
        }];
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| {
                let bar = Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 30.0));
                assert!(status_bar_windows_intersect_x_range(
                    ui,
                    bar,
                    &segments,
                    STATUS_EDGE_PAD,
                    (20.0, 40.0),
                ));
                assert!(!status_bar_windows_intersect_x_range(
                    ui,
                    bar,
                    &segments,
                    STATUS_EDGE_PAD,
                    (500.0, 540.0),
                ));
                assert_eq!(
                    status_bar_window_tab_row_count(
                        ui,
                        bar,
                        &segments,
                        STATUS_EDGE_PAD,
                        Some((20.0, 40.0)),
                    ),
                    2
                );
            },
        );
        assert!(output.viewport_output.contains_key(&egui::ViewportId::ROOT));
    }

    #[test]
    fn status_item_width_reserves_gauge_and_icon() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 300.0));
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| {
                let font = egui::FontId::monospace(12.0);
                let gauge = ResolvedItem {
                    text: "99%".to_owned(),
                    gauge: Some(0.99),
                    ..ResolvedItem::default()
                };
                let gauge_and_icon = ResolvedItem {
                    icon: Some("battery-charging".to_owned()),
                    ..gauge.clone()
                };

                assert_eq!(
                    item_width(ui, &gauge_and_icon, &font) - item_width(ui, &gauge, &font),
                    STATUS_ICON_GAP + STATUS_ICON_SIZE
                );
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
                    row_height: screen_rect.height(),
                    notch_x: None,
                    tab_rows: 1,
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
                egui::CentralPanel::default().show(ui, |ui| show(ui));
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
                egui::CentralPanel::default().show(ui, |ui| show(ui));
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
                    row_height: screen_rect.height(),
                    notch_x: None,
                    tab_rows: 1,
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
                egui::CentralPanel::default().show(ui, |ui| show(ui));
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
                egui::CentralPanel::default().show(ui, |ui| show(ui));
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

    fn window_tab(anchor: &str, index: &str, name: &str) -> Vec<ResolvedItem> {
        let cell = |text: &str| ResolvedItem {
            text: text.to_owned(),
            action: Some(format!("activate-window:{anchor}")),
            reorder_anchor: Some(anchor.to_owned()),
            module: "windows".to_owned(),
            ..ResolvedItem::default()
        };
        vec![cell(index), cell(name)]
    }

    #[test]
    fn status_notch_split_keeps_window_tab_cells_together() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 300.0));
        let mut items = Vec::new();
        items.extend(window_tab("@1", "1", "alpha"));
        items.extend(window_tab("@2", "2", "beta"));
        items.extend(window_tab("@3", "3", "gamma"));
        let segment = ResolvedSegment {
            align: SegmentAlign::Left,
            items,
        };
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| {
                let font = egui::FontId::monospace(12.0);
                let first_tab = segment.items[..2].iter().collect::<Vec<_>>();
                let first_tab_end = STATUS_EDGE_PAD + items_width(ui, &first_tab, &font);
                let segment_refs = [&segment];
                let rows = split_left_segments_for_tab_rows(
                    ui,
                    &segment_refs,
                    &font,
                    STATUS_EDGE_PAD,
                    &[first_tab_end + 0.5, 600.0],
                    true,
                );

                let top = &rows[0][0];

                assert_eq!(top.items.len(), 2);
                assert!(
                    top.items
                        .iter()
                        .all(|item| item.reorder_anchor.as_deref() == Some("@1"))
                );

                assert_eq!(rows[1].len(), 1);
                assert_eq!(rows[1][0].items[0].reorder_anchor.as_deref(), Some("@2"));
            },
        );
        assert!(output.viewport_output.contains_key(&egui::ViewportId::ROOT));
    }

    #[test]
    fn status_tab_row_end_gets_right_corners() {
        let rect = ModulePrimitive::Rect {
            fill: Some(egui::Color32::WHITE),
            stroke: None,
            x: ModuleCoord::default(),
            y: ModuleCoord::default(),
            w: ModuleCoord { frac: 1.0, px: 0.0 },
            h: ModuleCoord { frac: 1.0, px: 0.0 },
            radius: egui::CornerRadius::ZERO,
        };
        let mut items = window_tab("@1", "1", "alpha");
        items[1].primitives.push(rect);

        round_window_row_end(&mut items);

        let ModulePrimitive::Rect { radius, .. } = &items[1].primitives[0] else {
            panic!("expected rect primitive");
        };
        assert_eq!(radius.ne, STATUS_PILL_RADIUS);
        assert_eq!(radius.se, STATUS_PILL_RADIUS);
    }

    #[test]
    fn status_tab_rows_wrap_when_window_tabs_exhaust_width() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 300.0));
        let mut items = Vec::new();
        items.extend(window_tab("@1", "1", "alpha"));
        items.extend(window_tab("@2", "2", "beta"));
        items.extend(window_tab("@3", "3", "gamma"));
        let segments = [ResolvedSegment {
            align: SegmentAlign::Left,
            items,
        }];
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| {
                let font = egui::FontId::monospace(12.0);
                let first_tab = segments[0].items[..2].iter().collect::<Vec<_>>();
                let width = items_width(ui, &first_tab, &font)
                    + STATUS_EDGE_PAD * 2.0
                    + STATUS_ITEM_GAP
                    + 1.0;
                let bar = Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 30.0));

                assert_eq!(
                    status_bar_window_tab_row_count(ui, bar, &segments, STATUS_EDGE_PAD, None),
                    3
                );
            },
        );
        assert!(output.viewport_output.contains_key(&egui::ViewportId::ROOT));
    }

    #[test]
    fn status_tab_rows_reserve_bottom_space_for_right_segments() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 300.0));
        let mut items = Vec::new();
        items.extend(window_tab("@1", "1", "alpha"));
        items.extend(window_tab("@2", "2", "beta"));
        items.extend(window_tab("@3", "3", "gamma"));
        let windows = ResolvedSegment {
            align: SegmentAlign::Left,
            items,
        };
        let sysinfo = ResolvedSegment {
            align: SegmentAlign::Right,
            items: vec![ResolvedItem {
                text: "sysinfo-wide".to_owned(),
                module: "sysinfo".to_owned(),
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
                let first_two_tabs = windows.items[..4].iter().collect::<Vec<_>>();
                let width = items_width(ui, &first_two_tabs, &font) + STATUS_EDGE_PAD * 2.0 + 1.0;
                let bar = Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 30.0));
                let segments = [windows.clone(), sysinfo.clone()];

                assert_eq!(
                    status_bar_window_tab_row_count(ui, bar, &segments, STATUS_EDGE_PAD, None),
                    3
                );
            },
        );
        assert!(output.viewport_output.contains_key(&egui::ViewportId::ROOT));
    }

    #[test]
    fn status_bar_drag_gesture_emits_window_reorder() {
        let context = egui::Context::default();
        crate::ui::icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 30.0));
        let mut items = Vec::new();
        items.extend(window_tab("@1", "1", "alpha"));
        items.extend(window_tab("@2", "2", "beta"));
        items.extend(window_tab("@3", "3", "gamma"));
        let segments = [ResolvedSegment {
            align: SegmentAlign::Left,
            items,
        }];

        let frame = |events: Vec<egui::Event>, captured: &mut Option<StatusBarEvent>| {
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let event = show_status_bar(
                            ui,
                            ThemePalette::default(),
                            StatusBarModel {
                                segments: &segments,
                                background: ThemePalette::default().base,
                                left_padding: STATUS_EDGE_PAD,
                                row_height: 30.0,
                                notch_x: None,
                                tab_rows: 1,
                            },
                        );
                        if event.is_some() {
                            *captured = event;
                        }
                    });
                },
            );
        };

        // Press the first tab and drag it past the right edge: it drops at the end.
        let press = Pos2::new(28.0, 15.0);
        let far_right = Pos2::new(560.0, 15.0);
        let mut captured = None;
        frame(vec![egui::Event::PointerMoved(press)], &mut captured);
        frame(
            vec![egui::Event::PointerButton {
                pos: press,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );
        frame(
            vec![egui::Event::PointerMoved(Pos2::new(120.0, 15.0))],
            &mut captured,
        );
        frame(vec![egui::Event::PointerMoved(far_right)], &mut captured);
        frame(
            vec![egui::Event::PointerButton {
                pos: far_right,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );

        assert_eq!(
            captured,
            Some(StatusBarEvent::Reorder {
                module: "windows".to_owned(),
                source: "@1".to_owned(),
                before: None,
            })
        );
    }

    #[test]
    fn status_drop_target_skips_noop_and_targets_next_block() {
        let blocks = vec![
            StatusBlock {
                module: "windows".to_owned(),
                anchor: "@1".to_owned(),
                start_x: 0.0,
                end_x: 40.0,
                start_y: 0.0,
                end_y: 20.0,
            },
            StatusBlock {
                module: "windows".to_owned(),
                anchor: "@2".to_owned(),
                start_x: 40.0,
                end_x: 80.0,
                start_y: 0.0,
                end_y: 20.0,
            },
            StatusBlock {
                module: "windows".to_owned(),
                anchor: "@3".to_owned(),
                start_x: 80.0,
                end_x: 120.0,
                start_y: 0.0,
                end_y: 20.0,
            },
        ];
        // Dragging @1 over the right half of @2 inserts before @3.
        assert_eq!(
            status_drop_target(&blocks, "windows", "@1", Pos2::new(70.0, 10.0)),
            Some((Some("@3".to_owned()), 80.0))
        );
        // Dropping @1 onto its own slot (left of @2's midpoint) is a no-op.
        assert_eq!(
            status_drop_target(&blocks, "windows", "@1", Pos2::new(50.0, 10.0)),
            None
        );
        // Past the last block drops at the end.
        assert_eq!(
            status_drop_target(&blocks, "windows", "@1", Pos2::new(200.0, 10.0)),
            Some((None, 120.0))
        );
    }

    #[test]
    fn status_drop_target_uses_pointer_row_for_wrapped_tabs() {
        let blocks = vec![
            StatusBlock {
                module: "windows".to_owned(),
                anchor: "@1".to_owned(),
                start_x: 0.0,
                end_x: 40.0,
                start_y: 0.0,
                end_y: 20.0,
            },
            StatusBlock {
                module: "windows".to_owned(),
                anchor: "@2".to_owned(),
                start_x: 40.0,
                end_x: 80.0,
                start_y: 0.0,
                end_y: 20.0,
            },
            StatusBlock {
                module: "windows".to_owned(),
                anchor: "@3".to_owned(),
                start_x: 0.0,
                end_x: 40.0,
                start_y: 20.0,
                end_y: 40.0,
            },
            StatusBlock {
                module: "windows".to_owned(),
                anchor: "@4".to_owned(),
                start_x: 40.0,
                end_x: 80.0,
                start_y: 20.0,
                end_y: 40.0,
            },
        ];

        assert_eq!(
            status_drop_target(&blocks, "windows", "@1", Pos2::new(10.0, 30.0)),
            Some((Some("@3".to_owned()), 0.0))
        );
        assert_eq!(
            status_drop_target(&blocks, "windows", "@1", Pos2::new(70.0, 30.0)),
            Some((None, 80.0))
        );
    }
}
