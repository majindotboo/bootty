mod computer;
pub mod control;
pub mod desktop;
mod model;
mod platform;
mod snapshot;

pub use computer::{Computer, MAX_ICON_REQUEST};
pub use model::{
    ComputerAccess, ComputerAction, ComputerError, ComputerResult, ComputerStatus, ComputerTarget,
    DisplayBounds, HostCaptureRegion, Key, Modifier, MouseButton, Permission, PermissionStatus,
};

pub use snapshot::ComputerResultSnapshot;
