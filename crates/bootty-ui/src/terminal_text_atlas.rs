use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use num_traits::ToPrimitive as _;
use smallvec::SmallVec;

mod atlas;
mod clusters;

pub use atlas::{
    GlyphAtlas, GlyphAtlasEntry, GlyphAtlasError, GlyphAtlasFaceKey, GlyphAtlasFormat,
    GlyphAtlasKey, GlyphAtlasTextKey,
};
pub use clusters::{ShapedCluster, TerminalTextShaper};
mod coretext;
mod font_library;
mod font_raster;
mod platform_raster;
mod shaping;
mod sprite_raster;

use atlas::{GlyphAtlasRecord, alpha_to_atlas_pixels, atlas_uv};
use clusters::{
    cluster_constraint_cells, is_color_emoji_cluster, is_printable_ascii, single_ascii_cluster,
};
use font_library::FontLibrary;
use font_raster::{RasterizeClusterRequest, rasterize_cluster};
use sprite_raster::rasterize_sprite_commands;

use crate::{
    paint_plan::PlanColor,
    terminal_render::{SpriteCommandBatch, TextCommand},
    terminal_text::{FontFeature, FontStyle, ResolvedFontFace},
};
use bootty_terminal::geometry::SurfaceRect;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexturedGlyphQuad {
    pub rect: SurfaceRect,
    pub uv: SurfaceRect,
    /// Atlas allocation valid until another glyph is prepared. Text visitors must copy its
    /// pixels immediately because the next insertion may recycle the bounded atlas.
    pub atlas_entry: GlyphAtlasEntry,
    pub color: PlanColor,
    /// Pixel snapping is appropriate for cell-filling primitives, but would rescale ordinary
    /// glyphs when fit-to-window leaves a fractional cell pitch.
    pub snap_to_pixel_grid: bool,
}

#[derive(Clone, Debug)]
struct AsciiGlyphAtlasRecord {
    dilation: u8,
    face: GlyphAtlasFaceKey,
    font_size_bits: u32,
    pixels_per_point_bits: u32,
    width: u32,
    height: u32,
    atlas_resized_count: u64,
    record: GlyphAtlasRecord,
}

struct ClusterGlyphRequest<'a> {
    command: &'a TextCommand,
    cluster: &'a ShapedCluster,
    face_key: GlyphAtlasFaceKey,
    pixels_per_point: f32,
    constraint_cells: u16,
    glyph_width: u32,
    glyph_height: u32,
}

#[derive(Clone, Debug)]
struct PreparedTextCommandCacheEntry {
    command: TextCommand,
    pixels_per_point_bits: u32,
    atlas_resized_count: Option<u64>,
    quads: Vec<TexturedGlyphQuad>,
}

/// Identity of a shaped run: shaping output depends on the text, the resolved
/// face, the font size, and the ordered font-feature policy. A run that merely
/// moved or reappeared keys to the same entry. Position, color, and atlas state
/// are deliberately excluded.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ShapedRunCacheKey {
    text: String,
    face: GlyphAtlasFaceKey,
    font_size_bits: u32,
    font_features: Arc<[FontFeature]>,
}

#[derive(Clone, Debug)]
struct ShapedRunCacheEntry {
    total_cells: u16,
    clusters: Vec<ShapedCluster>,
}

/// Bounds the shaped-run cache so unbounded unique output (e.g. streaming a huge log)
/// can't grow it without limit; the cache clears wholesale when the cap is hit. The
/// working set of interactive use and scrollback stays well under this.
const SHAPED_RUN_CACHE_CAP: usize = 1024;

#[derive(Clone)]
pub struct TextAtlasBuilder {
    shaper: TerminalTextShaper,
    atlas: GlyphAtlas,
    fonts: FontLibrary,
    platform_text_system: Option<Arc<dyn gpui_kit::PlatformTextSystem>>,
    face_cache: HashMap<ResolvedFontFace, GlyphAtlasFaceKey>,
    text_cache: HashMap<String, GlyphAtlasTextKey>,
    ascii_char_cache: [Option<GlyphAtlasTextKey>; 128],
    ascii_glyph_cache: [Option<AsciiGlyphAtlasRecord>; 128],
    char_cache: HashMap<char, GlyphAtlasTextKey>,
    sprite_face_key: GlyphAtlasFaceKey,
    clusters: Vec<ShapedCluster>,
    shaped_run_cache: HashMap<ShapedRunCacheKey, ShapedRunCacheEntry>,
    prepared_text_cache: Vec<PreparedTextCommandCacheEntry>,
    prepared_text_cache_cursor: usize,
    prepared_text_frame_active: bool,
}

