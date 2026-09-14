use serde::{Deserialize, Serialize};

/// Limits apply to physical terminal rows and formatted UTF-8 bytes.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureOptions {
    pub scope: CaptureScope,
    pub format: CaptureFormat,
    pub max_lines: u32,
    pub max_bytes: usize,
    pub unwrap: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            scope: CaptureScope::Screen,
            format: CaptureFormat::Plain,
            max_lines: 10_000,
            max_bytes: 1024 * 1024,
            unwrap: true,
        }
    }
}

impl CaptureOptions {
    ///
    /// # Errors
    /// Returns an error when the row or byte limit is outside the supported range.
    pub fn validate(self) -> Result<(), String> {
        if !(1..=100_000).contains(&self.max_lines) {
            return Err("Capture line limit must be between 1 and 100000".to_owned());
        }
        if !(1..=2 * 1024 * 1024).contains(&self.max_bytes) {
            return Err("Capture byte limit must be between 1 and 2097152".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureScope {
    #[default]
    Screen,
    History,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFormat {
    #[default]
    Plain,
    Ansi,
    Html,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TerminalCapture {
    pub cols: u16,
    pub rows: u16,
    pub scope: CaptureScope,
    pub format: CaptureFormat,
    pub alternate_screen: bool,
    pub captured_lines: u32,
    pub omitted_lines: u64,
    /// Formatted VT state, not the original process output stream.
    pub text: String,
}
