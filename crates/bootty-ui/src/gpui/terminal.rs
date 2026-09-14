use std::{
    collections::{HashMap, HashSet, VecDeque, hash_map::Entry},
    hash::{Hash, Hasher},
    panic::Location,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use num_traits::ToPrimitive as _;

const GLYPH_IMAGE_CACHE_BYTE_BUDGET: usize = 32 * 1024 * 1024;

/// The same provider used by the application's text system, shared with terminal atlases.
pub struct TerminalPlatformTextSystem(pub Arc<dyn gpui_kit::PlatformTextSystem>);

impl gpui_kit::Global for TerminalPlatformTextSystem {}

use crate::{
    paint_plan::{
        CursorBlinkPhase, CursorShape, DecorationLine, DecorationStyle, PaintPlanner, PlanColor,
        TerminalPaintPlan, TextAttrs, TextRun as PlanTextRun,
    },
    terminal_render::{
        CursorCommand, FillRole, LineCommand, RenderFramePool, SpriteCommandBatch,
        TerminalRenderCommand, TerminalRenderFrame,
    },
    terminal_sprite::SpriteCommand,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
    terminal_text_atlas::{TextAtlasBuilder, TexturedGlyphQuad},
};
use bootty_terminal::geometry::{
    CellMetrics, SurfacePoint, SurfaceRect, TerminalSurface, ViewTransform,
};
use bootty_terminal::terminal_frame::RenderFrame;
use gpui_kit::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    PathBuilder, Pixels, Point, RenderImage, ShapedLine, Style, TextRun, Window, fill, point, px,
    rgba,
};
use image::ImageFormat as RasterImageFormat;
use libghostty_vt::{kitty::graphics::ImageFormat as KittyImageFormat, render::Dirty};

/// Resolve terminal grid metrics with the same Ghostty-derived contract as the glyph atlas.
pub fn terminal_cell_metrics(config: &TerminalTextConfig, window: &mut Window) -> CellMetrics {
    crate::terminal_cell_metrics::terminal_text_cell_metrics(config, window.scale_factor())
}

/// Owns the ordered terminal paint plan and lowers it directly into GPUI primitives.
///
/// The retained scene preserves terminal geometry, layering, selection colors, cursor state, and
/// native-symbol policy between paints.
pub struct GpuiTerminalAdapter {
    planner: PaintPlanner,
    render_frame_pool: RenderFramePool,
    render_frame_scratch: Option<TerminalRenderFrame>,
    prepared_rows: PreparedRowPool,
    glyph_layer_bins: GlyphLayerBins,
    scene: Option<CachedTerminalScene>,
    text_atlas: TextAtlasBuilder,
    glyph_images: TerminalGlyphImageCache,
    images: HashMap<TerminalImageKey, Option<Arc<RenderImage>>>,
    retired_images: Arc<Mutex<Vec<Arc<RenderImage>>>>,
    transition_key: Option<String>,
    transition_pending: bool,
    transition_source_frame: Option<Arc<RenderFrame>>,
    last_frame: Option<Arc<RenderFrame>>,
    search_pulse: SearchPulse,
    metrics: Option<TerminalRenderMetrics>,
}

impl Default for GpuiTerminalAdapter {
    fn default() -> Self {
        Self {
            planner: PaintPlanner::default(),
            render_frame_pool: RenderFramePool::default(),
            render_frame_scratch: None,
            prepared_rows: PreparedRowPool::default(),
            glyph_layer_bins: Vec::new(),
            scene: None,
            text_atlas: TextAtlasBuilder::default(),
            glyph_images: TerminalGlyphImageCache::new(GLYPH_IMAGE_CACHE_BYTE_BUDGET),
            images: HashMap::new(),
            retired_images: Arc::default(),
            transition_key: None,
            transition_pending: false,
            transition_source_frame: None,
            last_frame: None,
            search_pulse: SearchPulse::default(),
            metrics: None,
        }
    }
}

impl GpuiTerminalAdapter {
    pub fn with_platform_text_system(text_system: Arc<dyn gpui_kit::PlatformTextSystem>) -> Self {
        let mut adapter = Self::default();
        adapter.text_atlas.set_platform_text_system(text_system);
        adapter
    }

    /// Construct an adapter with a smaller glyph budget for deterministic cache tests.
    #[must_use]
    pub fn with_glyph_cache_byte_budget(byte_budget: usize) -> Self {
        Self {
            glyph_images: TerminalGlyphImageCache::new(byte_budget),
            ..Self::default()
        }
    }

    /// Enable opt-in renderer counters and CPU timing for subsequent elements.
    pub fn set_render_metrics(&mut self, metrics: Option<TerminalRenderMetrics>) {
        self.metrics = metrics;
    }

    /// Build a disposable GPUI element for one published terminal frame.
    #[track_caller]
    #[allow(clippy::too_many_arguments)]
    pub fn element(
        &mut self,
        surface: TerminalSurface,
        frame: &Arc<RenderFrame>,
        font_size: f32,
        text_cell_height: f32,
        pixels_per_point: f32,
        text_contract: &TerminalTextContract,
        cursor_blink_phase: CursorBlinkPhase,
        cursor_focused: bool,
        marked_text: &str,
    ) -> GpuiTerminalElement {
        let frame = self.frame_for_paint(frame);
        let search_pulse = self.search_pulse.overlay(surface, &frame);
        let scene = self.scene_for_frame(TerminalSceneKey {
            source_frame: &frame,
            surface,
            font_size,
            text_cell_height,
            pixels_per_point,
            text_contract,
            marked_text,
        });
        let interaction = GpuiTerminalInteraction::new(surface, Arc::clone(&frame));
        // Keep blink out of the retained scene key. A phase change only toggles the cursor tail
        // during paint, which lets a reset show the cursor immediately without rebuilding text.
        let cursor_opacity = if frame.cursor.is_some_and(|cursor| cursor.blinking) {
            cursor_blink_phase.opacity()
        } else {
            1.0
        };
        let cursor_glyphs = self.cursor_glyphs(&scene, cursor_opacity);
        self.last_frame = Some(frame);
        GpuiTerminalElement::with_frame_facts(
            scene,
            interaction,
            search_pulse,
            cursor_opacity,
            cursor_glyphs,
            cursor_focused,
            Arc::clone(&self.retired_images),
        )
        .with_metrics(self.metrics.clone())
    }

    fn scene_for_frame(&mut self, key: TerminalSceneKey<'_>) -> Arc<GpuiTerminalScene> {
        if let Some(cached) = self.scene.as_ref().filter(|cached| cached.matches(key)) {
            return Arc::clone(&cached.scene);
        }
        let TerminalSceneKey {
            source_frame: frame,
            surface,
            font_size,
            text_cell_height,
            pixels_per_point,
            text_contract,
            marked_text,
        } = key;
        let prepared_inputs_match = self.scene.as_ref().is_some_and(|cached| {
            cached.matches_render_inputs(key) && plain_frame(&cached.source_frame)
        });
        self.planner
            .set_underline_offset(crate::terminal_cell_metrics::terminal_underline_offset(
                &text_contract.config,
                text_cell_height,
                pixels_per_point,
            ));
        let mut previous_scene = self
            .scene
            .take()
            .and_then(|cached| Arc::try_unwrap(cached.scene).ok());
        let (render_frame, incremental, rebuilt_rows) = self.lower_render_frame(key);
        // Paint may retire old tiles while a zoom worker builds the next scene.
        // Never hold the shared retirement queue during rasterization.
        let mut retired_images = Vec::new();
        let scene = Arc::new(GpuiTerminalScene::new_with_reuse(
            render_frame,
            previous_scene.as_mut(),
            &mut self.prepared_rows,
            prepared_inputs_match && incremental && rebuilt_rows < usize::from(frame.rows),
            &frame.row_dirty,
            surface.content_origin().y,
            surface.cell.height,
            TerminalSceneResources {
                baseline_adjustment: text_contract.baseline_adjustment(),
                text_atlas: &mut self.text_atlas,
                glyph_images: &mut self.glyph_images,
                glyph_layer_bins: &mut self.glyph_layer_bins,
                images: &mut self.images,
                retired_images: &mut retired_images,
                metrics: self.metrics.as_ref(),
            },
            pixels_per_point,
        ));
        self.retired_images
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(retired_images);
        if let Some(previous_scene) = previous_scene {
            self.render_frame_scratch = Some(previous_scene.frame);
        }
        self.scene = Some(CachedTerminalScene {
            source_frame: Arc::clone(frame),
            surface,
            font_size_bits: font_size.to_bits(),
            text_cell_height_bits: text_cell_height.to_bits(),
            pixels_per_point_bits: pixels_per_point.to_bits(),
            text_contract: text_contract.clone(),
            marked_text: marked_text.to_owned(),
            scene: Arc::clone(&scene),
        });
        scene
    }

    fn lower_render_frame(
        &mut self,
        key: TerminalSceneKey<'_>,
    ) -> (TerminalRenderFrame, bool, usize) {
        let TerminalSceneKey {
            source_frame: frame,
            surface,
            font_size,
            text_cell_height,
            pixels_per_point: _,
            text_contract,
            marked_text,
        } = key;
        let reused_render_frame = self.render_frame_scratch.is_some();
        let mut render_frame = self
            .render_frame_scratch
            .take()
            .unwrap_or_else(empty_render_frame);
        let incremental = frame.dirty != Dirty::Full
            && frame.row_dirty.len() == usize::from(frame.rows)
            && frame.selections.is_empty()
            && frame.search_matches.is_empty()
            && frame.active_search_match.is_none()
            && frame.images.placements.is_empty()
            && frame.images.virtual_placements.is_empty()
            && frame.images.virtual_placeholder_rows.is_empty()
            && marked_text.is_empty();
        let rebuilt_rows = if incremental {
            let (plan, rebuilt_rows) =
                self.planner
                    .plan_incremental(surface, frame, font_size, text_cell_height);
            self.render_frame_pool.rebuild_from_plan_and_images(
                &mut render_frame,
                plan,
                text_contract,
                &frame.images,
            );
            rebuilt_rows
        } else {
            let plan = self
                .planner
                .plan_with_cursor_blink_phase_and_text_cell_height(
                    surface,
                    frame,
                    font_size,
                    text_cell_height,
                    CursorBlinkPhase::visible(),
                );
            if marked_text.is_empty() {
                self.render_frame_pool.rebuild_from_plan_and_images(
                    &mut render_frame,
                    plan,
                    text_contract,
                    &frame.images,
                );
            } else {
                self.render_frame_pool.rebuild_from_plan_and_images(
                    &mut render_frame,
                    &plan_without_cursor_during_preedit(plan.clone(), marked_text),
                    text_contract,
                    &frame.images,
                );
            }
            usize::from(frame.rows)
        };
        if let Some(metrics) = &self.metrics {
            metrics.record_planning(rebuilt_rows, incremental);
            metrics.record_lowering(reused_render_frame);
        }
        if let Some(preedit) =
            preedit_plan(surface, frame, marked_text, font_size, text_cell_height)
        {
            render_frame
                .commands
                .extend(TerminalRenderFrame::from_plan(&preedit, text_contract).commands);
        }
        (render_frame, incremental, rebuilt_rows)
    }