impl std::fmt::Debug for TextAtlasBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextAtlasBuilder")
            .field("atlas", &self.atlas)
            .finish_non_exhaustive()
    }
}

impl Default for TextAtlasBuilder {
    fn default() -> Self {
        Self::from_atlas(GlyphAtlas::default())
    }
}

impl TextAtlasBuilder {
    /// # Errors
    /// Returns `CapacityExceeded` when the atlas dimensions require more pixel storage than supported.
    pub fn new(width: u32, height: u32) -> Result<Self, GlyphAtlasError> {
        Self::with_format(width, height, GlyphAtlasFormat::Alpha)
    }

    /// # Errors
    /// Returns `CapacityExceeded` when the atlas dimensions require more pixel storage than supported.
    pub fn new_rgba(width: u32, height: u32) -> Result<Self, GlyphAtlasError> {
        Self::with_format(width, height, GlyphAtlasFormat::Rgba)
    }

    /// Use the host's text service for native text coverage and color-glyph rasterization.
    ///
    /// # Errors
    /// Returns `CapacityExceeded` when the atlas dimensions require more pixel storage than supported.
    pub fn with_platform_text_system(
        width: u32,
        height: u32,
        platform_text_system: Arc<dyn gpui_kit::PlatformTextSystem>,
    ) -> Result<Self, GlyphAtlasError> {
        let mut builder = Self::new_rgba(width, height)?;
        builder.set_platform_text_system(platform_text_system);
        Ok(builder)
    }

    pub(crate) fn set_platform_text_system(
        &mut self,
        platform_text_system: Arc<dyn gpui_kit::PlatformTextSystem>,
    ) {
        self.platform_text_system = Some(platform_text_system);
    }

    /// # Errors
    /// Returns `CapacityExceeded` when the atlas dimensions require more pixel storage than supported.
    pub fn with_format(
        width: u32,
        height: u32,
        format: GlyphAtlasFormat,
    ) -> Result<Self, GlyphAtlasError> {
        Ok(Self::from_atlas(GlyphAtlas::with_format(
            width, height, format,
        )?))
    }

    fn from_atlas(atlas: GlyphAtlas) -> Self {
        Self {
            shaper: TerminalTextShaper,
            atlas,
            fonts: FontLibrary::new(),
            platform_text_system: None,
            face_cache: HashMap::new(),
            text_cache: HashMap::new(),
            ascii_char_cache: std::array::from_fn(|_| None),
            ascii_glyph_cache: std::array::from_fn(|_| None),
            char_cache: HashMap::new(),
            sprite_face_key: GlyphAtlasFaceKey::new(ResolvedFontFace {
                assignment: bootty_config::FontStyleAssignment::Automatic,
                family: "Ghostty Sprite".to_owned(),
                fallback_families: Vec::new(),
                style: FontStyle::Regular,
            }),
            clusters: Vec::new(),
            shaped_run_cache: HashMap::new(),
            prepared_text_cache: Vec::new(),
            prepared_text_cache_cursor: 0,
            prepared_text_frame_active: false,
        }
    }

    pub(crate) const fn begin_text_frame(&mut self) {
        self.prepared_text_frame_active = true;
        self.prepared_text_cache_cursor = 0;
    }

    pub(crate) fn finish_text_frame(&mut self) {
        if self.prepared_text_frame_active {
            self.prepared_text_cache
                .truncate(self.prepared_text_cache_cursor);
            self.prepared_text_frame_active = false;
        }
    }
    /// Recycle atlas storage after a host detects that a complete frame rebuild is required.
    pub fn reset_atlas_for_frame_rebuild(&mut self) {
        self.atlas.recycle();
    }

