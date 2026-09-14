//! macOS code-signing identity for local builds.
//!
//! TCC grants (Accessibility, Screen Recording) are keyed to the app's designated requirement.
//! Ad-hoc signatures have no certificate, so macOS pins the grant to one build's code hash and
//! every reinstall silently loses it. A self-signed certificate that lives in the login keychain
//! gives every local build the same `identifier and certificate leaf` requirement, so grants
//! survive rebuilds. Release builds are out of scope: they need a Developer ID and notarization.
use std::fs;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::command;

pub const IDENTITY: &str = "Bootty Dev";
const ADHOC: &str = "-";

/// The `codesign --sign` argument for local bundles: the shared development identity when it
/// exists or can be created, otherwise ad-hoc.
#[must_use]
pub fn identity() -> String {
    if identity_is_valid() {
        return IDENTITY.to_owned();
    }
    // CI has no user to approve trusting a local development certificate.
    if std::env::var_os("CI").is_some() {
        return ADHOC.to_owned();
    }
    match setup() {
        Ok(()) => IDENTITY.to_owned(),
        Err(error) => {
            eprintln!(
                "note: signing ad-hoc; macOS permission grants will not survive reinstalls: {error:#}"
            );
            ADHOC.to_owned()
        }
    }
}

#[must_use]
pub fn is_adhoc(identity: &str) -> bool {
    identity == ADHOC
}

/// `mise run sign:setup`: create and trust the development identity, reporting the outcome.
/// # Errors
/// Returns an error on unsupported platforms or if development identity creation or trust fails.
pub fn run() -> Result<()> {
    if std::env::consts::OS != "macos" {
        bail!("code-signing identities are only used on macOS");
    }
    if identity_is_valid() {
        println!("code-signing identity \"{IDENTITY}\" is ready");
        return Ok(());
    }
    setup()?;
    println!("created and trusted code-signing identity \"{IDENTITY}\"");
    Ok(())
}

fn identity_is_valid() -> bool {
    Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&format!("\"{IDENTITY}\""))
        })
}

fn certificate_exists() -> bool {
    Command::new("security")
        .args(["find-certificate", "-c", IDENTITY])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Import a fresh self-signed code-signing certificate when none exists, then trust it for code
/// signing. Trusting prompts once for the user's password; that is the only interactive step.
fn setup() -> Result<()> {
    let scratch = tempfile::tempdir().context("create signing scratch directory")?;
    let certificate = scratch.path().join("bootty-dev.pem");
    if certificate_exists() {
        let pem = command::stdout(Command::new("security").args([
            "find-certificate",
            "-c",
            IDENTITY,
            "-p",
        ]))?;
        fs::write(&certificate, pem)?;
    } else {
        let key = scratch.path().join("bootty-dev.key");
        command::run(
            Command::new("openssl")
                .args([
                    "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650",
                ])
                .args(["-subj", &format!("/CN={IDENTITY}")])
                .args(["-addext", "basicConstraints=critical,CA:false"])
                .args(["-addext", "keyUsage=critical,digitalSignature"])
                .args(["-addext", "extendedKeyUsage=critical,codeSigning"])
                .arg("-keyout")
                .arg(&key)
                .arg("-out")
                .arg(&certificate),
        )?;
        // `-T` lets codesign use the key without a per-build keychain prompt.
        command::run(
            Command::new("security")
                .arg("import")
                .arg(&key)
                .args(["-T", "/usr/bin/codesign"]),
        )?;
        command::run(Command::new("security").arg("import").arg(&certificate))?;
    }
    eprintln!("trusting \"{IDENTITY}\" for code signing; macOS will ask for your password once");
    command::run(
        Command::new("security")
            .args(["add-trusted-cert", "-r", "trustRoot", "-p", "codeSign"])
            .arg(&certificate),
    )?;
    if !identity_is_valid() {
        bail!("\"{IDENTITY}\" was imported but is not a valid code-signing identity");
    }
    Ok(())
}
