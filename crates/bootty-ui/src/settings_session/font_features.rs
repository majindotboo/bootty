use bootty_config::FontFeature;

/// One editable OpenType feature in the same typed shape consumed by the font stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFeatureDraft {
    pub tag: String,
    pub value: u32,
}

impl FontFeatureDraft {
    /// Construct a feature draft with a validated tag.
    ///
    /// # Errors
    /// Rejects tags that are not four printable ASCII bytes or cannot round-trip as a setting.
    pub fn new(tag: impl Into<String>, value: u32) -> Result<Self, String> {
        let draft = Self {
            tag: tag.into(),
            value,
        };
        draft.feature()?;
        Ok(draft)
    }

    /// Parse a configured OpenType feature into an editable draft.
    ///
    /// # Errors
    /// Returns an error for invalid feature syntax.
    pub fn parse(setting: &str) -> Result<Self, String> {
        let feature = FontFeature::parse(setting)
            .ok_or_else(|| format!("Invalid OpenType feature {setting:?}."))?;
        Ok(Self::from(feature))
    }

    /// Serialize the edited tag and value as a font setting.
    ///
    /// # Errors
    /// Returns an error when the edited tag is invalid.
    pub fn setting(&self) -> Result<String, String> {
        Ok(self.feature()?.to_string())
    }

    fn feature(&self) -> Result<FontFeature, String> {
        let bytes: [u8; 4] = self.tag.as_bytes().try_into().map_err(|_| {
            "OpenType feature tags must contain exactly 4 ASCII characters.".to_owned()
        })?;
        if !bytes.iter().all(u8::is_ascii_graphic) {
            return Err(
                "OpenType feature tags must contain exactly 4 ASCII characters.".to_owned(),
            );
        }
        let feature = FontFeature::new(bytes, self.value);
        if FontFeature::parse(&feature.to_string()) != Some(feature) {
            return Err(format!("OpenType feature tag {:?} is invalid.", self.tag));
        }
        Ok(feature)
    }
}

impl From<FontFeature> for FontFeatureDraft {
    fn from(feature: FontFeature) -> Self {
        Self {
            tag: String::from_utf8_lossy(&feature.tag()).into_owned(),
            value: feature.value(),
        }
    }
}

/// Preserve product order while making the last value for a repeated tag authoritative.
#[must_use]
pub fn dedupe_font_features(features: Vec<FontFeatureDraft>) -> Vec<FontFeatureDraft> {
    let mut deduped = Vec::<FontFeatureDraft>::new();
    for feature in features {
        if let Some(existing) = deduped.iter_mut().find(|entry| entry.tag == feature.tag) {
            existing.value = feature.value;
        } else {
            deduped.push(feature);
        }
    }
    deduped
}
