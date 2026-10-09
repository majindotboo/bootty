//! Bounded terminal presentation: text and SGR styles, never terminal commands.

use anyhow::{Result, bail, ensure};

pub const MAX_HISTORY_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub struct HistorySizeLimit {
    pub bytes: usize,
    pub max_bytes: usize,
}

impl std::fmt::Display for HistorySizeLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Capture needs {} bytes, exceeding the {} byte limit; request fewer lines",
            self.bytes, self.max_bytes
        )
    }
}

impl std::error::Error for HistorySizeLimit {}

/// Validate persisted history without accepting terminal queries, modes or side effects.
/// # Errors
/// Rejects oversized text, non-style escapes and malformed or unsupported SGR parameters.
pub fn validate_history(text: &str) -> Result<()> {
    walk_history(text, false, true, |_| {})
}

/// Normalize saved rows for an output-only renderer, isolating styles from fresh output.
/// # Errors
/// Rejects oversized history and controls other than text, whitespace and SGR styles.
pub fn styled_history_bytes(text: &str) -> Result<Vec<u8>> {
    validate_history(text)?;
    let normalized = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    let styled = text.contains('\x1b');
    let mut bytes = Vec::new();
    if styled {
        bytes.extend_from_slice(b"\x1b[0m");
    }
    bytes.extend_from_slice(normalized.as_bytes());
    if styled {
        bytes.extend_from_slice(b"\x1b[0m");
    }
    if !normalized.is_empty() && !normalized.ends_with("\r\n") {
        bytes.extend_from_slice(b"\r\n");
    }
    Ok(bytes)
}

/// Keep displayed text and styles from a fresh formatter capture, discarding OSC metadata.
/// # Errors
/// Rejects incomplete controls, non-style commands and captures beyond the history budget.
pub fn sanitize_history(text: &str) -> Result<String> {
    check_size(text.len(), MAX_HISTORY_BYTES)?;
    let palette = capture_palette(text);
    let mut result = String::with_capacity(text.len().min(MAX_HISTORY_BYTES));
    walk_history(text, true, true, |part| {
        if let Some(resolved) = resolved_palette_style(part, &palette) {
            result.push_str(&resolved);
        } else {
            result.push_str(part);
        }
    })?;
    validate_history(&result)?;
    Ok(result)
}

fn capture_palette(text: &str) -> [Option<[u8; 3]>; 256] {
    let mut palette = [None; 256];
    for osc in text.split("\x1b]").skip(1) {
        let Some(record) = osc.split(['\x07', '\x1b']).next() else {
            continue;
        };
        let Some(record) = record.strip_prefix("4;") else {
            continue;
        };
        let Some((index, rgb)) = record.split_once(';') else {
            continue;
        };
        let Ok(index) = index.parse::<u8>() else {
            continue;
        };
        let Some(rgb) = rgb.strip_prefix("rgb:") else {
            continue;
        };
        let mut channels = rgb.split('/');
        let read = |value: Option<&str>| {
            value
                .filter(|value| value.len() == 2)
                .and_then(|value| u8::from_str_radix(value, 16).ok())
        };
        if let (Some(r), Some(g), Some(b), None) = (
            read(channels.next()),
            read(channels.next()),
            read(channels.next()),
            channels.next(),
        ) && let Some(color) = palette.get_mut(usize::from(index))
        {
            *color = Some([r, g, b]);
        }
    }
    palette
}

fn resolved_palette_style(style: &str, palette: &[Option<[u8; 3]>; 256]) -> Option<String> {
    // The pinned formatter emits each indexed color in its own SGR; extend for combined output if that changes.
    let parameters = style.strip_prefix("\x1b[")?.strip_suffix('m')?;
    let mut parts = parameters.split(';');
    let prefix = parts.next()?;
    if !matches!(prefix, "38" | "48" | "58") || parts.next()? != "5" {
        return None;
    }
    let index = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let [r, g, b] = (*palette.get(usize::from(index))?)?;
    Some(format!("\x1b[{prefix};2;{r};{g};{b}m"))
}