    /// Visit each glyph while its atlas pixels are valid, including runs larger than the atlas.
    /// Hosts must copy or upload the tile inside the callback rather than retaining its allocation.
    pub fn visit_text_command(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        mut visit: impl FnMut(&Self, TexturedGlyphQuad),
    ) {
        if self.prepared_text_frame_active {
            self.visit_text_command_cached(command, pixels_per_point, &mut visit);
        } else {
            self.visit_text_command_uncached(command, pixels_per_point, &mut visit);
        }
    }

    fn visit_text_command_cached(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        visit: &mut impl FnMut(&Self, TexturedGlyphQuad),
    ) {
        let cache_index = self.prepared_text_cache_cursor;
        self.prepared_text_cache_cursor = self.prepared_text_cache_cursor.saturating_add(1);
        let pixels_per_point_bits = pixels_per_point.to_bits();
        let atlas_resized_count = self.atlas.resized_count();

        if let Some(cached) = self.prepared_text_cache.get(cache_index)
            && cached.atlas_resized_count == Some(atlas_resized_count)
            && cached.pixels_per_point_bits == pixels_per_point_bits
            && cached.command == *command
        {
            for quad in &cached.quads {
                visit(self, *quad);
            }
            return;
        }

        let mut quads = Vec::new();
        self.visit_text_command_uncached(command, pixels_per_point, &mut |atlas, quad| {
            visit(atlas, quad);
            quads.push(quad);
        });
        let atlas_resized_count =
            (atlas_resized_count == self.atlas.resized_count()).then_some(atlas_resized_count);
        if atlas_resized_count.is_none() {
            // Earlier allocations may have been recycled while the visitor copied this run.
            quads.clear();
        }
        let cached = PreparedTextCommandCacheEntry {
            command: command.clone(),
            pixels_per_point_bits,
            atlas_resized_count,
            quads,
        };
        if let Some(slot) = self.prepared_text_cache.get_mut(cache_index) {
            *slot = cached;
        } else {
            self.prepared_text_cache.push(cached);
        }
    }

    fn visit_text_command_uncached(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        visit: &mut impl FnMut(&Self, TexturedGlyphQuad),
    ) {
        let face_key = self.intern_face(&command.face);
        self.visit_text_command_uncached_with_face(command, pixels_per_point, &face_key, visit);
    }
    fn visit_ascii_text_command_uncached_with_face(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        face_key: &GlyphAtlasFaceKey,
        visit: &mut impl FnMut(&Self, TexturedGlyphQuad),
    ) {
        let total_cells = u16::try_from(command.text.len()).unwrap_or(u16::MAX).max(1);
        let cell_width = command.rect.width() / f32::from(total_cells);
        let mut cluster = ShapedCluster {
            text: String::new(),
            cell: 0,
            cells: 1,
            is_whitespace: false,
            glyphs: SmallVec::new(),
        };

        for (cell, ch) in command.text.bytes().enumerate() {
            if ch == b' ' {
                continue;
            }
            let cell = u16::try_from(cell).unwrap_or(u16::MAX);
            cluster.text.clear();
            cluster.text.push(char::from(ch));
            cluster.cell = cell;
            cluster.is_whitespace = false;
            let rect = SurfaceRect::from_min_size(
                f32::mul_add(f32::from(cell), cell_width, command.rect.min_x),
                command.rect.min_y,
                cell_width,
                command.rect.height(),
            );
            let glyph_width = (rect.width() * pixels_per_point)
                .ceil()
                .max(1.0)
                .to_u32()
                .unwrap_or(u32::MAX);
            let glyph_height = (rect.height() * pixels_per_point)
                .ceil()
                .max(1.0)
                .to_u32()
                .unwrap_or(u32::MAX);
            let request = ClusterGlyphRequest {
                command,
                cluster: &cluster,
                face_key: face_key.clone(),
                pixels_per_point,
                constraint_cells: 1,
                glyph_width,
                glyph_height,
            };
            let record = self.prepare_ascii_cluster(ch, request);
            visit(
                self,
                self.textured_glyph_quad(record, rect, pixels_per_point, command.attrs.fg),
            );
        }
    }