    fn cursor_glyphs(
        &mut self,
        scene: &GpuiTerminalScene,
        opacity: f32,
    ) -> HashMap<usize, Vec<GpuiPreparedGlyph>> {
        if !(0.0..1.0).contains(&opacity) {
            return HashMap::new();
        }
        let alpha = (opacity * 255.0).round().to_u8().unwrap_or(0);
        let mut retired = Vec::new();
        let glyphs = scene
            .commands
            .iter()
            .enumerate()
            .skip(scene.stable_command_len)
            .filter_map(|(index, command)| {
                let glyphs = match command {
                    GpuiPreparedCommand::Glyphs(glyphs) => glyphs.as_slice(),
                    GpuiPreparedCommand::SpriteImage(Some(glyph)) => std::slice::from_ref(glyph),
                    _ => return None,
                };
                let glyphs = glyphs
                    .iter()
                    .filter_map(|glyph| {
                        let mut key = glyph.key.clone();
                        key.color[3] = u8::try_from(
                            u16::from(key.color[3]).saturating_mul(u16::from(alpha)) / 255,
                        )
                        .ok()?;
                        let image = self.glyph_images.get_or_insert_with(
                            key.clone(),
                            &mut retired,
                            || prepare_glyph_opacity_image(&glyph.image, alpha),
                        )?;
                        Some(GpuiPreparedGlyph {
                            rect: glyph.rect,
                            image,
                            key,
                        })
                    })
                    .collect();
                Some((index, glyphs))
            })
            .collect();
        self.retired_images
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(retired);
        glyphs
    }

    pub fn set_transition_key(&mut self, key: Option<String>) {
        if self.transition_key == key {
            return;
        }
        self.transition_source_frame = self.last_frame.clone();
        self.transition_key = key;
        self.transition_pending = true;
        self.planner.invalidate_incremental_cache();
    }

    fn frame_for_paint(&mut self, incoming: &Arc<RenderFrame>) -> Arc<RenderFrame> {
        let ready = !is_transition_placeholder_frame(incoming)
            && !self
                .transition_source_frame
                .as_ref()
                .is_some_and(|source| Arc::ptr_eq(source, incoming));
        if self.transition_pending && ready {
            self.transition_pending = false;
            self.transition_source_frame = None;
        }
        if self.transition_pending && is_transition_placeholder_frame(incoming) {
            return self
                .last_frame
                .as_ref()
                .filter(|frame| {
                    !is_transition_placeholder_frame(frame) && frame.images.placements.is_empty()
                })
                .cloned()
                .unwrap_or_else(|| Arc::clone(incoming));
        }
        Arc::clone(incoming)
    }

    /// Bypass planning when a caller already owns a `TerminalRenderFrame` cache.
    #[track_caller]
    #[must_use]
    pub fn render_frame(&self, frame: TerminalRenderFrame) -> GpuiTerminalElement {
        GpuiTerminalElement::new_with_metrics(frame, self.metrics.clone())
    }

    /// Release every GPUI image owned by this adapter when its view is torn down.
    pub fn take_render_images(&mut self) -> Vec<Arc<RenderImage>> {
        self.scene = None;
        let mut images = self.glyph_images.take_images();
        images.extend(std::mem::take(&mut self.images).into_values().flatten());
        images.extend(
            self.retired_images
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .drain(..),
        );
        let mut seen = HashSet::new();
        images.retain(|image| seen.insert(image.id));
        images
    }

    pub(crate) fn take_retired_images(&self) -> Vec<Arc<RenderImage>> {
        take_retired_images(&self.retired_images)
    }

    /// Cumulative counters for the bounded Ghostty-glyph image cache.
    #[must_use]
    pub fn glyph_cache_metrics(&self) -> TerminalGlyphCacheMetrics {
        self.glyph_images.metrics()
    }
}

struct CachedTerminalScene {
    source_frame: Arc<RenderFrame>,
    surface: TerminalSurface,
    font_size_bits: u32,
    text_cell_height_bits: u32,
    pixels_per_point_bits: u32,
    text_contract: TerminalTextContract,
    marked_text: String,
    scene: Arc<GpuiTerminalScene>,
}

#[derive(Clone, Copy)]
struct TerminalSceneKey<'a> {
    source_frame: &'a Arc<RenderFrame>,
    surface: TerminalSurface,
    font_size: f32,
    text_cell_height: f32,
    pixels_per_point: f32,
    text_contract: &'a TerminalTextContract,
    marked_text: &'a str,
}

impl CachedTerminalScene {
    fn matches(&self, key: TerminalSceneKey<'_>) -> bool {
        Arc::ptr_eq(&self.source_frame, key.source_frame) && self.matches_render_inputs(key)
    }

    fn matches_render_inputs(&self, key: TerminalSceneKey<'_>) -> bool {
        self.surface == key.surface
            && self.font_size_bits == key.font_size.to_bits()
            && self.text_cell_height_bits == key.text_cell_height.to_bits()
            && self.pixels_per_point_bits == key.pixels_per_point.to_bits()
            && self.text_contract == *key.text_contract
            && self.marked_text == key.marked_text
    }
}

const fn plain_frame(frame: &RenderFrame) -> bool {
    frame.selections.is_empty()
        && frame.search_matches.is_empty()
        && frame.active_search_match.is_none()
        && frame.images.placements.is_empty()
        && frame.images.virtual_placements.is_empty()
        && frame.images.virtual_placeholder_rows.is_empty()
}

fn empty_render_frame() -> TerminalRenderFrame {
    TerminalRenderFrame {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 0.0, 0.0),
        commands: Vec::new(),
    }
}

fn plan_without_cursor_during_preedit(
    mut plan: TerminalPaintPlan,
    marked_text: &str,
) -> TerminalPaintPlan {
    if !marked_text.is_empty() {
        plan.cursor = None;
    }
    plan
}

fn preedit_plan(
    surface: TerminalSurface,
    frame: &RenderFrame,
    marked_text: &str,
    font_size: f32,
    text_cell_height: f32,
) -> Option<TerminalPaintPlan> {
    let cursor = frame.cursor?;
    if marked_text.is_empty() {
        return None;
    }
    let start_x = if cursor.at_wide_tail {
        cursor.x.saturating_sub(1)
    } else {
        cursor.x
    };
    let cells = crate::terminal_text::for_terminal_text_cells(marked_text, |_, _| {}).max(1);
    let cell_rect = surface.run_rect(start_x, cursor.y, cells);
    let height = if text_cell_height.is_finite() && text_cell_height > 0.0 {
        text_cell_height.min(cell_rect.height())
    } else {
        cell_rect.height()
    };
    let text_rect = SurfaceRect::from_min_size(
        cell_rect.min_x,
        cell_rect.min_y + ((cell_rect.height() - height) * 0.5).max(0.0),
        cell_rect.width(),
        height,
    );
    Some(TerminalPaintPlan {
        surface: cell_rect,
        default_background: PlanColor::opaque(frame.colors.background),
        backgrounds: Vec::new(),
        text_runs: vec![PlanTextRun {
            rect: text_rect,
            cell_rect,
            cells,
            text: marked_text.to_owned(),
            attrs: TextAttrs {
                fg: PlanColor::opaque(frame.colors.foreground),
                bold: false,
                italic: false,
                underline: libghostty_vt::style::Underline::Single,
                strikethrough: false,
                overline: false,
            },
        }],
        decorations: vec![DecorationLine {
            start_x: text_rect.min_x,
            start_y: text_rect.min_y + font_size + 3.0,
            end_x: text_rect.max_x,
            end_y: text_rect.min_y + font_size + 3.0,
            color: PlanColor::opaque(frame.colors.foreground),
            style: DecorationStyle::Single,
        }],
        cursor: None,
    })
}

/// A GPUI API boundary that prevents a render command from disappearing silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuiTerminalLimit {
    /// The placement dimensions, crop, format, or payload were invalid.
    InvalidKittyImage { image_id: u32, placement_id: u32 },
    /// Virtual placements carry no pixels and must be resolved by the terminal image owner.
    KittyVirtualPlacement { image_id: u32, placement_id: u32 },
}

#[derive(Clone)]
pub struct GpuiTerminalElement {
    background_opacity: f32,
    scene: Arc<GpuiTerminalScene>,
    source_location: &'static Location<'static>,
    interaction: Option<GpuiTerminalInteraction>,
    view_transform: ViewTransform,
    search_pulse: Option<SearchPulseOverlay>,
    copy_mode_label: Option<String>,
    cursor_opacity: f32,
    cursor_glyphs: HashMap<usize, Vec<GpuiPreparedGlyph>>,
    cursor_focused: bool,
    retired_images: Arc<Mutex<Vec<Arc<RenderImage>>>>,
    metrics: Option<TerminalRenderMetrics>,
}

struct GpuiTerminalScene {
    frame: TerminalRenderFrame,
    commands: Vec<GpuiPreparedCommand>,
    stable_command_len: usize,
    text_spans: HashMap<usize, PreparedTextSpan>,
    limits: Vec<GpuiTerminalLimit>,
}

struct PreparedTextSpan {
    end: usize,
    layers: Vec<PreparedGlyphLayer>,
}

struct PreparedGlyphLayer {
    bounds: Bounds<f32>,
    glyphs: Vec<(usize, usize)>,
}

type GlyphLayerBins = Vec<smallvec::SmallVec<[(Bounds<f32>, usize); 4]>>;

fn glyph_layer_bin_indices(rect: SurfaceRect) -> Option<smallvec::SmallVec<[usize; 4]>> {
    let left = (rect.min_x / 32.0).floor().to_i32()?;
    let right = (rect.max_x / 32.0).floor().to_i32()?;
    let top = (rect.min_y / 32.0).floor().to_i32()?;
    let bottom = (rect.max_y / 32.0).floor().to_i32()?;
    let width = i64::from(right)
        .checked_sub(i64::from(left))?
        .checked_add(1)?;
    let height = i64::from(bottom)
        .checked_sub(i64::from(top))?
        .checked_add(1)?;
    if width <= 0 || height <= 0 || width > 64 || height > 64 || width.checked_mul(height)? > 64 {
        return None;
    }
    let mut seen = [0_u64; 4];
    let mut indices = smallvec::SmallVec::new();
    for y in top..=bottom {
        for x in left..=right {
            let bin = usize::from((x.wrapping_mul(17) ^ y).to_le_bytes()[0]);
            let bit = 1 << (bin & 63);
            let slot = seen.get_mut(bin >> 6)?;
            if *slot & bit == 0 {
                *slot |= bit;
                indices.push(bin);
            }
        }
    }
    Some(indices)
}

fn prepare_text_spans(
    frame: &TerminalRenderFrame,
    commands: &[GpuiPreparedCommand],
    stable_len: usize,
    bins: &mut GlyphLayerBins,
) -> HashMap<usize, PreparedTextSpan> {
    bins.resize_with(256, smallvec::SmallVec::new);
    let mut spans = HashMap::new();
    let mut index = 0_usize;
    while index < stable_len {
        if !matches!(
            frame.commands.get(index),
            Some(TerminalRenderCommand::Text(_))
        ) {
            index = index.saturating_add(1);
            continue;
        }
        let start = index;
        let mut layers = Vec::new();
        let mut pending = Vec::<PreparedGlyphLayer>::new();
        bins.iter_mut().for_each(smallvec::SmallVec::clear);
        while index < stable_len
            && matches!(
                frame.commands.get(index),
                Some(TerminalRenderCommand::Text(_))
            )
        {
            if let Some(GpuiPreparedCommand::Glyphs(glyphs)) = commands.get(index) {
                for (glyph, prepared) in glyphs.iter().enumerate() {
                    let rect = prepared.rect;
                    let bounds = Bounds::new(
                        point(rect.min_x, rect.min_y),
                        gpui_kit::size(rect.width(), rect.height()),
                    );
                    // Bound work for giant glyphs and dense overprints. Flushing the
                    // entire segment retains every overlap dependency across the boundary.
                    let Some(indices) = glyph_layer_bin_indices(rect) else {
                        layers.append(&mut pending);
                        bins.iter_mut().for_each(smallvec::SmallVec::clear);
                        layers.push(PreparedGlyphLayer {
                            bounds,
                            glyphs: vec![(index, glyph)],
                        });
                        continue;
                    };
                    if indices
                        .iter()
                        .any(|&bin| bins.get(bin).is_some_and(|bin| bin.len() >= 64))
                    {
                        layers.append(&mut pending);
                        bins.iter_mut().for_each(smallvec::SmallVec::clear);
                    }
                    let mut level = 0;
                    for &bin in &indices {
                        for (earlier, order) in bins.get(bin).into_iter().flatten() {
                            if bounds.intersects(earlier) {
                                level = level.max(order.saturating_add(1));
                            }
                        }
                    }
                    if let Some(layer) = pending.get_mut(level) {
                        layer.bounds = layer.bounds.union(&bounds);
                        layer.glyphs.push((index, glyph));
                    } else {
                        pending.push(PreparedGlyphLayer {
                            bounds,
                            glyphs: vec![(index, glyph)],
                        });
                    }
                    for bin in indices {
                        if let Some(bin) = bins.get_mut(bin) {
                            bin.push((bounds, level));
                        }
                    }
                }
            }
            index = index.saturating_add(1);
        }
        layers.append(&mut pending);
        spans.insert(start, PreparedTextSpan { end: index, layers });
    }
    spans
}

