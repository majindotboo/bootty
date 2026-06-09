use crate::{
    geometry::SurfaceRect, paint_plan::PlanColor, terminal_image::KittyImagePlacement,
    terminal_text_atlas::TexturedGlyphQuad,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TerminalQuadDraw {
    pub(super) rect: SurfaceRect,
    pub(super) color: PlanColor,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BackgroundVertex {
    pub(super) position: [f32; 2],
    pub(super) color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TextVertex {
    pub(super) position: [f32; 2],
    pub(super) uv: [f32; 2],
    pub(super) color: [f32; 4],
}

pub(super) fn background_vertices(
    surface: SurfaceRect,
    draws: &[TerminalQuadDraw],
) -> Vec<BackgroundVertex> {
    let mut vertices = Vec::with_capacity(draws.len() * 6);
    for draw in draws {
        let min = surface_to_ndc(surface, draw.rect.min_x, draw.rect.min_y);
        let max = surface_to_ndc(surface, draw.rect.max_x, draw.rect.max_y);
        let top_left = [min[0], min[1]];
        let top_right = [max[0], min[1]];
        let bottom_right = [max[0], max[1]];
        let bottom_left = [min[0], max[1]];
        let color = color_to_float(draw.color);

        vertices.extend([
            BackgroundVertex {
                position: top_left,
                color,
            },
            BackgroundVertex {
                position: bottom_left,
                color,
            },
            BackgroundVertex {
                position: bottom_right,
                color,
            },
            BackgroundVertex {
                position: top_left,
                color,
            },
            BackgroundVertex {
                position: bottom_right,
                color,
            },
            BackgroundVertex {
                position: top_right,
                color,
            },
        ]);
    }
    vertices
}

pub(super) fn text_vertices(surface: SurfaceRect, quads: &[TexturedGlyphQuad]) -> Vec<TextVertex> {
    let mut vertices = Vec::with_capacity(quads.len() * 6);
    for quad in quads {
        let min = surface_to_ndc(surface, quad.rect.min_x, quad.rect.min_y);
        let max = surface_to_ndc(surface, quad.rect.max_x, quad.rect.max_y);
        let color = color_to_float(quad.color);
        let top_left = TextVertex {
            position: [min[0], min[1]],
            uv: [quad.uv.min_x, quad.uv.min_y],
            color,
        };
        let top_right = TextVertex {
            position: [max[0], min[1]],
            uv: [quad.uv.max_x, quad.uv.min_y],
            color,
        };
        let bottom_right = TextVertex {
            position: [max[0], max[1]],
            uv: [quad.uv.max_x, quad.uv.max_y],
            color,
        };
        let bottom_left = TextVertex {
            position: [min[0], max[1]],
            uv: [quad.uv.min_x, quad.uv.max_y],
            color,
        };
        vertices.extend([
            top_left,
            bottom_left,
            bottom_right,
            top_left,
            bottom_right,
            top_right,
        ]);
    }
    vertices
}

pub(super) fn image_vertices(surface: SurfaceRect, image: &KittyImagePlacement) -> Vec<TextVertex> {
    let Some(uv) = super::image_upload::source_uv_rect(image) else {
        return Vec::new();
    };
    let min = surface_to_ndc(surface, image.destination.min_x, image.destination.min_y);
    let max = surface_to_ndc(surface, image.destination.max_x, image.destination.max_y);
    let color = [1.0, 1.0, 1.0, 1.0];
    let top_left = TextVertex {
        position: [min[0], min[1]],
        uv: [uv.min_x, uv.min_y],
        color,
    };
    let top_right = TextVertex {
        position: [max[0], min[1]],
        uv: [uv.max_x, uv.min_y],
        color,
    };
    let bottom_right = TextVertex {
        position: [max[0], max[1]],
        uv: [uv.max_x, uv.max_y],
        color,
    };
    let bottom_left = TextVertex {
        position: [min[0], max[1]],
        uv: [uv.min_x, uv.max_y],
        color,
    };
    vec![
        top_left,
        bottom_left,
        bottom_right,
        top_left,
        bottom_right,
        top_right,
    ]
}

fn surface_to_ndc(surface: SurfaceRect, x: f32, y: f32) -> [f32; 2] {
    let width = surface.width().max(1.0);
    let height = surface.height().max(1.0);
    [
        ((x - surface.min_x) / width) * 2.0 - 1.0,
        1.0 - ((y - surface.min_y) / height) * 2.0,
    ]
}

pub(super) fn color_to_float(color: PlanColor) -> [f32; 4] {
    [
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        f32::from(color.a) / 255.0,
    ]
}

pub(super) fn vertex_bytes<T>(vertices: &[T]) -> &[u8] {
    // SAFETY: `BackgroundVertex` and `TextVertex` are `#[repr(C)]` aggregates composed only of
    // `f32` arrays. Reinterpreting their contiguous slice storage as bytes is valid for upload to
    // WGPU, and `u8` has alignment 1 so there is no stricter alignment requirement.
    unsafe {
        std::slice::from_raw_parts(
            vertices.as_ptr().cast::<u8>(),
            std::mem::size_of_val(vertices),
        )
    }
}