    fn visit_text_command_uncached_with_face(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        face_key: &GlyphAtlasFaceKey,
        visit: &mut impl FnMut(&Self, TexturedGlyphQuad),
    ) {
        let mut clusters = std::mem::take(&mut self.clusters);
        // Shaping depends only on (text, face, font_size), so memoize it: scrolled or
        // repeated text reuses the shape and skips rustybuzz. Positioning and
        // (atlas-cached) rasterization still run per command, so output is identical.
        let cache_key = ShapedRunCacheKey {
            text: command.text.clone(),
            face: face_key.clone(),
            font_size_bits: command.font_size.to_bits(),
            font_features: Arc::clone(&command.font_features),
        };
        let (total_cells, cluster_len) = if let Some(entry) = self.shaped_run_cache.get(&cache_key)
        {
            clusters.clear();
            clusters.extend_from_slice(&entry.clusters);
            (entry.total_cells, entry.clusters.len())
        } else {
            let shaped = self.fonts.shape_into_clusters(
                &command.face,
                &command.text,
                command.font_size,
                &command.font_features,
                &mut clusters,
            );
            if let Some((total_cells, cluster_len)) = shaped {
                self.insert_shaped_run(cache_key, total_cells, clusters.iter().take(cluster_len));
                (total_cells, cluster_len)
            } else {
                // The font carries no ligature/contextual features (or shaping is
                // unavailable): keep the per-character fast paths unchanged.
                if is_printable_ascii(&command.text) {
                    self.clusters = clusters;
                    self.visit_ascii_text_command_uncached_with_face(
                        command,
                        pixels_per_point,
                        face_key,
                        visit,
                    );
                    return;
                }
                self.shaper
                    .shape_into_retained(&command.text, 0, &mut clusters)
            }
        };
        let cell_width = command.rect.width() / f32::from(total_cells);

        for (index, cluster) in clusters.iter().take(cluster_len).enumerate() {
            if cluster.is_whitespace {
                continue;
            }
            let constraint_cells = cluster_constraint_cells(
                index.checked_sub(1).and_then(|index| clusters.get(index)),
                cluster,
                index
                    .checked_add(1)
                    .filter(|index| *index < cluster_len)
                    .and_then(|index| clusters.get(index)),
            );
            let rect = SurfaceRect::from_min_size(
                f32::mul_add(f32::from(cluster.cell), cell_width, command.rect.min_x),
                command.rect.min_y,
                f32::from(constraint_cells) * cell_width,
                command.rect.height(),
            );
            // A color emoji fits its whole grid span (cells wide, cell tall) the way Ghostty draws
            // it; the platform raster scales the glyph's ink to that tile and centers it.
            let glyph_width = (rect.width() * pixels_per_point)
                .ceil()
                .max(1.0)
                .to_u32()
                .unwrap_or(u32::MAX);
            let glyph_height = (rect.height() * pixels_per_point)
                .ceil()
                .max(1.0)
                .to_u32()
                .unwrap_or(u32::MAX);
            let request = ClusterGlyphRequest {
                command,
                cluster,
                face_key: face_key.clone(),
                pixels_per_point,
                constraint_cells,
                glyph_width,
                glyph_height,
            };
            let record = if let Some(ch) = single_ascii_cluster(cluster) {
                self.prepare_ascii_cluster(ch, request)
            } else {
                self.prepare_cluster(request)
            };
            visit(
                self,
                self.textured_glyph_quad(record, rect, pixels_per_point, command.attrs.fg),
            );
        }
        self.clusters = clusters;
    }

    fn textured_glyph_quad(
        &self,
        record: GlyphAtlasRecord,
        rect: SurfaceRect,
        pixels_per_point: f32,
        foreground: PlanColor,
    ) -> TexturedGlyphQuad {
        let entry = record.entry;
        let [offset_x, offset_y] = record.offset;
        let color = if record.is_color_glyph {
            PlanColor {
                r: 255,
                g: 255,
                b: 255,
                a: foreground.a,
            }
        } else {
            foreground
        };
        TexturedGlyphQuad {
            rect: SurfaceRect::from_min_size(
                rect.min_x + offset_x.to_f32().unwrap_or_default() / pixels_per_point,
                rect.min_y + offset_y.to_f32().unwrap_or_default() / pixels_per_point,
                entry.width.to_f32().unwrap_or_default() / pixels_per_point,
                entry.height.to_f32().unwrap_or_default() / pixels_per_point,
            ),
            uv: atlas_uv(self.atlas.size(), entry),
            atlas_entry: entry,
            color,
            snap_to_pixel_grid: false,
        }
    }