struct ReusedPreparedScene {
    commands: Vec<GpuiPreparedCommand>,
    stable_command_len: usize,
}

enum GpuiPreparedCommand {
    None,
    Glyphs(Vec<GpuiPreparedGlyph>),
    Image(Option<Arc<RenderImage>>),
    Sprite(Vec<SpriteCommand>),
    SpriteImage(Option<GpuiPreparedGlyph>),
}

#[derive(Default)]
struct PreparedRowPool {
    rows: Vec<VecDeque<(usize, GpuiPreparedCommand)>>,
}

impl PreparedRowPool {
    fn stage(
        &mut self,
        scene: &mut GpuiTerminalScene,
        dirty_rows: &[bool],
        grid_min_y: f32,
        cell_height: f32,
    ) -> bool {
        for row in &mut self.rows {
            row.clear();
        }
        self.rows.resize_with(dirty_rows.len(), VecDeque::default);
        if scene.commands.len() != scene.frame.commands.len() {
            return false;
        }

        for (index, (prepared, command)) in std::mem::take(&mut scene.commands)
            .into_iter()
            .zip(&scene.frame.commands)
            .enumerate()
        {
            if index >= scene.stable_command_len {
                continue;
            }
            let Some(row) =
                terminal_command_row(command, grid_min_y, cell_height, dirty_rows.len())
            else {
                continue;
            };
            if !dirty_rows.get(row).copied().unwrap_or(true)
                && let Some(row) = self.rows.get_mut(row)
            {
                row.push_back((index, prepared));
            }
        }
        true
    }

    fn take(&mut self, row: usize) -> Option<(usize, GpuiPreparedCommand)> {
        self.rows.get_mut(row)?.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.rows.iter().all(VecDeque::is_empty)
    }

    fn clear(&mut self) {
        for row in &mut self.rows {
            row.clear();
        }
    }
}

#[derive(Clone)]
struct GpuiPreparedGlyph {
    rect: SurfaceRect,
    image: Arc<RenderImage>,
    key: TerminalGlyphImageKey,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct TerminalGlyphImageKey {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    atlas_generation: u64,
    color: [u8; 4],
}

struct CachedGlyphImage {
    image: Arc<RenderImage>,
    bytes: usize,
    last_used: u64,
}

struct TerminalGlyphImageCache {
    entries: HashMap<TerminalGlyphImageKey, CachedGlyphImage>,
    byte_budget: usize,
    bytes: usize,
    clock: u64,
    metrics: TerminalGlyphCacheMetrics,
}

/// Observable cache behavior used by the renderer benchmarks and regression tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalGlyphCacheMetrics {
    pub entries: usize,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub image_creations: u64,
    pub evictions: u64,
}

/// Opt-in counters for terminal work performed inside GPUI's prepaint and paint phases.
#[derive(Clone, Default)]
pub struct TerminalRenderMetrics {
    inner: Arc<Mutex<TerminalRenderMetricsSnapshot>>,
}

/// A cumulative terminal-renderer snapshot. Durations cover CPU work in this element only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalRenderMetricsSnapshot {
    /// Scenes rebuilt after the published frame or render inputs changed.
    pub scene_builds: u64,
    /// Rows whose paint fragments were rebuilt while constructing scenes.
    pub planned_rows: u64,
    /// Scene builds that used the row-fragment planner.
    pub incremental_plan_builds: u64,
    /// Scene builds that used the full planner.
    pub full_plan_builds: u64,
    /// Render frames whose command and text buffers were reclaimed from the prior scene.
    pub render_frame_reuses: u64,
    /// Render frames rebuilt without reclaiming a prior scene.
    pub render_frame_cold_builds: u64,
    /// Render commands prepared against GPUI resources for changed content.
    pub prepared_commands: u64,
    /// Stable prepared commands moved from an earlier scene.
    pub reused_prepared_commands: u64,
    /// Scene builds that reused at least one stable prepared command.
    pub incremental_scene_builds: u64,
    /// Scene builds that prepared the complete ordered command stream.
    pub full_scene_builds: u64,
    /// Terminal element prepaint invocations.
    pub prepaint_calls: u64,
    /// Terminal element paint invocations.
    pub paint_calls: u64,
    /// Visible glyph-backed `paint_image` calls after content-mask culling.
    pub glyph_primitives: u64,
    /// Visible terminal-image `paint_image` calls after content-mask culling.
    pub image_primitives: u64,
    /// Glyph or terminal-image cache hits while building scenes.
    pub cache_hits: u64,
    /// GPUI render images created while building scenes.
    pub cache_creations: u64,
    /// Superseded render images queued for retirement through GPUI.
    pub cache_retirements: u64,
    /// CPU time inside terminal prepaint, excluding GPUI's surrounding window work.
    pub prepaint_cpu: Duration,
    /// CPU time inside terminal paint, excluding GPUI's platform submission.
    pub paint_cpu: Duration,
}

impl TerminalRenderMetrics {
    pub fn snapshot(&self) -> TerminalRenderMetricsSnapshot {
        *self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn reset(&self) {
        *self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            TerminalRenderMetricsSnapshot::default();
    }

    fn record_scene(
        &self,
        cache_hits: u64,
        cache_creations: u64,
        cache_retirements: u64,
        prepared_commands: usize,
        reused_prepared_commands: usize,
    ) {
        let mut metrics = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        metrics.scene_builds = metrics.scene_builds.saturating_add(1);
        metrics.cache_hits = metrics.cache_hits.saturating_add(cache_hits);
        metrics.cache_creations = metrics.cache_creations.saturating_add(cache_creations);
        metrics.cache_retirements = metrics.cache_retirements.saturating_add(cache_retirements);
        metrics.prepared_commands = metrics
            .prepared_commands
            .saturating_add(u64::try_from(prepared_commands).unwrap_or(u64::MAX));
        metrics.reused_prepared_commands = metrics
            .reused_prepared_commands
            .saturating_add(u64::try_from(reused_prepared_commands).unwrap_or(u64::MAX));
        if reused_prepared_commands == 0 {
            metrics.full_scene_builds = metrics.full_scene_builds.saturating_add(1);
        } else {
            metrics.incremental_scene_builds = metrics.incremental_scene_builds.saturating_add(1);
        }
    }

    fn record_planning(&self, rebuilt_rows: usize, incremental: bool) {
        let mut metrics = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        metrics.planned_rows = metrics
            .planned_rows
            .saturating_add(u64::try_from(rebuilt_rows).unwrap_or(u64::MAX));
        if incremental {
            metrics.incremental_plan_builds = metrics.incremental_plan_builds.saturating_add(1);
        } else {
            metrics.full_plan_builds = metrics.full_plan_builds.saturating_add(1);
        }
    }

    fn record_lowering(&self, reused_render_frame: bool) {
        let mut metrics = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if reused_render_frame {
            metrics.render_frame_reuses = metrics.render_frame_reuses.saturating_add(1);
        } else {
            metrics.render_frame_cold_builds = metrics.render_frame_cold_builds.saturating_add(1);
        }
    }

    fn record_prepaint(&self, elapsed: Duration) {
        let mut metrics = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        metrics.prepaint_calls = metrics.prepaint_calls.saturating_add(1);
        metrics.prepaint_cpu = metrics.prepaint_cpu.saturating_add(elapsed);
    }

    fn record_paint(&self, elapsed: Duration, primitives: TerminalPrimitiveCounts) {
        let mut metrics = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        metrics.paint_calls = metrics.paint_calls.saturating_add(1);
        metrics.glyph_primitives = metrics.glyph_primitives.saturating_add(primitives.glyphs);
        metrics.image_primitives = metrics.image_primitives.saturating_add(primitives.images);
        metrics.paint_cpu = metrics.paint_cpu.saturating_add(elapsed);
    }
}

impl TerminalGlyphImageCache {
    fn new(byte_budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            byte_budget,
            bytes: 0,
            clock: 0,
            metrics: TerminalGlyphCacheMetrics::default(),
        }
    }

    fn get_or_insert_with(
        &mut self,
        key: TerminalGlyphImageKey,
        retired: &mut Vec<Arc<RenderImage>>,
        create: impl FnOnce() -> Option<Arc<RenderImage>>,
    ) -> Option<Arc<RenderImage>> {
        self.clock = self.clock.wrapping_add(1);
        if let Some(cached) = self.entries.get_mut(&key) {
            cached.last_used = self.clock;
            self.metrics.hits = self.metrics.hits.wrapping_add(1);
            return Some(Arc::clone(&cached.image));
        }
        self.metrics.misses = self.metrics.misses.wrapping_add(1);
        let image = create()?;
        let bytes = usize::try_from(key.width)
            .ok()?
            .checked_mul(usize::try_from(key.height).ok()?)?
            .checked_mul(4)?;
        self.metrics.image_creations = self.metrics.image_creations.wrapping_add(1);

        // A single oversized glyph is still paintable, but retaining it would violate the
        // byte bound. Its scene Arc owns it until retirement.
        if bytes > self.byte_budget {
            retired.push(Arc::clone(&image));
            return Some(image);
        }
        while self.bytes.saturating_add(bytes) > self.byte_budget {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(evicted.bytes);
                self.metrics.evictions = self.metrics.evictions.wrapping_add(1);
                retired.push(evicted.image);
            }
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            CachedGlyphImage {
                image: Arc::clone(&image),
                bytes,
                last_used: self.clock,
            },
        );
        Some(image)
    }

    fn metrics(&self) -> TerminalGlyphCacheMetrics {
        TerminalGlyphCacheMetrics {
            entries: self.entries.len(),
            bytes: self.bytes,
            ..self.metrics
        }
    }

    fn take_images(&mut self) -> Vec<Arc<RenderImage>> {
        self.bytes = 0;
        self.entries
            .drain()
            .map(|(_, cached)| cached.image)
            .collect()
    }
}

#[derive(Clone)]
struct TerminalImageKey {
    image_id: u32,
    image_width: u32,
    image_height: u32,
    image_format: KittyImageFormat,
    source: libghostty_vt::kitty::graphics::SourceRect,
    data: Arc<Vec<u8>>,
}

impl TerminalImageKey {
    fn new(placement: &bootty_terminal::terminal_image::KittyImagePlacement) -> Self {
        Self {
            image_id: placement.image_id,
            image_width: placement.image_width,
            image_height: placement.image_height,
            image_format: placement.image_format,
            source: placement.source,
            data: Arc::clone(&placement.data),
        }
    }
}

