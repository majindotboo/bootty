use std::{
    collections::HashMap,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
};

use crate::terminal_text::ResolvedFontFace;
use bootty_terminal::geometry::SurfaceRect;
use num_traits::ToPrimitive as _;

pub(super) const MAX_ATLAS_DIM: u16 = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphAtlasKey {
    pub face: GlyphAtlasFaceKey,
    pub text: GlyphAtlasTextKey,
    pub dilation: u8,
    pub font_size_bits: u32,
    pub pixels_per_point_bits: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub struct GlyphAtlasTextKey {
    text: Arc<str>,
    hash: u64,
}

#[derive(Clone, Debug)]
pub struct GlyphAtlasFaceKey {
    face: Arc<ResolvedFontFace>,
    hash: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphAtlasEntry {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GlyphAtlasRecord {
    pub(super) entry: GlyphAtlasEntry,
    pub(super) is_color_glyph: bool,
    pub(super) offset: [i32; 2],
}

pub(super) struct GlyphRaster {
    pub(super) pixels: Vec<u8>,
    pub(super) color: bool,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) offset: [i32; 2],
}

impl GlyphRaster {
    pub(super) const fn new(pixels: Vec<u8>, color: bool, width: u32, height: u32) -> Self {
        Self {
            pixels,
            color,
            width,
            height,
            offset: [0, 0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphAtlasFormat {
    Alpha,
    Bgr,
    Rgba,
}

impl GlyphAtlasFormat {
    #[must_use]
    pub const fn depth(self) -> u32 {
        match self {
            Self::Alpha => 1,
            Self::Bgr => 3,
            Self::Rgba => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GlyphAtlasError {
    #[error("glyph atlas capacity exceeded")]
    CapacityExceeded,
}

#[derive(Clone, Debug)]
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    format: GlyphAtlasFormat,
    allocations: Vec<GlyphAtlasEntry>,
    entries: HashMap<GlyphAtlasKey, GlyphAtlasRecord>,
    pixels: Vec<u8>,
    modified: u64,
    dirty_regions: Vec<(u64, GlyphAtlasEntry)>,
    next_x: u32,
    next_y: u32,
    row_height: u32,
    resized: u64,
    // Failed footprint: equal or larger requests cannot fit until growth or recycling frees
    // space. Remembering this avoids rescanning the saturated atlas for every new glyph.
    no_fit_at_least: Option<(u32, u32)>,
}

impl Hash for GlyphAtlasKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let mut hash = self.face.hash ^ self.text.hash.rotate_left(13);
        hash ^= u64::from(self.font_size_bits).rotate_left(29);
        hash ^= u64::from(self.pixels_per_point_bits).rotate_left(43);
        hash ^= u64::from(self.width) << 32 | u64::from(self.height);
        hash ^= u64::from(self.dilation).rotate_left(7);
        state.write_u64(hash);
    }
}

impl GlyphAtlasTextKey {
    pub fn new(text: impl AsRef<str>) -> Self {
        let text = text.as_ref();
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        Self {
            text: Arc::from(text),
            hash: hasher.finish(),
        }
    }

    pub(super) fn for_char(ch: char) -> Self {
        let mut buffer = [0_u8; 4];
        Self::new(ch.encode_utf8(&mut buffer))
    }
}

impl PartialEq for GlyphAtlasTextKey {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.text == other.text
    }
}

impl Eq for GlyphAtlasTextKey {}

impl Hash for GlyphAtlasTextKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

impl GlyphAtlasFaceKey {
    #[must_use]
    pub fn new(face: ResolvedFontFace) -> Self {
        let mut hasher = DefaultHasher::new();
        face.hash(&mut hasher);
        Self {
            face: Arc::new(face),
            hash: hasher.finish(),
        }
    }
}

impl PartialEq for GlyphAtlasFaceKey {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && (Arc::ptr_eq(&self.face, &other.face) || self.face == other.face)
    }
}

impl Eq for GlyphAtlasFaceKey {}

impl Hash for GlyphAtlasFaceKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

impl Default for GlyphAtlas {
    /// The renderer starts with a 1024 by 1024 RGBA atlas and grows it on demand.
    fn default() -> Self {
        Self::from_pixels(1024, 1024, GlyphAtlasFormat::Rgba, vec![0; 1024 * 1024 * 4])
    }
}

impl GlyphAtlas {
    /// # Errors
    /// Returns `CapacityExceeded` when the dimensions exceed the supported pixel storage range.
    pub fn new(width: u32, height: u32) -> Result<Self, GlyphAtlasError> {
        Self::with_format(width, height, GlyphAtlasFormat::Alpha)
    }

    /// # Errors
    /// Returns `CapacityExceeded` when pixel storage exceeds the byte limit or supported offset range.
    pub fn with_format(
        width: u32,
        height: u32,
        format: GlyphAtlasFormat,
    ) -> Result<Self, GlyphAtlasError> {
        Self::try_with_format(width, height, format, usize::MAX)
    }

    /// # Errors
    /// Returns `CapacityExceeded` when pixel storage exceeds the byte limit or supported offset range.
    pub fn try_with_format(
        width: u32,
        height: u32,
        format: GlyphAtlasFormat,
        byte_limit: usize,
    ) -> Result<Self, GlyphAtlasError> {
        let width = width.max(1);
        let height = height.max(1);
        let depth = format.depth();
        let byte_len = atlas_byte_len(width, height, depth)?;
        if byte_len > byte_limit {
            return Err(GlyphAtlasError::CapacityExceeded);
        }
        Ok(Self::from_pixels(width, height, format, vec![0; byte_len]))
    }

    fn from_pixels(width: u32, height: u32, format: GlyphAtlasFormat, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            format,
            allocations: Vec::new(),
            entries: HashMap::new(),
            pixels,
            next_x: 1,
            next_y: 1,
            row_height: 0,
            modified: 0,
            dirty_regions: Vec::new(),
            resized: 0,
            no_fit_at_least: None,
        }
    }

    pub fn insert_or_get(
        &mut self,
        key: GlyphAtlasKey,
        width: u32,
        height: u32,
        alpha: Vec<u8>,
    ) -> GlyphAtlasEntry {
        self.insert_or_get_with(key, width, height, || alpha)
    }

    pub fn insert_or_get_with(
        &mut self,
        key: GlyphAtlasKey,
        width: u32,
        height: u32,
        pixels: impl FnOnce() -> Vec<u8>,
    ) -> GlyphAtlasEntry {
        self.insert_or_get_with_color(key, width, height, || (pixels(), false))
            .0
    }

    pub(super) fn insert_or_get_with_color(
        &mut self,
        key: GlyphAtlasKey,
        width: u32,
        height: u32,
        pixels: impl FnOnce() -> (Vec<u8>, bool),
    ) -> (GlyphAtlasEntry, bool) {
        let record = self.insert_or_get_raster(key, || {
            let (pixels, color) = pixels();
            GlyphRaster::new(pixels, color, width, height)
        });
        (record.entry, record.is_color_glyph)
    }

    pub(super) fn insert_or_get_raster(
        &mut self,
        key: GlyphAtlasKey,
        rasterize: impl FnOnce() -> GlyphRaster,
    ) -> GlyphAtlasRecord {
        if let Some(record) = self.entries.get(&key) {
            return *record;
        }
        let raster = rasterize();
        let width = raster.width.max(1);
        let height = raster.height.max(1);
        // Grow first; recycling at the size cap is safe after the preceding glyph visitor returns.
        let mut reserved = self.reserve(width, height);
        while reserved.is_none() && self.grow_for_glyph(width, height) {
            reserved = self.reserve(width, height);
        }
        if reserved.is_none()
            && width
                .checked_add(2)
                .is_some_and(|width| width <= self.width)
            && height
                .checked_add(2)
                .is_some_and(|height| height <= self.height)
        {
            self.recycle();
            reserved = self.reserve(width, height);
        }
        let mut entry = reserved.unwrap_or(GlyphAtlasEntry {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        });
        if entry.width != width || entry.height != height {
            entry.width = entry.width.min(width);
            entry.height = entry.height.min(height);
        }
        self.set(entry, &raster.pixels);
        let record = GlyphAtlasRecord {
            entry,
            is_color_glyph: raster.color,
            offset: raster.offset,
        };
        self.entries.insert(key, record);
        record
    }

    #[must_use]
    pub fn get(&self, key: &GlyphAtlasKey) -> Option<GlyphAtlasEntry> {
        self.entries.get(key).map(|record| record.entry)
    }

    // Enlarge the atlas to make room for a glyph that did not fit. Returns false at the size cap.
    fn grow_for_glyph(&mut self, width: u32, height: u32) -> bool {
        let target_width = if width.saturating_add(2) > self.width {
            (width.saturating_add(2)).max(self.width)
        } else {
            self.width
        }
        .min(u32::from(MAX_ATLAS_DIM));
        // A glyph that fits dimensionally but still failed to reserve means the shelves are full,
        // so add height; otherwise grow to fit the oversized glyph itself.
        let target_height = if height.saturating_add(2) > self.height {
            (height.saturating_add(2)).max(self.height)
        } else {
            self.height.saturating_mul(2)
        }
        .min(u32::from(MAX_ATLAS_DIM));
        if target_width == self.width && target_height == self.height {
            return false;
        }
        self.grow(target_width, target_height).is_ok()
    }

    pub fn reserve(&mut self, width: u32, height: u32) -> Option<GlyphAtlasEntry> {
        let width = width.max(1);
        let height = height.max(1);
        if width.checked_add(2)? > self.width || height.checked_add(2)? > self.height {
            return None;
        }
        if let Some((fw, fh)) = self.no_fit_at_least
            && width >= fw
            && height >= fh
        {
            return None;
        }

        if let Some(entry) = self.reserve_next_shelf_slot(width, height) {
            return Some(entry);
        }

        if let Some(entry) = self.first_fit_in_gaps(width, height) {
            self.allocations.push(entry);
            Some(entry)
        } else {
            self.no_fit_at_least = Some((width, height));
            None
        }
    }

    // Top-left first-fit over the surface, identical in result to scanning every pixel but
    // visiting only positions flush against the surface edges or the edges of existing
    // allocations. The optimal first-fit corner always lands on one of these, so the candidate
    // grid is O(allocations) per axis instead of O(width * height).
    fn first_fit_in_gaps(&self, width: u32, height: u32) -> Option<GlyphAtlasEntry> {
        let max_x = (self.width.saturating_sub(1)).saturating_sub(width);
        let max_y = (self.height.saturating_sub(1)).saturating_sub(height);

        let mut columns: Vec<u32> = core::iter::once(1)
            .chain(
                self.allocations
                    .iter()
                    .map(|used| used.x.saturating_add(used.width)),
            )
            .filter(|&x| (1..=max_x).contains(&x))
            .collect();
        columns.sort_unstable();
        columns.dedup();

        let mut rows: Vec<u32> = core::iter::once(1)
            .chain(
                self.allocations
                    .iter()
                    .map(|used| used.y.saturating_add(used.height)),
            )
            .filter(|&y| (1..=max_y).contains(&y))
            .collect();
        rows.sort_unstable();
        rows.dedup();

        for &y in &rows {
            for &x in &columns {
                let entry = GlyphAtlasEntry {
                    x,
                    y,
                    width,
                    height,
                };
                if self
                    .allocations
                    .iter()
                    .all(|used| !rects_overlap(*used, entry))
                {
                    return Some(entry);
                }
            }
        }
        None
    }

    fn reserve_next_shelf_slot(&mut self, width: u32, height: u32) -> Option<GlyphAtlasEntry> {
        let usable_right = self.width.saturating_sub(1);
        let usable_bottom = self.height.saturating_sub(1);
        let max_x = usable_right.saturating_sub(width);
        let max_y = usable_bottom.saturating_sub(height);

        if self.next_x > max_x {
            self.next_x = 1;
            self.next_y = self.next_y.saturating_add(self.row_height.max(1));
            self.row_height = 0;
        }
        if self.next_y > max_y {
            return None;
        }

        let entry = GlyphAtlasEntry {
            x: self.next_x,
            y: self.next_y,
            width,
            height,
        };
        if self
            .allocations
            .iter()
            .any(|used| rects_overlap(*used, entry))
        {
            return None;
        }

        self.allocations.push(entry);
        self.next_x = self.next_x.saturating_add(width).saturating_add(1);
        self.row_height = self.row_height.max(height.saturating_add(1));
        Some(entry)
    }

    pub fn set(&mut self, entry: GlyphAtlasEntry, alpha: &[u8]) {
        blit_pixels_from_source(
            &mut self.pixels,
            BlitTarget {
                atlas_width: self.width,
                depth: self.format.depth(),
                entry,
            },
            BlitSource {
                pixels: alpha,
                width: entry.width,
                x: 0,
                y: 0,
                clear_missing: false,
            },
        );
        self.modified = self.modified.saturating_add(1);
        self.dirty_regions.push((self.modified, entry));
    }

    pub fn set_from_larger(
        &mut self,
        entry: GlyphAtlasEntry,
        alpha: &[u8],
        source_width: u32,
        source_x: u32,
        source_y: u32,
    ) {
        blit_pixels_from_source(
            &mut self.pixels,
            BlitTarget {
                atlas_width: self.width,
                depth: self.format.depth(),
                entry,
            },
            BlitSource {
                pixels: alpha,
                width: source_width,
                x: source_x,
                y: source_y,
                clear_missing: true,
            },
        );
        self.modified = self.modified.saturating_add(1);
        self.dirty_regions.push((self.modified, entry));
    }

    /// # Errors
    /// Returns `CapacityExceeded` when the requested pixel storage cannot be represented.
    pub fn grow(&mut self, width: u32, height: u32) -> Result<(), GlyphAtlasError> {
        self.try_grow_with_byte_limit(width, height, usize::MAX)
    }

    /// # Errors
    /// Returns `CapacityExceeded` when pixel storage exceeds the byte limit or supported offset range.
    pub fn try_grow_with_byte_limit(
        &mut self,
        width: u32,
        height: u32,
        byte_limit: usize,
    ) -> Result<(), GlyphAtlasError> {
        let width = width.max(self.width);
        let height = height.max(self.height);
        if width == self.width && height == self.height {
            return Ok(());
        }

        let depth = self.format.depth();
        let byte_len = atlas_byte_len(width, height, depth)?;
        if byte_len > byte_limit {
            return Err(GlyphAtlasError::CapacityExceeded);
        }

        let mut pixels = vec![0; byte_len];
        let old_stride = atlas_byte_len(self.width, 1, depth)?;
        let new_stride = atlas_byte_len(width, 1, depth)?;
        for (old_row, new_row) in self
            .pixels
            .chunks_exact(old_stride)
            .zip(pixels.chunks_exact_mut(new_stride))
        {
            let destination = new_row
                .get_mut(..old_stride)
                .ok_or(GlyphAtlasError::CapacityExceeded)?;
            destination.copy_from_slice(old_row);
        }
        self.width = width;
        self.height = height;
        self.pixels = pixels;
        self.modified = self.modified.saturating_add(1);
        self.resized = self.resized.saturating_add(1);
        // The larger surface may now hold rects that previously failed the gap scan.
        self.no_fit_at_least = None;
        Ok(())
    }

    pub(super) fn recycle(&mut self) {
        self.allocations.clear();
        self.entries.clear();
        self.pixels.fill(0);
        self.dirty_regions.clear();
        self.next_x = 1;
        self.next_y = 1;
        self.row_height = 0;
        self.no_fit_at_least = None;
        self.modified = self.modified.saturating_add(1);
        // Same-size recycle still invalidates every cached UV and GPU texture.
        self.resized = self.resized.saturating_add(1);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[must_use]
    pub fn atlas_pixel(&self, x: u32, y: u32) -> Option<u8> {
        self.atlas_pixel_channel(x, y, 0)
    }

    #[must_use]
    pub fn atlas_pixel_channel(&self, x: u32, y: u32, channel: u32) -> Option<u8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let depth = self.format.depth();
        if channel >= depth {
            return None;
        }
        self.pixels
            .get(
                atlas_pixel_offset(x, y, self.width, depth)?
                    .checked_add(usize::try_from(channel).ok()?)?,
            )
            .copied()
    }

    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[must_use]
    pub const fn modified_count(&self) -> u64 {
        self.modified
    }
    pub fn dirty_rect_since(&self, modified: u64) -> Option<GlyphAtlasEntry> {
        let start = self
            .dirty_regions
            .partition_point(|(version, _)| *version <= modified);
        self.dirty_regions
            .iter()
            .skip(start)
            .map(|(_, entry)| *entry)
            .reduce(union_atlas_entries)
    }

    #[must_use]
    pub const fn resized_count(&self) -> u64 {
        self.resized
    }

    #[must_use]
    pub const fn format(&self) -> GlyphAtlasFormat {
        self.format
    }
}

fn union_atlas_entries(left: GlyphAtlasEntry, right: GlyphAtlasEntry) -> GlyphAtlasEntry {
    let x = left.x.min(right.x);
    let y = left.y.min(right.y);
    let max_x = left
        .x
        .saturating_add(left.width)
        .max(right.x.saturating_add(right.width));
    let max_y = left
        .y
        .saturating_add(left.height)
        .max(right.y.saturating_add(right.height));
    GlyphAtlasEntry {
        x,
        y,
        width: max_x.saturating_sub(x),
        height: max_y.saturating_sub(y),
    }
}

pub(super) fn alpha_to_atlas_pixels(format: GlyphAtlasFormat, alpha: Vec<u8>) -> Vec<u8> {
    match format {
        GlyphAtlasFormat::Alpha => alpha,
        GlyphAtlasFormat::Bgr => alpha
            .into_iter()
            .flat_map(|alpha| [alpha, alpha, alpha])
            .collect(),
        GlyphAtlasFormat::Rgba => alpha
            .into_iter()
            .flat_map(|alpha| [255, 255, 255, alpha])
            .collect(),
    }
}

const fn rects_overlap(a: GlyphAtlasEntry, b: GlyphAtlasEntry) -> bool {
    a.x < b.x.saturating_add(b.width)
        && a.x.saturating_add(a.width) > b.x
        && a.y < b.y.saturating_add(b.height)
        && a.y.saturating_add(a.height) > b.y
}

fn atlas_byte_len(width: u32, height: u32, depth: u32) -> Result<usize, GlyphAtlasError> {
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(depth))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or(GlyphAtlasError::CapacityExceeded)
}

#[derive(Clone, Copy)]
struct BlitTarget {
    atlas_width: u32,
    depth: u32,
    entry: GlyphAtlasEntry,
}

#[derive(Clone, Copy)]
struct BlitSource<'a> {
    pixels: &'a [u8],
    width: u32,
    x: u32,
    y: u32,
    clear_missing: bool,
}

fn atlas_pixel_offset(x: u32, y: u32, width: u32, depth: u32) -> Option<usize> {
    let pixel = y.checked_mul(width)?.checked_add(x)?;
    usize::try_from(pixel.checked_mul(depth)?).ok()
}

fn blit_pixels_from_source(pixels: &mut [u8], target: BlitTarget, source: BlitSource<'_>) {
    let Ok(row_bytes) = atlas_byte_len(target.entry.width, 1, target.depth) else {
        return;
    };
    for y in 0..target.entry.height {
        let Some(dst_start) = target.entry.y.checked_add(y).and_then(|row| {
            atlas_pixel_offset(target.entry.x, row, target.atlas_width, target.depth)
        }) else {
            continue;
        };
        let Some(dst_end) = dst_start.checked_add(row_bytes) else {
            continue;
        };
        let Some(dst_row) = pixels.get_mut(dst_start..dst_end) else {
            continue;
        };
        if source.clear_missing {
            dst_row.fill(0);
        }
        let Some(src_start) = source
            .y
            .checked_add(y)
            .and_then(|row| atlas_pixel_offset(source.x, row, source.width, target.depth))
        else {
            continue;
        };
        let Some(src_end) = src_start.checked_add(row_bytes) else {
            continue;
        };
        if let Some(src_row) = source.pixels.get(src_start..src_end) {
            dst_row.copy_from_slice(src_row);
        }
    }
}

pub(super) fn atlas_uv(
    (atlas_width, atlas_height): (u32, u32),
    entry: GlyphAtlasEntry,
) -> SurfaceRect {
    let atlas_width = atlas_width.to_f32().unwrap_or(1.0);
    let atlas_height = atlas_height.to_f32().unwrap_or(1.0);
    let x = entry.x.to_f32().unwrap_or_default();
    let y = entry.y.to_f32().unwrap_or_default();
    let width = entry.width.to_f32().unwrap_or_default();
    let height = entry.height.to_f32().unwrap_or_default();
    SurfaceRect {
        min_x: (x + 0.5) / atlas_width,
        min_y: (y + 0.5) / atlas_height,
        max_x: (x + width) / atlas_width - 0.5 / atlas_width,
        max_y: (y + height) / atlas_height - 0.5 / atlas_height,
    }
}
