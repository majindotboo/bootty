use crate::terminal_sprite::families::primitives::{
    clear_stroke_segment, fill_rect, filled_polygon, filled_triangle, heavy_line_width,
    left_bottom, left_top, line_width, placeholder_commands, points_from_array, points_from_vec,
    right_bottom, right_top, sixel_grid_commands, stroke_polyline, stroke_segment,
};
use crate::terminal_sprite::{SpriteCommand, SpritePoint, SpriteShape};
use bootty_terminal::geometry::SurfaceRect;
use num_traits::ToPrimitive as _;

#[derive(Clone, Copy)]
enum EighthAxis {
    Columns,
    Rows,
}

pub(super) fn commands_for(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match u32::from(ch) {
        0x1FB00..=0x1FB3B => sextant_commands(ch, rect),
        0x1FB3C..=0x1FB67 => smooth_mosaic_commands(ch, rect),
        0x1FB68..=0x1FB6F | 0x1FB9A..=0x1FB9B => legacy_edge_triangle_commands(ch, rect),
        0x1FB70..=0x1FB97 => legacy_block_extension_commands(ch, rect),
        0x1FB98..=0x1FB99 => legacy_hatch_commands(ch, rect),
        0x1FB9C..=0x1FB9F => legacy_corner_triangle_shade_commands(ch, rect),
        0x1FBA0..=0x1FBAE => legacy_corner_diagonal_commands(ch, rect),
        0x1FBAF => legacy_mixed_box_connector_commands(rect),
        0x1FBBD..=0x1FBBF => legacy_inverse_diagonal_commands(ch, rect),
        0x1FBCE..=0x1FBCF | 0x1FBE4..=0x1FBE7 => legacy_fractional_block_commands(ch, rect),
        0x1FBD0..=0x1FBDF => legacy_cell_diagonal_commands(ch, rect),
        0x1FBE0..=0x1FBEF => legacy_circle_commands(ch, rect),
        _ => placeholder_commands(rect),
    }
}

fn smooth_mosaic_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let Some(pattern) = u32::from(ch)
        .checked_sub(0x1FB3C)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| SMOOTH_MOSAIC_PATTERNS.get(index))
    else {
        return placeholder_commands(rect);
    };
    let mosaic = SmoothMosaic::from_pattern(pattern);
    let points = mosaic_polygon_points(mosaic, rect);
    if points.len() < 3 {
        Vec::new()
    } else {
        vec![filled_polygon(points)]
    }
}

