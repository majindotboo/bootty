//! Compact raw bytes: base64 in human-readable formats, byte strings otherwise.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serializer};

/// # Errors
/// Returns the serializer's error.
pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    if serializer.is_human_readable() {
        serializer.serialize_str(&STANDARD.encode(bytes))
    } else {
        serializer.serialize_bytes(bytes)
    }
}

/// # Errors
/// Rejects malformed base64 or an invalid byte string.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    if deserializer.is_human_readable() {
        STANDARD
            .decode(String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    } else {
        serde_bytes::deserialize(deserializer)
    }
}

pub mod optional {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// # Errors
    /// Returns the serializer's error.
    pub fn serialize<S: Serializer>(
        bytes: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        bytes
            .as_ref()
            .map(|bytes| STANDARD.encode(bytes))
            .serialize(serializer)
    }

    /// # Errors
    /// Rejects malformed base64.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| STANDARD.decode(text).map_err(serde::de::Error::custom))
            .transpose()
    }
}
