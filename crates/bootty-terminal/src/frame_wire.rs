use crate::{
    geometry::SurfaceRect,
    terminal_frame::{
        CursorSnapshot, FrameColors, FrameCopyMode, FrameScrollbar, FrameSelection, FrameStats,
        RenderCell, RenderFrame,
    },
    terminal_image::{KittyImageLayer, KittyImagePlacement, KittyVirtualPlacement},
};
use libghostty_vt::{
    kitty::graphics::{ImageFormat, SourceRect},
    render::Dirty,
    style::RgbColor,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

// Limit: 16 MiB per message; raise only with negotiated transport budgets.
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;
// Limit: 64 MiB of image content and entries per frame; raise with negotiated image budgets.
pub const MAX_FRAME_IMAGE_BYTES: usize = 64 * 1024 * 1024;
// Limit: current plus incoming frame; increase only if the wire pipelines more frames.
pub const MAX_RETAINED_IMAGE_BYTES: usize = 2 * MAX_FRAME_IMAGE_BYTES;
// Limit: one image per message until chunked image transfer is supported.
const IMAGE_CONTENT_ENVELOPE_BYTES: usize = 128;
const MAX_IMAGE_CONTENT_BYTES: usize = MAX_MESSAGE_SIZE - IMAGE_CONTENT_ENVELOPE_BYTES;
const IMAGE_ENTRY_BYTES: usize = size_of::<(ImageKey, Arc<Vec<u8>>, Vec<u8>)>();
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    Size,
    Codec(String),
    WrongBase,
    InvalidRow,
    InvalidFrame,
    MissingImage(ImageKey),
    ImageBudget,
}
impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WireError {}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageKey {
    pub image_id: u32,
    pub generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageContent {
    pub key: ImageKey,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRow {
    #[serde(with = "wire_cells")]
    pub cells: Vec<RenderCell>,
    pub text: Vec<char>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WirePlacement {
    pub key: ImageKey,
    pub placement_id: u32,
    pub layer: u8,
    pub width: u32,
    pub height: u32,
    pub format: i32,
    pub source: [u32; 4],
    pub destination: SurfaceRect,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FrameRows {
    Full(Vec<WireRow>),
    Rows {
        base: u64,
        rows: Vec<(u16, WireRow)>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrameUpdate {
    pub sequence: u64,
    pub state: WireState,
    pub rows: FrameRows,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireState {
    pub cols: u16,
    pub rows: u16,
    pub colors: FrameColors,
    pub cursor: Option<CursorSnapshot>,
    pub row_wraps: Vec<bool>,
    pub search_matches: Vec<FrameSelection>,
    pub active_search_match: Option<FrameSelection>,
    pub active_search_segments: Vec<FrameSelection>,
    pub active_search_match_index: Option<usize>,
    pub search_match_count: usize,
    pub search_query: String,
    pub current_working_directory: Option<String>,
    pub progress: Option<(String, Option<u8>)>,
    pub search_pulse: u64,
    pub copy_mode: Option<FrameCopyMode>,
    pub mouse_tracking: bool,
    pub selections: Vec<FrameSelection>,
    pub scrollbar: Option<FrameScrollbar>,
    pub stats: FrameStats,
    pub placements: Vec<WirePlacement>,
    pub virtual_placements: Vec<KittyVirtualPlacement>,
    pub virtual_placeholder_rows: Vec<u16>,
}
impl WireState {
    fn from_frame(frame: &RenderFrame, encoder: &mut FrameEncoder) -> Self {
        Self {
            cols: frame.cols,
            rows: frame.rows,
            colors: frame.colors,
            cursor: frame.cursor,
            row_wraps: frame.row_wraps.clone(),
            search_matches: frame.search_matches.clone(),
            active_search_match: frame.active_search_match,
            active_search_segments: frame.active_search_segments.clone(),
            active_search_match_index: frame.active_search_match_index,
            search_match_count: frame.search_match_count,
            search_query: frame.search_query.clone(),
            current_working_directory: frame.current_working_directory.clone(),
            progress: frame.progress.clone(),
            search_pulse: frame.search_pulse,
            copy_mode: frame.copy_mode,
            mouse_tracking: frame.mouse_tracking,
            selections: frame.selections.clone(),
            scrollbar: frame.scrollbar,
            stats: frame.stats,
            placements: frame
                .images
                .placements
                .iter()
                .filter_map(|p| {
                    Some(WirePlacement {
                        key: encoder.reference_image(p)?,
                        placement_id: p.placement_id,
                        layer: match p.layer {
                            KittyImageLayer::BelowBackground => 0,
                            KittyImageLayer::BelowText => 1,
                            KittyImageLayer::AboveText => 2,
                        },
                        width: p.image_width,
                        height: p.image_height,
                        format: i32::from(p.image_format),
                        source: [p.source.x, p.source.y, p.source.width, p.source.height],
                        destination: p.destination,
                    })
                })
                .collect(),
            virtual_placements: frame.images.virtual_placements.clone(),
            virtual_placeholder_rows: frame.images.virtual_placeholder_rows.clone(),
        }
    }
    fn frame(&self) -> RenderFrame {
        RenderFrame {
            cols: self.cols,
            rows: self.rows,
            colors: self.colors,
            cursor: self.cursor,
            row_wraps: self.row_wraps.clone(),
            search_matches: self.search_matches.clone(),
            active_search_match: self.active_search_match,
            active_search_segments: self.active_search_segments.clone(),
            active_search_match_index: self.active_search_match_index,
            search_match_count: self.search_match_count,
            search_query: self.search_query.clone(),
            current_working_directory: self.current_working_directory.clone(),
            progress: self.progress.clone(),
            search_pulse: self.search_pulse,
            copy_mode: self.copy_mode,
            mouse_tracking: self.mouse_tracking,
            selections: self.selections.clone(),
            scrollbar: self.scrollbar,
            stats: self.stats,
            ..RenderFrame::default()
        }
    }
}
fn rows(frame: &RenderFrame) -> Vec<WireRow> {
    let mut rows = vec![
        WireRow {
            cells: Vec::new(),
            text: Vec::new()
        };
        usize::from(frame.rows)
    ];
    for cell in &frame.cells {
        let Some(row) = rows.get_mut(usize::from(cell.y)) else {
            continue;
        };
        let mut cell = cell.clone();
        let text = frame.cell_text(&cell);
        cell.text_start = row.text.len();
        cell.y = 0;
        row.text.extend_from_slice(text);
        row.cells.push(cell);
    }
    rows
}
#[derive(Default)]
pub struct FrameEncoder {
    previous: Option<(u64, u16, Vec<WireRow>)>,
    published: Option<RenderFrame>,
    sequence: u64,
    images: HashMap<u32, (ImageKey, Arc<Vec<u8>>)>,
    pending: Vec<ImageContent>,
    frame_image_bytes: usize,
    frame_images: HashSet<ImageKey>,
}
impl FrameEncoder {
    /// Encode only presentation changes; skipped frames do not advance the wire base.
    pub fn encode_changed(&mut self, frame: &RenderFrame) -> Option<FrameUpdate> {
        if self
            .published
            .as_ref()
            .is_some_and(|old| old.same_presentation(frame))
        {
            return None;
        }
        let update = self.encode(frame);
        self.published = Some(frame.clone());
        Some(update)
    }
    pub fn reset(&mut self) {
        self.previous = None;
        self.published = None;
        self.images.clear();
        self.pending.clear();
    }
    fn image_key(&mut self, placement: &KittyImagePlacement) -> ImageKey {
        if let Some((key, bytes)) = self.images.get(&placement.image_id)
            && **bytes == *placement.data
        {
            return *key;
        }
        let key = ImageKey {
            image_id: placement.image_id,
            generation: self.sequence,
        };
        self.images
            .insert(key.image_id, (key, placement.data.clone()));
        key
    }
    fn reference_image(&mut self, placement: &KittyImagePlacement) -> Option<ImageKey> {
        let known = self.images.get(&placement.image_id).map(|(key, _)| *key);
        let key = self.image_key(placement);
        if self.frame_images.insert(key) {
            let charge = placement.data.len().saturating_add(IMAGE_ENTRY_BYTES);
            if placement.data.len() <= MAX_IMAGE_CONTENT_BYTES
                && charge <= MAX_FRAME_IMAGE_BYTES.saturating_sub(self.frame_image_bytes)
            {
                self.frame_image_bytes = self.frame_image_bytes.saturating_add(charge);
                if known != Some(key) {
                    self.pending.push(ImageContent {
                        key,
                        bytes: (*placement.data).clone(),
                    });
                }
            } else {
                self.images.remove(&key.image_id);
                self.frame_images.remove(&key);
                return None;
            }
        }
        Some(key)
    }
    pub fn take_image_contents(&mut self) -> Vec<ImageContent> {
        std::mem::take(&mut self.pending)
    }
    #[must_use]
    pub fn encode(&mut self, frame: &RenderFrame) -> FrameUpdate {
        self.sequence = self.sequence.wrapping_add(1);
        let current = rows(frame);
        let update = match &self.previous {
            Some((base, cols, previous))
                if *cols == frame.cols && previous.len() == current.len() =>
            {
                FrameRows::Rows {
                    base: *base,
                    rows: (0..frame.rows)
                        .zip(&current)
                        .filter(|(y, row)| previous.get(usize::from(*y)) != Some(*row))
                        .map(|(y, row)| (y, row.clone()))
                        .collect(),
                }
            }
            _ => FrameRows::Full(current.clone()),
        };
        self.frame_image_bytes = 0;
        self.frame_images.clear();
        let state = WireState::from_frame(frame, self);
        let keys: HashSet<_> = state.placements.iter().map(|p| p.key).collect();
        self.images.retain(|_, (key, _)| keys.contains(key));
        self.pending.retain(|content| keys.contains(&content.key));
        self.previous = Some((self.sequence, frame.cols, current));
        FrameUpdate {
            sequence: self.sequence,
            state,
            rows: update,
        }
    }
}
#[derive(Default)]
pub struct FrameDecoder {
    previous: Option<(u64, Arc<RenderFrame>)>,
    images: HashMap<ImageKey, Arc<Vec<u8>>>,
}
impl FrameDecoder {
    /// Bytes retained for current and pending image content.
    #[must_use]
    pub fn retained_image_bytes(&self) -> usize {
        self.images.values().map(|bytes| bytes.len()).sum()
    }
    /// # Errors
    /// Rejects content exceeding the total retained image budget without mutation.
    pub fn insert_image(&mut self, content: ImageContent) -> Result<(), WireError> {
        // Charge entry overhead too, so even empty unsolicited images consume budget.
        let entry_bytes = IMAGE_ENTRY_BYTES;
        let retained = self
            .retained_image_bytes()
            .saturating_add(self.images.len().saturating_mul(entry_bytes));
        let old = self
            .images
            .get(&content.key)
            .map_or(0, |bytes| bytes.len().saturating_add(entry_bytes));
        let available = MAX_RETAINED_IMAGE_BYTES.saturating_sub(retained.saturating_sub(old));
        if content.bytes.len().saturating_add(entry_bytes) > available {
            return Err(WireError::ImageBudget);
        }
        self.images.insert(content.key, Arc::new(content.bytes));
        Ok(())
    }
    #[must_use]
    pub fn frame(&self) -> Option<&Arc<RenderFrame>> {
        self.previous.as_ref().map(|(_, frame)| frame)
    }
    fn placement(&self, p: &WirePlacement) -> Result<KittyImagePlacement, WireError> {
        let data = self
            .images
            .get(&p.key)
            .ok_or(WireError::MissingImage(p.key))?
            .clone();
        Ok(KittyImagePlacement {
            image_id: p.key.image_id,
            placement_id: p.placement_id,
            layer: match p.layer {
                0 => KittyImageLayer::BelowBackground,
                1 => KittyImageLayer::BelowText,
                2 => KittyImageLayer::AboveText,
                _ => return Err(WireError::InvalidFrame),
            },
            image_width: p.width,
            image_height: p.height,
            image_format: ImageFormat::try_from(p.format).map_err(|_| WireError::InvalidFrame)?,
            source: SourceRect {
                x: p.source.first().copied().ok_or(WireError::InvalidFrame)?,
                y: p.source.get(1).copied().ok_or(WireError::InvalidFrame)?,
                width: p.source.get(2).copied().ok_or(WireError::InvalidFrame)?,
                height: p.source.get(3).copied().ok_or(WireError::InvalidFrame)?,
            },
            destination: p.destination,
            data,
        })
    }
    /// # Errors
    /// Rejects malformed frames, missing images, and deltas with a different base.
    pub fn apply(&mut self, update: FrameUpdate) -> Result<Arc<RenderFrame>, WireError> {
        let full = matches!(update.rows, FrameRows::Full(_));
        let mut frame = update.state.frame();
        if frame.row_wraps.len() != usize::from(frame.rows) {
            return Err(WireError::InvalidFrame);
        }
        let mut current = match update.rows {
            FrameRows::Full(rows) => rows,
            FrameRows::Rows {
                base,
                rows: changes,
            } => {
                let Some((sequence, previous)) = &self.previous else {
                    return Err(WireError::WrongBase);
                };
                if base != *sequence
                    || update.sequence <= base
                    || previous.cols != frame.cols
                    || previous.rows != frame.rows
                {
                    return Err(WireError::WrongBase);
                }
                let mut current = rows(previous);
                let mut seen = vec![false; current.len()];
                for (y, row) in changes {
                    let y = usize::from(y);
                    let slot = current.get_mut(y).ok_or(WireError::InvalidRow)?;
                    let seen = seen.get_mut(y).ok_or(WireError::InvalidRow)?;
                    if *seen {
                        return Err(WireError::InvalidRow);
                    }
                    *seen = true;
                    *slot = row;
                }
                current
            }
        };
        if current.len() != usize::from(frame.rows) {
            return Err(WireError::InvalidRow);
        }
        let previous = self.previous.as_ref().map(|(_, frame)| rows(frame));
        frame.row_dirty = current
            .iter()
            .enumerate()
            .map(|(y, row)| {
                full || previous.as_ref().is_none_or(|old| old.get(y) != Some(row))
                    || self
                        .previous
                        .as_ref()
                        .is_some_and(|(_, old)| old.row_wraps.get(y) != frame.row_wraps.get(y))
            })
            .collect();
        for (y, row) in (0..frame.rows).zip(&mut current) {
            let mut end = 0;
            for cell in &mut row.cells {
                if cell.y != 0 || cell.x >= frame.cols || cell.text_start != end {
                    return Err(WireError::InvalidFrame);
                }
                end = end
                    .checked_add(cell.text_len)
                    .ok_or(WireError::InvalidFrame)?;
                if end > row.text.len() {
                    return Err(WireError::InvalidFrame);
                }
                cell.y = y;
                cell.text_start = cell
                    .text_start
                    .checked_add(frame.text.len())
                    .ok_or(WireError::InvalidFrame)?;
            }
            if end != row.text.len() {
                return Err(WireError::InvalidFrame);
            }
            frame.cells.append(&mut row.cells);
            frame.text.append(&mut row.text);
        }
        let keys: HashSet<_> = update.state.placements.iter().map(|p| p.key).collect();
        for p in update.state.placements {
            frame.images.placements.push(self.placement(&p)?);
        }
        frame.images.virtual_placements = update.state.virtual_placements;
        frame.images.virtual_placeholder_rows = update.state.virtual_placeholder_rows;
        if !full {
            frame.lineage = self
                .previous
                .as_ref()
                .ok_or(WireError::WrongBase)?
                .1
                .lineage
                .clone();
            frame.lineage.advance();
        }
        frame.dirty = if full { Dirty::Full } else { Dirty::Partial };
        let frame = Arc::new(frame);
        self.images.retain(|key, _| keys.contains(key));
        self.previous = Some((update.sequence, frame.clone()));
        Ok(frame)
    }
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes).map_err(|e| WireError::Codec(e.to_string()))?;
    if bytes.len() > MAX_MESSAGE_SIZE {
        return Err(WireError::Size);
    }
    Ok(bytes)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, WireError> {
    if bytes.len() > MAX_MESSAGE_SIZE {
        return Err(WireError::Size);
    }
    let mut cursor = std::io::Cursor::new(bytes);
    let value = ciborium::from_reader(&mut cursor).map_err(|e| WireError::Codec(e.to_string()))?;
    if cursor.position() != u64::try_from(bytes.len()).map_err(|_| WireError::Size)? {
        return Err(WireError::InvalidFrame);
    }
    Ok(value)
}
/// # Errors
/// Returns a size or CBOR encoding error.
pub fn encode_update(value: &FrameUpdate) -> Result<Vec<u8>, WireError> {
    encode(value)
}
/// # Errors
/// Rejects oversized, malformed, or trailing CBOR bytes.
pub fn decode_update(bytes: &[u8]) -> Result<FrameUpdate, WireError> {
    decode(bytes)
}
/// # Errors
/// Returns a size or CBOR encoding error.
pub fn encode_image_content(value: &ImageContent) -> Result<Vec<u8>, WireError> {
    encode(value)
}
/// # Errors
/// Rejects oversized, malformed, or trailing CBOR bytes.
pub fn decode_image_content(bytes: &[u8]) -> Result<ImageContent, WireError> {
    decode(bytes)
}
pub(crate) mod rgb {
    use super::{Deserialize, RgbColor, Serialize};
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde requires references"
    )]
    pub fn serialize<S: serde::Serializer>(v: &RgbColor, s: S) -> Result<S::Ok, S::Error> {
        [v.r, v.g, v.b].serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<RgbColor, D::Error> {
        let [r, g, b] = Deserialize::deserialize(d)?;
        Ok(RgbColor { r, g, b })
    }
}
#[expect(clippy::ref_option, reason = "serde requires references")]
pub(crate) mod optional_rgb {
    use super::{Deserialize, RgbColor, Serialize};
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde requires references"
    )]
    pub fn serialize<S: serde::Serializer>(v: &Option<RgbColor>, s: S) -> Result<S::Ok, S::Error> {
        v.map(|v| [v.r, v.g, v.b]).serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<RgbColor>, D::Error> {
        Ok(Option::<[u8; 3]>::deserialize(d)?.map(|[r, g, b]| RgbColor { r, g, b }))
    }
}
pub(crate) mod underline {
    use serde::{Deserialize, Serialize};
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde requires references"
    )]
    pub fn serialize<S: serde::Serializer>(
        v: &libghostty_vt::style::Underline,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        i32::from(*v).serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<libghostty_vt::style::Underline, D::Error> {
        libghostty_vt::style::Underline::try_from(i32::deserialize(d)?)
            .map_err(serde::de::Error::custom)
    }
}
pub(crate) mod cursor_style {
    use serde::{Deserialize, Serialize};
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde requires references"
    )]
    pub fn serialize<S: serde::Serializer>(
        v: &libghostty_vt::render::CursorVisualStyle,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        i32::from(*v).serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<libghostty_vt::render::CursorVisualStyle, D::Error> {
        libghostty_vt::render::CursorVisualStyle::try_from(i32::deserialize(d)?)
            .map_err(serde::de::Error::custom)
    }
}

pub(crate) mod palette {
    use super::{Deserialize, RgbColor, Serialize};
    pub fn serialize<S: serde::Serializer>(v: &[RgbColor], s: S) -> Result<S::Ok, S::Error> {
        v.iter()
            .map(|v| [v.r, v.g, v.b])
            .collect::<Vec<_>>()
            .serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<RgbColor>, D::Error> {
        Ok(Vec::<[u8; 3]>::deserialize(d)?
            .into_iter()
            .map(|[r, g, b]| RgbColor { r, g, b })
            .collect())
    }
}

mod wire_cells {
    use crate::terminal_frame::{CellStyle, RenderCell};
    use libghostty_vt::style::{RgbColor, Underline};
    use serde::{Deserialize, Serialize};
    #[derive(Serialize, Deserialize)]
    struct Cell(
        u16,
        usize,
        usize,
        Option<[u8; 3]>,
        Option<[u8; 3]>,
        u16,
        i32,
        Option<String>,
    );
    pub fn serialize<S: serde::Serializer>(
        cells: &[RenderCell],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        cells
            .iter()
            .map(|c| {
                let flags = [
                    c.style.bold,
                    c.style.italic,
                    c.style.faint,
                    c.style.blink,
                    c.style.inverse,
                    c.style.invisible,
                    c.style.strikethrough,
                    c.style.overline,
                ]
                .into_iter()
                .zip(0..8)
                .fold(0, |bits, (set, shift)| bits | (u16::from(set) << shift));
                Cell(
                    c.x,
                    c.text_start,
                    c.text_len,
                    c.fg.map(|v| [v.r, v.g, v.b]),
                    c.bg.map(|v| [v.r, v.g, v.b]),
                    flags,
                    i32::from(c.style.underline),
                    c.hyperlink.clone(),
                )
            })
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<RenderCell>, D::Error> {
        Vec::<Cell>::deserialize(deserializer)?
            .into_iter()
            .map(
                |Cell(x, text_start, text_len, fg, bg, flags, underline, hyperlink)| {
                    if flags > 255 {
                        return Err(serde::de::Error::custom("invalid style flags"));
                    }
                    Ok(RenderCell {
                        x,
                        y: 0,
                        text_start,
                        text_len,
                        fg: fg.map(|[r, g, b]| RgbColor { r, g, b }),
                        bg: bg.map(|[r, g, b]| RgbColor { r, g, b }),
                        hyperlink,
                        style: CellStyle {
                            bold: flags & 1 != 0,
                            italic: flags & 2 != 0,
                            faint: flags & 4 != 0,
                            blink: flags & 8 != 0,
                            inverse: flags & 16 != 0,
                            invisible: flags & 32 != 0,
                            strikethrough: flags & 64 != 0,
                            overline: flags & 128 != 0,
                            underline: Underline::try_from(underline)
                                .map_err(serde::de::Error::custom)?,
                        },
                    })
                },
            )
            .collect()
    }
}
