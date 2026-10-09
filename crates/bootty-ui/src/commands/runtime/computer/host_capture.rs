use std::{
    fmt::Write as _,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerError, ComputerResult, ComputerTarget,
    HostCaptureRegion,
};

/// Capture the owning desktop window using existing OS permission and fresh host-owned output.
/// The caller resolves the original `ApplicationWindow` and checks capture policy before entry.
pub async fn capture_host_window(
    computer: &Computer,
    native_id: NonZeroU32,
    directory: &Path,
    access: &ComputerAccess,
    region: Option<&HostCaptureRegion>,
) -> Result<(ComputerResult, PathBuf), ComputerError> {
    if !directory.is_absolute() {
        return Err(ComputerError::InvalidAction(
            "Host capture directory must be absolute".into(),
        ));
    }
    let result = capture_host_candidate(computer, native_id, access, region).await?;
    let destination = host_capture_destination(directory)?;
    Ok((result, destination))
}

/// Observe and capture without creating a directory or a retained image.
pub(super) async fn capture_host_candidate(
    computer: &Computer,
    native_id: NonZeroU32,
    access: &ComputerAccess,
    region: Option<&HostCaptureRegion>,
) -> Result<ComputerResult, ComputerError> {
    let process =
        i32::try_from(std::process::id()).map_err(|_| ComputerError::TargetUnavailable)?;
    let identity = bootty_config::ApplicationIdentity::for_process();
    let observed = computer.targets().await?;
    let target = select_host_window(&observed, native_id, process, identity.bundle_identifier())?;
    let action = match region {
        Some(region) => ComputerAction::SnapshotRegion {
            rect: region.resolve(&target)?,
        },
        None => ComputerAction::Snapshot,
    };
    computer.execute(access, &target, &action).await
}

fn select_host_window(
    observed: &[ComputerTarget],
    native_id: NonZeroU32,
    process: i32,
    bundle: &str,
) -> Result<ComputerTarget, ComputerError> {
    let mut matching = observed.iter().filter(|target| {
        target.window_id == native_id.get()
            && target.process_id == process
            && target.bundle_id == bundle
    });
    let target = matching.next().ok_or(ComputerError::TargetUnavailable)?;
    if matching.next().is_some() {
        return Err(ComputerError::StaleTarget);
    }
    target.validate()?;
    Ok(target.clone())
}

pub(super) fn host_capture_destination(directory: &Path) -> Result<PathBuf, ComputerError> {
    if !directory.is_absolute() {
        return Err(ComputerError::InvalidAction(
            "Host capture directory must be absolute".into(),
        ));
    }
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random)
        .map_err(|_| std::io::Error::other("Host capture name is unavailable"))?;
    let mut name = String::from("computer-");
    for byte in random {
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".png");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    // The computer runtime publishes through bootty-write::create; an external winner is preserved.
    Ok(directory.join(name))
}