fn legacy_block_extension_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    use EighthAxis::{Columns as C, Rows as R};

    let cp = u32::from(ch);
    if (0x1FB70..=0x1FB75).contains(&cp) {
        let Some(slot) = cp
            .checked_sub(0x1FB6F)
            .and_then(|slot| u8::try_from(slot).ok())
        else {
            return placeholder_commands(rect);
        };
        return vec![eighths(rect, C, slot, slot.saturating_add(1), 1.0)];
    }
    if (0x1FB76..=0x1FB7B).contains(&cp) {
        let Some(slot) = cp
            .checked_sub(0x1FB75)
            .and_then(|slot| u8::try_from(slot).ok())
        else {
            return placeholder_commands(rect);
        };
        return vec![eighths(rect, R, slot, slot.saturating_add(1), 1.0)];
    }

    match cp {
        0x1FB7C => vec![eighths(rect, C, 0, 1, 1.0), eighths(rect, R, 7, 8, 1.0)],
        0x1FB7D => vec![eighths(rect, C, 0, 1, 1.0), eighths(rect, R, 0, 1, 1.0)],
        0x1FB7E => vec![eighths(rect, C, 7, 8, 1.0), eighths(rect, R, 0, 1, 1.0)],
        0x1FB7F => vec![eighths(rect, C, 7, 8, 1.0), eighths(rect, R, 7, 8, 1.0)],
        0x1FB80 => vec![eighths(rect, R, 0, 1, 1.0), eighths(rect, R, 7, 8, 1.0)],
        0x1FB81 => vec![
            eighths(rect, R, 0, 1, 1.0),
            eighths(rect, R, 2, 3, 1.0),
            eighths(rect, R, 4, 5, 1.0),
            eighths(rect, R, 7, 8, 1.0),
        ],
        0x1FB82 => vec![eighths(rect, R, 0, 2, 1.0)],
        0x1FB83 => vec![eighths(rect, R, 0, 3, 1.0)],
        0x1FB84 => vec![eighths(rect, R, 0, 5, 1.0)],
        0x1FB85 => vec![eighths(rect, R, 0, 6, 1.0)],
        0x1FB86 => vec![eighths(rect, R, 0, 7, 1.0)],
        0x1FB87 => vec![eighths(rect, C, 6, 8, 1.0)],
        0x1FB88 => vec![eighths(rect, C, 5, 8, 1.0)],
        0x1FB89 => vec![eighths(rect, C, 3, 8, 1.0)],
        0x1FB8A => vec![eighths(rect, C, 2, 8, 1.0)],
        0x1FB8B => vec![eighths(rect, C, 1, 8, 1.0)],
        0x1FB8C => vec![eighths(rect, C, 0, 4, 0.5)],
        0x1FB8D => vec![eighths(rect, C, 4, 8, 0.5)],
        0x1FB8E => vec![eighths(rect, R, 0, 4, 0.5)],
        0x1FB8F => vec![eighths(rect, R, 4, 8, 0.5)],
        0x1FB90 => vec![shade_rect(rect, 0.5)],
        0x1FB91 => vec![shade_rect(rect, 0.5), eighths(rect, R, 0, 4, 1.0)],
        0x1FB92 => vec![shade_rect(rect, 0.5), eighths(rect, R, 4, 8, 1.0)],
        0x1FB93 => Vec::new(),
        0x1FB94 => vec![shade_rect(rect, 0.5), eighths(rect, C, 4, 8, 1.0)],
        0x1FB95 => checkerboard_commands(rect, 0),
        0x1FB96 => checkerboard_commands(rect, 1),
        0x1FB97 => vec![eighths(rect, R, 2, 4, 1.0), eighths(rect, R, 6, 8, 1.0)],
        _ => placeholder_commands(rect),
    }
}

fn legacy_hatch_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let Some(line_count) = (rect.width() / (2.0 * line_width(rect)))
        .floor()
        .max(1.0)
        .to_i32()
    else {
        return placeholder_commands(rect);
    };
    let stride = (rect.width() / line_count.to_f32().unwrap_or(1.0)).round();
    (line_count.saturating_neg()..=line_count)
        .map(|i| clipped_hatch_line(rect, i.to_f32().unwrap_or(0.0) * stride, ch == '\u{1FB99}'))
        .collect()
}

fn clipped_hatch_line(rect: SurfaceRect, offset: f32, descending: bool) -> SpriteCommand {
    let w = rect.width();
    let h = rect.height();
    let mut points = Vec::new();
    let add_unique = |points: &mut Vec<SpritePoint>, x: f32, y: f32| {
        let point = SpritePoint::new(x, y);
        if !points.iter().any(|existing| {
            (existing.x - point.x).abs() < 0.001 && (existing.y - point.y).abs() < 0.001
        }) {
            points.push(point);
        }
    };

    let (top_x, bottom_x, left_y, right_y) = if descending {
        (w + offset, offset, h * (w + offset) / w, h * offset / w)
    } else {
        (offset, w + offset, -offset * h / w, (w - offset) * h / w)
    };
    if (0.0..=w).contains(&top_x) {
        add_unique(&mut points, rect.min_x + top_x, rect.min_y);
    }
    if (0.0..=w).contains(&bottom_x) {
        add_unique(&mut points, rect.min_x + bottom_x, rect.max_y);
    }
    if (0.0..=h).contains(&left_y) {
        add_unique(&mut points, rect.min_x, rect.min_y + left_y);
    }
    if (0.0..=h).contains(&right_y) {
        add_unique(&mut points, rect.max_x, rect.min_y + right_y);
    }

    stroke_polyline(points, rect)
}

fn legacy_mixed_box_connector_commands(rect: SurfaceRect) -> Vec<SpriteCommand> {
    let light = line_width(rect);
    let heavy = heavy_line_width(rect);
    let h_light_top = rect.min_y + ((rect.height() - light) / 2.0).floor();
    let h_light_bottom = h_light_top + light;
    let v_heavy_left = rect.min_x + ((rect.width() - heavy) / 2.0).floor();

    vec![
        fill_rect(v_heavy_left, rect.min_y, heavy, h_light_bottom - rect.min_y),
        fill_rect(v_heavy_left, h_light_top, heavy, rect.max_y - h_light_top),
        fill_rect(rect.min_x, h_light_top, rect.width(), light),
    ]
}

