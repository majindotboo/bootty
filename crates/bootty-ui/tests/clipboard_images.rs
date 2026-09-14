#![cfg(test)]

use bootty_ui::platform::decode_clipboard_image;
use image::{DynamicImage, ImageFormat};
use rstest::rstest;
#[rstest]
#[case("image/png", ImageFormat::Png)]
#[case("image/jpeg", ImageFormat::Jpeg)]
#[case("image/gif", ImageFormat::Gif)]
#[case("image/webp", ImageFormat::WebP)]
fn clipboard_decodes_supported_formats_and_rejects_mime_mismatch(
    #[case] mime: &str,
    #[case] format: ImageFormat,
) {
    let mut bytes = std::io::Cursor::new(Vec::new());
    DynamicImage::new_rgb8(3, 2)
        .write_to(&mut bytes, format)
        .unwrap();
    let image = decode_clipboard_image(mime, bytes.get_ref()).unwrap();
    pretty_assertions::assert_eq!((image.width, image.height, image.bytes.len()), (3, 2, 24));
    let wrong = if mime == "image/png" {
        "image/jpeg"
    } else {
        "image/png"
    };
    assert!(decode_clipboard_image(wrong, bytes.get_ref()).is_err());
    assert!(decode_clipboard_image(mime, b"not an image").is_err());
}
