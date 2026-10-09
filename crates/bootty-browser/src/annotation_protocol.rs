use serde::Deserialize;

use crate::AnnotationAnchor;

/// Page messages carry local note data only; they cannot select an agent or send a batch.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnnotationEvent {
    CancelPick {
        address: String,
    },
    Pick {
        address: String,
        anchor: AnnotationAnchor,
    },
    Draft {
        address: String,
        id: String,
        note: String,
    },
    Save {
        address: String,
        id: String,
        note: String,
    },
    Cancel {
        address: String,
        id: String,
    },
}

impl AnnotationEvent {
    /// Parse a bounded untrusted page message. No page event grants native capabilities.
    #[must_use]
    pub fn parse(message: &str) -> Option<Self> {
        if message.len() > 16 * 1024 {
            return None;
        }
        serde_json::from_str(message).ok()
    }

    #[must_use]
    pub fn address(&self) -> &str {
        match self {
            Self::CancelPick { address }
            | Self::Pick { address, .. }
            | Self::Draft { address, .. }
            | Self::Save { address, .. }
            | Self::Cancel { address, .. } => address,
        }
    }
}
