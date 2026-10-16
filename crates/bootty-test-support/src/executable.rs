//! Executables a test writes, such as fake providers and tools, that start fast.
//!
//! macOS assesses each new executable file the first time it runs, which costs a launch 100 to
//! 200 ms. Each script is kept once, by content, and the test's path is a hard link to that copy,
//! so a script is assessed once rather than in every test that writes it. A test changes its
//! script by writing it again here; writing or changing the mode of the linked file in place
//! would change every test's copy.
//!
//! Deliberate limit: copies stay in the temporary folder, which the system clears. A script that
//! embeds a test's own paths differs every run, gains nothing, and leaves a small copy behind;
//! give such scripts their paths as arguments if the folder grows noticeably.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Writes the executable script `contents` at `path`, in a folder made if missing, replacing what's
/// there.
///
/// A `#!/usr/bin/env python3` script names the first `python3` on `PATH` outside version-manager
/// shims, which take longer to start than the scripts they run.
/// # Errors
/// Returns the failure to write the script or link `path` to it.
pub fn write_executable(path: impl AsRef<Path>, contents: impl AsRef<str>) -> io::Result<()> {
    let path = path.as_ref();
    let script = kept_script(&resolve_python(contents.as_ref()))?;
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    link(&script, path)
}

/// Puts the built executable `source` at `path` without making a new file, for a test that
/// needs it in a folder of its own.
/// # Errors
/// Returns the failure to place it.
pub fn place_executable(source: impl AsRef<Path>, path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    link(source.as_ref(), path)
}

/// The kept copy of `contents`, written the first time any test asks for it.
fn kept_script(contents: &str) -> io::Result<PathBuf> {
    use std::hash::{DefaultHasher, Hash as _, Hasher as _};
    let directory = std::env::temp_dir().join("bootty-test-executables");
    fs::create_dir_all(&directory)?;
    let mut hasher = DefaultHasher::new();
    contents.hash(&mut hasher);
    let script = directory.join(format!("{:016x}-{}", hasher.finish(), contents.len()));
    // A copy something wrote over in place is written again, under a new file.
    if fs::read(&script).ok().as_deref() != Some(contents.as_bytes()) {
        // Tests run in parallel; each writes its own file and the last rename wins whole.
        static WRITES: AtomicU64 = AtomicU64::new(0);
        let partial = script.with_extension(format!(
            "{}-{}.partial",
            std::process::id(),
            WRITES.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&partial, contents)?;
        set_executable(&partial)?;
        fs::rename(&partial, &script)?;
    }
    Ok(script)
}

/// `contents` with a `#!/usr/bin/env python3` line naming the interpreter it would find.
fn resolve_python(contents: &str) -> String {
    let Some(body) = contents.strip_prefix("#!/usr/bin/env python3\n") else {
        return contents.to_owned();
    };
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .filter(|directory| !directory.ends_with("shims"))
                .map(|directory| directory.join("python3"))
                .find(|candidate| candidate.is_file())
        })
        .map_or_else(
            || contents.to_owned(),
            |interpreter| format!("#!{}\n{body}", interpreter.display()),
        )
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// A hard link, so the path stays the test's own and resolves to its folder. A copy across
/// filesystems runs as a new file.
fn link(script: &Path, path: &Path) -> io::Result<()> {
    fs::hard_link(script, path).or_else(|_| fs::copy(script, path).map(|_| ()))
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> io::Result<()> {
    Ok(())
}
