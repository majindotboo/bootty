use smallvec::SmallVec;

use crate::{terminal_font_face::is_symbol_codepoint, terminal_text::for_terminal_text_cells};

/// One glyph produced by shaping a text run. Glyph ids index the same font face
/// that `ab_glyph` loads from the identical bytes, so they can be rasterized
/// directly via [`ab_glyph::GlyphId`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub glyph_id: u16,
    /// Cell-relative origin chosen by the Ghostty-compatible shaper. Glyphs
    /// that belong to the same ligature can share this origin even when
    /// `HarfBuzz` reports different source clusters.
    pub cluster: u32,
    pub x_offset: f32,
    pub y_offset: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalTextShaper;

impl TerminalTextShaper {
    #[must_use]
    pub fn shape(&self, text: &str, start_cell: u16) -> Vec<ShapedCluster> {
        let mut clusters = Vec::with_capacity(text.chars().count().max(1));
        self.shape_into(text, start_cell, &mut clusters);
        clusters
    }

    pub fn shape_into(
        &self,
        text: &str,
        start_cell: u16,
        clusters: &mut Vec<ShapedCluster>,
    ) -> u16 {
        let (total_cells, cluster_len) = self.shape_into_retained(text, start_cell, clusters);
        clusters.truncate(cluster_len);
        total_cells
    }

    #[allow(
        clippy::unused_self,
        reason = "Retains the shaper instance API used by the host and atlas."
    )]
    pub(super) fn shape_into_retained(
        &self,
        text: &str,
        start_cell: u16,
        clusters: &mut Vec<ShapedCluster>,
    ) -> (u16, usize) {
        if is_printable_ascii(text) {
            return shape_ascii_into_retained(text, start_cell, clusters);
        }

        let mut cluster_index = 0_usize;
        let mut previous_cell = 0;
        let total_cells = for_terminal_text_cells(text, |cell, text| {
            if let Some(previous) = cluster_index
                .checked_sub(1)
                .and_then(|index| clusters.get_mut(index))
            {
                previous.cells = cell.saturating_sub(previous_cell).max(1);
            }
            with_shaped_cluster(clusters, cluster_index, |cluster| {
                cluster.text.clear();
                cluster.glyphs.clear();
                cluster.text.push_str(text);
                cluster.cell = start_cell.saturating_add(cell);
                cluster.is_whitespace = text.chars().all(char::is_whitespace);
            });
            previous_cell = cell;
            cluster_index = cluster_index.saturating_add(1);
        });
        if let Some(last) = cluster_index
            .checked_sub(1)
            .and_then(|index| clusters.get_mut(index))
        {
            last.cells = total_cells.saturating_sub(previous_cell).max(1);
        }
        (total_cells.max(1), cluster_index)
    }
}

pub(super) fn is_printable_ascii(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b' '..=b'~'))
}

fn shape_ascii_into_retained(
    text: &str,
    start_cell: u16,
    clusters: &mut Vec<ShapedCluster>,
) -> (u16, usize) {
    let mut cell = start_cell;
    let mut cluster_index = 0_usize;
    for byte in text.bytes() {
        with_shaped_cluster(clusters, cluster_index, |cluster| {
            cluster.text.clear();
            cluster.glyphs.clear();
            cluster.text.push(char::from(byte));
            cluster.cell = cell;
            cluster.cells = 1;
            cluster.is_whitespace = byte == b' ';
        });
        cell = cell.saturating_add(1);
        cluster_index = cluster_index.saturating_add(1);
    }
    (
        u16::try_from(text.len()).unwrap_or(u16::MAX).max(1),
        cluster_index,
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapedCluster {
    pub text: String,
    pub cell: u16,
    pub cells: u16,
    pub is_whitespace: bool,
    /// Glyphs to rasterize by id when the font shaped this cluster into
    /// ligatures or contextual alternates. Empty means render the cluster
    /// through the per-character path (the common case, and all fallback,
    /// emoji, symbol, and combining-mark handling).
    pub(crate) glyphs: SmallVec<[ShapedGlyph; 2]>,
}

pub(super) fn with_shaped_cluster(
    clusters: &mut Vec<ShapedCluster>,
    index: usize,
    write: impl FnOnce(&mut ShapedCluster),
) {
    if let Some(cluster) = clusters.get_mut(index) {
        write(cluster);
    } else {
        let mut cluster = ShapedCluster {
            text: String::new(),
            cell: 0,
            cells: 0,
            is_whitespace: false,
            glyphs: smallvec::SmallVec::new(),
        };
        write(&mut cluster);
        clusters.push(cluster);
    }
}

pub(super) const fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch,
        '\u{0300}'..='\u{036F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{1DC0}'..='\u{1DFF}' | '\u{20D0}'..='\u{20FF}' | '\u{FE20}'..='\u{FE2F}'
    )
}

