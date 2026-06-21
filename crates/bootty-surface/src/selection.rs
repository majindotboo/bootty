#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectionPoint {
    pub x: u16,
    pub y: u16,
}

impl SelectionPoint {
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSelection {
    pub anchor: SelectionPoint,
    pub focus: SelectionPoint,
}

impl TerminalSelection {
    pub const fn new(anchor: SelectionPoint, focus: SelectionPoint) -> Self {
        Self { anchor, focus }
    }

    pub fn ordered(self) -> (SelectionPoint, SelectionPoint) {
        if self.anchor.y < self.focus.y
            || (self.anchor.y == self.focus.y && self.anchor.x <= self.focus.x)
        {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    pub fn is_collapsed(self) -> bool {
        self.anchor == self.focus
    }

    pub fn row_ranges(self, rows: u16, cols: u16) -> SelectionRowRanges {
        let (start, end) = self.ordered();
        SelectionRowRanges {
            start,
            end,
            row: start.y,
            rows,
            cols,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TerminalSelectionState {
    selection: Option<TerminalSelection>,
    dragging: bool,
}

impl TerminalSelectionState {
    pub fn begin(&mut self, point: SelectionPoint) {
        self.selection = Some(TerminalSelection::new(point, point));
        self.dragging = true;
    }

    pub fn select_range(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        self.selection = Some(TerminalSelection::new(anchor, focus));
        self.dragging = false;
    }

    pub fn drag_to(&mut self, point: SelectionPoint) {
        if let Some(selection) = &mut self.selection {
            selection.focus = point;
        }
    }

    pub fn finish(&mut self, point: SelectionPoint) {
        if !self.dragging {
            return;
        }
        self.drag_to(point);
        self.dragging = false;
        if self.selection.is_some_and(TerminalSelection::is_collapsed) {
            self.selection = None;
        }
    }

    pub fn clear(&mut self) {
        self.selection = None;
        self.dragging = false;
    }

    pub const fn selection(&self) -> Option<TerminalSelection> {
        self.selection
    }

    pub const fn is_dragging(&self) -> bool {
        self.dragging
    }
}

pub struct SelectionRowRanges {
    start: SelectionPoint,
    end: SelectionPoint,
    row: u16,
    rows: u16,
    cols: u16,
}

impl Iterator for SelectionRowRanges {
    type Item = (u16, std::ops::Range<u16>);

    fn next(&mut self) -> Option<Self::Item> {
        while self.row <= self.end.y && self.row < self.rows {
            let row = self.row;
            self.row = self.row.saturating_add(1);
            let start_x = if row == self.start.y { self.start.x } else { 0 };
            let end_x = if row == self.end.y {
                self.end.x.saturating_add(1)
            } else {
                self.cols
            };
            let start_x = start_x.min(self.cols);
            let end_x = end_x.min(self.cols);
            if start_x < end_x {
                return Some((row, start_x..end_x));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{SelectionPoint, TerminalSelection, TerminalSelectionState};

    #[test]
    fn selection_orders_anchor_and_focus() {
        let selection =
            TerminalSelection::new(SelectionPoint::new(4, 3), SelectionPoint::new(1, 2));

        assert_eq!(
            selection.ordered(),
            (SelectionPoint::new(1, 2), SelectionPoint::new(4, 3))
        );
    }

    #[test]
    fn row_ranges_span_partial_and_full_rows() {
        let selection =
            TerminalSelection::new(SelectionPoint::new(3, 1), SelectionPoint::new(2, 3));
        let ranges = selection.row_ranges(5, 8).collect::<Vec<_>>();

        assert_eq!(ranges, vec![(1, 3..8), (2, 0..8), (3, 0..3)]);
    }

    #[test]
    fn click_without_drag_clears_selection() {
        let mut state = TerminalSelectionState::default();

        state.begin(SelectionPoint::new(2, 1));
        state.finish(SelectionPoint::new(2, 1));

        assert_eq!(state.selection(), None);
        assert!(!state.is_dragging());
    }
}
