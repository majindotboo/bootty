use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

/// A requested text style follows the base font unless a named style overrides it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontStyleAssignment {
    #[default]
    Automatic,
    Disabled,
    Named(String),
}

impl Serialize for FontStyleAssignment {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Automatic => serializer.serialize_str("auto"),
            Self::Disabled => serializer.serialize_bool(false),
            Self::Named(name) => serializer.serialize_str(name),
        }
    }
}

impl<'de> Deserialize<'de> for FontStyleAssignment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Name(String),
            Enabled(bool),
        }
        match Value::deserialize(deserializer)? {
            Value::Name(name) if name == "auto" || name.is_empty() => Ok(Self::Automatic),
            Value::Name(name) => Ok(Self::Named(name)),
            Value::Enabled(false) => Ok(Self::Disabled),
            Value::Enabled(true) => Err(D::Error::custom(
                "font style must be a style name, \"auto\", or false",
            )),
        }
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum FontWeightRole {
    Thin,
    ExtraLight,
    Light,
    Normal,
    Medium,
    Semibold,
    Bold,
    ExtraBold,
    Black,
}

impl FontWeightRole {
    pub const ALL: [Self; 9] = [
        Self::Thin,
        Self::ExtraLight,
        Self::Light,
        Self::Normal,
        Self::Medium,
        Self::Semibold,
        Self::Bold,
        Self::ExtraBold,
        Self::Black,
    ];

    #[must_use]
    pub const fn weight(self) -> u16 {
        match self {
            Self::Thin => 100,
            Self::ExtraLight => 200,
            Self::Light => 300,
            Self::Normal => 400,
            Self::Medium => 500,
            Self::Semibold => 600,
            Self::Bold => 700,
            Self::ExtraBold => 800,
            Self::Black => 900,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Thin => "Thin",
            Self::ExtraLight => "Extra light",
            Self::Light => "Light",
            Self::Normal => "Regular",
            Self::Medium => "Medium",
            Self::Semibold => "Semibold",
            Self::Bold => "Bold",
            Self::ExtraBold => "Extra bold",
            Self::Black => "Black",
        }
    }
}

pub type FontWeightAssignments = BTreeMap<FontWeightRole, FontStyleAssignment>;
