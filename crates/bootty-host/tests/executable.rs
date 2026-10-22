#![cfg(windows)]

use assert_fs::{TempDir, prelude::*};
use bootty_host::{find_executable_in, local_command};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{ffi::OsStr, path::Path};

#[rstest]
#[case(".EXE;.CMD", "tool.exe")]
#[case(".CMD;.EXE", "tool.cmd")]
fn windows_search_uses_path_extensions_instead_of_posix_shims(
    #[case] extensions: &str,
    #[case] expected: &str,
) {
    let root = TempDir::new().unwrap();
    root.child("tool").write_str("#!/bin/sh\n").unwrap();
    root.child("tool.cmd").write_str("@echo off\r\n").unwrap();
    root.child("tool.exe").write_binary(b"MZfixture").unwrap();
    let found = find_executable_in(
        OsStr::new("tool"),
        Some(root.path().as_os_str()),
        Some(OsStr::new(extensions)),
    );
    assert_eq!(
        found.unwrap().canonicalize().unwrap(),
        root.path().join(expected).canonicalize().unwrap()
    );
}

#[test]
fn missing_programs_directories_and_posix_only_shims_are_not_available() {
    let root = TempDir::new().unwrap();
    root.child("tool.exe").create_dir_all().unwrap();
    root.child("tool").write_str("#!/bin/sh\n").unwrap();
    for program in ["tool", "missing", ""] {
        assert_eq!(
            find_executable_in(OsStr::new(program), Some(root.path().as_os_str()), None),
            None
        );
    }
}

#[test]
fn explicit_extensionless_paths_resolve_and_path_order_is_preserved() {
    let root = TempDir::new().unwrap();
    root.child("first/tool.cmd")
        .write_str("@echo first")
        .unwrap();
    root.child("second/tool.exe")
        .write_binary(b"MZfixture")
        .unwrap();
    let paths =
        std::env::join_paths([root.path().join("first"), root.path().join("second")]).unwrap();
    let expected = root.path().join("first/tool.cmd").canonicalize().unwrap();
    assert_eq!(
        find_executable_in(OsStr::new("tool"), Some(&paths), None)
            .map(|path| path.canonicalize().unwrap()),
        Some(expected.clone())
    );
    assert_eq!(
        find_executable_in(root.path().join("first/tool").as_os_str(), None, None)
            .map(|path| path.canonicalize().unwrap()),
        Some(expected)
    );
}

#[test]
fn batch_launch_keeps_literal_arguments() {
    let root = TempDir::new().unwrap();
    root.child("echo.py")
        .write_str("import json,sys\nprint(json.dumps(sys.argv[1:]))\n")
        .unwrap();
    let shim = root.child("provider.cmd");
    shim.write_str("@echo off\r\n@python \"%~dp0echo.py\" %*\r\n")
        .unwrap();
    let arguments = [
        "",
        "two words",
        "quote\"inside",
        "trailing\\",
        "%PATH%",
        "!delayed!",
        "a&echo injected>owned",
        "(x)|y^z",
        "ü 🥟",
    ];
    let mut command = local_command(shim.path());
    assert_eq!(Path::new(command.get_program()), shim.path());
    let output = command
        .args(arguments)
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let received: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(received, arguments);
    assert!(!root.path().join("owned").exists());
}
