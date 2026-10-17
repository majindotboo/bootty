//! Rows in lists, menus and panels, with their text at the start.
//!
//! GPUI Kit's `Button` centers its label across whatever width it gets, and a column stretches
//! its children, so a labeled button in a list reads as centered text. A row's label starts at
//! the left and truncates; anything added after it sits at the right.

use gpui_kit::component::button::Button;
use gpui_kit::{ParentElement as _, SharedString, Styled as _, div};

/// A button that is one row of a list, menu or panel.
pub trait RowLabel {
    /// Shows `label` at the start of the row, truncated when it doesn't fit. Screen readers read
    /// it as the button's name.
    #[must_use]
    fn row_label(self, label: impl Into<SharedString>) -> Self;
}

impl RowLabel for Button {
    fn row_label(self, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        self.accessibility_label(label.clone()).child(
            div()
                .flex_grow(1.)
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label),
        )
    }
}
