#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::native;
#[cfg(target_os = "macos")]
pub use macos::native;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub use unsupported::native;
#[cfg(target_os = "windows")]
pub use windows::native;

#[cfg(target_os = "linux")]
pub use linux::on_display;

#[cfg(not(target_os = "linux"))]
pub fn on_display(
    _display: &str,
) -> Result<std::sync::Arc<dyn crate::desktop::Desktop>, crate::ComputerError> {
    Err(crate::ComputerError::Unsupported)
}
