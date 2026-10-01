#!/usr/bin/env python3
"""Build the native iOS app; Cargo shares the desktop chat's advisory lock."""
import argparse
import fcntl
import os
import plistlib
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", action="store_true")
    parser.add_argument("--release", action="store_true")
    args = parser.parse_args()
    sdk = "iphoneos" if args.device else "iphonesimulator"
    triple = "aarch64-apple-ios" if args.device else "aarch64-apple-ios-sim"
    profile = "release" if args.release else "debug"
    with open("/tmp/bootty-cargo-build.lock", "a") as lock:
        print("Waiting for shared Bootty Cargo lock", flush=True)
        fcntl.flock(lock, fcntl.LOCK_EX)
        print("Building Rust mobile library", flush=True)
        subprocess.run(["cargo", "build", "--manifest-path", str(ROOT / "Cargo.toml"),
                        "--locked", "--lib", "--target", triple] +
                       (["--release"] if args.release else []), check=True,
                       env={**os.environ, "IPHONEOS_DEPLOYMENT_TARGET": "16.0"})
    sdk_path = subprocess.check_output(["xcrun", "--sdk", sdk, "--show-sdk-path"], text=True).strip()
    bundle = ROOT / "dist" / sdk / "BoottyMobile.app"
    bundle.mkdir(parents=True, exist_ok=True)
    entitlements = bundle.parent / "simulator.entitlements"
    if not args.device:
        # Simulator Keychain reads the Mach-O identity; physical signing supplies
        # the real team's application identifier through its provisioning profile.
        with entitlements.open("wb") as output:
            plistlib.dump({"application-identifier": "dev.bootty.mobile.dev"}, output)
    target = "arm64-apple-ios16.0" + ("" if args.device else "-simulator")
    library = ROOT / "target" / triple / profile / "libbootty_mobile.a"
    frameworks = ["UIKit", "Metal", "MetalKit", "QuartzCore", "CoreGraphics", "CoreText",
                  "CoreFoundation", "Foundation", "Security", "SystemConfiguration", "AVFoundation", "CoreMedia", "AudioToolbox"]
    command = ["xcrun", "--sdk", sdk, "swiftc", "-sdk", sdk_path, "-target", target,
               "-swift-version", "5", "-import-objc-header", str(ROOT / "ios" / "Embedding.h"),
               str(ROOT / "ios" / "App.swift"), "-o", str(bundle / "BoottyMobile"),
               "-parse-as-library", "-O" if args.release else "-Onone",
               "-Xlinker", "-force_load", "-Xlinker", str(library), "-lc++"]
    if not args.device:
        command += ["-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker",
                    "__entitlements", "-Xlinker", str(entitlements)]
    for framework in frameworks:
        command += ["-framework", framework]
    subprocess.run(command, check=True)
    partial = bundle.parent / "assetcatalog-info.plist"
    icon = subprocess.run([
        "xcrun", "actool", str(ROOT.parent / "crates/bootty/assets/bootty.icon"),
        "--compile", str(bundle), "--platform", sdk, "--minimum-deployment-target", "16.0",
        "--app-icon", "bootty", "--output-partial-info-plist", str(partial),
        "--target-device", "iphone", "--target-device", "ipad",
    ], capture_output=True, text=True)
    if icon.returncode:
        print(icon.stdout + icon.stderr)
    icon.check_returncode()
    with partial.open("rb") as info:
        icon_info = plistlib.load(info)
    with (bundle / "Info.plist").open("wb") as info:
        plistlib.dump({
            **icon_info,
            "CFBundleIdentifier": "dev.bootty.mobile.dev", "CFBundleName": "Bootty Mobile",
            "CFBundleExecutable": "BoottyMobile", "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": "0.1.0", "CFBundleVersion": "1",
            "MinimumOSVersion": "16.0", "LSRequiresIPhoneOS": True,
            "UIDeviceFamily": [1, 2], "UILaunchScreen": {},
            "NSLocalNetworkUsageDescription": "Connect to your paired Bootty computer and control its terminals.",
            "UIApplicationSceneManifest": {"UIApplicationSupportsMultipleScenes": False},
            "UISupportedInterfaceOrientations": ["UIInterfaceOrientationPortrait",
                                                "UIInterfaceOrientationLandscapeLeft",
                                                "UIInterfaceOrientationLandscapeRight"],
        }, info)
    if not args.device:
        subprocess.run(["codesign", "--force", "--sign", "-", str(bundle)], check=True)
    print(bundle)
    if args.device:
        print("Unsigned device build: development team, provisioning, and device signing are required.")


if __name__ == "__main__":
    main()
