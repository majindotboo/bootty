//! Host-neutral state for Bootty's product dialogs.
//!
//! These models own only disposable presentation state. Hosts inject accepted product data and
//! translate the typed outputs back to the application owner.

pub mod keybind_help;
pub mod searchable;
pub mod terminal_find;

pub use searchable::{SearchableEntry, SearchableIntent, SearchableList, SearchableRow};