fn legacy_inverse_diagonal_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let mut commands = vec![SpriteCommand::FillRect { rect, alpha: 1.0 }];
    match u32::from(ch) {
        0x1FBBD => commands.extend(light_diagonal_cross_clear_commands(rect)),
        0x1FBBE => {
            let (from, to) = legacy_corner_diagonal_segment(LegacyCorner::LowerRight, rect);
            commands.push(clear_stroke_segment(from, to, rect));
        }
        0x1FBBF => {
            commands.extend(
                [
                    LegacyCorner::UpperLeft,
                    LegacyCorner::UpperRight,
                    LegacyCorner::LowerLeft,
                    LegacyCorner::LowerRight,
                ]
                .into_iter()
                .map(|corner| {
                    let (from, to) = legacy_corner_diagonal_segment(corner, rect);
                    clear_stroke_segment(from, to, rect)
                }),
            );
        }
        _ => return placeholder_commands(rect),
    }
    commands
}

fn light_diagonal_cross_clear_commands(rect: SurfaceRect) -> Vec<SpriteCommand> {
    let slope_x = rect.width().min(rect.height()) / rect.height().max(1.0);
    let slope_y = rect.height().min(rect.width()) / rect.width().max(1.0);
    vec![
        clear_stroke_segment(
            SpritePoint::new(
                0.5f32.mul_add(slope_x, rect.max_x),
                0.5f32.mul_add(-slope_y, rect.min_y),
            ),
            SpritePoint::new(
                0.5f32.mul_add(-slope_x, rect.min_x),
                0.5f32.mul_add(slope_y, rect.max_y),
            ),
            rect,
        ),
        clear_stroke_segment(
            SpritePoint::new(
                0.5f32.mul_add(-slope_x, rect.min_x),
                0.5f32.mul_add(-slope_y, rect.min_y),
            ),
            SpritePoint::new(
                0.5f32.mul_add(slope_x, rect.max_x),
                0.5f32.mul_add(slope_y, rect.max_y),
            ),
            rect,
        ),
    ]
}

fn legacy_circle_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match u32::from(ch) {
        0x1FBE0 => vec![circle_arc_command(rect, LegacyCirclePosition::Top)],
        0x1FBE1 => vec![circle_arc_command(rect, LegacyCirclePosition::Right)],
        0x1FBE2 => vec![circle_arc_command(rect, LegacyCirclePosition::Bottom)],
        0x1FBE3 => vec![circle_arc_command(rect, LegacyCirclePosition::Left)],
        0x1FBE8 => vec![filled_circle_sector(rect, LegacyCirclePosition::Top)],
        0x1FBE9 => vec![filled_circle_sector(rect, LegacyCirclePosition::Right)],
        0x1FBEA => vec![filled_circle_sector(rect, LegacyCirclePosition::Bottom)],
        0x1FBEB => vec![filled_circle_sector(rect, LegacyCirclePosition::Left)],
        0x1FBEC => vec![filled_circle_sector(rect, LegacyCirclePosition::TopRight)],
        0x1FBED => vec![filled_circle_sector(rect, LegacyCirclePosition::BottomLeft)],
        0x1FBEE => vec![filled_circle_sector(
            rect,
            LegacyCirclePosition::BottomRight,
        )],
        0x1FBEF => vec![filled_circle_sector(rect, LegacyCirclePosition::TopLeft)],
        _ => placeholder_commands(rect),
    }
}

fn legacy_edge_triangle_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match u32::from(ch) {
        0x1FB68 => inverted_edge_triangle_commands(rect, LegacyEdge::Left),
        0x1FB69 => inverted_edge_triangle_commands(rect, LegacyEdge::Top),
        0x1FB6A => inverted_edge_triangle_commands(rect, LegacyEdge::Right),
        0x1FB6B => inverted_edge_triangle_commands(rect, LegacyEdge::Bottom),
        0x1FB6C => vec![edge_triangle_command(rect, LegacyEdge::Left)],
        0x1FB6D => vec![edge_triangle_command(rect, LegacyEdge::Top)],
        0x1FB6E => vec![edge_triangle_command(rect, LegacyEdge::Right)],
        0x1FB6F => vec![edge_triangle_command(rect, LegacyEdge::Bottom)],
        0x1FB9A => vec![
            edge_triangle_command(rect, LegacyEdge::Top),
            edge_triangle_command(rect, LegacyEdge::Bottom),
        ],
        0x1FB9B => vec![
            edge_triangle_command(rect, LegacyEdge::Left),
            edge_triangle_command(rect, LegacyEdge::Right),
        ],
        _ => placeholder_commands(rect),
    }
}

