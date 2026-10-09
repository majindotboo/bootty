use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Only states reported by the provider's native interactive interface.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAgentStatus {
    #[default]
    Starting,
    Idle,
    Working,
    Approval,
    Input,
    Waiting,
    Finished,
    Stopped,
    Error,
    Unavailable,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentObservation {
    pub session_id: Option<String>,
    pub session_file: Option<String>,
    pub status: TerminalAgentStatus,
    pub detail: Option<String>,
}

pub type ObservationSink = Arc<dyn Fn(AgentObservation) + Send + Sync>;

impl AgentObservation {
    pub(crate) fn bounded(mut self) -> Self {
        for value in [
            &mut self.session_id,
            &mut self.session_file,
            &mut self.detail,
        ]
        .into_iter()
        .flatten()
        {
            value.truncate(value.floor_char_boundary(4096));
        }
        self
    }
}

/// A random UUID for a provider that accepts an explicit fresh session identity.
/// # Errors
/// Returns an error when the operating system cannot provide randomness.
pub fn terminal_session_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    let version = bytes
        .get_mut(6)
        .ok_or("Session identity buffer is invalid")?;
    *version = (*version & 0x0f) | 0x40;
    let variant = bytes
        .get_mut(8)
        .ok_or("Session identity buffer is invalid")?;
    *variant = (*variant & 0x3f) | 0x80;
    let hex = bytes
        .iter()
        .try_fold(String::with_capacity(32), |mut hex, byte| {
            use std::fmt::Write as _;
            write!(hex, "{byte:02x}").map_err(|error| error.to_string())?;
            Ok::<_, String>(hex)
        })?;
    Ok(hex
        .chars()
        .enumerate()
        .flat_map(|(index, character)| {
            matches!(index, 8 | 12 | 16 | 20)
                .then_some('-')
                .into_iter()
                .chain(std::iter::once(character))
        })
        .collect())
}
