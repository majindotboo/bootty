/// A task's durable destination, independent of its terminal or provider status.
///
/// Setting a destination never starts, stops, or recreates a backend session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskLifecycle {
    Active,
    Settled,
    Archived,
}

#[cfg(feature = "terminal-runtime")]
impl TaskLifecycle {
    pub(crate) const fn storage_value(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Settled => "settled",
            Self::Archived => "archived",
        }
    }

    pub(crate) fn from_storage(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "settled" => Some(Self::Settled),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}
