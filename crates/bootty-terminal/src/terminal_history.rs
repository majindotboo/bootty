//! Styled history capture and renderer adapters for the shared presentation contract.

use crate::terminal_capture::{CaptureFormat, CaptureOptions, CaptureScope, TerminalCapture};
use anyhow::{Result, ensure};

pub use bootty_control::terminal_history::{
    HistorySizeLimit, MAX_HISTORY_BYTES, history_plain_text, sanitize_history,
    styled_history_bytes, validate_history,
};

/// Capture a complete styled tail, reducing physical rows only when its encoded bytes exceed budget.
/// At most 17 attempts cover the supported 100000-row limit down to one complete row.
/// # Errors
/// Rejects unsafe captures, invalid limits and a single row that still exceeds the byte budget.
pub fn capture_checkpoint(
    mut options: CaptureOptions,
    mut capture: impl FnMut(CaptureOptions) -> Result<TerminalCapture>,
) -> Result<TerminalCapture> {
    options.validate().map_err(anyhow::Error::msg)?;
    ensure!(
        options.scope == CaptureScope::History && options.format == CaptureFormat::Ansi,
        "checkpoints require styled history capture"
    );
    ensure!(
        options.max_bytes <= MAX_HISTORY_BYTES,
        "checkpoint exceeds the saved history budget"
    );
    loop {
        let result = capture(options).and_then(|mut captured| {
            captured.text = sanitize_history(&captured.text)?;
            if captured.text.len() > options.max_bytes {
                return Err(HistorySizeLimit {
                    bytes: captured.text.len(),
                    max_bytes: options.max_bytes,
                }
                .into());
            }
            Ok(captured)
        });
        match result {
            Ok(captured) => return Ok(captured),
            Err(error) if error.is::<HistorySizeLimit>() && options.max_lines > 1 => {
                options.max_lines /= 2;
            }
            Err(error) => return Err(error),
        }
    }
}
