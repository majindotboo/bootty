//! Both native tray hosts use the bundled mascot at a small, high-DPI size.
use bootty_config::config::TrayIconStyle;
use image::{ImageFormat, imageops};
use std::sync::OnceLock;

pub(super) const SIZE: u32 = 36;
static ICONS: OnceLock<Result<[Vec<u8>; 2], String>> = OnceLock::new();

pub(super) fn icon(style: TrayIconStyle) -> Result<Vec<u8>, String> {
    let icons = ICONS.get_or_init(load).as_ref().map_err(Clone::clone)?;
    Ok(match style {
        TrayIconStyle::Color => &icons[0],
        TrayIconStyle::Monochrome => &icons[1],
    }
    .clone())
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "The visible pixel bounds are ordered coordinates within the decoded image; 8-bit coverage products fit in u16."
)]
fn load() -> Result<[Vec<u8>; 2], String> {
    let image = image::load_from_memory_with_format(
        include_bytes!("../../assets/bootty-mascot.png"),
        ImageFormat::Png,
    )
    .map_err(|error| error.to_string())?
    .into_rgba8();
    // The application asset includes transparent padding. Use the mascot's visible bounds so
    // its face remains readable at menu bar size.
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel.0[3] > 16 {
            bounds = Some(bounds.map_or((x, y, x, y), |(left, top, right, bottom)| {
                (left.min(x), top.min(y), right.max(x), bottom.max(y))
            }));
        }
    }
    let (left, top, right, bottom) = bounds.ok_or("The mascot has no visible pixels")?;
    let cropped =
        imageops::crop_imm(&image, left, top, right - left + 1, bottom - top + 1).to_image();
    let mut mono = cropped.clone();
    for pixel in mono.pixels_mut() {
        let [red, green, blue, alpha] = pixel.0;
        // Bright mascot parts become ink; the dark face becomes negative space. Flattening
        // alpha alone would lose the eyes and mouth in a solid silhouette.
        let coverage = u16::from(red.max(green).max(blue)) * u16::from(alpha) / 255;
        let ink = if cfg!(target_os = "macos") { 0 } else { 128 };
        pixel.0 = [ink, ink, ink, u8::try_from(coverage).unwrap_or(255)];
    }
    Ok([fit(&cropped), fit(&mono)])
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "The nonempty crop gives a nonzero extent and the fitted dimensions cannot exceed SIZE."
)]
fn fit(image: &image::RgbaImage) -> Vec<u8> {
    let extent = image.width().max(image.height());
    let width = (image.width().saturating_mul(SIZE) / extent).max(1);
    let height = (image.height().saturating_mul(SIZE) / extent).max(1);
    let resized = imageops::resize(image, width, height, imageops::FilterType::Lanczos3);
    let mut icon = image::RgbaImage::new(SIZE, SIZE);
    imageops::overlay(
        &mut icon,
        &resized,
        i64::from((SIZE - width) / 2),
        i64::from((SIZE - height) / 2),
    );
    icon.into_raw()
}
