use std::borrow::Cow;

use memchr::{memchr, memchr_iter, memchr2_iter, memchr3_iter, memmem::find};

pub fn find_osc_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    for index in memchr2_iter(0x07, 0x1b, bytes) {
        match bytes.get(index) {
            Some(0x07) => return Some((index, 1)),
            Some(0x1b) if bytes.get(index.saturating_add(1)) == Some(&b'\\') => {
                return Some((index, 2));
            }
            _ => {}
        }
    }
    None
}

pub fn split_osc_payload(payload: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = memchr(b';', payload)?;
    let (kind, rest) = payload.split_at_checked(separator)?;
    Some((kind, rest.strip_prefix(b";")?))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent protocol recognizers may be enabled together."
)]
pub(super) struct TerminalWriteFeatures {
    pub(super) tmux_passthrough: bool,
    pub(super) kitty_graphics: bool,
    pub(super) osc_side_effect: bool,
    pub(super) osc_color: bool,
}

impl TerminalWriteFeatures {
    pub(super) const fn needs_sanitizing(self) -> bool {
        self.tmux_passthrough || self.kitty_graphics || self.osc_side_effect || self.osc_color
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamingControlState {
    Complete(usize),
    Incomplete,
    Unrecognized,
}

const STREAMING_CONTROL_PREFIXES: &[&[u8]] = &[
    b"\x1bPtmux;",
    b"\x1b_G",
    b"\x1b]0;",
    b"\x1b]1;",
    b"\x1b]2;",
    b"\x1b]7;",
    b"\x1b]4;",
    b"\x1b]10;",
    b"\x1b]11;",
    b"\x1b]9;",
    b"\x1b]22;",
    b"\x1b]52;",
    b"\x1b]5522;",
    b"\x1b]66;",
    b"\x1b]133;",
    b"\x1b]777;",
    b"\x1b]1337;",
];

const SIDE_EFFECT_OSC_PREFIXES: &[&[u8]] = &[
    b"1;", b"9;", b"22;", b"52;", b"5522;", b"66;", b"133;", b"777;", b"1337;",
];

const COLOR_OSC_PREFIXES: &[&[u8]] = &[
    b"4;", b"10;", b"11;", b"12;", b"13;", b"14;", b"15;", b"16;", b"17;", b"18;", b"19;", b"110",
    b"111", b"112", b"113", b"114", b"115", b"116", b"117", b"118", b"119",
];

pub(super) fn complete_streaming_control_prefix_len(data: &[u8]) -> usize {
    let mut index = 0_usize;
    while let Some(rest) = data.get(index..)
        && let Some(relative_start) = memchr(0x1b, rest)
    {
        let start = index.saturating_add(relative_start);
        let Some(control) = rest.get(relative_start..) else {
            break;
        };
        match streaming_control_state(control) {
            StreamingControlState::Complete(len) => index = start.saturating_add(len),
            StreamingControlState::Incomplete => {
                // Let the collector discard oversized clipboard packets without retaining them.
                if control.starts_with(b"\x1b]5522;")
                    && control.len() > crate::clipboard_write::MAX_PACKET + 7
                {
                    return data.len();
                }
                return start;
            }
            StreamingControlState::Unrecognized => index = start.saturating_add(1),
        }
    }
    data.len()
}

pub(super) fn contains_tracked_streaming_control(data: &[u8]) -> bool {
    if data.last() == Some(&0x1b) {
        return true;
    }

    for marker in memchr3_iter(b']', b'_', b'P', data) {
        if marker.checked_sub(1).and_then(|index| data.get(index)) != Some(&0x1b) {
            continue;
        }

        match data.get(marker) {
            Some(b']') => return true,
            Some(b'_')
                if data
                    .get(marker.saturating_add(1))
                    .is_none_or(|byte| *byte == b'G') =>
            {
                return true;
            }
            Some(b'P') => {
                let Some(rest) = data.get(marker.saturating_sub(1)..) else {
                    continue;
                };
                if b"\x1bPtmux;".starts_with(rest.get(..7).unwrap_or(rest)) {
                    return true;
                }
            }
            _ => {}
        }
    }

    false
}

pub(super) const CURSOR_HOME: &[u8; 3] = b"\x1b[H";

pub(super) fn repeated_cursor_home_prefix_len(
    data: &[u8],
    pending_len: usize,
) -> Option<(usize, usize)> {
    let mut state = pending_len;
    let mut complete = 0_usize;
    for byte in data {
        if Some(byte) != CURSOR_HOME.get(state) {
            return None;
        }
        state = state.checked_add(1)?;
        if state == CURSOR_HOME.len() {
            complete = complete.checked_add(1)?;
            state = 0;
        }
    }
    Some((complete, state))
}

fn streaming_control_state(data: &[u8]) -> StreamingControlState {
    if STREAMING_CONTROL_PREFIXES
        .iter()
        .any(|prefix| data.len() < prefix.len() && prefix.starts_with(data))
    {
        return StreamingControlState::Incomplete;
    }

    if data.starts_with(b"\x1bPtmux;") {
        return find_tmux_passthrough(data)
            .map_or(StreamingControlState::Incomplete, |(len, _)| {
                StreamingControlState::Complete(len)
            });
    }
    if let Some(payload) = data.strip_prefix(b"\x1b_G") {
        return find_osc_terminator(payload).map_or(
            StreamingControlState::Incomplete,
            |(payload_len, terminator_len)| {
                StreamingControlState::Complete(
                    payload_len.saturating_add(3).saturating_add(terminator_len),
                )
            },
        );
    }
    if let Some(payload) = data.strip_prefix(b"\x1b]") {
        return match osc_streaming_prefix_state(payload) {
            StreamingControlState::Complete(_) => find_osc_terminator(payload).map_or(
                StreamingControlState::Incomplete,
                |(payload_len, terminator_len)| {
                    StreamingControlState::Complete(
                        payload_len.saturating_add(2).saturating_add(terminator_len),
                    )
                },
            ),
            state => state,
        };
    }

    StreamingControlState::Unrecognized
}

fn osc_streaming_prefix_state(data: &[u8]) -> StreamingControlState {
    let mut incomplete = false;
    for prefix in SIDE_EFFECT_OSC_PREFIXES
        .iter()
        .copied()
        .chain(COLOR_OSC_PREFIXES.iter().copied())
        .chain(std::iter::once(b"7;".as_slice()))
    {
        if data.starts_with(prefix) {
            return StreamingControlState::Complete(0);
        }
        incomplete |= data.len() < prefix.len() && prefix.starts_with(data);
    }
    if incomplete {
        StreamingControlState::Incomplete
    } else {
        StreamingControlState::Unrecognized
    }
}

fn find_tmux_passthrough(data: &[u8]) -> Option<(usize, bool)> {
    let mut cursor = 7_usize;
    let mut has_escaped_escape = false;
    while let Some(relative_escape) = memchr(0x1b, data.get(cursor..)?) {
        cursor = cursor.checked_add(relative_escape)?;
        match data.get(cursor.checked_add(1)?) {
            Some(&0x1b) => {
                has_escaped_escape = true;
                cursor = cursor.checked_add(2)?;
            }
            Some(&b'\\') => return Some((cursor.checked_add(2)?, has_escaped_escape)),
            _ => cursor = cursor.checked_add(1)?,
        }
    }
    None
}

pub(super) fn terminal_write_features(data: &[u8]) -> TerminalWriteFeatures {
    let mut features = TerminalWriteFeatures::default();
    for start in memchr_iter(0x1b, data) {
        match data.get(start.saturating_add(1)).copied() {
            Some(b'P')
                if data.get(start.saturating_add(2)..start.saturating_add(7)) == Some(b"tmux;") =>
            {
                features.tmux_passthrough = true;
            }
            Some(b'_') if data.get(start.saturating_add(2)) == Some(&b'G') => {
                features.kitty_graphics = true;
            }
            Some(b']') => {
                let osc = data.get(start.saturating_add(2)..).unwrap_or_default();
                if has_osc_prefix(osc, COLOR_OSC_PREFIXES) {
                    features.osc_color = true;
                } else if has_osc_prefix(osc, SIDE_EFFECT_OSC_PREFIXES) {
                    features.osc_side_effect = true;
                }
            }
            _ => {}
        }
        if features.tmux_passthrough
            && features.kitty_graphics
            && features.osc_side_effect
            && features.osc_color
        {
            break;
        }
    }
    features
}

pub(super) fn unwrap_tmux_passthrough_commands(data: &[u8]) -> Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut pending = data;
    let mut remaining = data;
    while let Some(start) = find(remaining, b"\x1bPtmux;") {
        let Some(packet) = remaining.get(start..) else {
            break;
        };
        let Some((control_len, escaped)) = find_tmux_passthrough(packet) else {
            break;
        };
        let Some((control, rest)) = packet.split_at_checked(control_len) else {
            break;
        };
        let Some(payload) = control
            .strip_prefix(b"\x1bPtmux;")
            .and_then(|p| p.strip_suffix(b"\x1b\\"))
        else {
            break;
        };
        let prefix_len = pending.len().saturating_sub(packet.len());
        let Some(prefix) = pending.get(..prefix_len) else {
            break;
        };
        let output = out.get_or_insert_with(|| Vec::with_capacity(data.len()));
        output.extend_from_slice(prefix);
        if escaped {
            let mut bytes = payload.iter().copied().peekable();
            while let Some(byte) = bytes.next() {
                output.push(byte);
                if byte == 0x1b && bytes.peek() == Some(&0x1b) {
                    bytes.next();
                }
            }
        } else {
            output.extend_from_slice(payload);
        }
        pending = rest;
        remaining = rest;
    }
    out.map_or(Cow::Borrowed(data), |mut output| {
        output.extend_from_slice(pending);
        Cow::Owned(output)
    })
}

pub(super) struct SanitizedKittyGraphics<'a> {
    pub(super) bytes: Cow<'a, [u8]>,
    pub(super) touched: bool,
}

