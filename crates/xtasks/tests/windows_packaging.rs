#![cfg(windows)]

use std::fs;
use std::process::Command;

use assert_fs::TempDir;

#[test]
fn local_install_builds_host_daemon_before_replacing_existing_files() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir(workspace.join("src")).unwrap();
    fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = \"bootty-daemon\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         [profile.daemon-release]\ninherits = \"release\"\n",
    )
    .unwrap();
    fs::write(
        workspace.join("src/main.rs"),
        "compile_error!(\"host daemon build probe\");\nfn main() {}\n",
    )
    .unwrap();
    let install = workspace.join("installed");
    let dist = workspace.join("dist");
    for directory in [&install, &dist] {
        fs::create_dir(directory).unwrap();
        fs::write(directory.join("existing"), "previous installation").unwrap();
    }

    let output = Command::new(env!("CARGO_BIN_EXE_xtasks"))
        .arg("install")
        .current_dir(workspace.path())
        .env_remove("BOOTTY_DAEMON_OUTPUT_DIR")
        .env_remove("CARGO_BUILD_TARGET")
        .env("CARGO_TARGET_DIR", workspace.join("target"))
        .env("BOOTTY_INSTALL_DIR", &install)
        .env("BOOTTY_DIST_DIR", &dist)
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("host daemon build probe"), "{stderr}");
    for directory in [&install, &dist] {
        assert_eq!(
            fs::read_to_string(directory.join("existing")).unwrap(),
            "previous installation"
        );
    }
}