fn legacy_corner_triangle_shade_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let points = match u32::from(ch) {
        0x1FB9C => [left_top(rect), right_top(rect), left_bottom(rect)],
        0x1FB9D => [left_top(rect), right_top(rect), right_bottom(rect)],
        0x1FB9E => [right_top(rect), right_bottom(rect), left_bottom(rect)],
        0x1FB9F => [left_top(rect), left_bottom(rect), right_bottom(rect)],
        _ => return placeholder_commands(rect),
    };
    vec![SpriteCommand::FillPolygon {
        shape: SpriteShape::Triangle,
        points: points_from_array(points),
        alpha: 0.5,
    }]
}

fn legacy_fractional_block_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match u32::from(ch) {
        0x1FBCE => vec![fill_fractional_rect(rect, 0.0, 0.0, 2.0 / 3.0, 1.0)],
        0x1FBCF => vec![fill_fractional_rect(rect, 0.0, 0.0, 1.0 / 3.0, 1.0)],
        0x1FBE4 => vec![fill_fractional_rect(rect, 0.25, 0.0, 0.5, 0.5)],
        0x1FBE5 => vec![fill_fractional_rect(rect, 0.25, 0.5, 0.5, 0.5)],
        0x1FBE6 => vec![fill_fractional_rect(rect, 0.0, 0.25, 0.5, 0.5)],
        0x1FBE7 => vec![fill_fractional_rect(rect, 0.5, 0.25, 0.5, 0.5)],
        _ => placeholder_commands(rect),
    }
}

#[derive(Clone, Copy)]
pub(super) enum LegacyCirclePosition {
    Top,
    Right,
    Bottom,
    Left,
    TopRight,
    BottomLeft,
    BottomRight,
    TopLeft,
}

pub(super) fn circle_arc_command(
    rect: SurfaceRect,
    position: LegacyCirclePosition,
) -> SpriteCommand {
    SpriteCommand::StrokePolyline {
        points: points_from_vec(circle_arc_points(rect, position)),
        width: line_width(rect),
        alpha: 1.0,
    }
}

fn filled_circle_sector(rect: SurfaceRect, position: LegacyCirclePosition) -> SpriteCommand {
    let mut points = vec![circle_center(rect, position)];
    points.extend(circle_arc_points(rect, position));
    filled_polygon(points)
}

fn circle_arc_points(rect: SurfaceRect, position: LegacyCirclePosition) -> Vec<SpritePoint> {
    let (start, end) = circle_angles(position);
    let center = circle_center(rect, position);
    let radius = rect.width().min(rect.height()) * 0.5;
    let steps: u8 = if (end - start).abs() > std::f32::consts::FRAC_PI_2 {
        8
    } else {
        4
    };

    (0..=steps)
        .map(|step| {
            let t = f32::from(step) / f32::from(steps);
            let angle = f32::mul_add(end - start, t, start);
            SpritePoint::new(
                radius.mul_add(angle.cos(), center.x),
                radius.mul_add(angle.sin(), center.y),
            )
        })
        .collect()
}

fn circle_center(rect: SurfaceRect, position: LegacyCirclePosition) -> SpritePoint {
    let x = match position {
        LegacyCirclePosition::Left
        | LegacyCirclePosition::TopLeft
        | LegacyCirclePosition::BottomLeft => rect.min_x,
        LegacyCirclePosition::Right
        | LegacyCirclePosition::TopRight
        | LegacyCirclePosition::BottomRight => rect.max_x,
        LegacyCirclePosition::Top | LegacyCirclePosition::Bottom => {
            rect.width().mul_add(0.5, rect.min_x)
        }
    };
    let y = match position {
        LegacyCirclePosition::Top
        | LegacyCirclePosition::TopLeft
        | LegacyCirclePosition::TopRight => rect.min_y,
        LegacyCirclePosition::Bottom
        | LegacyCirclePosition::BottomLeft
        | LegacyCirclePosition::BottomRight => rect.max_y,
        LegacyCirclePosition::Left | LegacyCirclePosition::Right => {
            rect.height().mul_add(0.5, rect.min_y)
        }
    };
    SpritePoint::new(x, y)
}