impl PartialEq for TerminalImageKey {
    fn eq(&self, other: &Self) -> bool {
        self.image_id == other.image_id
            && self.image_width == other.image_width
            && self.image_height == other.image_height
            && self.image_format == other.image_format
            && self.source == other.source
            && Arc::ptr_eq(&self.data, &other.data)
    }
}

impl Eq for TerminalImageKey {}

impl Hash for TerminalImageKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.image_id.hash(state);
        self.image_width.hash(state);
        self.image_height.hash(state);
        std::mem::discriminant(&self.image_format).hash(state);
        self.source.x.hash(state);
        self.source.y.hash(state);
        self.source.width.hash(state);
        self.source.height.hash(state);
        Arc::as_ptr(&self.data).hash(state);
    }
}

#[derive(Clone)]
pub struct GpuiTerminalInteraction {
    surface: TerminalSurface,
    frame: Arc<RenderFrame>,
    view: ViewTransform,
    cursor_bounds: Option<SurfaceRect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GpuiHyperlink {
    pub target: bootty_terminal::terminal_links::LinkTarget,
    pub url: String,
    pub rect: SurfaceRect,
}

impl GpuiTerminalInteraction {
    fn new(surface: TerminalSurface, frame: Arc<RenderFrame>) -> Self {
        let cursor_bounds = frame.cursor.map(|cursor| {
            let x = if cursor.at_wide_tail {
                cursor.x.saturating_sub(1)
            } else {
                cursor.x
            };
            surface.run_rect(x, cursor.y, if cursor.at_wide_tail { 2 } else { 1 })
        });
        Self {
            surface,
            frame,
            view: ViewTransform::IDENTITY,
            cursor_bounds,
        }
    }

    #[must_use]
    pub fn frame(&self) -> &RenderFrame {
        &self.frame
    }

    /// The exact geometry used when this frame was presented.
    #[must_use]
    pub const fn surface(&self) -> TerminalSurface {
        self.surface
    }

    /// The transform applied to the presented terminal geometry.
    pub const fn view_transform(&self) -> ViewTransform {
        self.view
    }

    /// Apply the render-only transform used by the corresponding terminal element.
    #[must_use]
    pub const fn with_view_transform(mut self, view: ViewTransform) -> Self {
        self.view = view;
        self
    }

    #[must_use]
    pub fn hyperlink_at(&self, point: SurfacePoint) -> Option<GpuiHyperlink> {
        // The terminal remains hit-testable only inside its original viewport. Zoomed content
        // may extend beyond that viewport visually, where GPUI clips it.
        if !self.surface.surface_rect().contains(point) {
            return None;
        }
        let point = self.view.inverse_point(point);
        if !self.surface.surface_rect().contains(point)
            || self.frame.cols == 0
            || self.frame.rows == 0
        {
            return None;
        }
        let grid = self.surface.surface_to_grid(point);
        let link = bootty_terminal::terminal_links::link_at(&self.frame, grid)?;
        let segment = link.segments.iter().find(|segment| segment.row == grid.y)?;
        Some(GpuiHyperlink {
            url: link.target.location(),
            target: link.target,
            rect: transform_rect(
                self.surface.run_rect(
                    segment.start_col,
                    segment.row,
                    segment
                        .end_col
                        .saturating_sub(segment.start_col)
                        .saturating_add(1),
                ),
                self.view,
            ),
        })
    }

    /// Current terminal cursor geometry for native IME candidate-window placement.
    #[must_use]
    pub fn cursor_bounds(&self) -> Option<SurfaceRect> {
        self.cursor_bounds
            .map(|bounds| transform_rect(bounds, self.view))
    }
}

#[derive(Default)]
struct SearchPulse {
    pulse: u64,
    started: Option<Instant>,
}

#[derive(Clone, Copy)]
struct SearchPulseOverlay {
    rect: SurfaceRect,
    progress: f32,
}

impl SearchPulse {
    fn overlay(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
    ) -> Option<SearchPulseOverlay> {
        if self.pulse != frame.search_pulse {
            self.pulse = frame.search_pulse;
            self.started = Some(Instant::now());
        }
        let active = frame.active_search_match?;
        let progress =
            self.started?.elapsed().as_secs_f32() / Duration::from_millis(180).as_secs_f32();
        if progress >= 1.0 {
            self.started = None;
            return None;
        }
        Some(SearchPulseOverlay {
            rect: surface.run_rect(
                active.start_col,
                active.row,
                active
                    .end_col
                    .saturating_sub(active.start_col)
                    .saturating_add(1),
            ),
            progress,
        })
    }
}

pub struct TerminalPrepaint {
    commands: Vec<GpuiPrepaintCommand>,
    copy_mode: Option<ShapedLine>,
}

impl TerminalPrepaint {
    /// Return the copy-mode label width after it was shaped with the active window text style.
    #[must_use]
    pub fn copy_mode_width(&self) -> Option<Pixels> {
        self.copy_mode.as_ref().map(|line| line.width)
    }
}

enum GpuiPrepaintCommand {
    Hidden,
    Visible,
}

struct TerminalSceneResources<'a> {
    baseline_adjustment: f32,
    text_atlas: &'a mut TextAtlasBuilder,
    glyph_images: &'a mut TerminalGlyphImageCache,
    glyph_layer_bins: &'a mut GlyphLayerBins,
    images: &'a mut HashMap<TerminalImageKey, Option<Arc<RenderImage>>>,
    retired_images: &'a mut Vec<Arc<RenderImage>>,
    metrics: Option<&'a TerminalRenderMetrics>,
}

impl GpuiTerminalScene {
    #[allow(clippy::too_many_arguments)]
    fn new_with_reuse(
        frame: TerminalRenderFrame,
        previous: Option<&mut Self>,
        prepared_rows: &mut PreparedRowPool,
        allow_reuse: bool,
        dirty_rows: &[bool],
        grid_min_y: f32,
        cell_height: f32,
        mut resources: TerminalSceneResources<'_>,
        pixels_per_point: f32,
    ) -> Self {
        if allow_reuse
            && let Some(previous) = previous
            && let Some(reused) = Self::try_prepare_reusing(
                &frame,
                previous,
                prepared_rows,
                dirty_rows,
                grid_min_y,
                cell_height,
                &mut resources,
                pixels_per_point,
            )
        {
            let text_spans = prepare_text_spans(
                &frame,
                &reused.commands,
                reused.stable_command_len,
                resources.glyph_layer_bins,
            );
            return Self {
                frame,
                text_spans,
                commands: reused.commands,
                stable_command_len: reused.stable_command_len,
                limits: Vec::new(),
            };
        }
        prepared_rows.clear();
        Self::new(frame, resources, pixels_per_point)
    }

    #[allow(clippy::too_many_arguments)]
    fn try_prepare_reusing(
        frame: &TerminalRenderFrame,
        previous: &mut Self,
        prepared_rows: &mut PreparedRowPool,
        dirty_rows: &[bool],
        grid_min_y: f32,
        cell_height: f32,
        resources: &mut TerminalSceneResources<'_>,
        pixels_per_point: f32,
    ) -> Option<ReusedPreparedScene> {
        // Image placement, overlays, and changed render contracts stay on the full path until
        // they have stable command identities that can be proven equal independently per row.
        if !resources.images.is_empty()
            || !previous.limits.is_empty()
            || previous.frame.surface != frame.surface
            || !cell_height.is_finite()
            || cell_height <= 0.0
            || frame.commands.iter().any(|command| {
                matches!(
                    command,
                    TerminalRenderCommand::Image(_)
                        | TerminalRenderCommand::KittyVirtualPlacement(_)
                )
            })
            || !prepared_rows.stage(previous, dirty_rows, grid_min_y, cell_height)
        {
            return None;
        }

        let stable_command_len = stable_command_len(frame);
        let metrics_before = resources.metrics.map(|metrics| {
            (
                metrics,
                resources.glyph_images.metrics(),
                resources.retired_images.len(),
            )
        });
        let mut commands = Vec::with_capacity(frame.commands.len());
        let mut prepared_commands = 0_usize;
        let mut reused_prepared_commands = 0_usize;

        for (index, command) in frame.commands.iter().enumerate() {
            if index < stable_command_len
                && let Some(row) =
                    terminal_command_row(command, grid_min_y, cell_height, dirty_rows.len())
                && !dirty_rows.get(row).copied().unwrap_or(true)
            {
                let (previous_index, prepared) = prepared_rows.take(row)?;
                if previous.frame.commands.get(previous_index) != Some(command) {
                    return None;
                }
                commands.push(prepared);
                reused_prepared_commands = reused_prepared_commands.saturating_add(1);
                continue;
            }

            commands.push(match command {
                TerminalRenderCommand::Text(text) => prepare_text_command(
                    text,
                    resources.text_atlas,
                    resources.glyph_images,
                    resources.retired_images,
                    pixels_per_point,
                    resources.baseline_adjustment,
                ),
                TerminalRenderCommand::Sprite(sprite) => prepare_sprite_command(
                    sprite,
                    resources.text_atlas,
                    resources.glyph_images,
                    resources.retired_images,
                    pixels_per_point,
                ),
                TerminalRenderCommand::FillRect(_)
                | TerminalRenderCommand::Decoration(_)
                | TerminalRenderCommand::Cursor(_) => GpuiPreparedCommand::None,
                TerminalRenderCommand::Image(_)
                | TerminalRenderCommand::KittyVirtualPlacement(_) => return None,
            });
            prepared_commands = prepared_commands.saturating_add(1);
        }
        if !prepared_rows.is_empty() || reused_prepared_commands == 0 {
            return None;
        }

        if let Some((metrics, glyph_cache_before, retired_before)) = metrics_before {
            let glyph_cache_after = resources.glyph_images.metrics();
            metrics.record_scene(
                glyph_cache_after
                    .hits
                    .saturating_sub(glyph_cache_before.hits),
                glyph_cache_after
                    .image_creations
                    .saturating_sub(glyph_cache_before.image_creations),
                u64::try_from(
                    resources
                        .retired_images
                        .len()
                        .saturating_sub(retired_before),
                )
                .unwrap_or(u64::MAX),
                prepared_commands,
                reused_prepared_commands,
            );
        }
        prepared_rows.clear();
        Some(ReusedPreparedScene {
            commands,
            stable_command_len,
        })
    }

