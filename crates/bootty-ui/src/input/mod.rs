mod config;
pub mod focus;
pub mod router;

pub use bootty_terminal::terminal_input::TerminalInputCommand;
pub(crate) use config::apply_modifier_remap;
pub use config::{ModifierRemapConfigError, resolve_modifier_remaps};