/// Extract readable archive text from a validated styled checkpoint.
/// # Errors
/// Rejects history that cannot safely be restored.
pub fn history_plain_text(text: &str) -> Result<String> {
    let mut result = String::with_capacity(text.len().min(MAX_HISTORY_BYTES));
    walk_history(text, false, false, |part| result.push_str(part))?;
    Ok(result)
}

fn walk_history(
    text: &str,
    strip_osc: bool,
    keep_styles: bool,
    mut push: impl FnMut(&str),
) -> Result<()> {
    check_size(text.len(), MAX_HISTORY_BYTES)?;
    let mut rest = text;
    while !rest.is_empty() {
        let plain_end = rest.find('\x1b').unwrap_or(rest.len());
        let (plain, controls) = rest
            .split_at_checked(plain_end)
            .ok_or_else(|| anyhow::anyhow!("invalid terminal history text boundary"))?;
        ensure!(
            !plain
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')),
            "saved terminal history contains non-presentation controls"
        );
        push(plain);
        rest = controls;
        if rest.is_empty() {
            break;
        }
        if strip_osc && let Some(body) = rest.strip_prefix("\x1b]") {
            let end = body
                .find(['\x07', '\x1b'])
                .ok_or_else(|| anyhow::anyhow!("incomplete terminal capture OSC"))?;
            let (_, terminator) = body
                .split_at_checked(end)
                .ok_or_else(|| anyhow::anyhow!("invalid terminal capture OSC boundary"))?;
            rest = terminator
                .strip_prefix('\x07')
                .or_else(|| terminator.strip_prefix("\x1b\\"))
                .ok_or_else(|| anyhow::anyhow!("invalid terminal capture OSC terminator"))?;
            continue;
        }
        let parameters = rest
            .strip_prefix("\x1b[")
            .ok_or_else(|| anyhow::anyhow!("saved terminal history contains a non-style escape"))?;
        let end = parameters
            .find(|ch: char| !ch.is_ascii_digit() && !matches!(ch, ';' | ':'))
            .ok_or_else(|| anyhow::anyhow!("incomplete saved terminal style"))?;
        let (parameters, terminator) = parameters
            .split_at_checked(end)
            .ok_or_else(|| anyhow::anyhow!("invalid saved terminal style boundary"))?;
        let after = terminator.strip_prefix('m').ok_or_else(|| {
            anyhow::anyhow!("saved terminal history contains a non-style command")
        })?;
        validate_sgr(parameters)?;
        if keep_styles {
            let style = rest
                .strip_suffix(after)
                .ok_or_else(|| anyhow::anyhow!("invalid saved terminal style suffix"))?;
            push(style);
        }
        rest = after;
    }
    Ok(())
}

fn check_size(bytes: usize, max_bytes: usize) -> Result<()> {
    if bytes > max_bytes {
        return Err(HistorySizeLimit { bytes, max_bytes }.into());
    }
    Ok(())
}

fn validate_sgr(parameters: &str) -> Result<()> {
    // Accept the pinned formatter's SGR subset; extend this list when its emitted styles grow.
    let mut parts = parameters.split(';');
    while let Some(part) = parts.next() {
        if let Some(underline) = part.strip_prefix("4:") {
            ensure!(
                matches!(underline, "0" | "1" | "2" | "3" | "4" | "5"),
                "invalid saved underline style"
            );
            continue;
        }
        let code = if part.is_empty() {
            0
        } else {
            part.parse::<u8>()?
        };
        match code {
            0..=9 | 21..=25 | 27..=37 | 39..=47 | 49 | 53 | 55 | 59 | 90..=97 | 100..=107 => {}
            38 | 48 | 58 => {
                let count = match parts.next() {
                    Some("5") => 1,
                    Some("2") => 3,
                    _ => bail!("invalid saved terminal color mode"),
                };
                for _ in 0..count {
                    let value = parts
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("incomplete saved terminal color"))?;
                    value.parse::<u8>()?;
                }
            }
            _ => bail!("unsupported saved terminal style"),
        }
    }
    Ok(())
}
