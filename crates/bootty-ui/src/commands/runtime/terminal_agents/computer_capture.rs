use std::num::NonZeroU32;

use bootty_agents::{ToolCapture, ToolCapturedCommand};
use bootty_config::config::ComputerConfig;
use bootty_control::{Caller, CommandInvocation, CommandTarget, ResourceKind};

/// Only the original host window is attached; tools never receive native IDs or output paths.
pub fn computer_launch_capture(
    policy: ComputerConfig,
    caller: Caller,
    window: Option<CommandTarget>,
    native_id: Option<NonZeroU32>,
) -> Option<ToolCapturedCommand> {
    if !cfg!(target_os = "macos")
        || !policy.enabled
        || !policy.capture_enabled
        || native_id.is_none()
    {
        return None;
    }
    let window = window.filter(|window| {
        window.kind == ResourceKind::ApplicationWindow
            && !window.handle.is_empty()
            && window.handle.len() <= 8192
            && window.generation > 0
    })?;
    let mut invocation = CommandInvocation::new("computer.capture", Vec::new(), caller);
    invocation.target = Some(window);
    Some(ToolCapturedCommand {
        capture: ToolCapture::Computer,
        invocation,
    })
}