fn circle_angles(position: LegacyCirclePosition) -> (f32, f32) {
    let pi = std::f32::consts::PI;
    let half = std::f32::consts::FRAC_PI_2;
    match position {
        LegacyCirclePosition::Top => (0.0, pi),
        LegacyCirclePosition::Right => (half, pi + half),
        LegacyCirclePosition::Bottom => (pi, 2.0 * pi),
        LegacyCirclePosition::Left => (-half, half),
        LegacyCirclePosition::TopRight => (half, pi),
        LegacyCirclePosition::BottomLeft => (-half, 0.0),
        LegacyCirclePosition::BottomRight => (pi, pi + half),
        LegacyCirclePosition::TopLeft => (0.0, half),
    }
}

#[derive(Clone, Copy)]
enum LegacyEdge {
    Top,
    Left,
    Bottom,
    Right,
}

fn edge_triangle_command(rect: SurfaceRect, edge: LegacyEdge) -> SpriteCommand {
    let center = SpritePoint::new(
        rect.width().mul_add(0.5, rect.min_x),
        rect.height().mul_add(0.5, rect.min_y),
    );
    let (a, b) = edge_span(edge, rect);
    filled_triangle([center, a, b])
}

fn inverted_edge_triangle_commands(rect: SurfaceRect, edge: LegacyEdge) -> Vec<SpriteCommand> {
    let center = SpritePoint::new(
        rect.width().mul_add(0.5, rect.min_x),
        rect.height().mul_add(0.5, rect.min_y),
    );
    match edge {
        LegacyEdge::Left => vec![
            filled_triangle([left_top(rect), right_top(rect), center]),
            filled_triangle([center, right_bottom(rect), left_bottom(rect)]),
        ],
        LegacyEdge::Top => vec![
            filled_triangle([left_top(rect), left_bottom(rect), center]),
            filled_triangle([center, right_bottom(rect), right_top(rect)]),
        ],
        LegacyEdge::Right => vec![
            filled_triangle([right_top(rect), left_top(rect), center]),
            filled_triangle([center, left_bottom(rect), right_bottom(rect)]),
        ],
        LegacyEdge::Bottom => vec![
            filled_triangle([left_bottom(rect), left_top(rect), center]),
            filled_triangle([center, right_top(rect), right_bottom(rect)]),
        ],
    }
}

const fn edge_span(edge: LegacyEdge, rect: SurfaceRect) -> (SpritePoint, SpritePoint) {
    match edge {
        LegacyEdge::Top => (right_top(rect), left_top(rect)),
        LegacyEdge::Left => (left_top(rect), left_bottom(rect)),
        LegacyEdge::Bottom => (left_bottom(rect), right_bottom(rect)),
        LegacyEdge::Right => (right_bottom(rect), right_top(rect)),
    }
}

fn legacy_corner_diagonal_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let cp = u32::from(ch);
    let corners: &[LegacyCorner] = match cp {
        0x1FBA0 => &[LegacyCorner::UpperLeft],
        0x1FBA1 => &[LegacyCorner::UpperRight],
        0x1FBA2 => &[LegacyCorner::LowerLeft],
        0x1FBA3 => &[LegacyCorner::LowerRight],
        0x1FBA4 => &[LegacyCorner::UpperLeft, LegacyCorner::LowerLeft],
        0x1FBA5 => &[LegacyCorner::UpperRight, LegacyCorner::LowerRight],
        0x1FBA6 => &[LegacyCorner::LowerLeft, LegacyCorner::LowerRight],
        0x1FBA7 => &[LegacyCorner::UpperLeft, LegacyCorner::UpperRight],
        0x1FBA8 => &[LegacyCorner::UpperLeft, LegacyCorner::LowerRight],
        0x1FBA9 => &[LegacyCorner::UpperRight, LegacyCorner::LowerLeft],
        0x1FBAA => &[
            LegacyCorner::UpperRight,
            LegacyCorner::LowerLeft,
            LegacyCorner::LowerRight,
        ],
        0x1FBAB => &[
            LegacyCorner::UpperLeft,
            LegacyCorner::LowerLeft,
            LegacyCorner::LowerRight,
        ],
        0x1FBAC => &[
            LegacyCorner::UpperLeft,
            LegacyCorner::UpperRight,
            LegacyCorner::LowerRight,
        ],
        0x1FBAD => &[
            LegacyCorner::UpperLeft,
            LegacyCorner::UpperRight,
            LegacyCorner::LowerLeft,
        ],
        0x1FBAE => &[
            LegacyCorner::UpperLeft,
            LegacyCorner::UpperRight,
            LegacyCorner::LowerLeft,
            LegacyCorner::LowerRight,
        ],
        _ => return placeholder_commands(rect),
    };

    corners
        .iter()
        .map(|corner| {
            let (from, to) = legacy_corner_diagonal_segment(*corner, rect);
            stroke_segment(from, to, rect)
        })
        .collect()
}

