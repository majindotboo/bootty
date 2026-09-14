use bootty_config::config::MultiplexerBackendConfig;
use serde::{Deserialize, Serialize};

mod catalog;
pub use catalog::{Backend, CATALOG_VERSION, Catalog, LegacyCatalogPaths};

/// The versioned wire value returned by a remote Space catalog.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteSpaceSummary {
    pub catalog_version: u32,
    pub id: String,
    pub name: String,
    pub backend: MultiplexerBackendConfig,
}
