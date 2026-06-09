use std::io::Cursor;

use eframe::egui;

#[cfg(not(target_os = "macos"))]
const NATIVE_APP_ICON_PNG: &[u8] = MASCOT_ICON_PNG;
const MASCOT_ICON_PNG: &[u8] = include_bytes!("../assets/bootty-mascot.png");
#[cfg(target_os = "macos")]
pub(crate) const MACOS_DOCK_ICON_ICNS: &[u8] =
    include_bytes!("../assets/bootty-icon-macos-dock.icns");

pub(crate) fn title_icon_color_image() -> egui::ColorImage {
    let icon = decode_png(MASCOT_ICON_PNG);
    egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    )
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn native_app_icon_data() -> egui::IconData {
    let icon = decode_png(NATIVE_APP_ICON_PNG);
    egui::IconData {
        rgba: icon.rgba,
        width: icon.width,
        height: icon.height,
    }
}

struct AppIconPng {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

fn decode_png(png_bytes: &[u8]) -> AppIconPng {
    let mut decoder = png::Decoder::new(Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .expect("embedded Bootty icon PNG header decodes");
    let output_size = reader
        .output_buffer_size()
        .expect("embedded Bootty icon PNG output size is known");
    let mut output = vec![0; output_size];
    let info = reader
        .next_frame(&mut output)
        .expect("embedded Bootty icon PNG frame decodes");
    let bytes = &output[..info.buffer_size()];
    let rgba = rgba_from_png(bytes, info.color_type);
    assert_eq!(
        rgba.len(),
        (info.width * info.height * 4) as usize,
        "embedded Bootty icon decodes to rgba8"
    );
    AppIconPng {
        rgba,
        width: info.width,
        height: info.height,
    }
}

fn rgba_from_png(bytes: &[u8], color_type: png::ColorType) -> Vec<u8> {
    match color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect(),
        png::ColorType::Grayscale => bytes
            .iter()
            .flat_map(|gray| [*gray, *gray, *gray, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => bytes
            .chunks_exact(2)
            .flat_map(|gray| [gray[0], gray[0], gray[0], gray[1]])
            .collect(),
        png::ColorType::Indexed => panic!("indexed Bootty icon PNG is unsupported"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_icon_decodes_to_rgba8_bytes() {
        #[cfg(target_os = "macos")]
        assert!(!MACOS_DOCK_ICON_ICNS.is_empty());

        #[cfg(not(target_os = "macos"))]
        {
            let icon = native_app_icon_data();
            assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        }
    }

    #[test]
    fn title_icon_decodes_to_color_image() {
        let image = title_icon_color_image();

        assert_eq!(image.pixels.len(), image.size[0] * image.size[1]);
    }
}
