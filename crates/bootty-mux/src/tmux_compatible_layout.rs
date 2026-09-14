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
