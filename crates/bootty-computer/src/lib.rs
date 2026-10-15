mod computer;
pub mod control;
pub mod desktop;
mod model;
mod platform;
mod snapshot;

pub use computer::{Computer, MAX_ICON_REQUEST};
pub use model::{
    ComputerAccess, ComputerAction, ComputerElement, ComputerError, ComputerResult, ComputerStatus,
    ComputerTarget, DisplayBounds, ElementState, HostCaptureRegion, Key, MAX_ELEMENT_DEPTH,
    MAX_ELEMENTS, Modifier, MouseButton, Permission, PermissionStatus, PressMethod,
};

pub use snapshot::ComputerResultSnapshot;
