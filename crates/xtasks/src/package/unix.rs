use std::env;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{Args, Layout};
use crate::daemon::{DaemonTarget, TARGETS};
use crate::{command, filesystem};

const BINARY: &str = "bootty";
const DAEMON: &str = "bootty-daemon";

pub(super) fn run(args: Args, layout: &Layout) -> Result<()> {
    let host_daemon = build_daemon(layout)?;
    fs::create_dir_all(&layout.dist_dir)?;
    crate::build::run(&crate::build::BuildArgs { release: !args.dev })?;

    match env::consts::OS {
        "macos" => package_macos(layout, &host_daemon)?,
        "linux" => package_linux(layout, &host_daemon)?,
        os => bail!("unsupported OS: {os}"),
    }
    super::print_dist_files(layout)
}

fn build_daemon(layout: &Layout) -> Result<PathBuf> {
    if layout.all_daemons {
        // The daemon xtask owns cross compilation. Keeping this as an in-process
        // call avoids a second Rust bootstrap and preserves its target policy.
        crate::daemon::build_all()?;
        crate::daemon::verify(&layout.daemon_output_dir)?;
        Ok(layout
            .daemon_output_dir
            .join(host_daemon_target()?.artifact_name()))
    } else {
        let mut command = Command::new("cargo");
        command
            .arg("build")
            .arg("--release")
            .args(["-p", DAEMON, "--bin", DAEMON]);
        command::run(&mut command)?;
        Ok(layout.target_root.join("release").join(DAEMON))
    }
}

fn host_daemon_target() -> Result<DaemonTarget> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok(DaemonTarget::Aarch64AppleDarwin),
        ("macos", "x86_64") => Ok(DaemonTarget::X86_64AppleDarwin),
        ("linux", "x86_64") => Ok(DaemonTarget::X86_64UnknownLinuxGnu),
        ("linux", "aarch64") => Ok(DaemonTarget::Aarch64UnknownLinuxGnu),
        (os, arch) => bail!("unsupported host daemon target: {os} {arch}"),
    }
}

fn copy_bundled_daemons(layout: &Layout, destination: &Path) -> Result<()> {
    if !layout.all_daemons {
        return Ok(());
    }
    for target in TARGETS {
        let artifact = target.artifact_name();
        filesystem::copy_executable(
            &layout.daemon_output_dir.join(&artifact),
            &destination.join(artifact),
        )?;
    }
    Ok(())
}

fn package_macos(layout: &Layout, host_daemon: &Path) -> Result<()> {
    let bundle = layout.dist_dir.join(format!("{}.app", layout.app_name));
    filesystem::recreate_dir(&bundle)?;
    let contents = bundle.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;
    let binary = macos.join(BINARY);
    filesystem::copy_executable(
        &layout.target_root.join(layout.profile).join(BINARY),
        &binary,
    )?;
    filesystem::copy_executable(host_daemon, &macos.join(DAEMON))?;
    copy_bundled_daemons(layout, &resources.join("daemons"))?;
    compile_macos_icon(&contents, &resources)?;
    fs::write(
        contents.join("Info.plist"),
        info_plist(layout, &super::workspace_version()?),
    )?;
    sign_macos_bundle(layout, &bundle, &macos)?;
    let archive_name = format!("{}-macos-{}.app.zip", layout.app_name, macos_archive_arch());
    let archive = layout.dist_dir.join(&archive_name);
    remove_file_if_exists(&archive)?;
    command::run(
        Command::new("zip")
            .current_dir(&layout.dist_dir)
            .args(["-q", "-r", "-y"])
            .arg(archive_name)
            .arg(format!("{}.app", layout.app_name)),
    )
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn macos_archive_arch() -> &'static str {
    match env::consts::ARCH {
        "aarch64" => "arm64",
        arch => arch,
    }
}