    fn new(
        frame: TerminalRenderFrame,
        resources: TerminalSceneResources<'_>,
        pixels_per_point: f32,
    ) -> Self {
        let TerminalSceneResources {
            baseline_adjustment,
            text_atlas,
            glyph_images,
            glyph_layer_bins,
            images,
            retired_images,
            metrics,
        } = resources;
        // `TerminalRenderFrame` appends the cursor and its optional replacement glyphs after all
        // stable content. Retain that exact tail ordering while letting blink only toggle whether
        // the tail is submitted to GPUI.
        let stable_command_len = stable_command_len(&frame);
        let mut limits = Vec::new();
        let mut active_images = HashSet::new();
        let metrics_before =
            metrics.map(|metrics| (metrics, glyph_images.metrics(), retired_images.len()));
        let mut image_cache_hits = 0_u64;
        let mut image_cache_creations = 0_u64;
        text_atlas.begin_text_frame();
        let mut commands = Vec::with_capacity(frame.commands.len());
        for command in &frame.commands {
            let prepared = match command {
                TerminalRenderCommand::Text(text) => prepare_text_command(
                    text,
                    text_atlas,
                    glyph_images,
                    retired_images,
                    pixels_per_point,
                    baseline_adjustment,
                ),
                TerminalRenderCommand::Sprite(sprite) => prepare_sprite_command(
                    sprite,
                    text_atlas,
                    glyph_images,
                    retired_images,
                    pixels_per_point,
                ),
                TerminalRenderCommand::FillRect(_)
                | TerminalRenderCommand::Decoration(_)
                | TerminalRenderCommand::Cursor(_) => GpuiPreparedCommand::None,
                TerminalRenderCommand::Image(image) => {
                    let key = TerminalImageKey::new(image);
                    let (prepared, cache_hit) = cached_terminal_image(images, key.clone(), image);
                    if metrics.is_some() {
                        if cache_hit {
                            image_cache_hits = image_cache_hits.saturating_add(1);
                        } else if prepared.is_some() {
                            image_cache_creations = image_cache_creations.saturating_add(1);
                        }
                    }
                    if prepared.is_none() {
                        limits.push(GpuiTerminalLimit::InvalidKittyImage {
                            image_id: image.image_id,
                            placement_id: image.placement_id,
                        });
                    }
                    active_images.insert(key);
                    GpuiPreparedCommand::Image(prepared)
                }
                TerminalRenderCommand::KittyVirtualPlacement(placement) => {
                    limits.push(GpuiTerminalLimit::KittyVirtualPlacement {
                        image_id: placement.image_id,
                        placement_id: placement.placement_id,
                    });
                    GpuiPreparedCommand::None
                }
            };
            commands.push(prepared);
        }
        text_atlas.finish_text_frame();

        // Image generations are represented by distinct data handles. Retain only the images the
        // current scene can use so a long graphics session cannot grow this cache without bound.
        retired_images.extend(
            images
                .extract_if(|key, _| !active_images.contains(key))
                .filter_map(|(_, image)| image),
        );
        if let Some((metrics, glyph_cache_before, retired_before)) = metrics_before {
            let glyph_cache_after = glyph_images.metrics();
            metrics.record_scene(
                glyph_cache_after
                    .hits
                    .saturating_sub(glyph_cache_before.hits)
                    .saturating_add(image_cache_hits),
                glyph_cache_after
                    .image_creations
                    .saturating_sub(glyph_cache_before.image_creations)
                    .saturating_add(image_cache_creations),
                u64::try_from(retired_images.len().saturating_sub(retired_before))
                    .unwrap_or(u64::MAX),
                frame.commands.len(),
                0,
            );
        }
        let text_spans =
            prepare_text_spans(&frame, &commands, stable_command_len, glyph_layer_bins);
        Self {
            frame,
            commands,
            stable_command_len,
            text_spans,
            limits,
        }
    }
}

fn stable_command_len(frame: &TerminalRenderFrame) -> usize {
    frame
        .commands
        .iter()
        .position(|command| matches!(command, TerminalRenderCommand::Cursor(_)))
        .unwrap_or(frame.commands.len())
}

fn terminal_command_row(
    command: &TerminalRenderCommand,
    grid_min_y: f32,
    cell_height: f32,
    rows: usize,
) -> Option<usize> {
    let y = match command {
        TerminalRenderCommand::FillRect(command) => {
            (command.role == FillRole::CellBackground).then_some(command.rect.min_y)?
        }
        TerminalRenderCommand::Text(command) => command.rect.min_y,
        TerminalRenderCommand::Sprite(command) => command.rect.min_y,
        TerminalRenderCommand::Decoration(command) => command.start_y.min(command.end_y),
        TerminalRenderCommand::Cursor(_)
        | TerminalRenderCommand::Image(_)
        | TerminalRenderCommand::KittyVirtualPlacement(_) => return None,
    };
    let relative = (y - grid_min_y) / cell_height;
    if !relative.is_finite() || relative < 0.0 {
        return None;
    }
    let row = relative.floor().to_usize()?;
    (row < rows).then_some(row)
}

fn prepare_atlas_glyph(
    rect: SurfaceRect,
    quad: &TexturedGlyphQuad,
    atlas: &TextAtlasBuilder,
    glyph_images: &mut TerminalGlyphImageCache,
    retired_images: &mut Vec<Arc<RenderImage>>,
    scale: f32,
    baseline_adjustment: f32,
) -> Option<GpuiPreparedGlyph> {
    let spec = glyph_tile_spec(rect, quad, scale, baseline_adjustment)?;
    let key = TerminalGlyphImageKey {
        x: quad.atlas_entry.x.checked_add(spec.source_x)?,
        y: quad.atlas_entry.y.checked_add(spec.source_y)?,
        width: spec.width,
        height: spec.height,
        atlas_generation: atlas.atlas_resized_count(),
        color: [quad.color.r, quad.color.g, quad.color.b, quad.color.a],
    };
    let image = glyph_images.get_or_insert_with(key.clone(), retired_images, || {
        prepare_glyph_tile_image(
            atlas,
            quad,
            spec.source_x,
            spec.source_y,
            spec.width,
            spec.height,
        )
    })?;
    Some(GpuiPreparedGlyph {
        rect: spec.rect,
        image,
        key,
    })
}

fn prepare_text_command(
    text: &crate::terminal_render::TextCommand,
    text_atlas: &mut TextAtlasBuilder,
    glyph_images: &mut TerminalGlyphImageCache,
    retired_images: &mut Vec<Arc<RenderImage>>,
    pixels_per_point: f32,
    baseline_adjustment: f32,
) -> GpuiPreparedCommand {
    let scale = pixels_per_point.max(1.0);
    let mut glyphs = Vec::new();
    text_atlas.visit_text_command(text, scale, |atlas, quad| {
        if let Some(glyph) = prepare_atlas_glyph(
            text.rect,
            &quad,
            atlas,
            glyph_images,
            retired_images,
            scale,
            baseline_adjustment,
        ) {
            glyphs.push(glyph);
        }
    });
    GpuiPreparedCommand::Glyphs(glyphs)
}

fn prepare_sprite_command(
    sprite: &SpriteCommandBatch,
    text_atlas: &mut TextAtlasBuilder,
    glyph_images: &mut TerminalGlyphImageCache,
    retired_images: &mut Vec<Arc<RenderImage>>,
    pixels_per_point: f32,
) -> GpuiPreparedCommand {
    let primitives = sprite.glyph.commands_for(sprite.rect);
    if primitives
        .iter()
        .any(|command| matches!(command, SpriteCommand::ClearStrokePolyline { .. }))
    {
        // Destination-out is represented by the atlas's final alpha mask, then submitted
        // as one cached image so cleared pixels remain transparent.
        let scale = pixels_per_point.max(1.0);
        let quad = text_atlas.prepare_sprite_command(sprite, scale);
        let prepared = prepare_atlas_glyph(
            sprite.rect,
            &quad,
            text_atlas,
            glyph_images,
            retired_images,
            scale,
            0.0,
        );
        GpuiPreparedCommand::SpriteImage(prepared)
    } else {
        GpuiPreparedCommand::Sprite(primitives)
    }
}

impl GpuiTerminalElement {
    #[must_use]
    pub const fn with_background_opacity(mut self, opacity: f32) -> Self {
        self.background_opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self
    }

    fn layout_surface(&self) -> SurfaceRect {
        self.interaction
            .as_ref()
            .map_or(self.scene.frame.surface, |interaction| {
                interaction.surface.rect
            })
    }

    #[track_caller]
    #[must_use]
    pub fn new(frame: TerminalRenderFrame) -> Self {
        Self::new_with_metrics(frame, None)
    }

    #[track_caller]
    fn new_with_metrics(
        frame: TerminalRenderFrame,
        metrics: Option<TerminalRenderMetrics>,
    ) -> Self {
        let retired_images = Arc::<Mutex<Vec<_>>>::default();
        let mut pending_retirement = retired_images
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scene = GpuiTerminalScene::new(
            frame,
            TerminalSceneResources {
                baseline_adjustment: 0.0,
                text_atlas: &mut TextAtlasBuilder::default(),
                glyph_images: &mut TerminalGlyphImageCache::new(GLYPH_IMAGE_CACHE_BYTE_BUDGET),
                glyph_layer_bins: &mut Vec::new(),
                images: &mut HashMap::new(),
                retired_images: &mut pending_retirement,
                metrics: metrics.as_ref(),
            },
            1.0,
        );
        drop(pending_retirement);
        Self {
            scene: Arc::new(scene),
            background_opacity: 1.0,
            source_location: Location::caller(),
            interaction: None,
            view_transform: ViewTransform::IDENTITY,
            search_pulse: None,
            copy_mode_label: None,
            cursor_opacity: 1.0,
            cursor_glyphs: HashMap::new(),
            cursor_focused: true,
            retired_images,
            metrics,
        }
    }

    fn with_frame_facts(
        scene: Arc<GpuiTerminalScene>,
        interaction: GpuiTerminalInteraction,
        search_pulse: Option<SearchPulseOverlay>,
        cursor_opacity: f32,
        cursor_glyphs: HashMap<usize, Vec<GpuiPreparedGlyph>>,
        cursor_focused: bool,
        retired_images: Arc<Mutex<Vec<Arc<RenderImage>>>>,
    ) -> Self {
        let copy_mode_label = copy_mode_position_label(interaction.frame());
        Self {
            scene,
            background_opacity: 1.0,
            source_location: Location::caller(),
            interaction: Some(interaction),
            view_transform: ViewTransform::IDENTITY,
            search_pulse,
            copy_mode_label,
            cursor_opacity,
            cursor_glyphs,
            cursor_focused,
            retired_images,
            metrics: None,
        }
    }

