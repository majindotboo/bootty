//! Shared support for Bootty's integration tests. Only test targets depend on it.

mod executable;

pub use executable::{place_executable, write_executable};