fn compile_macos_icon(contents: &Path, resources: &Path) -> Result<()> {
    let contents = fs::canonicalize(contents)
        .with_context(|| format!("failed to resolve {}", contents.display()))?;
    let resources = fs::canonicalize(resources)
        .with_context(|| format!("failed to resolve {}", resources.display()))?;
    let icon = fs::canonicalize("crates/bootty/assets/bootty.icon")
        .context("failed to resolve the macOS app icon")?;
    let actool = command::stdout(Command::new("xcrun").args(["--find", "actool"]));
    let actool = actool.as_deref().ok().map(str::trim);
    if actool.is_none()
        || actool
            .map(actool_major_version)
            .transpose()?
            .is_some_and(|version| version < 26)
    {
        eprintln!("Liquid Glass icon compilation unavailable; using legacy macOS .icns fallback");
        return filesystem::copy_file(
            Path::new("crates/bootty-ui/assets/bootty-icon-macos-fallback.icns"),
            &resources.join("bootty.icns"),
        );
    }
    let actool = actool.context("failed to locate actool")?;
    let partial = contents.join("assetcatalog-info.plist");
    let output = Command::new(actool)
        .arg(icon)
        .args(["--compile", resources.to_string_lossy().as_ref()])
        .args(["--app-icon", "bootty", "--enable-on-demand-resources", "NO"])
        .args(["--development-region", "en", "--target-device", "mac"])
        .args(["--platform", "macosx", "--include-all-app-icons"])
        .args(["--minimum-deployment-target", "13.0"])
        .args([
            "--output-partial-info-plist",
            partial.to_string_lossy().as_ref(),
        ])
        .arg("--enable-icon-stack-fallback-generation=enabled")
        .output()
        .context("failed to run actool")?;
    let _ = fs::remove_file(partial);
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "actool failed compiling the app icon:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

fn actool_major_version(actool: &str) -> Result<u64> {
    let version = command::stdout(Command::new(actool).arg("--version"))?;
    let short_version = version
        .split_once("<key>short-bundle-version</key>")
        .and_then(|(_, suffix)| suffix.split_once("<string>"))
        .and_then(|(_, suffix)| suffix.split_once("</string>"))
        .map(|(version, _)| version.trim())
        .context("actool --version did not report its short bundle version")?;
    short_version
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .context("actool reported an invalid short bundle version")
}

fn info_plist(layout: &Layout, version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>{}</string>
  <key>CFBundleExecutable</key><string>bootty</string>
  <key>CFBundleIconFile</key><string>bootty</string>
  <key>CFBundleIconName</key><string>bootty</string>
  <key>CFBundleIdentifier</key><string>{}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>{}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key><string>{version}</string>
  <key>CFBundleURLTypes</key><array><dict>
    <key>CFBundleURLSchemes</key><array><string>{scheme}</string></array>
  </dict></array>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Programs running in Bootty can request microphone access.</string>
</dict>
</plist>
"#,
        layout.app_name,
        layout.bundle_identifier,
        layout.app_name,
        scheme = layout.development_namespace.as_deref().unwrap_or("bootty"),
    )
}

fn sign_macos_bundle(layout: &Layout, bundle: &Path, macos: &Path) -> Result<()> {
    if !executable_on_path("codesign") {
        return Ok(());
    }
    let identity = crate::signing::identity();
    command::run(
        Command::new("codesign")
            .args(["--force", "--sign", &identity])
            .arg(macos.join(DAEMON)),
    )?;
    let mut sign = Command::new("codesign");
    sign.args(["--force", "--sign", &identity]);
    // A certificate-backed signature gets codesign's default designated requirement, which
    // pins identifier and certificate leaf. Ad-hoc has no leaf, so name the identifier only.
    if crate::signing::is_adhoc(&identity) {
        sign.arg("--requirements").arg(format!(
            "=designated => identifier \"{}\"",
            layout.bundle_identifier
        ));
    }
    command::run(sign.arg(bundle))
}

fn executable_on_path(program: &str) -> bool {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .any(|directory| directory.join(program).is_file())
}

fn package_linux(layout: &Layout, host_daemon: &Path) -> Result<()> {
    let name = format!("{}-linux-{}", layout.app_name, env::consts::ARCH);
    let root = layout.dist_dir.join(&name);
    filesystem::recreate_dir(&root)?;
    let bin = root.join("bin");
    let applications = root.join("share/applications");
    let png = root.join("share/icons/hicolor/256x256/apps/bootty.png");
    let svg = root.join("share/icons/hicolor/scalable/apps/bootty.svg");
    fs::create_dir_all(&bin)?;
    fs::create_dir_all(&applications)?;
    filesystem::copy_executable(
        &layout.target_root.join(layout.profile).join(BINARY),
        &bin.join(&layout.cli_name),
    )?;
    filesystem::copy_executable(host_daemon, &bin.join(DAEMON))?;
    copy_bundled_daemons(layout, &root.join("share/bootty/daemons"))?;
    filesystem::copy_file(Path::new("crates/bootty-ui/assets/bootty-mascot.png"), &png)?;
    filesystem::copy_file(Path::new("crates/bootty-ui/assets/bootty-mascot.svg"), &svg)?;
    fs::write(
        applications.join(format!("{}.desktop", layout.bundle_identifier)),
        desktop_entry(layout),
    )?;
    let archive = layout.dist_dir.join(format!("{name}.tar.gz"));
    remove_file_if_exists(&archive)?;
    command::run(
        Command::new("tar")
            .args(["-C", layout.dist_dir.to_string_lossy().as_ref(), "-czf"])
            .arg(archive)
            .arg(name),
    )
}

fn desktop_entry(layout: &Layout) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={}\nComment=Native GPU-rendered terminal\nExec={}\nIcon=bootty\nTerminal=false\nCategories=System;TerminalEmulator;\n",
        layout.app_name, layout.cli_name
    )
}
