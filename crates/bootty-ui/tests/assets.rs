#![cfg(test)]

use bootty_ui::assets::BoottyAssets;
use gpui_kit::AssetSource;
use gpui_kit::component::{IconName, IconNamed as _};
use pretty_assertions::assert_eq;
use rstest::rstest;

const FONT_ASSETS: &[&str] = &[
    "fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf",
    "fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf",
    "fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf",
    "fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf",
    "fonts/ibm-plex-sans/license.txt",
    "fonts/lilex/Lilex-Bold.ttf",
    "fonts/lilex/Lilex-BoldItalic.ttf",
    "fonts/lilex/Lilex-Italic.ttf",
    "fonts/lilex/Lilex-Regular.ttf",
    "fonts/lilex/OFL.txt",
];

#[rstest]
fn bundled_font_files_and_licenses_are_embedded() {
    let assets = BoottyAssets;
    let listed = assets.list("fonts").expect("list bundled fonts");

    for path in FONT_ASSETS {
        assert!(
            listed.iter().any(|listed| listed.as_ref() == *path),
            "{path} is listed"
        );
        let bytes = assets
            .load(path)
            .expect("load bundled asset")
            .unwrap_or_else(|| panic!("{path} is embedded"));
        assert!(!bytes.is_empty(), "{path} has content");
    }
}

#[rstest]
fn component_controls_load_their_real_svg_assets() {
    let path = IconName::ChevronDown.path();
    let bytes = BoottyAssets
        .load(&path)
        .expect("load component control icon")
        .unwrap_or_else(|| panic!("{path} is embedded"));

    assert!(bytes.starts_with(b"<svg"), "{path} is an SVG asset");
}

#[rstest]
fn bootty_title_icon_loads_the_app_artwork() {
    let bytes = BoottyAssets
        .load("icons/bootty.png")
        .expect("load Bootty title icon")
        .expect("Bootty title icon is embedded");

    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
}

#[rstest]
fn font_directory_listing_matches_asset_source_prefixes() {
    let assets = BoottyAssets;
    let lilex = assets.list("fonts/lilex").expect("list Lilex fonts");
    let plex = assets
        .list("fonts/ibm-plex-sans")
        .expect("list IBM Plex Sans fonts");

    assert_eq!(lilex.len(), 5);
    assert_eq!(plex.len(), 5);
}
