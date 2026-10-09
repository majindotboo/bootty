//! Reuse the same pinned cross-platform native audio host as Pi voice.
use anyhow::Result;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

const REVISION: &str = "dac9040e408c92aae5d375cd3218af31748d8146";

pub fn bundle(target_root: &Path, destination: &Path) -> Result<()> {
    let root = target_root.join("voice-runtime").join(REVISION);
    let name = if cfg!(windows) {
        "voice-host.exe"
    } else {
        "voice-host"
    };
    let binary: PathBuf = root.join("bin").join(name);
    if !binary.is_file() {
        crate::command::run(
            Command::new("cargo")
                // Opus still declares pre-3.5 policies; remove after its CMake minimum is updated.
                .env("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
                .args([
                    "install",
                    "--git",
                    "https://github.com/luan/agents",
                    "--rev",
                    REVISION,
                    "--locked",
                    "--root",
                ])
                .arg(&root)
                .arg("voice-host"),
        )?;
    }
    crate::filesystem::copy_file(&binary, &destination.join(name))
}
