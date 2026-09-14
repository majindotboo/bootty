use anyhow::{Result, bail};
use libghostty_vt::{
    fmt::Format,
    screen::Screen,
    selection::{FormatOptions, Selection},
    terminal::{Point, PointCoordinate},
};

use super::TerminalEngine;
use crate::terminal_capture::{CaptureFormat, CaptureOptions, CaptureScope, TerminalCapture};

impl TerminalEngine {
    /// Snapshot the requested range without installing or moving the user's selection.
    ///
    /// # Errors
    /// Returns an error for invalid limits or a failure to read or format terminal rows.
    pub fn capture(&self, options: CaptureOptions) -> Result<TerminalCapture> {
        options.validate().map_err(anyhow::Error::msg)?;
        let (cols, rows) = self.grid_size();
        let total = match options.scope {
            CaptureScope::Screen => u64::from(rows),
            CaptureScope::History => self.terminal.scrollbar()?.total,
        };
        let captured = total.min(u64::from(options.max_lines));
        let start = total.saturating_sub(captured);
        let point = |x, y| match options.scope {
            CaptureScope::Screen => Point::Active(PointCoordinate { x, y }),
            CaptureScope::History => Point::Screen(PointCoordinate { x, y }),
        };
        let selection = Selection::new(
            self.terminal.grid_ref(point(0, u32::try_from(start)?))?,
            self.terminal.grid_ref(point(
                cols.saturating_sub(1),
                u32::try_from(total.saturating_sub(1))?,
            ))?,
            false,
        );
        let format = || {
            FormatOptions::new()
                .with_selection(&selection)
                .with_emit_format(match options.format {
                    CaptureFormat::Plain => Format::Plain,
                    CaptureFormat::Ansi => Format::Vt,
                    CaptureFormat::Html => Format::Html,
                })
                .with_unwrap(options.unwrap)
                .with_trim(true)
        };
        let size = match self.terminal.format_selection_buf(format(), &mut []) {
            Ok(size) => size.unwrap_or_default(),
            Err(libghostty_vt::Error::OutOfSpace { required }) => required,
            Err(error) => return Err(error.into()),
        };
        // Never truncate encoded styles or a UTF-8 codepoint. The caller can request fewer rows.
        if size > options.max_bytes {
            bail!(
                "Capture needs {size} bytes, exceeding the {} byte limit; request fewer lines",
                options.max_bytes
            );
        }
        let mut bytes = vec![0; size];
        let written = self
            .terminal
            .format_selection_buf(format(), &mut bytes)?
            .unwrap_or_default();
        bytes.truncate(written);
        Ok(TerminalCapture {
            cols,
            rows,
            scope: options.scope,
            format: options.format,
            alternate_screen: self.terminal.active_screen()? == Screen::Alternate,
            captured_lines: u32::try_from(captured)?,
            omitted_lines: start,
            text: String::from_utf8(bytes)?,
        })
    }
}
