use std::{env, error::Error, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=native/main.swift");
    if env::var("CARGO_CFG_TARGET_OS")? != "macos" {
        return Ok(());
    }
    let output =
        PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?).join("bootty-computer");
    let architecture = match env::var("CARGO_CFG_TARGET_ARCH")?.as_str() {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => return Err(format!("unsupported computer-control architecture: {other}").into()),
    };
    let status = Command::new("xcrun")
        .args(["swiftc", "-O", "-parse-as-library", "-target"])
        .arg(format!("{architecture}-apple-macosx13.0"))
        .args(["native/main.swift", "-o"])
        .arg(output)
        .status()?;
    if !status.success() {
        return Err("native computer-control helper compilation failed".into());
    }
    Ok(())
}