fn fill_fractional_rect(
    rect: SurfaceRect,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> SpriteCommand {
    fill_rect(
        rect.width().mul_add(x, rect.min_x),
        rect.height().mul_add(y, rect.min_y),
        rect.width() * width,
        rect.height() * height,
    )
}

fn legacy_cell_diagonal_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let cp = u32::from(ch);
    let pairs: &[(LegacyAlignment, LegacyAlignment)] = match cp {
        0x1FBD0 => &[(LegacyAlignment::MiddleRight, LegacyAlignment::LowerLeft)],
        0x1FBD1 => &[(LegacyAlignment::UpperRight, LegacyAlignment::MiddleLeft)],
        0x1FBD2 => &[(LegacyAlignment::UpperLeft, LegacyAlignment::MiddleRight)],
        0x1FBD3 => &[(LegacyAlignment::MiddleLeft, LegacyAlignment::LowerRight)],
        0x1FBD4 => &[(LegacyAlignment::UpperLeft, LegacyAlignment::LowerCenter)],
        0x1FBD5 => &[(LegacyAlignment::UpperCenter, LegacyAlignment::LowerRight)],
        0x1FBD6 => &[(LegacyAlignment::UpperRight, LegacyAlignment::LowerCenter)],
        0x1FBD7 => &[(LegacyAlignment::UpperCenter, LegacyAlignment::LowerLeft)],
        0x1FBD8 => &[
            (LegacyAlignment::UpperLeft, LegacyAlignment::MiddleCenter),
            (LegacyAlignment::MiddleCenter, LegacyAlignment::UpperRight),
        ],
        0x1FBD9 => &[
            (LegacyAlignment::UpperRight, LegacyAlignment::MiddleCenter),
            (LegacyAlignment::MiddleCenter, LegacyAlignment::LowerRight),
        ],
        0x1FBDA => &[
            (LegacyAlignment::LowerLeft, LegacyAlignment::MiddleCenter),
            (LegacyAlignment::MiddleCenter, LegacyAlignment::LowerRight),
        ],
        0x1FBDB => &[
            (LegacyAlignment::UpperLeft, LegacyAlignment::MiddleCenter),
            (LegacyAlignment::MiddleCenter, LegacyAlignment::LowerLeft),
        ],
        0x1FBDC => &[
            (LegacyAlignment::UpperLeft, LegacyAlignment::LowerCenter),
            (LegacyAlignment::LowerCenter, LegacyAlignment::UpperRight),
        ],
        0x1FBDD => &[
            (LegacyAlignment::UpperRight, LegacyAlignment::MiddleLeft),
            (LegacyAlignment::MiddleLeft, LegacyAlignment::LowerRight),
        ],
        0x1FBDE => &[
            (LegacyAlignment::LowerLeft, LegacyAlignment::UpperCenter),
            (LegacyAlignment::UpperCenter, LegacyAlignment::LowerRight),
        ],
        0x1FBDF => &[
            (LegacyAlignment::UpperLeft, LegacyAlignment::MiddleRight),
            (LegacyAlignment::MiddleRight, LegacyAlignment::LowerLeft),
        ],
        _ => return placeholder_commands(rect),
    };

    pairs
        .iter()
        .map(|(from, to)| {
            stroke_polyline(
                vec![
                    legacy_alignment_point(*from, rect),
                    legacy_alignment_point(*to, rect),
                ],
                rect,
            )
        })
        .collect()
}