    fn insert_shaped_run<'a>(
        &mut self,
        key: ShapedRunCacheKey,
        total_cells: u16,
        clusters: impl Iterator<Item = &'a ShapedCluster>,
    ) {
        if self.shaped_run_cache.len() >= SHAPED_RUN_CACHE_CAP {
            self.shaped_run_cache.clear();
        }
        self.shaped_run_cache.insert(
            key,
            ShapedRunCacheEntry {
                total_cells,
                clusters: clusters.cloned().collect(),
            },
        );
    }

    fn prepare_ascii_cluster(
        &mut self,
        ch: u8,
        request: ClusterGlyphRequest<'_>,
    ) -> GlyphAtlasRecord {
        // Contextual alternates and feature substitutions need the full glyph key.
        // Only an unshaped character has one raster per face, size, and cell.
        if !request.cluster.glyphs.is_empty() {
            return self.prepare_cluster(request);
        }
        let font_size_bits = request.command.font_size.to_bits();
        let pixels_per_point_bits = request.pixels_per_point.to_bits();
        let dilation = self.glyph_dilation(request.command.attrs.fg);
        let cache_index = usize::from(ch);
        let atlas_resized_count = self.atlas.resized_count();
        if let Some(cached) = self
            .ascii_glyph_cache
            .get(cache_index)
            .and_then(Option::as_ref)
            && cached.dilation == dilation
            && cached.face == request.face_key
            && cached.font_size_bits == font_size_bits
            && cached.pixels_per_point_bits == pixels_per_point_bits
            && cached.width == request.glyph_width
            && cached.height == request.glyph_height
            && cached.atlas_resized_count == atlas_resized_count
        {
            return cached.record;
        }

        let face_key = request.face_key.clone();
        let width = request.glyph_width;
        let height = request.glyph_height;
        let record = self.prepare_cluster(request);
        if let Some(slot) = self.ascii_glyph_cache.get_mut(cache_index) {
            *slot = Some(AsciiGlyphAtlasRecord {
                dilation,
                face: face_key,
                font_size_bits,
                pixels_per_point_bits,
                width,
                height,
                atlas_resized_count: self.atlas.resized_count(),
                record,
            });
        }
        record
    }

    fn glyph_dilation(&self, color: PlanColor) -> u8 {
        self.platform_text_system
            .as_deref()
            .map_or(0, |system| platform_raster::glyph_dilation(system, color))
    }

    fn prepare_cluster(&mut self, request: ClusterGlyphRequest<'_>) -> GlyphAtlasRecord {
        let key = GlyphAtlasKey {
            dilation: self.glyph_dilation(request.command.attrs.fg),
            face: request.face_key,
            text: self.intern_cluster_key(
                request.cluster,
                &request.command.font_features,
                request.command.attrs.fg,
            ),
            font_size_bits: request.command.font_size.to_bits(),
            pixels_per_point_bits: request.pixels_per_point.to_bits(),
            width: request.glyph_width,
            height: request.glyph_height,
        };
        let format = self.atlas.format();
        self.atlas.insert_or_get_raster(key, || {
            rasterize_cluster(
                &mut self.fonts,
                RasterizeClusterRequest {
                    platform_text_system: self.platform_text_system.as_deref(),
                    foreground: request.command.attrs.fg,
                    face: &request.command.face,
                    cluster: request.cluster,
                    font_size: request.command.font_size,
                    pixels_per_point: request.pixels_per_point,
                    constraint_cells: request.constraint_cells,
                    tile: (request.glyph_width, request.glyph_height),
                    format,
                },
            )
        })
    }

    fn intern_text(&mut self, text: &str) -> GlyphAtlasTextKey {
        if let Some(cached) = self.text_cache.get(text) {
            return cached.clone();
        }
        // Retain at most one shaped-run working set of arbitrary grapheme signatures.
        if self.text_cache.len() >= SHAPED_RUN_CACHE_CAP {
            self.text_cache.clear();
        }
        let cached = GlyphAtlasTextKey::new(text);
        self.text_cache.insert(text.to_owned(), cached.clone());
        cached
    }

    fn intern_cluster_text(&mut self, text: &str) -> GlyphAtlasTextKey {
        let mut chars = text.chars();
        if let Some(ch) = chars.next()
            && chars.next().is_none()
        {
            return self.intern_char(ch);
        }
        self.intern_text(text)
    }

    /// Atlas key for a cluster. Shaped (ligature/contextual) clusters key on
    /// their glyph ids rather than source text, because the same characters can
    /// shape to different glyphs depending on run context.
    fn intern_cluster_key(
        &mut self,
        cluster: &ShapedCluster,
        font_features: &[FontFeature],
        foreground: PlanColor,
    ) -> GlyphAtlasTextKey {
        // An emoji grapheme may also contain monochrome marks. Their tint is baked
        // alongside native color pixels; opacity still applies once at paint time.
        if is_color_emoji_cluster(cluster) {
            return self.intern_text(&format!(
                "\u{3}{:02x}{:02x}{:02x}{}",
                foreground.r, foreground.g, foreground.b, cluster.text
            ));
        }
        if cluster.glyphs.is_empty() {
            return self.intern_cluster_text(&cluster.text);
        }
        let mut signature =
            String::with_capacity(cluster.glyphs.len().saturating_mul(22).saturating_add(1));
        signature.push('\u{1}');
        for feature in font_features {
            let [a, b, c, d] = feature.tag();
            // String formatting cannot fail.
            let _ = write!(
                signature,
                "{:02x}{:02x}{:02x}{:02x}{:08x}",
                a,
                b,
                c,
                d,
                feature.value()
            );
        }
        signature.push('\u{2}');
        let first_cell = cluster.glyphs.first().map_or(0, |glyph| glyph.cluster);
        for glyph in &cluster.glyphs {
            signature.push_str(&glyph.cluster.saturating_sub(first_cell).to_string());
            signature.push(':');
            signature.push_str(&glyph.glyph_id.to_string());
            signature.push('@');
            signature.push_str(&glyph.x_offset.to_bits().to_string());
            signature.push(':');
            signature.push_str(&glyph.y_offset.to_bits().to_string());
            signature.push(',');
        }
        self.intern_text(&signature)
    }

    fn intern_char(&mut self, ch: char) -> GlyphAtlasTextKey {
        if let Some(slot) = usize::try_from(u32::from(ch))
            .ok()
            .and_then(|index| self.ascii_char_cache.get_mut(index))
        {
            return slot
                .get_or_insert_with(|| GlyphAtlasTextKey::for_char(ch))
                .clone();
        }
        if let Some(cached) = self.char_cache.get(&ch) {
            return cached.clone();
        }
        let cached = GlyphAtlasTextKey::for_char(ch);
        self.char_cache.insert(ch, cached.clone());
        cached
    }

    fn intern_face(&mut self, face: &ResolvedFontFace) -> GlyphAtlasFaceKey {
        if let Some(cached) = self.face_cache.get(face) {
            return cached.clone();
        }
        let cached = GlyphAtlasFaceKey::new(face.clone());
        self.face_cache.insert(face.clone(), cached.clone());
        cached
    }

    pub fn prepare_sprite_command(
        &mut self,
        command: &SpriteCommandBatch,
        pixels_per_point: f32,
    ) -> TexturedGlyphQuad {
        // Oversized sprites are downsampled to the atlas capacity; the quad keeps its logical
        // size. A larger raster requires increasing the atlas limit and its GPU texture budget.
        let dimension = |logical: f32| {
            let requested = (logical * pixels_per_point)
                .ceil()
                .max(1.0)
                .to_u32()
                .unwrap_or(u32::MAX);
            let bounded = requested.min(u32::from(atlas::MAX_ATLAS_DIM.saturating_sub(2)));
            (requested, u16::try_from(bounded).unwrap_or(1))
        };
        let (width, raster_width) = dimension(command.rect.width());
        let (height, raster_height) = dimension(command.rect.height());
        let key = GlyphAtlasKey {
            dilation: 0,
            face: self.sprite_face_key.clone(),
            text: self.intern_char(command.glyph.ch),
            font_size_bits: command.rect.height().to_bits(),
            pixels_per_point_bits: pixels_per_point.to_bits(),
            width,
            height,
        };
        let format = self.atlas.format();
        let entry = self.atlas.insert_or_get_with(
            key,
            u32::from(raster_width),
            u32::from(raster_height),
            || {
                let commands = command.glyph.commands_for(command.rect);
                let alpha =
                    rasterize_sprite_commands(&commands, command.rect, raster_width, raster_height);
                alpha_to_atlas_pixels(format, alpha)
            },
        );
        TexturedGlyphQuad {
            rect: command.rect,
            uv: atlas_uv(self.atlas.size(), entry),
            atlas_entry: entry,
            color: command.color,
            snap_to_pixel_grid: true,
        }
    }

    #[must_use]
    pub fn atlas_len(&self) -> usize {
        self.atlas.len()
    }

    #[must_use]
    pub fn atlas_pixels(&self) -> &[u8] {
        self.atlas.pixels()
    }

    /// Extract one atlas tile as BGRA pixels for GPUI's image renderer.
    ///
    /// The atlas owns constrained glyph geometry and color-emoji rasterization. This conversion
    /// only applies the terminal foreground color and changes channel order; it does not shape,
    /// scale, or reposition the glyph.
    #[must_use]
    pub fn bgra_tile(&self, quad: &TexturedGlyphQuad) -> Option<Vec<u8>> {
        if self.atlas.format() != GlyphAtlasFormat::Rgba {
            return None;
        }
        let (atlas_width, atlas_height) = self.atlas.size();
        let entry = quad.atlas_entry;
        if entry.x.checked_add(entry.width)? > atlas_width
            || entry.y.checked_add(entry.height)? > atlas_height
        {
            return None;
        }
        let row_bytes = usize::try_from(atlas_width).ok()?.checked_mul(4)?;
        let tile_row_bytes = usize::try_from(entry.width).ok()?.checked_mul(4)?;
        let mut bgra =
            Vec::with_capacity(tile_row_bytes.checked_mul(usize::try_from(entry.height).ok()?)?);
        for y in entry.y..entry.y.checked_add(entry.height)? {
            let start = usize::try_from(y)
                .ok()?
                .checked_mul(row_bytes)?
                .checked_add(usize::try_from(entry.x).ok()?.checked_mul(4)?)?;
            let row = self
                .atlas
                .pixels()
                .get(start..start.checked_add(tile_row_bytes)?)?;
            for pixel in row.as_chunks::<4>().0 {
                let alpha =
                    u8::try_from(u16::from(pixel[3]).saturating_mul(u16::from(quad.color.a)) / 255)
                        .ok()?;
                let color_glyph = pixel[0] != 255 || pixel[1] != 255 || pixel[2] != 255;
                let (red, green, blue) = if color_glyph {
                    (pixel[0], pixel[1], pixel[2])
                } else {
                    (quad.color.r, quad.color.g, quad.color.b)
                };
                bgra.extend_from_slice(&[blue, green, red, alpha]);
            }
        }
        Some(bgra)
    }

    #[must_use]
    pub const fn atlas_size(&self) -> (u32, u32) {
        self.atlas.size()
    }

    #[must_use]
    pub const fn atlas_modified_count(&self) -> u64 {
        self.atlas.modified_count()
    }
    #[must_use]
    pub fn atlas_dirty_rect_since(&self, modified: u64) -> Option<GlyphAtlasEntry> {
        self.atlas.dirty_rect_since(modified)
    }

    #[must_use]
    pub const fn atlas_resized_count(&self) -> u64 {
        self.atlas.resized_count()
    }

    #[must_use]
    pub const fn atlas_format(&self) -> GlyphAtlasFormat {
        self.atlas.format()
    }
}
