//! Platforms without a desktop adapter report computer use as unsupported.
use std::sync::Arc;

use crate::{
    ComputerError, ComputerStatus, PermissionStatus,
    desktop::{Desktop, Focus, Image, InputEvent, NativeWindow},
};

pub fn native() -> Arc<dyn Desktop> {
    Arc::new(UnsupportedDesktop)
}

struct UnsupportedDesktop;

impl Desktop for UnsupportedDesktop {
    fn status(&self) -> ComputerStatus {
        ComputerStatus {
            accessibility: PermissionStatus::Unsupported,
            screen_recording: PermissionStatus::Unsupported,
            secure_input: false,
        }
    }

    fn windows(&self) -> Vec<NativeWindow> {
        Vec::new()
    }

    fn focus(&self) -> Option<Focus> {
        None
    }

    fn secure_input(&self) -> bool {
        false
    }

    fn capture(&self, _: &NativeWindow, _: u32, _: u32, _: bool) -> Result<Image, ComputerError> {
        Err(ComputerError::Unsupported)
    }

    fn post(&self, _: &NativeWindow, _: &InputEvent) -> Result<(), ComputerError> {
        Err(ComputerError::Unsupported)
    }

    fn raise(&self, _: &NativeWindow) -> Result<(), ComputerError> {
        Err(ComputerError::Unsupported)
    }

    fn icon(&self, _: &str, _: u32) -> Option<Image> {
        None
    }
}