#[derive(Clone, Copy)]
pub(super) enum LegacyCorner {
    UpperLeft,
    UpperRight,
    LowerLeft,
    LowerRight,
}

fn legacy_corner_diagonal_segment(
    corner: LegacyCorner,
    rect: SurfaceRect,
) -> (SpritePoint, SpritePoint) {
    let center_x = rect.width().mul_add(0.5, rect.min_x);
    let center_y = rect.height().mul_add(0.5, rect.min_y);
    let edge_y = match corner {
        LegacyCorner::UpperLeft | LegacyCorner::UpperRight => rect.min_y,
        LegacyCorner::LowerLeft | LegacyCorner::LowerRight => rect.max_y,
    };
    let edge_x = match corner {
        LegacyCorner::UpperLeft | LegacyCorner::LowerLeft => rect.min_x,
        LegacyCorner::UpperRight | LegacyCorner::LowerRight => rect.max_x,
    };
    (
        SpritePoint::new(center_x, edge_y),
        SpritePoint::new(edge_x, center_y),
    )
}

#[derive(Clone, Copy)]
enum LegacyAlignment {
    UpperLeft,
    UpperCenter,
    UpperRight,
    MiddleLeft,
    MiddleCenter,
    MiddleRight,
    LowerLeft,
    LowerCenter,
    LowerRight,
}

fn legacy_alignment_point(alignment: LegacyAlignment, rect: SurfaceRect) -> SpritePoint {
    let x = match alignment {
        LegacyAlignment::UpperLeft | LegacyAlignment::MiddleLeft | LegacyAlignment::LowerLeft => {
            rect.min_x
        }
        LegacyAlignment::UpperCenter
        | LegacyAlignment::MiddleCenter
        | LegacyAlignment::LowerCenter => rect.width().mul_add(0.5, rect.min_x),
        LegacyAlignment::UpperRight
        | LegacyAlignment::MiddleRight
        | LegacyAlignment::LowerRight => rect.max_x,
    };
    let y = match alignment {
        LegacyAlignment::UpperLeft | LegacyAlignment::UpperCenter | LegacyAlignment::UpperRight => {
            rect.min_y
        }
        LegacyAlignment::MiddleLeft
        | LegacyAlignment::MiddleCenter
        | LegacyAlignment::MiddleRight => rect.height().mul_add(0.5, rect.min_y),
        LegacyAlignment::LowerLeft | LegacyAlignment::LowerCenter | LegacyAlignment::LowerRight => {
            rect.max_y
        }
    };
    SpritePoint::new(x, y)
}

#[derive(Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Mosaic edge bits are independent parts of the Unicode glyph."
)]
struct SmoothMosaic {
    tl: bool,
    ul: bool,
    ll: bool,
    bl: bool,
    bc: bool,
    br: bool,
    lr: bool,
    ur: bool,
    tr: bool,
    tc: bool,
}

impl SmoothMosaic {
    const fn from_pattern(pattern: &[u8; 12]) -> Self {
        let [tl, tc, tr, ul, _, ur, ll, _, lr, bl, bc, br] = *pattern;
        Self {
            tl: tl == b'#',
            ul: ul == b'#' && (tl != b'#' || ll != b'#'),
            ll: ll == b'#' && (ul != b'#' || bl != b'#'),
            bl: bl == b'#',
            bc: bc == b'#' && (bl != b'#' || br != b'#'),
            br: br == b'#',
            lr: lr == b'#' && (br != b'#' || ur != b'#'),
            ur: ur == b'#' && (lr != b'#' || tr != b'#'),
            tr: tr == b'#',
            tc: tc == b'#' && (tr != b'#' || tl != b'#'),
        }
    }
}

fn mosaic_polygon_points(mosaic: SmoothMosaic, rect: SurfaceRect) -> Vec<SpritePoint> {
    let upper = rect.min_y + rect.height() / 3.0;
    let lower = rect.min_y + rect.height() * 2.0 / 3.0;
    let center = rect.width().mul_add(0.5, rect.min_x);
    let mut points = Vec::new();

    if mosaic.tl {
        points.push(SpritePoint::new(rect.min_x, rect.min_y));
    }
    if mosaic.ul {
        points.push(SpritePoint::new(rect.min_x, upper));
    }
    if mosaic.ll {
        points.push(SpritePoint::new(rect.min_x, lower));
    }
    if mosaic.bl {
        points.push(SpritePoint::new(rect.min_x, rect.max_y));
    }
    if mosaic.bc {
        points.push(SpritePoint::new(center, rect.max_y));
    }
    if mosaic.br {
        points.push(SpritePoint::new(rect.max_x, rect.max_y));
    }
    if mosaic.lr {
        points.push(SpritePoint::new(rect.max_x, lower));
    }
    if mosaic.ur {
        points.push(SpritePoint::new(rect.max_x, upper));
    }
    if mosaic.tr {
        points.push(SpritePoint::new(rect.max_x, rect.min_y));
    }
    if mosaic.tc {
        points.push(SpritePoint::new(center, rect.min_y));
    }

    points
}

