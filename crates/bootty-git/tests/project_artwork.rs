use std::io::Cursor;

use assert_fs::{TempDir, prelude::*};
use bootty_git::{
    detect_project_icon,
    project_icon::{MAX_PROJECT_ICON_BYTES, decode_project_icon, detect_project_icon_with_runner},
    project_monogram,
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

#[fixture]
fn project() -> Result<TempDir, assert_fs::fixture::FixtureError> {
    TempDir::new()
}

fn png(width: u32, height: u32) -> Result<Vec<u8>, image::ImageError> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

#[rstest]
#[case(1, 1)]
#[case(256, 128)]
#[case(128, 256)]
fn detected_artwork_is_bounded_and_preserves_color_and_aspect(
    project: Result<TempDir, assert_fs::fixture::FixtureError>,
    #[case] width: u32,
    #[case] height: u32,
) {
    let project = project.expect("project fixture");
    project.child("public").create_dir_all().unwrap();
    project
        .child("public/favicon.png")
        .write_binary(&png(width, height).expect("PNG fixture"))
        .unwrap();
    let icon = detect_project_icon(project.path()).expect("project artwork");
    assert_eq!(icon.source, "public/favicon.png");
    assert!(icon.width <= 64 && icon.height <= 64);
    assert_eq!(
        icon.width.checked_mul(height).unwrap(),
        icon.height.checked_mul(width).unwrap()
    );
    assert_eq!(&icon.bgra[..4], &[30, 20, 10, 255]);
    assert_eq!(
        icon.bgra.len(),
        usize::try_from(
            icon.width
                .checked_mul(icon.height)
                .unwrap()
                .checked_mul(4)
                .unwrap()
        )
        .unwrap()
    );
}

#[rstest]
fn damaged_or_oversized_candidates_do_not_hide_valid_artwork(
    project: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let project = project.expect("project fixture");
    project.child("public").create_dir_all().unwrap();
    project
        .child("public/apple-touch-icon.png")
        .write_binary(&vec![0; MAX_PROJECT_ICON_BYTES + 1])
        .unwrap();
    project
        .child("apple-touch-icon.png")
        .write_str("not an image")
        .unwrap();
    project
        .child("public/favicon.png")
        .write_binary(&png(32, 32).expect("PNG fixture"))
        .unwrap();
    assert_eq!(
        detect_project_icon(project.path()).unwrap().source,
        "public/favicon.png"
    );
}

#[rstest]
#[case::too_wide(2049, 1)]
#[case::too_tall(1, 2049)]
fn oversized_image_dimensions_are_rejected_before_decoding(
    #[case] width: u32,
    #[case] height: u32,
) {
    assert_eq!(
        decode_project_icon("icon.png", &png(width, height).expect("PNG fixture")),
        None
    );
}

#[cfg(unix)]
#[rstest]
fn artwork_symlinks_cannot_read_outside_project(
    project: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let project = project.expect("project fixture");
    let other = TempDir::new().unwrap();
    other
        .child("icon.png")
        .write_binary(&png(32, 32).expect("PNG fixture"))
        .unwrap();
    std::os::unix::fs::symlink(
        other.child("icon.png").path(),
        project.child("icon.png").path(),
    )
    .unwrap();
    assert_eq!(detect_project_icon(project.path()), None);
    assert_eq!(
        detect_project_icon_with_runner(
            &project.path().to_string_lossy(),
            &bootty_git::SystemCommandRunner
        ),
        None
    );
}

#[cfg(unix)]
#[rstest]
fn host_reader_reads_quoted_paths_without_shell_interpolation(
    project: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let project = project.expect("project fixture");
    let folder = project.child("project ' $() with spaces");
    folder.create_dir_all().unwrap();
    folder
        .child("favicon.png")
        .write_binary(&png(16, 16).expect("PNG fixture"))
        .unwrap();
    let icon = detect_project_icon_with_runner(
        &folder.path().to_string_lossy(),
        &bootty_git::SystemCommandRunner,
    )
    .expect("host artwork");
    assert_eq!(icon.source, "favicon.png");
    assert_eq!(&icon.bgra[..4], &[30, 20, 10, 255]);
}

#[derive(Clone)]
struct UnavailableHost;
impl bootty_git::CommandRunner for UnavailableHost {
    fn run(&self, _: &str, _: &[String]) -> anyhow::Result<bootty_git::CommandOutput> {
        anyhow::bail!("host unavailable")
    }
}

#[rstest]
fn unavailable_host_never_reads_same_named_local_project(
    project: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let project = project.expect("project fixture");
    project
        .child("icon.png")
        .write_binary(&png(16, 16).expect("PNG fixture"))
        .unwrap();
    assert!(detect_project_icon(project.path()).is_some());
    assert_eq!(
        detect_project_icon_with_runner(&project.path().to_string_lossy(), &UnavailableHost),
        None
    );
}

#[rstest]
#[case(" bootty", "B")]
#[case("-workspace", "W")]
#[case("雪", "雪")]
#[case("", "?")]
fn monograms_use_the_first_project_letter(#[case] name: &str, #[case] expected: &str) {
    assert_eq!(project_monogram(name), expected);
}

#[derive(Clone, Debug, proptest_derive::Arbitrary)]
struct IconSample {
    #[proptest(strategy = "1u32..=192")]
    width: u32,
    #[proptest(strategy = "1u32..=192")]
    height: u32,
    rgb: [u8; 3],
}

#[rstest]
fn uniform_project_artwork_preserves_color_within_resampling_precision() {
    use proptest::{
        prelude::*,
        test_runner::{Config, TestRunner},
    };
    TestRunner::new(Config {
        cases: 24,
        ..Config::default()
    })
    .run(&any::<IconSample>(), |sample| {
        let pixel = image::Rgba([sample.rgb[0], sample.rgb[1], sample.rgb[2], 255]);
        let image = image::RgbaImage::from_pixel(sample.width, sample.height, pixel);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let icon = decode_project_icon("icon.png", bytes.get_ref()).unwrap();
        prop_assert!((1..=64).contains(&icon.width));
        prop_assert!((1..=64).contains(&icon.height));
        let expected = [sample.rgb[2], sample.rgb[1], sample.rgb[0], 255];
        for pixel in icon.bgra.as_chunks::<4>().0 {
            let differences =
                std::array::from_fn::<_, 4, _>(|index| pixel[index].abs_diff(expected[index]));
            assert_eq!(
                differences.map(|difference| difference <= 1),
                [true; 4],
                "BGRA differences: {differences:?}"
            );
        }
        Ok(())
    })
    .unwrap();
}

#[rstest]
fn vector_artwork_is_rendered_as_a_native_thumbnail() {
    let bytes = br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><path fill="#0a141e" d="M0 0H32V32H0Z"/></svg>"##;
    let icon = decode_project_icon("favicon.svg", bytes).expect("SVG artwork");
    assert!(icon.width <= 64 && icon.height <= 64);
    assert_eq!(&icon.bgra[..4], &[30, 20, 10, 255]);
}

#[rstest]
fn vector_artwork_does_not_follow_external_image_references(
    project: Result<TempDir, assert_fs::fixture::FixtureError>,
) {
    let project = project.expect("project fixture");
    let image = project.child("private.png");
    image
        .write_binary(&png(16, 16).expect("PNG fixture"))
        .unwrap();
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><image href=\"{}\" width=\"16\" height=\"16\"/></svg>",
        image.path().display()
    );
    assert_eq!(decode_project_icon("favicon.svg", svg.as_bytes()), None);
}
