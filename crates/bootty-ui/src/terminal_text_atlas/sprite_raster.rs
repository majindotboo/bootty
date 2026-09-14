use crate::terminal_sprite::SpriteCommand;
use bootty_terminal::geometry::SurfaceRect;
use num_traits::ToPrimitive as _;

pub(super) fn rasterize_sprite_commands(
    commands: &[SpriteCommand],
    rect: SurfaceRect,
    width: u16,
    height: u16,
) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut alpha = vec![0; usize::from(width).saturating_mul(usize::from(height))];
    for command in commands {
        match command {
            SpriteCommand::FillRect {
                rect: fill,
                alpha: coverage,
            } => {
                fill_mask_rect(&mut alpha, rect, *fill, width, height, *coverage);
            }
            SpriteCommand::FillPolygon {
                points,
                alpha: coverage,
                ..
            } => {
                fill_mask_polygon(&mut alpha, rect, points, width, height, *coverage);
            }
            SpriteCommand::StrokePolyline {
                points,
                width: stroke_width,
                alpha: coverage,
            } => {
                for &segment in points.array_windows::<2>() {
                    paint_mask_stroke_segment(
                        &mut alpha,
                        rect,
                        segment,
                        *stroke_width,
                        (width, height),
                        *coverage,
                        u8::max,
                    );
                }
            }
            SpriteCommand::ClearStrokePolyline {
                points,
                width: stroke_width,
                alpha: coverage,
            } => {
                for &segment in points.array_windows::<2>() {
                    paint_mask_stroke_segment(
                        &mut alpha,
                        rect,
                        segment,
                        *stroke_width,
                        (width, height),
                        1.0 - coverage.clamp(0.0, 1.0),
                        u8::min,
                    );
                }
            }
        }
    }
    alpha
}

fn fill_mask_rect(
    pixels: &mut [u8],
    cell: SurfaceRect,
    fill: SurfaceRect,
    width: u16,
    height: u16,
    coverage: f32,
) {
    let min_x = (((fill.min_x - cell.min_x) / cell.width().max(1.0)) * f32::from(width))
        .floor()
        .clamp(0.0, f32::from(width))
        .to_usize()
        .unwrap_or_default();
    let max_x = (((fill.max_x - cell.min_x) / cell.width().max(1.0)) * f32::from(width))
        .ceil()
        .clamp(0.0, f32::from(width))
        .to_usize()
        .unwrap_or_default();
    let min_y = (((fill.min_y - cell.min_y) / cell.height().max(1.0)) * f32::from(height))
        .floor()
        .clamp(0.0, f32::from(height))
        .to_usize()
        .unwrap_or_default();
    let max_y = (((fill.max_y - cell.min_y) / cell.height().max(1.0)) * f32::from(height))
        .ceil()
        .clamp(0.0, f32::from(height))
        .to_usize()
        .unwrap_or_default();
    let value = (coverage.clamp(0.0, 1.0) * 255.0)
        .round()
        .to_u8()
        .unwrap_or_default();
    for row in pixels
        .chunks_exact_mut(usize::from(width))
        .take(max_y)
        .skip(min_y)
    {
        for dst in row.iter_mut().take(max_x).skip(min_x) {
            *dst = (*dst).max(value);
        }
    }
}

fn fill_mask_polygon(
    pixels: &mut [u8],
    cell: SurfaceRect,
    points: &[crate::terminal_sprite::SpritePoint],
    width: u16,
    height: u16,
    coverage: f32,
) {
    if points.len() < 3 {
        return;
    }
    let value = (coverage.clamp(0.0, 1.0) * 255.0)
        .round()
        .to_u8()
        .unwrap_or_default();
    for (y, row) in (0..height).zip(pixels.chunks_exact_mut(usize::from(width))) {
        for (x, dst) in (0..width).zip(row) {
            let px = ((f32::from(x) + 0.5) / f32::from(width)).mul_add(cell.width(), cell.min_x);
            let py = ((f32::from(y) + 0.5) / f32::from(height)).mul_add(cell.height(), cell.min_y);
            if point_in_polygon(px, py, points) {
                *dst = (*dst).max(value);
            }
        }
    }
}

fn paint_mask_stroke_segment(
    pixels: &mut [u8],
    cell: SurfaceRect,
    [start, end]: [crate::terminal_sprite::SpritePoint; 2],
    stroke_width: f32,
    size: (u16, u16),
    coverage: f32,
    blend: fn(u8, u8) -> u8,
) {
    let (width, height) = size;
    let value = (coverage.clamp(0.0, 1.0) * 255.0)
        .round()
        .to_u8()
        .unwrap_or_default();
    for (y, row) in (0..height).zip(pixels.chunks_exact_mut(usize::from(width))) {
        for (x, dst) in (0..width).zip(row) {
            let px = ((f32::from(x) + 0.5) / f32::from(width)).mul_add(cell.width(), cell.min_x);
            let py = ((f32::from(y) + 0.5) / f32::from(height)).mul_add(cell.height(), cell.min_y);
            if distance_to_segment(px, py, start, end) <= stroke_width * 0.5 {
                *dst = blend(*dst, value);
            }
        }
    }
}

fn point_in_polygon(x: f32, y: f32, points: &[crate::terminal_sprite::SpritePoint]) -> bool {
    let mut inside = false;
    let Some(mut previous_point) = points.last() else {
        return false;
    };
    for current_point in points {
        if ((current_point.y > y) != (previous_point.y > y))
            && (x
                < (previous_point.x - current_point.x) * (y - current_point.y)
                    / (previous_point.y - current_point.y)
                    + current_point.x)
        {
            inside = !inside;
        }
        previous_point = current_point;
    }
    inside
}

fn distance_to_segment(
    x: f32,
    y: f32,
    start: crate::terminal_sprite::SpritePoint,
    end: crate::terminal_sprite::SpritePoint,
) -> f32 {
    let vx = end.x - start.x;
    let vy = end.y - start.y;
    let wx = x - start.x;
    let wy = y - start.y;
    let len_squared = vy.mul_add(vy, vx * vx);
    if len_squared <= f32::EPSILON {
        return (x - start.x).hypot(y - start.y);
    }
    let t = (wy.mul_add(vy, wx * vx) / len_squared).clamp(0.0, 1.0);
    let proj_x = t.mul_add(vx, start.x);
    let proj_y = t.mul_add(vy, start.y);
    (x - proj_x).hypot(y - proj_y)
}
