use serde::Deserialize;
use serde_json::Value;

use crate::{
    TerminalPresentation,
    connection::{CommandResult, Connection, Invocation, Target},
};

#[derive(Clone, Deserialize)]
pub struct Session {
    pub name: String,
    pub target: Target,
    #[serde(default)]
    pub terminal_target: Option<Target>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub pane_target: Option<Target>,
    #[serde(default)]
    pub topology_supported: bool,
    #[serde(default)]
    pub windows: Vec<TerminalWindow>,
}

#[derive(Clone, Deserialize)]
pub struct TerminalWindow {
    pub name: String,
    pub target: Target,
    pub panes: Vec<Pane>,
}

#[derive(Clone, Deserialize)]
pub struct Pane {
    pub target: Target,
    pub terminal_target: Target,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct Space {
    pub scope: String,
    pub name: String,
    pub backend: String,
    pub host: String,
    pub target: Target,
    pub sessions: Vec<Session>,
}

pub struct LiveWorkspace {
    pub spaces: Vec<Space>,
    pub terminal: Option<TerminalPresentation>,
    pub capture_error: Option<String>,
}

impl LiveWorkspace {
    /// Fetches current owner state; input/capture never changes desktop selection.
    /// # Errors
    /// Fails closed on incompatible topology, limits or explicit host/capability failure.
    pub fn refresh(connection: &Connection, terminal: Option<&Target>) -> Result<Self, String> {
        let spaces = value(connection.invoke(&Invocation::new("spaces.list", vec![], None))?)?;
        let spaces = Self::decode_spaces(spaces)?;
        let terminal = terminal
            .filter(|target| Self::contains_terminal(&spaces, target))
            .map(|target| {
                let capture = value(connection.invoke(&Invocation::new(
                    "terminal.capture",
                    vec!["ansi".into(), "history".into(), "80".into()],
                    Some(target.clone()),
                ))?)?;
                capture["capture"]["text"]
                    .as_str()
                    .map(TerminalPresentation::parse)
                    .ok_or_else(|| "Computer omitted its terminal output".to_owned())
            })
            .transpose();
        let (terminal, capture_error) = match terminal {
            Ok(output) => (output, None),
            Err(error) => (None, Some(error)),
        };
        // A pane may close between listing and capture; publish the fresh list either way.
        Ok(Self {
            spaces,
            terminal,
            capture_error,
        })
    }

    #[must_use]
    pub fn contains_terminal(spaces: &[Space], target: &Target) -> bool {
        spaces
            .iter()
            .flat_map(|space| &space.sessions)
            .any(|session| {
                session.terminal_target.as_ref() == Some(target)
                    || session
                        .windows
                        .iter()
                        .flat_map(|window| &window.panes)
                        .any(|pane| &pane.terminal_target == target)
            })
    }

    /// # Errors
    /// Rejects oversized topology and invalid/duplicate host-issued identities.
    pub fn decode_spaces(value: Value) -> Result<Vec<Space>, String> {
        let spaces: Vec<Space> =
            serde_json::from_value(value).map_err(|_| "Computer returned incompatible Spaces")?;
        if spaces.len() > 128
            || spaces
                .iter()
                .map(|space| space.sessions.len())
                .sum::<usize>()
                > 512
        {
            return Err("Computer has more sessions than the mobile view supports".into());
        }
        let mut targets = std::collections::BTreeSet::new();
        for space in &spaces {
            if space.target.kind != "binding"
                || !targets.insert((&space.target.handle, &space.target.generation))
            {
                return Err("Computer returned incompatible Space targets".into());
            }
            for session in &space.sessions {
                if session.target.kind != "session"
                    || !targets.insert((&session.target.handle, &session.target.generation))
                    || session
                        .terminal_target
                        .as_ref()
                        .is_some_and(|target| target.kind != "terminal")
                    || session
                        .pane_target
                        .as_ref()
                        .is_some_and(|target| target.kind != "pane")
                    || session.windows.len() > 128
                    || session.windows.iter().any(|window| {
                        window.target.kind != "mux_window"
                            || window.panes.len() > 128
                            || window.panes.iter().any(|pane| {
                                pane.target.kind != "pane"
                                    || pane.terminal_target.kind != "terminal"
                            })
                    })
                {
                    return Err("Computer returned incompatible session targets".into());
                }
            }
        }
        Ok(spaces)
    }
}

fn value(result: CommandResult) -> Result<Value, String> {
    match result {
        CommandResult::Value(value) => Ok(value),
        CommandResult::Confirmation(_) => {
            Err("Computer unexpectedly requires confirmation for a read".into())
        }
    }
}