    fn with_metrics(mut self, metrics: Option<TerminalRenderMetrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Apply a render-only transform without changing terminal layout or the retained scene.
    #[must_use]
    pub fn with_view_transform(mut self, view: ViewTransform) -> Self {
        self.view_transform = view;
        if let Some(interaction) = self.interaction.take() {
            self.interaction = Some(interaction.with_view_transform(view));
        }
        self
    }

    #[must_use]
    pub fn interaction(&self) -> Option<GpuiTerminalInteraction> {
        self.interaction.clone()
    }

    pub(crate) const fn with_search_pulse_from(mut self, source: &Self) -> Self {
        self.search_pulse = source.search_pulse;
        self
    }

    pub(crate) fn source_frame(&self) -> Option<&Arc<RenderFrame>> {
        self.interaction
            .as_ref()
            .map(|interaction| &interaction.frame)
    }

    #[must_use]
    pub fn frame(&self) -> &TerminalRenderFrame {
        &self.scene.frame
    }

    /// Unsupported commands remain in the ordered frame and are reported here.
    #[must_use]
    pub fn limits(&self) -> &[GpuiTerminalLimit] {
        &self.scene.limits
    }

    /// Prepared device-aligned glyph and raster-sprite rectangles, in command order.
    #[must_use]
    pub fn glyph_sprite_rects(&self) -> Vec<SurfaceRect> {
        self.scene
            .commands
            .iter()
            .flat_map(|command| match command {
                GpuiPreparedCommand::Glyphs(glyphs) => {
                    glyphs.iter().map(|glyph| glyph.rect).collect::<Vec<_>>()
                }
                GpuiPreparedCommand::SpriteImage(Some(sprite)) => vec![sprite.rect],
                _ => Vec::new(),
            })
            .collect()
    }
}

impl IntoElement for GpuiTerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for GpuiTerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        Some(self.source_location)
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = px(self.layout_surface().width()).into();
        style.size.height = px(self.layout_surface().height()).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let started = self.metrics.as_ref().map(|_| Instant::now());
        let offset = point(
            px(f32::from(bounds.origin.x) - self.layout_surface().min_x),
            px(f32::from(bounds.origin.y) - self.layout_surface().min_y),
        );
        let content_mask = window.content_mask().bounds;
        let commands = self
            .scene
            .frame
            .commands
            .iter()
            .zip(&self.scene.commands)
            .map(|(command, prepared)| {
                let visible = prepared_command_rect(command, prepared).is_some_and(|rect| {
                    content_mask.intersects(&bounds_for(rect, offset, self.view_transform))
                });
                match (visible, prepared) {
                    (false, _) => GpuiPrepaintCommand::Hidden,
                    (true, _) => GpuiPrepaintCommand::Visible,
                }
            })
            .collect();
        let copy_mode = self.copy_mode_label.as_ref().map(|label| {
            let zoom = self.view_transform.zoom;
            window.text_system().shape_line(
                label.clone().into(),
                px(12.0 * zoom),
                &[TextRun {
                    len: label.len(),
                    font: window.text_style().font(),
                    color: rgba(0xffff_ffe6).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
        });
        let prepaint = TerminalPrepaint {
            commands,
            copy_mode,
        };
        if let (Some(metrics), Some(started)) = (&self.metrics, started) {
            metrics.record_prepaint(started.elapsed());
        }
        prepaint
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepared: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let started = self.metrics.as_ref().map(|_| Instant::now());
        let mut primitives = TerminalPrimitiveCounts::default();
        // `RenderImage` does not evict its platform-atlas tile on drop. Retire superseded
        // terminal images through GPUI while the current window is available, before uploading
        // any replacement images for this scene.
        let retired_images = take_retired_images(&self.retired_images);
        for image in retired_images {
            cx.drop_image(image, Some(window));
        }

        let offset = point(
            px(f32::from(bounds.origin.x) - self.layout_surface().min_x),
            px(f32::from(bounds.origin.y) - self.layout_surface().min_y),
        );
        let scale_factor = window.scale_factor().max(1.0);

        primitives.append(self.paint_stable_commands(prepared, offset, scale_factor, window));
        if !self.cursor_focused {
            if self.scene.stable_command_len < self.scene.frame.commands.len() {
                primitives.append(self.paint_scene_command(
                    self.scene.stable_command_len,
                    prepared,
                    TerminalCommandPaint {
                        offset,
                        scale_factor,
                        view: self.view_transform,
                        opacity: 1.0,
                        cursor_shape: Some(CursorShape::HollowBlock),
                    },
                    window,
                ));
            }
        } else if self.cursor_opacity > 0.0 {
            for index in self.scene.stable_command_len..self.scene.frame.commands.len() {
                primitives.append(self.paint_scene_command(
                    index,
                    prepared,
                    TerminalCommandPaint {
                        offset,
                        scale_factor,
                        view: self.view_transform,
                        opacity: self.cursor_opacity,
                        cursor_shape: None,
                    },
                    window,
                ));
            }
        }

        self.paint_search_pulse(offset, scale_factor, window);

        if let (Some(line), Some(interaction)) = (&prepared.copy_mode, &self.interaction) {
            // Shaping already uses the zoomed font; the rectangle below is still logical.
            let width = f32::from(line.width) / self.view_transform.zoom;
            let height = 18.0;
            let right_clearance = interaction.frame.scrollbar.map_or(0.0, |_| 16.0);
            let rect = SurfaceRect::from_min_size(
                interaction.surface.rect.max_x - width - right_clearance - 12.0,
                interaction.surface.rect.min_y + 6.0,
                width + 12.0,
                height,
            );
            window.paint_quad(fill(
                snapped_bounds_for(rect, offset, scale_factor, self.view_transform),
                rgba(0x0000_00b8),
            ));
            let _ = line.paint(
                translated_snapped_point(
                    rect.min_x + 6.0,
                    rect.min_y + 2.0,
                    offset,
                    scale_factor,
                    self.view_transform,
                ),
                px(14.0 * self.view_transform.zoom),
                gpui_kit::TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        if let (Some(metrics), Some(started)) = (&self.metrics, started) {
            metrics.record_paint(started.elapsed(), primitives);
        }
    }
}

impl GpuiTerminalElement {
    fn paint_search_pulse(&self, offset: Point<Pixels>, scale_factor: f32, window: &mut Window) {
        if let Some(pulse) = self.search_pulse {
            let expansion = 7.0f32.mul_add(pulse.progress, 2.0);
            let rect = SurfaceRect {
                min_x: pulse.rect.min_x - expansion,
                min_y: pulse.rect.min_y - expansion,
                max_x: pulse.rect.max_x + expansion,
                max_y: pulse.rect.max_y + expansion,
            };
            let alpha = ((1.0 - pulse.progress) * 180.0)
                .round()
                .to_u8()
                .unwrap_or(0);
            let color = rgba(u32::from_be_bytes([255, 238, 128, alpha]));
            let stroke = 2.0;
            for edge in [
                SurfaceRect::from_min_size(rect.min_x, rect.min_y, rect.width(), stroke),
                SurfaceRect::from_min_size(rect.min_x, rect.max_y - stroke, rect.width(), stroke),
                SurfaceRect::from_min_size(rect.min_x, rect.min_y, stroke, rect.height()),
                SurfaceRect::from_min_size(rect.max_x - stroke, rect.min_y, stroke, rect.height()),
            ] {
                window.paint_quad(fill(
                    snapped_bounds_for(edge, offset, scale_factor, self.view_transform),
                    color,
                ));
            }
            window.request_animation_frame();
        }
    }

    fn paint_stable_commands(
        &self,
        prepared: &TerminalPrepaint,
        offset: Point<Pixels>,
        scale_factor: f32,
        window: &mut Window,
    ) -> TerminalPrimitiveCounts {
        let mut primitives = TerminalPrimitiveCounts::default();
        let mut index = 0_usize;
        while let Some(command) = self.scene.frame.commands.get(index)
            && index < self.scene.stable_command_len
        {
            let mut end = index.saturating_add(1);
            if let Some(span) = self.scene.text_spans.get(&index) {
                primitives.append(self.paint_text_span(span, prepared, offset, window));
                index = span.end;
                continue;
            }
            let mut layer =
                disjoint_command_bounds(command, offset, scale_factor, self.view_transform);
            if let Some(rect) = layer.as_mut() {
                while let Some(next) = self.scene.frame.commands.get(end)
                    && end < self.scene.stable_command_len
                {
                    let Some(next) =
                        disjoint_command_bounds(next, offset, scale_factor, self.view_transform)
                    else {
                        break;
                    };
                    if rect.intersects(&next) {
                        break;
                    }
                    *rect = rect.union(&next);
                    end = end.saturating_add(1);
                }
            }
            let mut paint = |window: &mut Window| {
                for command in index..end {
                    primitives.append(self.paint_scene_command(
                        command,
                        prepared,
                        TerminalCommandPaint {
                            offset,
                            scale_factor,
                            view: self.view_transform,
                            opacity: 1.0,
                            cursor_shape: None,
                        },
                        window,
                    ));
                }
            };
            if let Some(rect) = layer.filter(|_| end > index.saturating_add(1)) {
                // Disjoint ink can share one native layer across text runs and rows.
                // Overlapping glyphs, images and overlays retain their original ordering.
                window.paint_layer(rect, paint);
            } else {
                paint(window);
            }
            index = end;
        }
        primitives
    }

    fn paint_text_span(
        &self,
        span: &PreparedTextSpan,
        prepared: &TerminalPrepaint,
        offset: Point<Pixels>,
        window: &mut Window,
    ) -> TerminalPrimitiveCounts {
        let mut primitives = TerminalPrimitiveCounts::default();
        for layer in &span.layers {
            let bounds = SurfaceRect::from_min_size(
                layer.bounds.origin.x,
                layer.bounds.origin.y,
                layer.bounds.size.width,
                layer.bounds.size.height,
            );
            window.paint_layer(bounds_for(bounds, offset, self.view_transform), |window| {
                for &(command, glyph) in &layer.glyphs {
                    if matches!(
                        prepared.commands.get(command),
                        None | Some(GpuiPrepaintCommand::Hidden)
                    ) {
                        continue;
                    }
                    let Some(GpuiPreparedCommand::Glyphs(glyphs)) =
                        self.scene.commands.get(command)
                    else {
                        continue;
                    };
                    let Some(glyph) = glyphs.get(glyph) else {
                        continue;
                    };
                    let bounds = bounds_for(glyph.rect, offset, self.view_transform);
                    let _ = window.paint_image(
                        bounds,
                        padded_glyph_bounds(bounds, &glyph.image),
                        gpui_kit::Corners::default(),
                        Arc::clone(&glyph.image),
                        0,
                        false,
                    );
                    primitives.glyphs = primitives.glyphs.saturating_add(1);
                }
            });
        }
        primitives
    }

    fn paint_text_commands(
        &self,
        commands: std::ops::Range<usize>,
        prepared: &TerminalPrepaint,
        offset: Point<Pixels>,
        view: ViewTransform,
        window: &mut Window,
    ) -> TerminalPrimitiveCounts {
        let mut glyphs = commands
            .filter_map(|index| {
                if matches!(
                    prepared.commands.get(index),
                    None | Some(GpuiPrepaintCommand::Hidden)
                ) {
                    return None;
                }
                let GpuiPreparedCommand::Glyphs(glyphs) = self.scene.commands.get(index)? else {
                    return None;
                };
                Some(self.cursor_glyphs.get(&index).unwrap_or(glyphs).iter())
            })
            .flatten()
            .peekable();
        let mut primitives = TerminalPrimitiveCounts::default();
        while let Some(first) = glyphs.peek() {
            let mut bounds = bounds_for(first.rect, offset, view);
            let mut count = 1_usize;
            // Style boundaries do not change paint order. Split only where actual ink
            // overlaps, retaining the order of combining glyphs and overhangs.
            for next in glyphs.clone().skip(1) {
                let next = bounds_for(next.rect, offset, view);
                if bounds.intersects(&next) {
                    break;
                }
                bounds = bounds.union(&next);
                count = count.saturating_add(1);
            }
            let mut paint = |window: &mut Window| {
                for glyph in glyphs.by_ref().take(count) {
                    let bounds = bounds_for(glyph.rect, offset, view);
                    let _ = window.paint_image(
                        bounds,
                        padded_glyph_bounds(bounds, &glyph.image),
                        gpui_kit::Corners::default(),
                        Arc::clone(&glyph.image),
                        0,
                        false,
                    );
                }
            };
            if count > 1 {
                window.paint_layer(bounds, paint);
            } else {
                paint(window);
            }
            primitives.glyphs = primitives
                .glyphs
                .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        }
        primitives
    }

    fn paint_prepared_sprite(
        &self,
        index: usize,
        command: &SpriteCommandBatch,
        paint: TerminalCommandPaint,
        window: &mut Window,
    ) -> TerminalPrimitiveCounts {
        let TerminalCommandPaint {
            offset,
            scale_factor,
            view,
            opacity,
            ..
        } = paint;
        let mut primitives = TerminalPrimitiveCounts::default();
        match self.scene.commands.get(index) {
            Some(GpuiPreparedCommand::Sprite(primitives)) => {
                paint_sprite(
                    command,
                    primitives,
                    offset,
                    scale_factor,
                    view,
                    opacity,
                    window,
                );
            }
            Some(GpuiPreparedCommand::SpriteImage(Some(sprite))) if opacity > 0.0 => {
                let sprite = self
                    .cursor_glyphs
                    .get(&index)
                    .and_then(|glyphs| glyphs.first())
                    .unwrap_or(sprite);
                let bounds = snapped_bounds_for(sprite.rect, offset, scale_factor, view);
                let _ = window.paint_image(
                    bounds,
                    padded_glyph_bounds(bounds, &sprite.image),
                    gpui_kit::Corners::default(),
                    Arc::clone(&sprite.image),
                    0,
                    false,
                );
                primitives.glyphs = 1;
            }
            _ => {}
        }
        primitives
    }

    fn paint_scene_command(
        &self,
        index: usize,
        prepared: &TerminalPrepaint,
        paint: TerminalCommandPaint,
        window: &mut Window,
    ) -> TerminalPrimitiveCounts {
        let TerminalCommandPaint {
            offset,
            scale_factor,
            view,
            opacity,
            cursor_shape,
        } = paint;
        if matches!(
            prepared.commands.get(index),
            None | Some(GpuiPrepaintCommand::Hidden)
        ) {
            return TerminalPrimitiveCounts::default();
        }
        let Some(command) = self.scene.frame.commands.get(index) else {
            return TerminalPrimitiveCounts::default();
        };
        let mut primitives = TerminalPrimitiveCounts::default();
        match command {
            TerminalRenderCommand::FillRect(command) => {
                window.paint_quad(fill(
                    snapped_bounds_for(command.rect, offset, scale_factor, view),
                    color_with_alpha(
                        command.color,
                        if command.role == FillRole::SurfaceBackground {
                            opacity * self.background_opacity
                        } else {
                            opacity
                        },
                    ),
                ));
            }
            TerminalRenderCommand::Text(_) => {
                if opacity > 0.0 {
                    primitives.append(self.paint_text_commands(
                        index..index.saturating_add(1),
                        prepared,
                        offset,
                        view,
                        window,
                    ));
                }
            }
            TerminalRenderCommand::Sprite(command) => {
                primitives.append(self.paint_prepared_sprite(index, command, paint, window));
            }
            TerminalRenderCommand::Decoration(command) => {
                paint_decoration(command, offset, scale_factor, view, window);
            }
            TerminalRenderCommand::Cursor(command) => {
                paint_cursor(
                    command,
                    cursor_shape.unwrap_or(command.shape),
                    offset,
                    scale_factor,
                    view,
                    opacity,
                    window,
                );
            }
            TerminalRenderCommand::Image(image) => {
                let Some(GpuiPreparedCommand::Image(Some(data))) = self.scene.commands.get(index)
                else {
                    return primitives;
                };
                let bounds = snapped_bounds_for(image.destination, offset, scale_factor, view);
                let _ = window.paint_image(
                    bounds,
                    bounds,
                    gpui_kit::Corners::default(),
                    Arc::clone(data),
                    0,
                    false,
                );
                primitives.images = 1;
            }
            TerminalRenderCommand::KittyVirtualPlacement(_) => {}
        }
        primitives
    }
}

#[derive(Clone, Copy, Default)]
struct TerminalPrimitiveCounts {
    glyphs: u64,
    images: u64,
}

impl TerminalPrimitiveCounts {
    const fn append(&mut self, rhs: Self) {
        self.glyphs = self.glyphs.saturating_add(rhs.glyphs);
        self.images = self.images.saturating_add(rhs.images);
    }
}

#[derive(Clone, Copy)]
struct TerminalCommandPaint {
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
    opacity: f32,
    cursor_shape: Option<CursorShape>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct GlyphTileSpec {
    rect: SurfaceRect,
    source_x: u32,
    source_y: u32,
    width: u32,
    height: u32,
}

fn glyph_tile_spec(
    rect: SurfaceRect,
    quad: &TexturedGlyphQuad,
    scale: f32,
    baseline_adjustment: f32,
) -> Option<GlyphTileSpec> {
    // The rasterizer centers glyphs by default; only an explicit user adjustment moves the tile.
    let baseline_adjustment = if baseline_adjustment.is_finite() {
        baseline_adjustment
    } else {
        0.0
    };
    if !scale.is_finite()
        || scale <= 0.0
        || !quad.rect.min_x.is_finite()
        || !quad.rect.min_y.is_finite()
        || quad.atlas_entry.width == 0
        || quad.atlas_entry.height == 0
    {
        return None;
    }
    // Atlas tiles already own glyph fitting. A text run is a style/shaping span,
    // not a clip: symbols may use spare cells and baseline shifts move the whole ink.
    let min_x = rect.min_x + ((quad.rect.min_x - rect.min_x) * scale).round() / scale;
    let min_y = rect.min_y
        + (((quad.rect.min_y - rect.min_y) * scale).round()
            - (baseline_adjustment * scale).round())
            / scale;
    let width = quad.atlas_entry.width;
    let height = quad.atlas_entry.height;
    Some(GlyphTileSpec {
        rect: SurfaceRect::from_min_size(
            min_x,
            min_y,
            width.to_f32()? / scale,
            height.to_f32()? / scale,
        ),
        source_x: 0,
        source_y: 0,
        width,
        height,
    })
}

fn prepare_glyph_tile_image(
    atlas: &TextAtlasBuilder,
    quad: &TexturedGlyphQuad,
    source_x: u32,
    source_y: u32,
    width: u32,
    height: u32,
) -> Option<Arc<RenderImage>> {
    let tile = atlas.bgra_tile(quad)?;
    let source_width = usize::try_from(quad.atlas_entry.width).ok()?;
    let source_x = usize::try_from(source_x).ok()?;
    let source_y = usize::try_from(source_y).ok()?;
    let crop_width = usize::try_from(width).ok()?;
    // GPUI's linear sampler can reach adjacent atlas allocations at a tile's edge. Keep a
    // transparent texel around our crop; paint only the original interior, at its original size.
    let padded_width = width.checked_add(2)?;
    let padded_height = height.checked_add(2)?;
    let stride = usize::try_from(padded_width).ok()?.checked_mul(4)?;
    let mut pixels = vec![0; stride.checked_mul(usize::try_from(padded_height).ok()?)?];
    for row in 0..usize::try_from(height).ok()? {
        let start = source_y
            .checked_add(row)?
            .checked_mul(source_width)?
            .checked_add(source_x)?
            .checked_mul(4)?;
        let len = crop_width.checked_mul(4)?;
        let destination = row.checked_add(1)?.checked_mul(stride)?.checked_add(4)?;
        pixels
            .get_mut(destination..destination.checked_add(len)?)?
            .copy_from_slice(tile.get(start..start.checked_add(len)?)?);
    }
    let buffer = image::ImageBuffer::from_raw(padded_width, padded_height, pixels)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

fn padded_glyph_bounds(bounds: Bounds<Pixels>, image: &RenderImage) -> Bounds<Pixels> {
    let size = image.size(0);
    let Some(width) = size
        .width
        .0
        .checked_sub(2)
        .filter(|width| *width > 0)
        .and_then(|width| width.to_f32())
    else {
        return bounds;
    };
    let Some(height) = size
        .height
        .0
        .checked_sub(2)
        .filter(|height| *height > 0)
        .and_then(|height| height.to_f32())
    else {
        return bounds;
    };
    let x = f32::from(bounds.size.width) / width;
    let y = f32::from(bounds.size.height) / height;
    Bounds::from_corners(
        point(
            px(f32::from(bounds.left()) - x),
            px(f32::from(bounds.top()) - y),
        ),
        point(
            px(f32::from(bounds.right()) + x),
            px(f32::from(bounds.bottom()) + y),
        ),
    )
}

fn prepare_glyph_opacity_image(source: &RenderImage, opacity: u8) -> Option<Arc<RenderImage>> {
    let size = source.size(0);
    let width = u32::try_from(size.width.0).ok()?;
    let height = u32::try_from(size.height.0).ok()?;
    let mut pixels = source.as_bytes(0)?.to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] =
            u8::try_from(u16::from(pixel[3]).saturating_mul(u16::from(opacity)) / 255).ok()?;
    }
    let buffer = image::ImageBuffer::from_raw(width, height, pixels)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

fn cached_terminal_image(
    images: &mut HashMap<TerminalImageKey, Option<Arc<RenderImage>>>,
    key: TerminalImageKey,
    placement: &bootty_terminal::terminal_image::KittyImagePlacement,
) -> (Option<Arc<RenderImage>>, bool) {
    match images.entry(key) {
        Entry::Occupied(entry) => (entry.get().clone(), true),
        Entry::Vacant(entry) => (entry.insert(prepare_image(placement)).clone(), false),
    }
}

fn prepare_image(
    placement: &bootty_terminal::terminal_image::KittyImagePlacement,
) -> Option<Arc<RenderImage>> {
    let pixels = rgba_image_pixels(placement)?;
    let source = placement.source;
    let max_x = source.x.checked_add(source.width)?;
    let max_y = source.y.checked_add(source.height)?;
    if source.width == 0
        || source.height == 0
        || max_x > placement.image_width
        || max_y > placement.image_height
    {
        return None;
    }

    let row_bytes = usize::try_from(placement.image_width)
        .ok()?
        .checked_mul(4)?;
    let crop_row_bytes = usize::try_from(source.width).ok()?.checked_mul(4)?;
    let crop_height = usize::try_from(source.height).ok()?;
    let mut bgra = Vec::with_capacity(crop_row_bytes.checked_mul(crop_height)?);
    for y in source.y..max_y {
        let start = usize::try_from(y)
            .ok()?
            .checked_mul(row_bytes)?
            .checked_add(usize::try_from(source.x).ok()?.checked_mul(4)?)?;
        let row = pixels.get(start..start.checked_add(crop_row_bytes)?)?;
        for pixel in row.as_chunks::<4>().0 {
            bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    let buffer = image::ImageBuffer::from_raw(source.width, source.height, bgra)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

fn rgba_image_pixels(
    placement: &bootty_terminal::terminal_image::KittyImagePlacement,
) -> Option<Vec<u8>> {
    let pixels =
        usize::try_from(placement.image_width.checked_mul(placement.image_height)?).ok()?;
    let channels = match placement.image_format {
        KittyImageFormat::Rgba => 4,
        KittyImageFormat::Rgb => 3,
        KittyImageFormat::GrayAlpha => 2,
        KittyImageFormat::Gray => 1,
        KittyImageFormat::Png => return decode_png_rgba(placement),
        _ => return None,
    };
    let expected = pixels.checked_mul(channels)?;
    expand_rgba(placement.data.get(..expected)?, channels, pixels)
}

fn decode_png_rgba(
    placement: &bootty_terminal::terminal_image::KittyImagePlacement,
) -> Option<Vec<u8>> {
    let image = image::load_from_memory_with_format(&placement.data, RasterImageFormat::Png)
        .ok()?
        .into_rgba8();
    if image.width() != placement.image_width || image.height() != placement.image_height {
        return None;
    }
    Some(image.into_raw())
}

fn expand_rgba(data: &[u8], channels: usize, pixels: usize) -> Option<Vec<u8>> {
    let data = data.get(..pixels.checked_mul(channels)?)?;
    let mut rgba = Vec::with_capacity(pixels.checked_mul(4)?);
    for pixel in data.chunks_exact(channels) {
        match pixel {
            [r, g, b, a] => rgba.extend_from_slice(&[*r, *g, *b, *a]),
            [r, g, b] => rgba.extend_from_slice(&[*r, *g, *b, 255]),
            [gray, alpha] => rgba.extend_from_slice(&[*gray, *gray, *gray, *alpha]),
            [gray] => rgba.extend_from_slice(&[*gray, *gray, *gray, 255]),
            _ => return None,
        }
    }
    (rgba.len() == pixels.checked_mul(4)?).then_some(rgba)
}

fn disjoint_command_bounds(
    command: &TerminalRenderCommand,
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
) -> Option<Bounds<Pixels>> {
    match command {
        TerminalRenderCommand::FillRect(fill) if fill.role == FillRole::CellBackground => {
            Some(snapped_bounds_for(fill.rect, offset, scale_factor, view))
        }
        // Block glyphs are filled subrectangles contained within their terminal cell.
        TerminalRenderCommand::Sprite(sprite)
            if sprite.glyph.family == crate::terminal_sprite::SpriteFamily::Block =>
        {
            Some(snapped_bounds_for(sprite.rect, offset, scale_factor, view))
        }
        _ => None,
    }
}

fn paint_cursor(
    command: &CursorCommand,
    shape: CursorShape,
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
    opacity: f32,
    window: &mut Window,
) {
    if shape != CursorShape::HollowBlock {
        window.paint_quad(fill(
            snapped_bounds_for(command.fill_rect, offset, scale_factor, view),
            color_with_alpha(command.color, opacity),
        ));
        return;
    }

    let rect = command.rect;
    let stroke = 1.0_f32.min(rect.width()).min(rect.height());
    let edges = [
        SurfaceRect::from_min_size(rect.min_x, rect.min_y, rect.width(), stroke),
        SurfaceRect::from_min_size(rect.min_x, rect.max_y - stroke, rect.width(), stroke),
        SurfaceRect::from_min_size(rect.min_x, rect.min_y, stroke, rect.height()),
        SurfaceRect::from_min_size(rect.max_x - stroke, rect.min_y, stroke, rect.height()),
    ];
    for edge in edges {
        window.paint_quad(fill(
            snapped_bounds_for(edge, offset, scale_factor, view),
            color_with_alpha(command.color, opacity),
        ));
    }
}

fn paint_decoration(
    command: &LineCommand,
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
    window: &mut Window,
) {
    match command.style {
        DecorationStyle::Double => {
            paint_line(command, offset, scale_factor, view, 0.0, None, window);
            paint_line(command, offset, scale_factor, view, 2.0, None, window);
        }
        DecorationStyle::Dotted => {
            paint_line(
                command,
                offset,
                scale_factor,
                view,
                0.0,
                Some(&[px(1.0), px(2.0)]),
                window,
            );
        }
        DecorationStyle::Dashed => {
            paint_line(
                command,
                offset,
                scale_factor,
                view,
                0.0,
                Some(&[px(4.0), px(3.0)]),
                window,
            );
        }
        DecorationStyle::Curly => paint_curly_line(command, offset, scale_factor, view, window),
        DecorationStyle::Single | DecorationStyle::Strikethrough | DecorationStyle::Overline => {
            paint_line(command, offset, scale_factor, view, 0.0, None, window);
        }
    }
}

fn paint_line(
    command: &LineCommand,
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
    y_offset: f32,
    dash: Option<&[Pixels]>,
    window: &mut Window,
) {
    let mut builder = PathBuilder::stroke(px(1.0 * view.zoom));
    if let Some(dash) = dash {
        let dash = dash
            .iter()
            .map(|length| px(f32::from(*length) * view.zoom))
            .collect::<Vec<_>>();
        builder = builder.dash_array(&dash);
    }
    builder.move_to(translated_snapped_point(
        command.start_x,
        command.start_y + y_offset,
        offset,
        scale_factor,
        view,
    ));
    builder.line_to(translated_snapped_point(
        command.end_x,
        command.end_y + y_offset,
        offset,
        scale_factor,
        view,
    ));
    if let Ok(path) = builder.build() {
        window.paint_path(path, color(command.color));
    }
}

fn paint_curly_line(
    command: &LineCommand,
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
    window: &mut Window,
) {
    let distance = (command.end_x - command.start_x).abs().max(1.0);
    let Some(segments) = distance.ceil().to_usize() else {
        return;
    };
    let mut builder = PathBuilder::stroke(px(1.0 * view.zoom));
    for index in 0..=segments {
        let progress = index.to_f32().unwrap_or_default() / segments.to_f32().unwrap_or(1.0);
        let x = (command.end_x - command.start_x).mul_add(progress, command.start_x);
        let y = (command.end_y - command.start_y).mul_add(progress, command.start_y)
            + (progress * distance * std::f32::consts::PI).sin();
        let point = translated_snapped_point(x, y, offset, scale_factor, view);
        if index == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color(command.color));
    }
}

fn paint_sprite(
    command: &SpriteCommandBatch,
    primitives: &[SpriteCommand],
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
    opacity: f32,
    window: &mut Window,
) {
    for primitive in primitives {
        match primitive {
            SpriteCommand::FillRect { rect, alpha } => window.paint_quad(fill(
                snapped_bounds_for(*rect, offset, scale_factor, view),
                color_with_alpha(command.color, *alpha * opacity),
            )),
            SpriteCommand::FillPolygon { points, alpha, .. } => {
                let mut builder = PathBuilder::fill();
                for (index, sprite_point) in points.iter().enumerate() {
                    let point = translated_snapped_point(
                        sprite_point.x,
                        sprite_point.y,
                        offset,
                        scale_factor,
                        view,
                    );
                    if index == 0 {
                        builder.move_to(point);
                    } else {
                        builder.line_to(point);
                    }
                }
                builder.close();
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color_with_alpha(command.color, *alpha * opacity));
                }
            }
            SpriteCommand::StrokePolyline {
                points,
                width,
                alpha,
            } => {
                paint_sprite_polyline(
                    points,
                    *width,
                    color_with_alpha(command.color, *alpha * opacity),
                    offset,
                    scale_factor,
                    view,
                    window,
                );
            }
            // This command subtracts from the coverage accumulated by earlier sprite commands.
            // GPUI 0.2.2 has no destination-out blend primitive, so retain and report the limit.
            SpriteCommand::ClearStrokePolyline { .. } => {}
        }
    }
}

fn paint_sprite_polyline(
    points: &[crate::terminal_sprite::SpritePoint],
    width: f32,
    color_value: gpui_kit::Rgba,
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
    window: &mut Window,
) {
    let mut builder = PathBuilder::stroke(px(width.max(1.0) * view.zoom));
    for (index, sprite_point) in points.iter().enumerate() {
        let point =
            translated_snapped_point(sprite_point.x, sprite_point.y, offset, scale_factor, view);
        if index == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color_value);
    }
}

fn command_rect(command: &TerminalRenderCommand) -> Option<SurfaceRect> {
    match command {
        TerminalRenderCommand::FillRect(command) => Some(command.rect),
        TerminalRenderCommand::Text(command) => Some(command.rect),
        TerminalRenderCommand::Sprite(command) => Some(command.rect),
        TerminalRenderCommand::Decoration(command) => Some(SurfaceRect::from_min_size(
            command.start_x.min(command.end_x),
            command.start_y.min(command.end_y) - 2.0,
            (command.end_x - command.start_x).abs().max(1.0),
            (command.end_y - command.start_y).abs() + 4.0,
        )),
        TerminalRenderCommand::Cursor(command) => Some(command.rect),
        TerminalRenderCommand::Image(command) => Some(command.destination),
        TerminalRenderCommand::KittyVirtualPlacement(_) => None,
    }
}

fn prepared_command_rect(
    command: &TerminalRenderCommand,
    prepared: &GpuiPreparedCommand,
) -> Option<SurfaceRect> {
    match prepared {
        GpuiPreparedCommand::Glyphs(glyphs) => {
            glyphs
                .iter()
                .map(|glyph| glyph.rect)
                .reduce(|a, b| SurfaceRect {
                    min_x: a.min_x.min(b.min_x),
                    min_y: a.min_y.min(b.min_y),
                    max_x: a.max_x.max(b.max_x),
                    max_y: a.max_y.max(b.max_y),
                })
        }
        _ => command_rect(command),
    }
}

fn bounds_for(rect: SurfaceRect, offset: Point<Pixels>, view: ViewTransform) -> Bounds<Pixels> {
    let rect = transform_rect(rect, view);
    Bounds::from_corners(
        translated_point(rect.min_x, rect.min_y, offset),
        translated_point(rect.max_x, rect.max_y, offset),
    )
}

fn snapped_bounds_for(
    rect: SurfaceRect,
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
) -> Bounds<Pixels> {
    let rect = transform_rect(rect, view);
    let min = point(
        floor_to_device_pixel(px(f32::from(offset.x) + rect.min_x), scale_factor),
        floor_to_device_pixel(px(f32::from(offset.y) + rect.min_y), scale_factor),
    );
    let max = point(
        ceil_to_device_pixel(px(f32::from(offset.x) + rect.max_x), scale_factor),
        ceil_to_device_pixel(px(f32::from(offset.y) + rect.max_y), scale_factor),
    );
    Bounds::from_corners(min, max)
}

fn translated_point(x: f32, y: f32, offset: Point<Pixels>) -> Point<Pixels> {
    point(px(f32::from(offset.x) + x), px(f32::from(offset.y) + y))
}

fn translated_snapped_point(
    x: f32,
    y: f32,
    offset: Point<Pixels>,
    scale_factor: f32,
    view: ViewTransform,
) -> Point<Pixels> {
    let transformed = transform_point(SurfacePoint { x, y }, view);
    point(
        floor_to_device_pixel(px(f32::from(offset.x) + transformed.x), scale_factor),
        floor_to_device_pixel(px(f32::from(offset.y) + transformed.y), scale_factor),
    )
}

const fn transform_point(point: SurfacePoint, view: ViewTransform) -> SurfacePoint {
    SurfacePoint {
        x: point.x.mul_add(view.zoom, view.pan_x),
        y: point.y.mul_add(view.zoom, view.pan_y),
    }
}

const fn transform_rect(rect: SurfaceRect, view: ViewTransform) -> SurfaceRect {
    let min = transform_point(
        SurfacePoint {
            x: rect.min_x,
            y: rect.min_y,
        },
        view,
    );
    let max = transform_point(
        SurfacePoint {
            x: rect.max_x,
            y: rect.max_y,
        },
        view,
    );
    SurfaceRect {
        min_x: min.x.min(max.x),
        min_y: min.y.min(max.y),
        max_x: min.x.max(max.x),
        max_y: min.y.max(max.y),
    }
}

fn floor_to_device_pixel(value: Pixels, scale_factor: f32) -> Pixels {
    px((f32::from(value) * scale_factor).floor() / scale_factor)
}

fn ceil_to_device_pixel(value: Pixels, scale_factor: f32) -> Pixels {
    px((f32::from(value) * scale_factor).ceil() / scale_factor)
}

fn color(value: PlanColor) -> gpui_kit::Rgba {
    rgba(u32::from_be_bytes([value.r, value.g, value.b, value.a]))
}

fn color_with_alpha(value: PlanColor, alpha: f32) -> gpui_kit::Rgba {
    let alpha = (f32::from(value.a) * alpha.clamp(0.0, 1.0))
        .round()
        .to_u8()
        .unwrap_or(0);
    rgba(u32::from_be_bytes([value.r, value.g, value.b, alpha]))
}

fn copy_mode_position_label(frame: &RenderFrame) -> Option<String> {
    frame.copy_mode?;
    let cursor = frame.cursor?;
    let total = frame
        .scrollbar
        .map_or_else(|| u64::from(frame.rows), |scrollbar| scrollbar.total)
        .max(1);
    let offset = frame.scrollbar.map_or(0, |scrollbar| scrollbar.offset);
    let current = offset
        .saturating_add(u64::from(cursor.y))
        .saturating_add(1)
        .min(total);
    Some(format!("[{current}/{total}]"))
}

const fn is_transition_placeholder_frame(frame: &RenderFrame) -> bool {
    frame.cols == 0
        || frame.rows == 0
        || (frame.cells.is_empty()
            && frame.text.is_empty()
            && frame.images.placements.is_empty()
            && frame.images.virtual_placements.is_empty())
}

fn take_retired_images(queue: &Mutex<Vec<Arc<RenderImage>>>) -> Vec<Arc<RenderImage>> {
    let mut pending = queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (ready, retained): (Vec<_>, Vec<_>) = pending
        .drain(..)
        .partition(|image| Arc::strong_count(image) == 1);
    *pending = retained;
    ready
}
