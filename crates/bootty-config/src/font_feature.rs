use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontFeature {
    tag: [u8; 4],
    value: u32,
}

impl FontFeature {
    #[must_use]
    pub const fn new(tag: [u8; 4], value: u32) -> Self {
        Self { tag, value }
    }

    #[must_use]
    pub fn parse(setting: &str) -> Option<Self> {
        let setting = setting.split_once(',').map_or(setting, |(head, _)| head);
        parse_font_feature_setting(setting)
    }

    #[must_use]
    pub const fn tag(self) -> [u8; 4] {
        self.tag
    }

    #[must_use]
    pub const fn value(self) -> u32 {
        self.value
    }

    fn tag_str(self) -> String {
        String::from_utf8_lossy(&self.tag).into_owned()
    }
}

impl fmt::Display for FontFeature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.value <= 1 {
            formatter.write_str(if self.value == 0 { "-" } else { "+" })?;
            formatter.write_str(&self.tag_str())
        } else {
            write!(formatter, "{}={}", self.tag_str(), self.value)
        }
    }
}

pub fn parse_font_features(settings: &str) -> Vec<FontFeature> {
    settings
        .split(',')
        .filter_map(parse_font_feature_setting)
        .collect()
}

fn parse_font_feature_setting(setting: &str) -> Option<FontFeature> {
    let mut input = setting.trim_start_matches([' ', '\t']).as_bytes();
    let prefixed_value = if let Some(rest) = input.strip_prefix(b"+") {
        input = rest;
        Some(1)
    } else if let Some(rest) = input.strip_prefix(b"-") {
        input = rest;
        Some(0)
    } else {
        None
    };
    let mut tag = [0_u8; 4];
    for slot in &mut tag {
        loop {
            let (&byte, rest) = input.split_first()?;
            input = rest;
            if matches!(byte, b'\'' | b'"') {
                continue;
            }
            if matches!(byte, b' ' | b'\t' | b'=' | b',') {
                return None;
            }
            *slot = byte;
            break;
        }
    }
    let rest = std::str::from_utf8(input)
        .ok()?
        .trim_start_matches([' ', '\t', '\'', '"']);

    let value = if let Some(value) = prefixed_value {
        if rest.trim_matches([' ', '\t']).is_empty() {
            value
        } else {
            return None;
        }
    } else if rest.trim_matches([' ', '\t']).is_empty() {
        1
    } else {
        let rest = rest.trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix('=').map_or(rest, |value| value);
        parse_font_feature_value(rest.trim_matches([' ', '\t']))?
    };

    Some(FontFeature { tag, value })
}

fn parse_font_feature_value(value: &str) -> Option<u32> {
    match value {
        "on" | "ON" | "On" => Some(1),
        "off" | "OFF" | "Off" => Some(0),
        _ if value.bytes().all(|byte| byte.is_ascii_digit()) => value.parse().ok(),
        _ => None,
    }
}
