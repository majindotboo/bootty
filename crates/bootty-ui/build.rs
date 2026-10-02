fn main() {
    // These linker flags depend only on the target, not on UI source or test edits.
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "macos" {
        // Apply to this crate's test/example executables as well as the app binary.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
