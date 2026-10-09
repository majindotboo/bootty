use std::{
    collections::HashMap,
    io::Cursor,
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::Context as _;
use assert_fs::TempDir;
use bootty_ui::project_artwork::{
    MAX_ARTWORK_BYTES, ProjectArtworkCache, discover_project_artwork,
};
use image::{DynamicImage, ImageFormat, RgbaImage};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn png(width: u32, height: u32) -> anyhow::Result<Vec<u8>> {
    let pixels = RgbaImage::from_pixel(width, height, image::Rgba([17, 41, 63, 255]));
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(pixels).write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

fn write(root: &Path, relative: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().context("artwork parent")?)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

#[rstest]
#[case("logo.png", None)]
#[case("public/favicon.png", None)]
#[case("src-tauri/icons/icon.png", None)]
#[case(
    "Assets.xcassets/AppIcon.appiconset/app.png",
    Some("Assets.xcassets/AppIcon.appiconset/Contents.json")
)]
fn repository_web_and_native_artwork_are_decoded_from_the_project_host(
    #[case] relative: &str,
    #[case] contents: Option<&str>,
) -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    write(directory.path(), relative, &png(16, 16)?)?;
    if let Some(contents) = contents {
        write(
            directory.path(),
            contents,
            br#"{"images":[{"filename":"app.png"}]}"#,
        )?;
    }
    let cwd = directory.path().to_str().context("UTF-8 fixture")?;
    let image = discover_project_artwork(cwd, |path| std::fs::read(path).ok())
        .context("decoded artwork")?;
    assert_eq!(
        image.as_bytes(0).and_then(|pixels| pixels.get(..4)),
        Some([63, 41, 17, 255].as_slice())
    );
    Ok(())
}

#[rstest]
fn bootty_workspace_uses_its_embedded_application_artwork() {
    let mut requested = Vec::new();
    let image = discover_project_artwork("/owning-host/project", |path| {
        requested.push(path.to_owned());
        (path == "/owning-host/project/crates/bootty/Cargo.toml")
            .then(|| b"[package]\nname = \"bootty\"\n".to_vec())
    });
    assert!(image.is_some());
    assert_eq!(
        requested,
        [
            "/owning-host/project/Cargo.toml",
            "/owning-host/project/crates/bootty/Cargo.toml"
        ]
    );
}

#[rstest]
#[case::missing(None)]
#[case::invalid(Some(b"not an image".to_vec()))]
#[case::oversized(Some(vec![0; MAX_ARTWORK_BYTES.saturating_add(1)]))]
fn failed_artwork_preserves_the_folder_fallback(#[case] bytes: Option<Vec<u8>>) {
    let image = discover_project_artwork("/owning-host/project", |path| {
        (path == "/owning-host/project/icon.png")
            .then(|| bytes.clone())
            .flatten()
    });
    assert_eq!(image, None);
}

#[rstest]
fn oversized_decoded_dimensions_do_not_allocate_project_artwork() -> anyhow::Result<()> {
    let bytes = png(1025, 1)?;
    let image = discover_project_artwork("/owning-host/project", |path| {
        (path == "/owning-host/project/icon.png").then(|| bytes.clone())
    });
    assert_eq!(image, None);
    Ok(())
}

#[rstest]
fn native_manifest_cannot_read_outside_the_captured_project() {
    let mut requests = Vec::new();
    let image = discover_project_artwork("/owning-host/project", |path| {
        requests.push(path.to_owned());
        (path == "/owning-host/project/Assets.xcassets/AppIcon.appiconset/Contents.json").then(
            || {
                br#"{"images":[{"filename":"../../private.png"},{"filename":"/private.png"}]}"#
                    .to_vec()
            },
        )
    });
    assert_eq!(image, None);
    assert!(
        requests
            .iter()
            .all(|path| path.starts_with("/owning-host/project/"))
    );
    assert!(!requests.iter().any(|path| path.ends_with("private.png")));
}

#[rstest]
fn remote_reader_is_authoritative_even_when_a_local_path_has_the_same_name() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    write(directory.path(), "icon.png", &png(16, 16)?)?;
    let cwd = directory.path().to_str().context("UTF-8 fixture")?;
    let absent_on_remote = discover_project_artwork(cwd, |_| None);
    assert_eq!(absent_on_remote, None);
    let remote = HashMap::from([(format!("{cwd}/public/favicon.png"), png(8, 8)?)]);
    let observed_on_remote = discover_project_artwork(cwd, |path| remote.get(path).cloned());
    observed_on_remote.context("artwork supplied by the captured remote reader")?;
    Ok(())
}

#[rstest]
fn cache_coalesces_work_and_preserves_prior_artwork_until_refresh_finishes() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    write(directory.path(), "icon.png", &png(16, 16)?)?;
    let cwd = directory.path().to_str().context("UTF-8 fixture")?;
    let (sender, receiver) = mpsc::channel();
    let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = sender.send(());
    });
    let cache = ProjectArtworkCache::default();
    let now = Instant::now();
    assert_eq!(cache.request("local", cwd, None, now, &repaint), None);
    receiver.recv_timeout(Duration::from_secs(2))?;
    let first = cache
        .request("local", cwd, None, now, &repaint)
        .context("cached image")?;
    assert_eq!(
        cache.request("local", cwd, None, now, &repaint),
        Some(Arc::clone(&first))
    );
    anyhow::ensure!(
        receiver.try_recv().is_err(),
        "cached lookup scheduled a duplicate repaint"
    );

    std::fs::remove_file(directory.path().join("icon.png"))?;
    let refresh = now
        .checked_add(Duration::from_mins(6))
        .context("refresh timestamp")?;
    assert_eq!(
        cache.request("local", cwd, None, refresh, &repaint),
        Some(first)
    );
    receiver.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(cache.request("local", cwd, None, refresh, &repaint), None);
    assert_eq!(
        cache.request("different-binding", cwd, None, refresh, &repaint),
        None
    );
    receiver.recv_timeout(Duration::from_secs(2))?;
    cache.retire();
    assert_eq!(cache.request("local", cwd, None, refresh, &repaint), None);
    anyhow::ensure!(
        receiver.try_recv().is_err(),
        "retired cache scheduled a repaint"
    );
    Ok(())
}

proptest! {
    #[test]
    fn malformed_early_candidates_do_not_hide_later_valid_artwork(tail in prop::collection::vec(any::<u8>(), 0..128)) {
        let valid = png(8, 8).unwrap();
        let mut malformed = b"invalid artwork ".to_vec();
        malformed.extend(tail);
        let image = discover_project_artwork("/project", |path| match path {
            "/project/icon.png" => Some(malformed.clone()),
            "/project/public/favicon.png" => Some(valid.clone()),
            _ => None,
        });
        prop_assert!(image.is_some());
    }
}