fn sextant_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let cp = u32::from(ch);
    let Some(idx) = cp
        .checked_sub(0x1FB00)
        .and_then(|idx| u8::try_from(idx).ok())
    else {
        return placeholder_commands(rect);
    };
    let pattern = idx.saturating_add(idx / 0x14).saturating_add(1);
    sixel_grid_commands(pattern, rect, 3, 2)
}
fn eighths(rect: SurfaceRect, axis: EighthAxis, start: u8, end: u8, alpha: f32) -> SpriteCommand {
    let (x, y, width, height) = match axis {
        EighthAxis::Columns => {
            let eighth = rect.width() / 8.0;
            (
                f32::mul_add(f32::from(start), eighth, rect.min_x),
                rect.min_y,
                f32::from(end.saturating_sub(start)) * eighth,
                rect.height(),
            )
        }
        EighthAxis::Rows => {
            let eighth = rect.height() / 8.0;
            (
                rect.min_x,
                f32::mul_add(f32::from(start), eighth, rect.min_y),
                rect.width(),
                f32::from(end.saturating_sub(start)) * eighth,
            )
        }
    };
    SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(x, y, width, height),
        alpha,
    }
}

const fn shade_rect(rect: SurfaceRect, alpha: f32) -> SpriteCommand {
    SpriteCommand::FillRect { rect, alpha }
}

fn checkerboard_commands(rect: SurfaceRect, parity: usize) -> Vec<SpriteCommand> {
    let x_cells = 4_usize;
    let Some(y_cells) = (4.0 * (rect.height() / rect.width()))
        .round()
        .max(1.0)
        .to_usize()
    else {
        return placeholder_commands(rect);
    };
    let Some(capacity) = x_cells.checked_mul(y_cells) else {
        return placeholder_commands(rect);
    };
    let cell_width = rect.width() / 4.0;
    let cell_height = rect.height() / y_cells.to_f32().unwrap_or(1.0);
    let mut commands = Vec::with_capacity(capacity);

    for x in 0..x_cells {
        for y in 0..y_cells {
            if (x % 2) ^ (y % 2) != parity {
                continue;
            }
            commands.push(fill_rect(
                x.to_f32().unwrap_or(0.0).mul_add(cell_width, rect.min_x),
                y.to_f32().unwrap_or(0.0).mul_add(cell_height, rect.min_y),
                cell_width,
                cell_height,
            ));
        }
    }

    commands
}

const SMOOTH_MOSAIC_PATTERNS: [&[u8; 12]; 44] = [
    b"......#..##.",
    b"......#\\.###",
    b"...#..#\\.##.",
    b"...#..##.###",
    b"#..#..##.##.",
    b"/###########",
    b"./##########",
    b".##.########",
    b"..#.########",
    b".##.##.#####",
    b"..../#######",
    b"........#.##",
    b"......./####",
    b".....#./#.##",
    b".....#.#####",
    b"..#..#.##.##",
    b"##\\#########",
    b"#\\.#########",
    b"##.##.######",
    b"#..##.######",
    b"##.##.##.###",
    b"...#\\.######",
    b"#########\\##",
    b"#########.\\#",
    b"######.##.##",
    b"######.##..#",
    b"###.##.##.##",
    b"##.#........",
    b"####/.......",
    b"##.#/.#.....",
    b"#####.#.....",
    b"##.##.#..#..",
    b"#######/....",
    b"###########/",
    b"##########/.",
    b"########.##.",
    b"########.#..",
    b"#####.##.##.",
    b".##..#......",
    b"###.\\#......",
    b".##.\\#..#...",
    b"###.##..#...",
    b".##.##..#..#",
    b"######.\\#...",
];
