//! Renderer-neutral ownership for one settings editing session.

mod effect;
mod font_features;
mod numeric;
mod remotes;
mod state;
mod status_segments;
mod writeback;

pub use effect::{ModuleOutcome, RemoteOutcome, SettingsEffect, SettingsOutcome};
pub use font_features::{FontFeatureDraft, dedupe_font_features};
pub use numeric::{normalize_number, parse_display_number};
pub use remotes::{DefaultRemote, RemoteDraft, RemoteEditorSnapshot, RemoteProfile};
pub use state::{AcceptedSettings, Catalogs, EnvironmentDraft, SettingsSession};
pub use status_segments::StatusSegmentEdit;
pub use writeback::DraftWriteback;
