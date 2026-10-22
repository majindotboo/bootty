use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

/// Find a local program using the same search policy as `local_command`.
#[must_use]
pub fn find_executable(program: impl AsRef<OsStr>) -> Option<PathBuf> {
    find_executable_in(
        program.as_ref(),
        env::var_os("PATH").as_deref(),
        env::var_os("PATHEXT").as_deref(),
    )
}

/// Resolve against a captured search environment without changing process-global variables.
#[must_use]
pub fn find_executable_in(
    program: &OsStr,
    path: Option<&OsStr>,
    extensions: Option<&OsStr>,
) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    let program = Path::new(program);
    if program.components().count() > 1 || program.is_absolute() {
        return candidate(program, extensions);
    }
    path.into_iter()
        .flat_map(env::split_paths)
        .filter(|directory| !directory.as_os_str().is_empty())
        .find_map(|directory| candidate(&directory.join(program), extensions))
}

/// Build a local command, retaining structured arguments and Rust's batch-file escaping.
/// Missing programs retain their name so spawning reports its normal OS error.
#[must_use]
pub fn local_command(program: impl AsRef<OsStr>) -> Command {
    let program = program.as_ref();
    #[cfg(windows)]
    {
        Command::new(
            find_executable(program)
                .map_or_else(|| program.to_os_string(), PathBuf::into_os_string),
        )
    }
    #[cfg(not(windows))]
    Command::new(program)
}

#[cfg(unix)]
fn candidate(path: &Path, _extensions: Option<&OsStr>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = path.metadata().ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .then(|| path.canonicalize().ok())
        .flatten()
}

#[cfg(windows)]
fn candidate(path: &Path, extensions: Option<&OsStr>) -> Option<PathBuf> {
    if executable_file(path) {
        return std::path::absolute(path).ok();
    }
    if path.extension().is_some() {
        return None;
    }
    let extensions = extensions
        .unwrap_or_else(|| OsStr::new(".COM;.EXE;.BAT;.CMD"))
        .to_string_lossy();
    extensions
        .split(';')
        .filter(|extension| supported_extension(extension.trim_start_matches('.')))
        .find_map(|extension| {
            let mut filename = path.as_os_str().to_os_string();
            filename.push(extension);
            let candidate = PathBuf::from(filename);
            executable_file(&candidate)
                .then(|| std::path::absolute(candidate).ok())
                .flatten()
        })
}

#[cfg(windows)]
fn supported_extension(extension: &str) -> bool {
    ["exe", "com", "bat", "cmd"]
        .iter()
        .any(|supported| extension.eq_ignore_ascii_case(supported))
}

#[cfg(windows)]
fn executable_file(path: &Path) -> bool {
    use std::io::Read as _;
    if !path.is_file() {
        return false;
    }
    if path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(supported_extension)
    {
        return true;
    }
    // npm installs a POSIX text shim beside its Windows .cmd shim. Only native
    // images can be launched directly without a recognized Windows extension.
    let mut magic = [0_u8; 2];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok()
        && magic == *b"MZ"
}
