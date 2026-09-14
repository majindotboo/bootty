use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// Parse an RGB or RGBA hexadecimal color.
    ///
    /// # Errors
    /// Returns an error for a value other than six or eight hexadecimal digits.
    pub fn from_hex(input: &str) -> Result<Self, String> {
        let input = input.trim();
        let hex = input.strip_prefix('#').unwrap_or(input);
        if !matches!(hex.len(), 6 | 8) {
            return Err(format!(
                "expected #RRGGBB or #RRGGBBAA color, got {input:?}"
            ));
        }
        let value = u32::from_str_radix(hex, 16)
            .map_err(|_| format!("expected #RRGGBB or #RRGGBBAA color, got {input:?}"))?;
        let [first, second, third, fourth] = value.to_be_bytes();
        let [r, g, b, a] = if hex.len() == 8 {
            [first, second, third, fourth]
        } else {
            [second, third, fourth, 0xff]
        };
        Ok(Self { r, g, b, a })
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_hex(&value).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        };
        serializer.serialize_str(&value)
    }
}
