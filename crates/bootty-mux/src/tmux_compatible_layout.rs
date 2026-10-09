use crate::snapshot::{MuxPaneLayout, MuxPaneSplitDirection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TmuxCompatibleLayoutParseError {
    FormatError,
    SyntaxError,
    ChecksumMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedLayout {
    width: usize,
    height: usize,
    content: ParsedLayoutContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ParsedLayoutContent {
    Pane(usize),
    Horizontal(Vec<ParsedLayout>),
    Vertical(Vec<ParsedLayout>),
}

/// Parses a tmux-compatible layout without requiring its checksum.
/// # Errors
/// Returns a format or syntax error for malformed layout dimensions or pane structure.
pub fn parse(input: &str) -> Result<MuxPaneLayout, TmuxCompatibleLayoutParseError> {
    parse_tree(input).and_then(into_mux_layout)
}

/// Parses a tmux-compatible layout and validates its four-digit checksum.
/// # Errors
/// Returns a syntax, checksum, or layout error when validation fails.
pub fn parse_with_checksum(input: &str) -> Result<MuxPaneLayout, TmuxCompatibleLayoutParseError> {
    if input.len() < 5 || input.as_bytes().get(4) != Some(&b',') {
        return Err(TmuxCompatibleLayoutParseError::SyntaxError);
    }

    let layout = input
        .get(5..)
        .ok_or(TmuxCompatibleLayoutParseError::SyntaxError)?;
    let checksum = tmux_layout_checksum(layout);
    if input.get(..4) != Some(tmux_layout_checksum_string(checksum).as_str()) {
        return Err(TmuxCompatibleLayoutParseError::ChecksumMismatch);
    }

    parse(layout)
}

#[must_use]
pub fn tmux_layout_checksum(input: &str) -> u16 {
    tmux_layout_checksum_bytes(input.as_bytes())
}

#[must_use]
pub fn tmux_layout_checksum_bytes(input: &[u8]) -> u16 {
    input.iter().fold(0u16, |checksum, byte| {
        checksum.rotate_right(1).wrapping_add(u16::from(*byte))
    })
}

#[must_use]
pub fn tmux_layout_checksum_string(checksum: u16) -> String {
    format!("{checksum:04x}")
}

fn parse_tree(input: &str) -> Result<ParsedLayout, TmuxCompatibleLayoutParseError> {
    let mut parser = TmuxLayoutParser { input };
    let layout = parser.parse_next()?;
    if parser.input.is_empty() {
        Ok(layout)
    } else {
        Err(TmuxCompatibleLayoutParseError::SyntaxError)
    }
}

fn into_mux_layout(layout: ParsedLayout) -> Result<MuxPaneLayout, TmuxCompatibleLayoutParseError> {
    match layout.content {
        ParsedLayoutContent::Pane(pane_id) => Ok(MuxPaneLayout::Pane(format!("%{pane_id}"))),
        ParsedLayoutContent::Horizontal(children) => {
            fold_children(MuxPaneSplitDirection::Right, children, |layout| {
                layout.width
            })
        }
        ParsedLayoutContent::Vertical(children) => {
            fold_children(MuxPaneSplitDirection::Down, children, |layout| {
                layout.height
            })
        }
    }
}

fn fold_children(
    direction: MuxPaneSplitDirection,
    children: Vec<ParsedLayout>,
    extent: fn(&ParsedLayout) -> usize,
) -> Result<MuxPaneLayout, TmuxCompatibleLayoutParseError> {
    let mut children = children.into_iter();
    let first = children
        .next()
        .ok_or(TmuxCompatibleLayoutParseError::SyntaxError)?;
    let rest = children.collect::<Vec<_>>();
    if rest.is_empty() {
        return into_mux_layout(first);
    }

    let first_extent = extent(&first);
    let total_extent = rest
        .iter()
        .map(extent)
        .try_fold(first_extent, usize::checked_add)
        .ok_or(TmuxCompatibleLayoutParseError::FormatError)?;
    let ratio_millis = first_extent
        .checked_mul(1000)
        .and_then(|scaled| scaled.checked_add(total_extent.checked_div(2)?))
        .and_then(|rounded| rounded.checked_div(total_extent.max(1)))
        .and_then(|ratio| u16::try_from(ratio.clamp(1, 999)).ok())
        .ok_or(TmuxCompatibleLayoutParseError::FormatError)?;
    let first_layout = into_mux_layout(first)?;
    let second_layout = fold_children(direction.clone(), rest, extent)?;

    Ok(MuxPaneLayout::Split {
        direction,
        ratio_millis,
        first: Box::new(first_layout),
        second: Box::new(second_layout),
    })
}

struct TmuxLayoutParser<'a> {
    input: &'a str,
}

impl TmuxLayoutParser<'_> {
    fn parse_next(&mut self) -> Result<ParsedLayout, TmuxCompatibleLayoutParseError> {
        let width = self.read_number_until(b'x', true)?;
        let height = self.read_number_until(b',', true)?;
        let _x = self.read_number_until(b',', true)?;
        let _y = self.read_number_until_any(b",{[")?;
        let delimiter = *self
            .input
            .as_bytes()
            .first()
            .ok_or(TmuxCompatibleLayoutParseError::SyntaxError)?;

        let content = match delimiter {
            b',' => {
                self.consume(1)?;
                let pane_id = self.read_number_until_any(b",}]")?;
                ParsedLayoutContent::Pane(pane_id)
            }
            b'{' | b'[' => {
                self.consume(1)?;
                let mut children = Vec::new();
                loop {
                    children.push(self.parse_next()?);
                    let next = *self
                        .input
                        .as_bytes()
                        .first()
                        .ok_or(TmuxCompatibleLayoutParseError::SyntaxError)?;
                    if next == b',' {
                        self.consume(1)?;
                        continue;
                    }

                    let expected = if delimiter == b'{' { b'}' } else { b']' };
                    if next != expected {
                        return Err(TmuxCompatibleLayoutParseError::SyntaxError);
                    }
                    self.consume(1)?;
                    break;
                }
                if delimiter == b'{' {
                    ParsedLayoutContent::Horizontal(children)
                } else {
                    ParsedLayoutContent::Vertical(children)
                }
            }
            _ => return Err(TmuxCompatibleLayoutParseError::SyntaxError),
        };

        Ok(ParsedLayout {
            width,
            height,
            content,
        })
    }

    fn consume(&mut self, count: usize) -> Result<(), TmuxCompatibleLayoutParseError> {
        self.input = self
            .input
            .get(count..)
            .ok_or(TmuxCompatibleLayoutParseError::SyntaxError)?;
        Ok(())
    }

    fn read_number_until(
        &mut self,
        delimiter: u8,
        consume: bool,
    ) -> Result<usize, TmuxCompatibleLayoutParseError> {
        let index = self
            .input
            .as_bytes()
            .iter()
            .position(|byte| *byte == delimiter)
            .ok_or(TmuxCompatibleLayoutParseError::SyntaxError)?;
        let number = self.read_number(index)?;
        if consume {
            self.consume(1)?;
        }
        Ok(number)
    }

    fn read_number_until_any(
        &mut self,
        delimiters: &[u8],
    ) -> Result<usize, TmuxCompatibleLayoutParseError> {
        let index = self
            .input
            .as_bytes()
            .iter()
            .position(|byte| delimiters.contains(byte))
            .unwrap_or(self.input.len());
        self.read_number(index)
    }

    fn read_number(&mut self, count: usize) -> Result<usize, TmuxCompatibleLayoutParseError> {
        let (digits, rest) = self
            .input
            .split_at_checked(count)
            .ok_or(TmuxCompatibleLayoutParseError::SyntaxError)?;
        let number =
            parse_tmux_number(digits).map_err(|_| TmuxCompatibleLayoutParseError::SyntaxError)?;
        self.input = rest;
        Ok(number)
    }
}

fn parse_tmux_number(input: &str) -> Result<usize, TmuxCompatibleLayoutParseError> {
    input
        .parse::<usize>()
        .map_err(|_| TmuxCompatibleLayoutParseError::FormatError)
}

/// Encodes saved geometry using only freshly created backend pane identities.
/// # Errors
/// Returns invalid pane mappings, sizes, or saved layout errors.
pub fn restore_window_layout(
    window: &crate::session_snapshot::SavedTerminalWindow,
    pane_ids: &[String],
) -> anyhow::Result<(u16, u16, String)> {
    anyhow::ensure!(
        window.panes.len() == pane_ids.len(),
        "restored pane count differs"
    );
    let first = window
        .panes
        .first()
        .ok_or_else(|| anyhow::anyhow!("saved window has no panes"))?;
    let layout =
        window.layout.clone().unwrap_or_else(|| {
            window.panes.iter().skip(1).fold(
                MuxPaneLayout::Pane(first.id.clone()),
                |first, pane| MuxPaneLayout::Split {
                    direction: MuxPaneSplitDirection::Down,
                    ratio_millis: 500,
                    first: Box::new(first),
                    second: Box::new(MuxPaneLayout::Pane(pane.id.clone())),
                },
            )
        });
    let (cols, rows) = saved_layout_size(&layout, window)?;
    let cols = u16::try_from(cols)?;
    let rows = u16::try_from(rows)?;
    let text = encode_saved_layout(
        &layout,
        window,
        pane_ids,
        (u32::from(cols), u32::from(rows), 0, 0),
    )?;
    Ok((
        cols,
        rows,
        format!(
            "{},{}",
            tmux_layout_checksum_string(tmux_layout_checksum(&text)),
            text
        ),
    ))
}

fn saved_layout_size(
    layout: &MuxPaneLayout,
    window: &crate::session_snapshot::SavedTerminalWindow,
) -> anyhow::Result<(u32, u32)> {
    match layout {
        MuxPaneLayout::Pane(id) => {
            let pane = window
                .panes
                .iter()
                .find(|pane| pane.id == *id)
                .ok_or_else(|| anyhow::anyhow!("layout pane is absent from saved window"))?;
            Ok((
                u32::from(if pane.cols == 0 { 80 } else { pane.cols }).max(2),
                u32::from(if pane.rows == 0 { 24 } else { pane.rows }).max(2),
            ))
        }
        MuxPaneLayout::Split {
            direction,
            first,
            second,
            ..
        } => {
            let (first_cols, first_rows) = saved_layout_size(first, window)?;
            let (second_cols, second_rows) = saved_layout_size(second, window)?;
            combined_layout_size(
                direction,
                (first_cols, first_rows),
                (second_cols, second_rows),
            )
        }
    }
}

fn encode_saved_layout(
    layout: &MuxPaneLayout,
    window: &crate::session_snapshot::SavedTerminalWindow,
    pane_ids: &[String],
    geometry: (u32, u32, u32, u32),
) -> anyhow::Result<String> {
    let (cols, rows, x, y) = geometry;
    let prefix = format!("{cols}x{rows},{x},{y}");
    match layout {
        MuxPaneLayout::Pane(id) => {
            let index = window
                .panes
                .iter()
                .position(|pane| pane.id == *id)
                .ok_or_else(|| anyhow::anyhow!("layout pane is absent from saved window"))?;
            let id = pane_ids
                .get(index)
                .ok_or_else(|| anyhow::anyhow!("missing restored pane"))?;
            let id = id
                .strip_prefix('%')
                .ok_or_else(|| anyhow::anyhow!("invalid restored pane identity"))?
                .parse::<u32>()?;
            Ok(format!("{prefix},{id}"))
        }
        MuxPaneLayout::Split {
            direction,
            ratio_millis,
            first,
            second,
        } => {
            let (first_cols, first_rows) = minimum_layout_size(first)?;
            let (second_cols, second_rows) = minimum_layout_size(second)?;
            let (extent, minimum_first, minimum_second) = match direction {
                MuxPaneSplitDirection::Right => (cols, first_cols, second_cols),
                MuxPaneSplitDirection::Down => (rows, first_rows, second_rows),
            };
            let available = extent
                .checked_sub(1)
                .ok_or_else(|| anyhow::anyhow!("saved split is too small"))?;
            let minimum = minimum_first
                .checked_add(minimum_second)
                .ok_or_else(|| anyhow::anyhow!("saved split dimensions overflow"))?;
            anyhow::ensure!(available >= minimum, "saved split is too small");
            let scaled = u64::from(available)
                .checked_mul(u64::from(*ratio_millis))
                .and_then(|scaled| scaled.checked_add(500))
                .and_then(|scaled| scaled.checked_div(1000))
                .ok_or_else(|| anyhow::anyhow!("saved split ratio overflows"))?;
            let first_extent = u32::try_from(scaled)?
                .clamp(minimum_first, available.saturating_sub(minimum_second));
            let second_extent = available.saturating_sub(first_extent);
            let (first_geometry, second_geometry, open, close) = match direction {
                MuxPaneSplitDirection::Right => (
                    (first_extent, rows, x, y),
                    (second_extent, rows, split_offset(x, first_extent)?, y),
                    '{',
                    '}',
                ),
                MuxPaneSplitDirection::Down => (
                    (cols, first_extent, x, y),
                    (cols, second_extent, x, split_offset(y, first_extent)?),
                    '[',
                    ']',
                ),
            };
            Ok(format!(
                "{prefix}{open}{},{}{close}",
                encode_saved_layout(first, window, pane_ids, first_geometry)?,
                encode_saved_layout(second, window, pane_ids, second_geometry)?
            ))
        }
    }
}

fn minimum_layout_size(layout: &MuxPaneLayout) -> anyhow::Result<(u32, u32)> {
    match layout {
        MuxPaneLayout::Pane(_) => Ok((2, 2)),
        MuxPaneLayout::Split {
            direction,
            first,
            second,
            ..
        } => {
            let (first_cols, first_rows) = minimum_layout_size(first)?;
            let (second_cols, second_rows) = minimum_layout_size(second)?;
            combined_layout_size(
                direction,
                (first_cols, first_rows),
                (second_cols, second_rows),
            )
        }
    }
}

fn combined_layout_size(
    direction: &MuxPaneSplitDirection,
    first: (u32, u32),
    second: (u32, u32),
) -> anyhow::Result<(u32, u32)> {
    Ok(match direction {
        MuxPaneSplitDirection::Right => (split_offset(first.0, second.0)?, first.1.max(second.1)),
        MuxPaneSplitDirection::Down => (first.0.max(second.0), split_offset(first.1, second.1)?),
    })
}

fn split_offset(offset: u32, extent: u32) -> anyhow::Result<u32> {
    offset
        .checked_add(extent)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| anyhow::anyhow!("saved layout dimensions overflow"))
}