pub(super) const fn is_variation_selector(ch: char) -> bool {
    matches!(ch, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

pub(super) const fn is_default_emoji_presentation(ch: char) -> bool {
    matches!(
        ch,
        '\u{231A}'..='\u{231B}' | '\u{23E9}'..='\u{23EC}' | '\u{23F0}' | '\u{23F3}' | '\u{25FD}'..='\u{25FE}'
            | '\u{2614}'..='\u{2615}' | '\u{2648}'..='\u{2653}' | '\u{267F}' | '\u{2693}' | '\u{26A1}'
            | '\u{26AA}'..='\u{26AB}' | '\u{26BD}'..='\u{26BE}' | '\u{26C4}'..='\u{26C5}' | '\u{26CE}'
            | '\u{26D4}' | '\u{26EA}' | '\u{26F2}'..='\u{26F3}' | '\u{26F5}' | '\u{26FA}' | '\u{26FD}'
            | '\u{2705}' | '\u{270A}'..='\u{270B}' | '\u{2728}' | '\u{274C}' | '\u{274E}'
            | '\u{2753}'..='\u{2755}' | '\u{2757}' | '\u{2795}'..='\u{2797}' | '\u{27B0}' | '\u{27BF}'
            | '\u{2B1B}'..='\u{2B1C}' | '\u{2B50}' | '\u{2B55}' | '\u{1F004}' | '\u{1F0CF}' | '\u{1F18E}'
            | '\u{1F191}'..='\u{1F19A}' | '\u{1F1E6}'..='\u{1F1FF}' | '\u{1F201}' | '\u{1F21A}' | '\u{1F22F}'
            | '\u{1F232}'..='\u{1F236}' | '\u{1F238}'..='\u{1F23A}' | '\u{1F250}'..='\u{1F251}'
            | '\u{1F300}'..='\u{1F320}' | '\u{1F32D}'..='\u{1F335}' | '\u{1F337}'..='\u{1F37C}'
            | '\u{1F37E}'..='\u{1F393}' | '\u{1F3A0}'..='\u{1F3CA}' | '\u{1F3CF}'..='\u{1F3D3}'
            | '\u{1F3E0}'..='\u{1F3F0}' | '\u{1F3F4}' | '\u{1F3F8}'..='\u{1F43E}' | '\u{1F440}'
            | '\u{1F442}'..='\u{1F4FC}' | '\u{1F4FF}'..='\u{1F53D}' | '\u{1F54B}'..='\u{1F54E}'
            | '\u{1F550}'..='\u{1F567}' | '\u{1F57A}' | '\u{1F595}'..='\u{1F596}' | '\u{1F5A4}'
            | '\u{1F5FB}'..='\u{1F64F}' | '\u{1F680}'..='\u{1F6C5}' | '\u{1F6CC}' | '\u{1F6D0}'..='\u{1F6D2}'
            | '\u{1F6D5}'..='\u{1F6D7}' | '\u{1F6DC}'..='\u{1F6DF}' | '\u{1F6EB}'..='\u{1F6EC}'
            | '\u{1F6F4}'..='\u{1F6FC}' | '\u{1F7E0}'..='\u{1F7EB}' | '\u{1F7F0}' | '\u{1F90C}'..='\u{1F93A}'
            | '\u{1F93C}'..='\u{1F945}' | '\u{1F947}'..='\u{1F9FF}' | '\u{1FA70}'..='\u{1FA7C}'
            | '\u{1FA80}'..='\u{1FA89}' | '\u{1FA8F}'..='\u{1FAC6}' | '\u{1FACE}'..='\u{1FADC}'
            | '\u{1FADF}'..='\u{1FAE9}' | '\u{1FAF0}'..='\u{1FAF8}'
    )
}

pub(super) const fn is_private_use(ch: char) -> bool {
    matches!(
        ch,
        '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}'
    )
}

pub(super) fn is_symbol_like(ch: char) -> bool {
    is_symbol_codepoint(u32::from(ch))
}

pub(super) const fn is_terminal_graphics_symbol(ch: char) -> bool {
    matches!(
        ch,
        '\u{2500}'..='\u{259F}' | '\u{1CC00}'..='\u{1CEBF}' | '\u{1FB00}'..='\u{1FBFF}' | '\u{E0B0}'..='\u{E0D7}'
    )
}

pub(super) const fn is_symbol_space(ch: char) -> bool {
    matches!(ch, '\u{0020}' | '\u{2002}')
}

pub(super) fn single_ascii_cluster(cluster: &ShapedCluster) -> Option<u8> {
    let [byte] = cluster.text.as_bytes() else {
        return None;
    };
    byte.is_ascii().then_some(*byte)
}

pub(super) fn is_color_emoji_cluster(cluster: &ShapedCluster) -> bool {
    if cluster.text.contains('\u{fe0e}') {
        return false;
    }
    cluster
        .text
        .chars()
        .any(|ch| ch == '\u{fe0f}' || is_default_emoji_presentation(ch))
}

pub(super) fn cluster_constraint_cells(
    previous: Option<&ShapedCluster>,
    cluster: &ShapedCluster,
    next: Option<&ShapedCluster>,
) -> u16 {
    if cluster.cells > 1 {
        return cluster.cells;
    }
    // A color emoji fills exactly its grid cells. The neighbor-based widening below is for lone
    // monochrome symbols (arrows, shapes) that read better spanning two cells; applied to an emoji
    // it spills the glyph into the next column — eating a following space and making the rendered
    // width flip with whatever character happens to come after it.
    if is_color_emoji_cluster(cluster) {
        return cluster.cells;
    }
    let Some(ch) = cluster.text.chars().next() else {
        return cluster.cells;
    };
    if is_terminal_graphics_symbol(ch) {
        return 1;
    }
    if !is_symbol_like(ch) {
        return cluster.cells;
    }
    if previous
        .and_then(|previous| previous.text.chars().next())
        .is_some_and(|previous| is_symbol_like(previous) && !is_terminal_graphics_symbol(previous))
    {
        return 1;
    }
    if next
        .and_then(|next| next.text.chars().next())
        .is_none_or(is_symbol_space)
    {
        2
    } else {
        cluster.cells
    }
}
