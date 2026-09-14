use num_traits::ToPrimitive as _;

use libghostty_vt::{
    render::CursorVisualStyle,
    style::{RgbColor, Underline},
};

use bootty_terminal::geometry::{SurfaceRect, TerminalSurface};
use bootty_terminal::terminal::{FrameSelection, RenderCell, RenderFrame};
use bootty_terminal::terminal_frame::FrameLineage;

const TEXT_Y_OFFSET: f32 = 2.0;
const CURSOR_BAR_LOGICAL_WIDTH: f32 = 1.0;
const OVERLAY_SELECTION: u8 = 1;
const OVERLAY_ACTIVE_SEARCH: u8 = 2;
const OVERLAY_SEARCH: u8 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct PlanColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl PlanColor {
    pub const fn opaque(color: RgbColor) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
            a: 255,
        }
    }

    pub fn gamma_multiply(self, factor: f32) -> Self {
        Self {
            r: (f32::from(self.r) * factor)
                .round()
                .clamp(0.0, 255.0)
                .to_u8()
                .unwrap_or(0),
            g: (f32::from(self.g) * factor)
                .round()
                .clamp(0.0, 255.0)
                .to_u8()
                .unwrap_or(0),
            b: (f32::from(self.b) * factor)
                .round()
                .clamp(0.0, 255.0)
                .to_u8()
                .unwrap_or(0),
            a: self.a,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent VT rendition flags may be combined."
)]
pub struct TextAttrs {
    pub fg: PlanColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: Underline,
    pub strikethrough: bool,
    pub overline: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundRect {
    pub rect: SurfaceRect,
    pub color: PlanColor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub rect: SurfaceRect,
    pub cell_rect: SurfaceRect,
    pub cells: u16,
    pub text: String,
    pub attrs: TextAttrs,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecorationLine {
    pub start_x: f32,
    pub start_y: f32,
    pub end_x: f32,
    pub end_y: f32,
    pub color: PlanColor,
    pub style: DecorationStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationStyle {
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
    Strikethrough,
    Overline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    HollowBlock,
    Bar,
    Underline,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorBlinkPhase {
    opacity: f32,
}

impl CursorBlinkPhase {
    #[must_use]
    pub const fn visible() -> Self {
        Self { opacity: 1.0 }
    }

    #[must_use]
    pub const fn hidden() -> Self {
        Self { opacity: 0.0 }
    }

    #[must_use]
    pub const fn from_opacity(opacity: f32) -> Self {
        Self {
            opacity: opacity.clamp(0.0, 1.0),
        }
    }

    #[must_use]
    pub const fn opacity(self) -> f32 {
        self.opacity
    }

    fn alpha(self) -> u8 {
        (self.opacity * 255.0)
            .round()
            .clamp(0.0, 255.0)
            .to_u8()
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CursorPlan {
    pub rect: SurfaceRect,
    pub color: PlanColor,
    pub shape: CursorShape,
    pub text_under_cursor: Option<CursorTextPlan>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CursorTextPlan {
    pub rect: SurfaceRect,
    pub text: String,
    pub color: PlanColor,
}

#[must_use]
pub fn cursor_fill_rect(shape: CursorShape, rect: SurfaceRect) -> SurfaceRect {
    match shape {
        CursorShape::Bar => {
            // Keep the terminal geometry at one logical unit. The GPUI owner snaps both edges
            // to device pixels when it lowers this rect, so this must not be widened here.
            let width = CURSOR_BAR_LOGICAL_WIDTH.min(rect.width()).max(0.0);
            SurfaceRect::from_min_size(rect.min_x, rect.min_y, width, rect.height())
        }
        CursorShape::Underline => SurfaceRect::from_min_size(
            rect.min_x,
            (rect.max_y - 2.0).max(rect.min_y),
            rect.width(),
            2.0_f32.min(rect.height()).max(1.0),
        ),
        CursorShape::Block | CursorShape::HollowBlock => rect,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalPaintPlan {
    pub surface: SurfaceRect,
    pub default_background: PlanColor,
    pub backgrounds: Vec<BackgroundRect>,
    pub text_runs: Vec<TextRun>,
    pub decorations: Vec<DecorationLine>,
    pub cursor: Option<CursorPlan>,
}

impl Default for TerminalPaintPlan {
    fn default() -> Self {
        Self {
            surface: SurfaceRect::from_min_size(0.0, 0.0, 0.0, 0.0),
            default_background: PlanColor::default(),
            backgrounds: Vec::new(),
            text_runs: Vec::new(),
            decorations: Vec::new(),
            cursor: None,
        }
    }
}

#[derive(Default)]
pub struct PaintPlanner {
    plan: TerminalPaintPlan,
    run_text_pool: Vec<String>,
    overlay_mask: Vec<u8>,
    row_fragments: Vec<RowPaintFragment>,
    row_cache_key: Option<RowPaintCacheKey>,
    row_cache_lineage: Option<FrameLineage>,
    underline_offset: Option<f32>,
}

#[derive(Default)]
struct RowPaintFragment {
    backgrounds: Vec<BackgroundRect>,
    text_runs: Vec<TextRun>,
    decorations: Vec<DecorationLine>,
}

impl RowPaintFragment {
    fn clear(&mut self, text_pool: &mut Vec<String>) {
        for run in self.text_runs.drain(..) {
            let mut text = run.text;
            text.clear();
            text_pool.push(text);
        }
        self.backgrounds.clear();
        self.decorations.clear();
    }

    fn append_to(&self, plan: &mut TerminalPaintPlan, text_pool: &mut Vec<String>) {
        plan.backgrounds.extend(self.backgrounds.iter().cloned());
        plan.text_runs.extend(self.text_runs.iter().map(|run| {
            let mut text = text_pool.pop().unwrap_or_default();
            text.clear();
            text.push_str(&run.text);
            TextRun {
                rect: run.rect,
                cell_rect: run.cell_rect,
                cells: run.cells,
                text,
                attrs: run.attrs,
            }
        }));
        plan.decorations.extend(self.decorations.iter().copied());
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RowPaintCacheKey {
    surface: TerminalSurface,
    cols: u16,
    rows: u16,
    font_size_bits: u32,
    text_cell_height_bits: u32,
    default_foreground: PlanColor,
    default_background: PlanColor,
}

impl PaintPlanner {
    /// Position underlines relative to the text cell using the host's font metrics.
    pub fn set_underline_offset(&mut self, offset: f32) {
        if offset.is_finite() && self.underline_offset != Some(offset) {
            self.underline_offset = Some(offset);
            self.row_cache_key = None;
        }
    }

    pub fn plan(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
    ) -> &TerminalPaintPlan {
        self.plan_with_cursor_blink_phase_and_text_cell_height(
            surface,
            frame,
            font_size,
            surface.cell.height,
            CursorBlinkPhase::visible(),
        )
    }

    pub fn plan_with_minimum_contrast(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
    ) -> &TerminalPaintPlan {
        self.plan_with_options(
            surface,
            frame,
            font_size,
            surface.cell.height,
            CursorBlinkPhase::visible(),
            true,
        )
    }

    pub fn plan_with_cursor_blink_phase_and_text_cell_height(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
        text_cell_height: f32,
        cursor_blink_phase: CursorBlinkPhase,
    ) -> &TerminalPaintPlan {
        self.plan_with_options(
            surface,
            frame,
            font_size,
            text_cell_height,
            cursor_blink_phase,
            false,
        )
    }

    /// Rebuild only the rows marked dirty after the row cache is warm.
    ///
    /// This path is intentionally limited to plain terminal content. Callers must use the full
    /// planner when geometry, overlays, images, or text configuration changes. A complete
    /// [`TerminalPaintPlan`] is still returned, so command ordering and consumers remain unchanged.
    pub fn plan_incremental(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
        text_cell_height: f32,
    ) -> (&TerminalPaintPlan, usize) {
        let default_background = PlanColor::opaque(frame.colors.background);
        let default_foreground = PlanColor::opaque(frame.colors.foreground);
        let key = RowPaintCacheKey {
            surface,
            cols: frame.cols,
            rows: frame.rows,
            font_size_bits: font_size.to_bits(),
            text_cell_height_bits: text_cell_height.to_bits(),
            default_foreground,
            default_background,
        };
        let cache_ready = self.row_cache_key == Some(key)
            && self.row_fragments.len() == usize::from(frame.rows)
            && self
                .row_cache_lineage
                .as_ref()
                .is_some_and(|cached| frame.lineage.follows(cached));
        if !cache_ready {
            self.invalidate_incremental_cache();
            self.row_fragments
                .resize_with(usize::from(frame.rows), RowPaintFragment::default);
            self.row_cache_key = Some(key);
        }

        self.row_cache_lineage = Some(frame.lineage.clone());
        let mut rebuilt_rows = 0_usize;
        for (row, fragment) in (0..frame.rows).zip(&mut self.row_fragments) {
            let index = usize::from(row);
            if !cache_ready || frame.row_dirty.get(index).copied().unwrap_or(true) {
                plan_row_fragment(
                    fragment,
                    &mut self.run_text_pool,
                    surface,
                    frame,
                    row,
                    self.underline_offset.unwrap_or(font_size + 3.0),
                    text_cell_height,
                    default_foreground,
                    default_background,
                );
                rebuilt_rows = rebuilt_rows.saturating_add(1);
            }
        }

        recycle_plan(
            &mut self.plan,
            &mut self.run_text_pool,
            surface.grid_rect(frame.cols, frame.rows),
            default_background,
        );
        for fragment in &self.row_fragments {
            fragment.append_to(&mut self.plan, &mut self.run_text_pool);
        }
        plan_cursor(
            &mut self.plan,
            surface,
            frame,
            default_foreground,
            default_background,
            text_cell_height,
            CursorBlinkPhase::visible(),
        );
        (&self.plan, rebuilt_rows)
    }

    /// Drop the row cache before a full redraw or a plan using overlays/contrast.
    pub fn invalidate_incremental_cache(&mut self) {
        for fragment in &mut self.row_fragments {
            fragment.clear(&mut self.run_text_pool);
        }
        self.row_fragments.clear();
        self.row_cache_key = None;
        self.row_cache_lineage = None;
    }

    fn plan_with_options(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
        text_cell_height: f32,
        cursor_blink_phase: CursorBlinkPhase,
        minimum_contrast: bool,
    ) -> &TerminalPaintPlan {
        self.invalidate_incremental_cache();
        let default_background = PlanColor::opaque(frame.colors.background);
        let default_foreground = PlanColor::opaque(frame.colors.foreground);
        recycle_plan(
            &mut self.plan,
            &mut self.run_text_pool,
            surface.grid_rect(frame.cols, frame.rows),
            default_background,
        );

        plan_backgrounds(
            &mut self.plan.backgrounds,
            surface,
            frame,
            default_foreground,
            default_background,
            None,
        );
        plan_overlays(
            &mut self.plan.backgrounds,
            &mut self.overlay_mask,
            surface,
            frame,
            default_foreground,
        );
        plan_text_runs(
            &mut self.plan.text_runs,
            &mut self.plan.decorations,
            &mut self.run_text_pool,
            &self.overlay_mask,
            surface,
            frame,
            TextPlanContext {
                default_foreground,
                default_background,
                underline_offset: self.underline_offset.unwrap_or(font_size + 3.0),
                text_cell_height,
                minimum_contrast,
            },
            None,
        );
        plan_cursor(
            &mut self.plan,
            surface,
            frame,
            default_foreground,
            default_background,
            text_cell_height,
            cursor_blink_phase,
        );

        &self.plan
    }
}

fn recycle_plan(
    plan: &mut TerminalPaintPlan,
    pool: &mut Vec<String>,
    surface: SurfaceRect,
    default_background: PlanColor,
) {
    for run in plan.text_runs.drain(..) {
        let mut text = run.text;
        text.clear();
        pool.push(text);
    }
    plan.surface = surface;
    plan.default_background = default_background;
    plan.backgrounds.clear();
    plan.decorations.clear();
    plan.cursor = None;
}

fn plan_backgrounds(
    backgrounds: &mut Vec<BackgroundRect>,
    surface: TerminalSurface,
    frame: &RenderFrame,
    default_foreground: PlanColor,
    default_background: PlanColor,
    row: Option<u16>,
) {
    for cell in row_cells(frame, row) {
        if !cell.style.inverse && cell.bg.is_none() {
            continue;
        }
        let bg = cell_background(cell, default_foreground, default_background);
        // Explicit cell colors stay opaque even when equal to a translucent default background.
        push_background(backgrounds, surface.cell_rect(cell.x, cell.y), bg);
    }
}

fn push_background(backgrounds: &mut Vec<BackgroundRect>, rect: SurfaceRect, color: PlanColor) {
    if let Some(last) = backgrounds.last_mut()
        && last.color == color
        && last.rect.min_y.to_bits() == rect.min_y.to_bits()
        && last.rect.max_y.to_bits() == rect.max_y.to_bits()
        && (last.rect.max_x - rect.min_x).abs() <= f32::EPSILON
    {
        last.rect.max_x = rect.max_x;
        return;
    }

    backgrounds.push(BackgroundRect { rect, color });
}

#[derive(Clone, Copy)]
struct TextPlanContext {
    default_foreground: PlanColor,
    default_background: PlanColor,
    underline_offset: f32,
    text_cell_height: f32,
    minimum_contrast: bool,
}

fn plan_overlays(
    backgrounds: &mut Vec<BackgroundRect>,
    mask: &mut Vec<u8>,
    surface: TerminalSurface,
    frame: &RenderFrame,
    default_foreground: PlanColor,
) {
    if frame.selections.is_empty()
        && frame.search_matches.is_empty()
        && frame.active_search_match.is_none()
    {
        mask.clear();
        return;
    }
    mask.resize(
        usize::from(frame.cols).saturating_mul(usize::from(frame.rows)),
        0,
    );
    mask.fill(0);

    let mut append = |ranges: &[FrameSelection], background: PlanColor, overlay| {
        for range in ranges {
            if range.end_col >= range.start_col {
                let cells = range
                    .end_col
                    .saturating_sub(range.start_col)
                    .saturating_add(1);
                push_background(
                    backgrounds,
                    surface.run_rect(range.start_col, range.row, cells),
                    background,
                );
            }

            if range.row >= frame.rows || range.start_col >= frame.cols {
                continue;
            }
            let end_col = range.end_col.min(frame.cols.saturating_sub(1));
            if end_col < range.start_col {
                continue;
            }
            let row_start = usize::from(range.row).saturating_mul(usize::from(frame.cols));
            let start = row_start.saturating_add(usize::from(range.start_col));
            let end = row_start.saturating_add(usize::from(end_col));
            if let Some(cells) = mask.get_mut(start..=end) {
                cells.fill(overlay);
            }
        }
    };

    append(
        &frame.search_matches,
        search_match_background(),
        OVERLAY_SEARCH,
    );
    if !frame.active_search_segments.is_empty() {
        append(
            &frame.active_search_segments,
            active_search_match_background(),
            OVERLAY_ACTIVE_SEARCH,
        );
    } else if let Some(active) = frame.active_search_match {
        append(
            std::slice::from_ref(&active),
            active_search_match_background(),
            OVERLAY_ACTIVE_SEARCH,
        );
    }
    append(
        &frame.selections,
        frame
            .colors
            .selection_background
            .map_or(default_foreground, PlanColor::opaque),
        OVERLAY_SELECTION,
    );
}

fn cell_overlay(mask: &[u8], cols: u16, cell: &RenderCell) -> u8 {
    if mask.is_empty() {
        return 0;
    }
    mask.get(
        usize::from(cell.y)
            .saturating_mul(usize::from(cols))
            .saturating_add(usize::from(cell.x)),
    )
    .copied()
    .unwrap_or_default()
}

#[allow(
    clippy::too_many_arguments,
    reason = "the row cache redirects the same planner into either the full plan or one row fragment"
)]
fn plan_text_runs(
    text_runs: &mut Vec<TextRun>,
    decorations: &mut Vec<DecorationLine>,
    pool: &mut Vec<String>,
    overlay_mask: &[u8],
    surface: TerminalSurface,
    frame: &RenderFrame,
    context: TextPlanContext,
    row: Option<u16>,
) {
    let colors = OverlayTextColors {
        selection: selection_text_foreground(frame, context.default_background),
        search: search_match_text_foreground(),
        active_search: active_search_match_text_foreground(),
    };
    let attrs_for = |cell: &RenderCell, text: &[char]| {
        paint_attrs(
            cell,
            text,
            cell_overlay(overlay_mask, frame.cols, cell),
            context.default_foreground,
            context.default_background,
            colors,
            context.minimum_contrast,
        )
    };
    let cells = row_cells(frame, row);
    let mut cell_index = 0_usize;
    while let Some(first) = cells.get(cell_index) {
        let first_text = frame.cell_text(first);

        if first.style.invisible || first_text.is_empty() {
            cell_index = cell_index.saturating_add(1);
            continue;
        }

        let attrs = attrs_for(first, first_text);
        let mut run_text = pool.pop().unwrap_or_default();
        run_text.clear();
        run_text.extend(first_text);

        let start_x = first.x;
        let start_y = first.y;
        let mut end_x = first.x.saturating_add(cell_text_width(first_text));
        let mut next_index = cell_index.saturating_add(1);

        if !context.minimum_contrast {
            while let Some(next) = cells.get(next_index) {
                let next_text = frame.cell_text(next);
                if next.y != start_y
                    || next.x != end_x
                    || next.style.invisible
                    || next_text.is_empty()
                    || attrs_for(next, next_text) != attrs
                {
                    break;
                }

                run_text.extend(next_text);
                end_x = end_x.saturating_add(cell_text_width(next_text));
                next_index = next_index.saturating_add(1);
            }
        }

        let row_rect = surface.run_rect(start_x, start_y, end_x.saturating_sub(start_x));
        let rect = text_rect_for_row(row_rect, context.text_cell_height);
        text_runs.push(TextRun {
            cell_rect: row_rect,
            rect,
            cells: end_x.saturating_sub(start_x),
            text: run_text,
            attrs,
        });

        plan_decorations(decorations, rect, attrs, context.underline_offset);
        cell_index = next_index;
    }
}

fn row_cells(frame: &RenderFrame, row: Option<u16>) -> &[RenderCell] {
    let Some(row) = row else {
        return &frame.cells;
    };
    // `TerminalEngine::assemble_cached_frame` concatenates its row caches in order.
    let start = frame.cells.partition_point(|cell| cell.y < row);
    let end = frame.cells.partition_point(|cell| cell.y <= row);
    frame.cells.get(start..end).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn plan_row_fragment(
    fragment: &mut RowPaintFragment,
    text_pool: &mut Vec<String>,
    surface: TerminalSurface,
    frame: &RenderFrame,
    row: u16,
    underline_offset: f32,
    text_cell_height: f32,
    default_foreground: PlanColor,
    default_background: PlanColor,
) {
    fragment.clear(text_pool);
    plan_backgrounds(
        &mut fragment.backgrounds,
        surface,
        frame,
        default_foreground,
        default_background,
        Some(row),
    );
    plan_text_runs(
        &mut fragment.text_runs,
        &mut fragment.decorations,
        text_pool,
        &[],
        surface,
        frame,
        TextPlanContext {
            default_foreground,
            default_background,
            underline_offset,
            text_cell_height,
            minimum_contrast: false,
        },
        Some(row),
    );
}

fn plan_decorations(
    decorations: &mut Vec<DecorationLine>,
    rect: SurfaceRect,
    attrs: TextAttrs,
    underline_offset: f32,
) {
    let underline = match attrs.underline {
        Underline::None => None,
        Underline::Double => Some(DecorationStyle::Double),
        Underline::Curly => Some(DecorationStyle::Curly),
        Underline::Dotted => Some(DecorationStyle::Dotted),
        Underline::Dashed => Some(DecorationStyle::Dashed),
        _ => Some(DecorationStyle::Single),
    };
    let lines = [
        underline.map(|style| (rect.min_y + underline_offset, style)),
        attrs.strikethrough.then_some((
            rect.height().mul_add(0.55, rect.min_y),
            DecorationStyle::Strikethrough,
        )),
        attrs
            .overline
            .then_some((rect.min_y + TEXT_Y_OFFSET, DecorationStyle::Overline)),
    ];
    decorations.extend(
        lines
            .into_iter()
            .flatten()
            .map(|(y, style)| DecorationLine {
                start_x: rect.min_x,
                start_y: y,
                end_x: rect.max_x,
                end_y: y,
                color: attrs.fg,
                style,
            }),
    );
}

fn plan_cursor(
    plan: &mut TerminalPaintPlan,
    surface: TerminalSurface,
    frame: &RenderFrame,
    default_foreground: PlanColor,
    default_background: PlanColor,
    text_cell_height: f32,
    cursor_blink_phase: CursorBlinkPhase,
) {
    let Some(cursor) = frame.cursor else {
        return;
    };
    let cursor_alpha = if cursor.blinking {
        cursor_blink_phase.alpha()
    } else {
        255
    };
    if cursor_alpha == 0 {
        return;
    }
    let color = cursor
        .color
        .or(frame.colors.cursor)
        .map_or(default_foreground, PlanColor::opaque);
    let color = PlanColor {
        a: cursor_alpha,
        ..color
    };
    let shape = match cursor.style {
        CursorVisualStyle::Bar => CursorShape::Bar,
        CursorVisualStyle::Underline => CursorShape::Underline,
        CursorVisualStyle::BlockHollow => CursorShape::HollowBlock,
        _ => CursorShape::Block,
    };
    let cursor_x = if cursor.at_wide_tail {
        cursor.x.saturating_sub(1)
    } else {
        cursor.x
    };
    let cells = if cursor.at_wide_tail { 2 } else { 1 };
    let rect = surface.run_rect(cursor_x, cursor.y, cells);
    let text_under_cursor = if shape == CursorShape::Block {
        cursor_cell(frame, cursor_x, cursor.y).and_then(|cell| {
            if cell.style.invisible {
                return None;
            }
            let text = frame.cell_text(cell).iter().collect::<String>();
            let (_, cell_bg) = cell_colors(cell, default_foreground, default_background);
            (!text.is_empty()).then_some(CursorTextPlan {
                rect: text_rect_for_row(rect, text_cell_height),
                text,
                color: frame.colors.cursor_text.map_or_else(
                    || cursor_text_color(cell_bg, color, default_foreground, default_background),
                    PlanColor::opaque,
                ),
            })
        })
    } else {
        None
    };

    plan.cursor = Some(CursorPlan {
        rect,
        color,
        shape,
        text_under_cursor,
    });
}

fn text_rect_for_row(row_rect: SurfaceRect, text_cell_height: f32) -> SurfaceRect {
    let height = if text_cell_height.is_finite() && text_cell_height > 0.0 {
        text_cell_height.min(row_rect.height())
    } else {
        row_rect.height()
    };
    let y_offset = ((row_rect.height() - height) * 0.5).max(0.0);
    SurfaceRect::from_min_size(
        row_rect.min_x,
        row_rect.min_y + y_offset,
        row_rect.width(),
        height,
    )
}

fn cursor_cell(frame: &RenderFrame, x: u16, y: u16) -> Option<&RenderCell> {
    let dense_index = usize::from(y)
        .checked_mul(usize::from(frame.cols))
        .and_then(|offset| offset.checked_add(usize::from(x)));
    dense_index
        .and_then(|index| frame.cells.get(index))
        .filter(|cell| cell.x == x && cell.y == y)
        .or_else(|| frame.cells.iter().find(|cell| cell.x == x && cell.y == y))
}

const fn cursor_text_color(
    cell_bg: PlanColor,
    cursor_color: PlanColor,
    default_foreground: PlanColor,
    default_background: PlanColor,
) -> PlanColor {
    let mut color = if same_rgb(cell_bg, cursor_color) {
        if same_rgb(default_background, cursor_color) {
            default_foreground
        } else {
            default_background
        }
    } else {
        cell_bg
    };
    color.a = cursor_color.a;
    color
}

const fn same_rgb(left: PlanColor, right: PlanColor) -> bool {
    left.r == right.r && left.g == right.g && left.b == right.b
}

fn cell_colors(
    cell: &RenderCell,
    default_foreground: PlanColor,
    default_background: PlanColor,
) -> (PlanColor, PlanColor) {
    let mut fg = cell.fg.map_or(default_foreground, PlanColor::opaque);
    let mut bg = cell.bg.map_or(default_background, PlanColor::opaque);
    if cell.style.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.style.faint {
        fg = fg.gamma_multiply(0.62);
    }
    (fg, bg)
}

pub(crate) const fn search_match_background() -> PlanColor {
    PlanColor {
        r: 245,
        g: 194,
        b: 66,
        a: 210,
    }
}

pub(crate) const fn search_match_text_foreground() -> PlanColor {
    PlanColor {
        r: 20,
        g: 20,
        b: 20,
        a: 255,
    }
}

pub(crate) const fn active_search_match_background() -> PlanColor {
    PlanColor {
        r: 255,
        g: 235,
        b: 120,
        a: 255,
    }
}

pub(crate) const fn active_search_match_text_foreground() -> PlanColor {
    PlanColor {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    }
}

fn selection_text_foreground(frame: &RenderFrame, default_background: PlanColor) -> PlanColor {
    frame
        .colors
        .selection_foreground
        .map_or(default_background, PlanColor::opaque)
}

fn cell_background(
    cell: &RenderCell,
    default_foreground: PlanColor,
    default_background: PlanColor,
) -> PlanColor {
    if cell.style.inverse {
        cell.fg.map_or(default_foreground, PlanColor::opaque)
    } else {
        cell.bg.map_or(default_background, PlanColor::opaque)
    }
}

#[derive(Clone, Copy)]
struct OverlayTextColors {
    selection: PlanColor,
    search: PlanColor,
    active_search: PlanColor,
}

fn paint_attrs(
    cell: &RenderCell,
    text: &[char],
    overlay: u8,
    default_foreground: PlanColor,
    default_background: PlanColor,
    colors: OverlayTextColors,
    minimum_contrast: bool,
) -> TextAttrs {
    let (mut fg, bg) = cell_colors(cell, default_foreground, default_background);
    if minimum_contrast && overlay == 0 {
        fg = adjust_text_contrast(text, fg, bg);
    }
    if overlay == OVERLAY_SELECTION {
        fg = colors.selection;
    } else if overlay == OVERLAY_ACTIVE_SEARCH {
        fg = colors.active_search;
    } else if overlay == OVERLAY_SEARCH {
        fg = colors.search;
    }
    TextAttrs {
        fg,
        bold: cell.style.bold,
        italic: cell.style.italic,
        underline: cell.style.underline,
        strikethrough: cell.style.strikethrough,
        overline: cell.style.overline,
    }
}

fn adjust_text_contrast(text: &[char], foreground: PlanColor, background: PlanColor) -> PlanColor {
    if matches!(text, [ch] if is_old_graphics_character(*ch))
        || contrast_distance(foreground, background) >= 96
    {
        return foreground;
    }

    let light = PlanColor {
        r: 255,
        g: 255,
        b: 255,
        a: foreground.a,
    };
    let dark = PlanColor {
        r: 0,
        g: 0,
        b: 0,
        a: foreground.a,
    };
    if contrast_distance(light, background) >= contrast_distance(dark, background) {
        light
    } else {
        dark
    }
}

const fn is_old_graphics_character(ch: char) -> bool {
    matches!(
        ch,
        '▀'..='▐'
            | '▔'
            | '▕'
            | '░'
            | '▒'
            | '▓'
            | '▖'..='▟'
            | '─'..='╿'
            | '\u{E0B0}'..='\u{E0BF}'
            | '\u{2800}'..='\u{28FF}'
            | '\u{25A0}'..='\u{25FF}'
            | '\u{1FB00}'..='\u{1FBFF}'
    )
}

fn contrast_distance(left: PlanColor, right: PlanColor) -> u16 {
    u16::from(left.r.abs_diff(right.r))
        .saturating_add(u16::from(left.g.abs_diff(right.g)))
        .saturating_add(u16::from(left.b.abs_diff(right.b)))
}

fn cell_text_width(text: &[char]) -> u16 {
    crate::terminal_text::terminal_grapheme_cells(text)
}