pub(super) fn sanitize_kitty_graphics_commands(data: &[u8]) -> SanitizedKittyGraphics<'_> {
    let mut out: Option<Vec<u8>> = None;
    let mut pending = data;
    let mut remaining = data;
    let mut touched = false;
    while let Some(start) = find(remaining, b"\x1b_G") {
        touched = true;
        let Some(packet) = remaining.get(start..) else {
            break;
        };
        let Some(payload) = packet.strip_prefix(b"\x1b_G") else {
            break;
        };
        let Some((payload_len, terminator_len)) = find_osc_terminator(payload) else {
            break;
        };
        let Some((payload, terminated)) = payload.split_at_checked(payload_len) else {
            break;
        };
        let Some((terminator, rest)) = terminated.split_at_checked(terminator_len) else {
            break;
        };
        remaining = rest;
        let control_end = memchr(b';', payload).unwrap_or(payload.len());
        let Some((control, body)) = payload.split_at_checked(control_end) else {
            break;
        };
        let Some(sanitized) = sanitize_kitty_graphics_control(control) else {
            continue;
        };
        let prefix_len = pending.len().saturating_sub(packet.len());
        let Some(prefix) = pending.get(..prefix_len) else {
            break;
        };
        let output = out.get_or_insert_with(|| Vec::with_capacity(data.len()));
        output.extend_from_slice(prefix);
        output.extend_from_slice(b"\x1b_G");
        output.extend_from_slice(&sanitized);
        output.extend_from_slice(body);
        output.extend_from_slice(terminator);
        pending = rest;
    }
    SanitizedKittyGraphics {
        bytes: out.map_or(Cow::Borrowed(data), |mut output| {
            output.extend_from_slice(pending);
            Cow::Owned(output)
        }),
        touched,
    }
}

fn sanitize_kitty_graphics_control(control: &[u8]) -> Option<Vec<u8>> {
    if control
        .split(|byte| *byte == b',')
        .all(valid_kitty_graphics_field)
    {
        return None;
    }

    let mut sanitized = Vec::with_capacity(control.len());
    for (index, field) in control
        .split(|byte| *byte == b',')
        .filter(|field| valid_kitty_graphics_field(field))
        .enumerate()
    {
        if index > 0 {
            sanitized.push(b',');
        }
        sanitized.extend_from_slice(field);
    }
    Some(sanitized)
}

fn valid_kitty_graphics_field(field: &[u8]) -> bool {
    memchr(b'=', field).is_none_or(|separator| {
        separator == 1 && field.len().saturating_sub(separator).saturating_sub(1) <= 11
    })
}

fn has_osc_prefix(data: &[u8], prefixes: &[&[u8]]) -> bool {
    prefixes.iter().any(|prefix| data.starts_with(prefix))
}
