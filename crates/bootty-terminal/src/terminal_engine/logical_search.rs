use crate::terminal_frame::{FrameSelection, RenderFrame};
use crate::terminal_search::SearchPattern;

use libghostty_vt::terminal::PointCoordinate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CopyModeSearchMatch {
    pub(super) start: PointCoordinate,
    pub(super) end: PointCoordinate,
}

pub(super) fn frame_search_matches(
    frame: &RenderFrame,
    query: &SearchPattern,
) -> Vec<Vec<FrameSelection>> {
    let mut rows = vec![vec![None; usize::from(frame.cols)]; usize::from(frame.rows)];
    for cell in &frame.cells {
        if !cell.style.invisible
            && cell.text_len > 0
            && let Some(slot) = rows
                .get_mut(usize::from(cell.y))
                .and_then(|row| row.get_mut(usize::from(cell.x)))
        {
            *slot = Some(frame.cell_text(cell));
        }
    }
    let mut matches = Vec::new();
    let mut logical = Vec::new();
    let mut positions = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        let wrapped = frame.row_wraps.get(row_index).copied().unwrap_or(false);
        let length = if wrapped {
            row.len()
        } else {
            row.iter()
                .rposition(Option::is_some)
                .map_or(0, |x| x.saturating_add(1))
        };
        let mut col = 0;
        while col < length {
            let chars = row.get(col).copied().flatten().unwrap_or(&[' ']);
            let (_, width) = libghostty_vt::unicode::grapheme_width(chars);
            let width = usize::from(width.max(1));
            for &ch in chars {
                logical.push(ch);
                positions.push((
                    u16::try_from(row_index).unwrap_or(u16::MAX),
                    u16::try_from(col).unwrap_or(u16::MAX),
                    u16::try_from(
                        col.saturating_add(width)
                            .saturating_sub(1)
                            .min(row.len().saturating_sub(1)),
                    )
                    .unwrap_or(u16::MAX),
                ));
            }
            col = col.saturating_add(width);
        }
        if !wrapped {
            push_frame_matches(&mut matches, &logical, &positions, query);
            logical.clear();
            positions.clear();
        }
    }
    push_frame_matches(&mut matches, &logical, &positions, query);
    matches
}

pub(super) fn copy_mode_logical_search_matches(
    logical: &[char],
    positions: &[(PointCoordinate, PointCoordinate)],
    query: &SearchPattern,
) -> Vec<CopyModeSearchMatch> {
    query
        .ranges(logical)
        .into_iter()
        .filter_map(|range| {
            let matched = positions.get(range)?;
            Some(CopyModeSearchMatch {
                start: matched.first()?.0,
                end: matched.last()?.1,
            })
        })
        .collect()
}

fn push_frame_matches(
    matches: &mut Vec<Vec<FrameSelection>>,
    logical: &[char],
    positions: &[(u16, u16, u16)],
    query: &SearchPattern,
) {
    for range in query.ranges(logical) {
        let mut segments = Vec::new();
        let Some(positions) = positions.get(range) else {
            continue;
        };
        push_position_range(&mut segments, positions);
        matches.push(segments);
    }
}

fn push_position_range(matches: &mut Vec<FrameSelection>, positions: &[(u16, u16, u16)]) {
    let Some((&(mut row, mut start_col, mut end_col), remaining)) = positions.split_first() else {
        return;
    };
    for &(next_row, next_col, next_end) in remaining {
        if next_row == row && next_col <= end_col.saturating_add(1) {
            end_col = next_end;
            continue;
        }
        matches.push(FrameSelection {
            row,
            start_col,
            end_col,
        });
        row = next_row;
        start_col = next_col;
        end_col = next_end;
    }
    matches.push(FrameSelection {
        row,
        start_col,
        end_col,
    });
}
