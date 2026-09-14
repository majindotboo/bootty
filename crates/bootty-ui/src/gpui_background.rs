use bootty_config::config::{BackgroundMaterial, WindowConfig};
use gpui_kit::{
    AnyElement, IntoElement, ObjectFit, ParentElement, Styled, StyledImage,
    WindowBackgroundAppearance, div, img, linear_color_stop, linear_gradient, rgba,
};

pub const fn material(config: &WindowConfig) -> WindowBackgroundAppearance {
    match config.background_material {
        BackgroundMaterial::Opaque => WindowBackgroundAppearance::Opaque,
        BackgroundMaterial::Transparent => WindowBackgroundAppearance::Transparent,
        BackgroundMaterial::Blurred => WindowBackgroundAppearance::Blurred,
        BackgroundMaterial::Mica if cfg!(target_os = "windows") => {
            WindowBackgroundAppearance::MicaBackdrop
        }
        BackgroundMaterial::MicaAlt if cfg!(target_os = "windows") => {
            WindowBackgroundAppearance::MicaAltBackdrop
        }
        BackgroundMaterial::Mica | BackgroundMaterial::MicaAlt => {
            WindowBackgroundAppearance::Transparent
        }
    }
}

/// A single workspace backdrop keeps images and gradients continuous across pane splits.
pub fn layer(config: &WindowConfig, config_path: &std::path::Path) -> AnyElement {
    let mut layer = div().absolute().inset_0().overflow_hidden();
    if let (Some(start), Some(end)) = (
        config.background_gradient_start,
        config.background_gradient_end,
    ) {
        let color = |color: bootty_config::color::Color| {
            rgba(u32::from_be_bytes([color.r, color.g, color.b, color.a]))
        };
        layer = layer.bg(linear_gradient(
            config.background_gradient_angle,
            linear_color_stop(color(start), 0.0),
            linear_color_stop(color(end), 1.0),
        ));
    }
    if let Some(path) = &config.background_image {
        layer = layer.child(
            img(if path.is_absolute() {
                path.clone()
            } else {
                config_path
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join(path)
            })
            .absolute()
            .inset_0()
            .size_full()
            .object_fit(ObjectFit::Cover)
            .opacity(config.background_image_opacity)
            .with_fallback(|| {
                div()
                    .text_sm()
                    .child("Background image unavailable")
                    .into_any_element()
            }),
        );
    }
    layer.into_any_element()
}
