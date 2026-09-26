fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=windows/bootty.rc");
    println!("cargo:rerun-if-changed=assets/bootty.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "macos" {
        // AVPlayer's Swift bridge uses the system concurrency runtime (macOS 13+).
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        embed_resource::compile("windows/bootty.rc", embed_resource::NONE).manifest_optional()?;
    }
    Ok(())
}
