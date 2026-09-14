use crate::terminal_sprite::families::primitives::{
    fill_rect, heavy_line_width, line_width, placeholder_commands, points_from_array,
    points_from_vec, sample_cubic,
};
use crate::terminal_sprite::{SpriteCommand, SpritePoint};
use bootty_terminal::geometry::SurfaceRect;

pub(super) fn commands_for(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    if let Some(dashes) = box_dash_spec(ch) {
        return box_dash_commands(dashes, rect);
    }
    if let Some(lines) = box_line_spec(ch) {
        return box_line_commands(lines, rect);
    }
    if let Some(diagonals) = box_diagonal_spec(ch) {
        return box_diagonal_commands(diagonals, rect);
    }
    if let Some(corner) = box_rounded_corner_spec(ch) {
        return vec![box_rounded_corner_command(corner, rect)];
    }

    placeholder_commands(rect)
}

#[derive(Clone, Copy)]
enum BoxDashAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy)]
struct BoxDashes {
    axis: BoxDashAxis,
    count: u8,
    style: BoxLineStyle,
    desired_gap: BoxLineStyle,
    min_gap: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BoxLineStyle {
    None,
    Light,
    Heavy,
    Double,
}

#[derive(Clone, Copy)]
struct BoxLines {
    up: BoxLineStyle,
    right: BoxLineStyle,
    down: BoxLineStyle,
    left: BoxLineStyle,
}

#[derive(Clone, Copy)]
struct BoxDiagonals {
    upper_left_to_lower_right: bool,
    upper_right_to_lower_left: bool,
}

#[derive(Clone, Copy)]
enum BoxRoundedCorner {
    UpperLeft,
    UpperRight,
    LowerRight,
    LowerLeft,
}

const fn box_dash_spec(ch: char) -> Option<BoxDashes> {
    use BoxDashAxis::{Horizontal as HAxis, Vertical as VAxis};
    use BoxLineStyle::{Heavy as H, Light as L};
    let (axis, count, style, desired_gap, min_gap) = match ch {
        '\u{2504}' => (HAxis, 3, L, L, 4.0),
        '\u{2505}' => (HAxis, 3, H, L, 4.0),
        '\u{2506}' => (VAxis, 3, L, L, 4.0),
        '\u{2507}' => (VAxis, 3, H, L, 4.0),
        '\u{2508}' => (HAxis, 4, L, L, 4.0),
        '\u{2509}' => (HAxis, 4, H, L, 4.0),
        '\u{250A}' => (VAxis, 4, L, L, 4.0),
        '\u{250B}' => (VAxis, 4, H, L, 4.0),
        '\u{254C}' => (HAxis, 2, L, L, 0.0),
        '\u{254D}' => (HAxis, 2, H, L, 0.0),
        '\u{254E}' => (VAxis, 2, L, H, 0.0),
        '\u{254F}' => (VAxis, 2, H, H, 0.0),
        _ => return None,
    };
    Some(BoxDashes {
        axis,
        count,
        style,
        desired_gap,
        min_gap,
    })
}

const fn box_line_spec(ch: char) -> Option<BoxLines> {
    use BoxLineStyle::{Heavy as H, Light as L, None as N};
    let lines = match ch {
        '\u{2500}' => (N, L, N, L),
        '\u{2501}' => (N, H, N, H),
        '\u{2502}' => (L, N, L, N),
        '\u{2503}' => (H, N, H, N),
        '\u{250C}' => (N, L, L, N),
        '\u{250D}' => (N, H, L, N),
        '\u{250E}' => (N, L, H, N),
        '\u{250F}' => (N, H, H, N),
        '\u{2510}' => (N, N, L, L),
        '\u{2511}' => (N, N, L, H),
        '\u{2512}' => (N, N, H, L),
        '\u{2513}' => (N, N, H, H),
        '\u{2514}' => (L, L, N, N),
        '\u{2515}' => (L, H, N, N),
        '\u{2516}' => (H, L, N, N),
        '\u{2517}' => (H, H, N, N),
        '\u{2518}' => (L, N, N, L),
        '\u{2519}' => (L, N, N, H),
        '\u{251A}' => (H, N, N, L),
        '\u{251B}' => (H, N, N, H),
        '\u{251C}' => (L, L, L, N),
        '\u{251D}' => (L, H, L, N),
        '\u{251E}' => (H, L, L, N),
        '\u{251F}' => (L, L, H, N),
        '\u{2520}' => (H, L, H, N),
        '\u{2521}' => (H, H, L, N),
        '\u{2522}' => (L, H, H, N),
        '\u{2523}' => (H, H, H, N),
        '\u{2524}' => (L, N, L, L),
        '\u{2525}' => (L, N, L, H),
        '\u{2526}' => (H, N, L, L),
        '\u{2527}' => (L, N, H, L),
        '\u{2528}' => (H, N, H, L),
        '\u{2529}' => (H, N, L, H),
        '\u{252A}' => (L, N, H, H),
        '\u{252B}' => (H, N, H, H),
        '\u{252C}' => (N, L, L, L),
        '\u{252D}' => (N, L, L, H),
        '\u{252E}' => (N, H, L, L),
        '\u{252F}' => (N, H, L, H),
        '\u{2530}' => (N, L, H, L),
        '\u{2531}' => (N, L, H, H),
        '\u{2532}' => (N, H, H, L),
        '\u{2533}' => (N, H, H, H),
        '\u{2534}' => (L, L, N, L),
        '\u{2535}' => (L, L, N, H),
        '\u{2536}' => (L, H, N, L),
        '\u{2537}' => (L, H, N, H),
        '\u{2538}' => (H, L, N, L),
        '\u{2539}' => (H, L, N, H),
        '\u{253A}' => (H, H, N, L),
        '\u{253B}' => (H, H, N, H),
        '\u{253C}' => (L, L, L, L),
        '\u{253D}' => (L, L, L, H),
        '\u{253E}' => (L, H, L, L),
        '\u{253F}' => (L, H, L, H),
        '\u{2540}' => (H, L, L, L),
        '\u{2541}' => (L, L, H, L),
        '\u{2542}' => (H, L, H, L),
        '\u{2543}' => (H, L, L, H),
        '\u{2544}' => (H, H, L, L),
        '\u{2545}' => (L, L, H, H),
        '\u{2546}' => (L, H, H, L),
        '\u{2547}' => (H, H, L, H),
        '\u{2548}' => (L, H, H, H),
        '\u{2549}' => (H, L, H, H),
        '\u{254A}' => (H, H, H, L),
        '\u{254B}' => (H, H, H, H),
        '\u{2574}' => (N, N, N, L),
        '\u{2575}' => (L, N, N, N),
        '\u{2576}' => (N, L, N, N),
        '\u{2577}' => (N, N, L, N),
        '\u{2578}' => (N, N, N, H),
        '\u{2579}' => (H, N, N, N),
        '\u{257A}' => (N, H, N, N),
        '\u{257B}' => (N, N, H, N),
        '\u{257C}' => (N, H, N, L),
        '\u{257D}' => (L, N, H, N),
        '\u{257E}' => (N, L, N, H),
        '\u{257F}' => (H, N, L, N),
        _ => return double_box_line_spec(ch),
    };
    Some(BoxLines {
        up: lines.0,
        right: lines.1,
        down: lines.2,
        left: lines.3,
    })
}

const fn double_box_line_spec(ch: char) -> Option<BoxLines> {
    use BoxLineStyle::{Double as D, Light as L, None as N};
    let lines = match ch {
        '\u{2550}' => (N, D, N, D),
        '\u{2551}' => (D, N, D, N),
        '\u{2552}' => (N, D, L, N),
        '\u{2553}' => (N, L, D, N),
        '\u{2554}' => (N, D, D, N),
        '\u{2555}' => (N, N, L, D),
        '\u{2556}' => (N, N, D, L),
        '\u{2557}' => (N, N, D, D),
        '\u{2558}' => (L, D, N, N),
        '\u{2559}' => (D, L, N, N),
        '\u{255A}' => (D, D, N, N),
        '\u{255B}' => (L, N, N, D),
        '\u{255C}' => (D, N, N, L),
        '\u{255D}' => (D, N, N, D),
        '\u{255E}' => (L, D, L, N),
        '\u{255F}' => (D, L, D, N),
        '\u{2560}' => (D, D, D, N),
        '\u{2561}' => (L, N, L, D),
        '\u{2562}' => (D, N, D, L),
        '\u{2563}' => (D, N, D, D),
        '\u{2564}' => (N, D, L, D),
        '\u{2565}' => (N, L, D, L),
        '\u{2566}' => (N, D, D, D),
        '\u{2567}' => (L, D, N, D),
        '\u{2568}' => (D, L, N, L),
        '\u{2569}' => (D, D, N, D),
        '\u{256A}' => (L, D, L, D),
        '\u{256B}' => (D, L, D, L),
        '\u{256C}' => (D, D, D, D),
        _ => return None,
    };
    Some(BoxLines {
        up: lines.0,
        right: lines.1,
        down: lines.2,
        left: lines.3,
    })
}

const fn box_diagonal_spec(ch: char) -> Option<BoxDiagonals> {
    Some(match ch {
        '\u{2571}' => BoxDiagonals {
            upper_left_to_lower_right: false,
            upper_right_to_lower_left: true,
        },
        '\u{2572}' => BoxDiagonals {
            upper_left_to_lower_right: true,
            upper_right_to_lower_left: false,
        },
        '\u{2573}' => BoxDiagonals {
            upper_left_to_lower_right: true,
            upper_right_to_lower_left: true,
        },
        _ => return None,
    })
}

const fn box_rounded_corner_spec(ch: char) -> Option<BoxRoundedCorner> {
    Some(match ch {
        '\u{256D}' => BoxRoundedCorner::UpperLeft,
        '\u{256E}' => BoxRoundedCorner::UpperRight,
        '\u{256F}' => BoxRoundedCorner::LowerRight,
        '\u{2570}' => BoxRoundedCorner::LowerLeft,
        _ => return None,
    })
}

fn box_dash_commands(dashes: BoxDashes, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let count = f32::from(dashes.count);
    let line_width = box_line_width(dashes.style, rect);
    let (axis_length, start, cross_position) = match dashes.axis {
        BoxDashAxis::Horizontal => (
            rect.width(),
            rect.min_x,
            (rect.height() - line_width).mul_add(0.5, rect.min_y),
        ),
        BoxDashAxis::Vertical => (
            rect.height(),
            rect.min_y,
            (rect.width() - line_width).mul_add(0.5, rect.min_x),
        ),
    };
    let gap = box_line_width(dashes.desired_gap, rect)
        .max(dashes.min_gap)
        .min((axis_length / (2.0 * count)).floor());
    let total_dash_length = f32::mul_add(count, -gap, axis_length);
    let dash_length = (total_dash_length / count).floor();
    let mut extra = total_dash_length % count;
    let mut position = start
        + match dashes.axis {
            BoxDashAxis::Horizontal => (gap / 2.0).floor(),
            BoxDashAxis::Vertical => 0.0,
        };
    let fill_dash = |position, length| match dashes.axis {
        BoxDashAxis::Horizontal => fill_rect(position, cross_position, length, line_width),
        BoxDashAxis::Vertical => fill_rect(cross_position, position, line_width, length),
    };
    let mut commands = Vec::with_capacity(usize::from(dashes.count));

    for _ in 0..dashes.count {
        let mut length = dash_length;
        if extra > 0.0 {
            extra -= 1.0;
            length += 1.0;
        }
        commands.push(fill_dash(position, length));
        position += length + gap;
    }
    commands
}

fn box_diagonal_commands(diagonals: BoxDiagonals, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let slope_x = rect.width().min(rect.height()) / rect.height();
    let slope_y = rect.width().min(rect.height()) / rect.width();
    let mut commands = Vec::with_capacity(2);

    if diagonals.upper_right_to_lower_left {
        commands.push(SpriteCommand::StrokePolyline {
            points: points_from_array([
                SpritePoint::new(
                    0.5f32.mul_add(slope_x, rect.max_x),
                    0.5f32.mul_add(-slope_y, rect.min_y),
                ),
                SpritePoint::new(
                    0.5f32.mul_add(-slope_x, rect.min_x),
                    0.5f32.mul_add(slope_y, rect.max_y),
                ),
            ]),
            width: line_width(rect),
            alpha: 1.0,
        });
    }
    if diagonals.upper_left_to_lower_right {
        commands.push(SpriteCommand::StrokePolyline {
            points: points_from_array([
                SpritePoint::new(
                    0.5f32.mul_add(-slope_x, rect.min_x),
                    0.5f32.mul_add(-slope_y, rect.min_y),
                ),
                SpritePoint::new(
                    0.5f32.mul_add(slope_x, rect.max_x),
                    0.5f32.mul_add(slope_y, rect.max_y),
                ),
            ]),
            width: line_width(rect),
            alpha: 1.0,
        });
    }
    commands
}

fn box_rounded_corner_command(corner: BoxRoundedCorner, rect: SurfaceRect) -> SpriteCommand {
    let thick = line_width(rect);
    let center_x = thick.mul_add(0.5, rect.min_x + ((rect.width() - thick) * 0.5).floor());
    let center_y = thick.mul_add(0.5, rect.min_y + ((rect.height() - thick) * 0.5).floor());
    let radius = rect.width().min(rect.height()) * 0.5;
    let s = 0.25;
    let mut points = Vec::new();
    let (edge_y, y_sign, x_sign) = match corner {
        BoxRoundedCorner::UpperLeft => (rect.max_y, 1.0, 1.0),
        BoxRoundedCorner::UpperRight => (rect.max_y, 1.0, -1.0),
        BoxRoundedCorner::LowerRight => (rect.min_y, -1.0, -1.0),
        BoxRoundedCorner::LowerLeft => (rect.min_y, -1.0, 1.0),
    };
    points.push(SpritePoint::new(center_x, edge_y));
    points.push(SpritePoint::new(
        center_x,
        f32::mul_add(y_sign, radius, center_y),
    ));
    sample_cubic(
        [
            SpritePoint::new(center_x, f32::mul_add(y_sign, radius, center_y)),
            SpritePoint::new(center_x, f32::mul_add(y_sign * s, radius, center_y)),
            SpritePoint::new(f32::mul_add(x_sign * s, radius, center_x), center_y),
            SpritePoint::new(f32::mul_add(x_sign, radius, center_x), center_y),
        ],
        &mut points,
    );

    SpriteCommand::StrokePolyline {
        points: points_from_vec(points),
        width: thick,
        alpha: 1.0,
    }
}

fn box_line_commands(lines: BoxLines, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let geometry = BoxLineGeometry::new(lines, rect);
    let mut commands = Vec::with_capacity(8);
    geometry.draw_up(lines, &mut commands);
    geometry.draw_down(lines, &mut commands);
    geometry.draw_left(lines, &mut commands);
    geometry.draw_right(lines, &mut commands);
    commands
}

struct BoxLineGeometry {
    rect: SurfaceRect,
    light: f32,
    center_x: f32,
    center_y: f32,
    h_light_top: f32,
    h_light_bottom: f32,
    h_double_top: f32,
    v_light_left: f32,
    v_light_right: f32,
    v_double_left: f32,
    up_bottom: f32,
    down_top: f32,
    left_right: f32,
    right_left: f32,
}

impl BoxLineGeometry {
    fn new(lines: BoxLines, rect: SurfaceRect) -> Self {
        let light = line_width(rect);
        let heavy = heavy_line_width(rect);
        let center_x = rect.width().mul_add(0.5, rect.min_x);
        let center_y = rect.height().mul_add(0.5, rect.min_y);
        let h_light_top = light.mul_add(-0.5, center_y);
        let h_light_bottom = light.mul_add(0.5, center_y);
        let h_heavy_top = heavy.mul_add(-0.5, center_y);
        let h_heavy_bottom = heavy.mul_add(0.5, center_y);
        let h_double_top = h_light_top - light;
        let h_double_bottom = h_light_bottom + light;
        let v_light_left = light.mul_add(-0.5, center_x);
        let v_light_right = light.mul_add(0.5, center_x);
        let v_heavy_left = heavy.mul_add(-0.5, center_x);
        let v_heavy_right = heavy.mul_add(0.5, center_x);
        let v_double_left = v_light_left - light;
        let v_double_right = v_light_right + light;
        let horizontal_has_heavy =
            lines.left == BoxLineStyle::Heavy || lines.right == BoxLineStyle::Heavy;
        let horizontal_has_double =
            lines.left == BoxLineStyle::Double || lines.right == BoxLineStyle::Double;
        let horizontal_is_empty =
            lines.left == BoxLineStyle::None && lines.right == BoxLineStyle::None;
        let vertical_has_heavy =
            lines.up == BoxLineStyle::Heavy || lines.down == BoxLineStyle::Heavy;
        let vertical_has_double =
            lines.up == BoxLineStyle::Double || lines.down == BoxLineStyle::Double;
        let vertical_is_empty = lines.up == BoxLineStyle::None && lines.down == BoxLineStyle::None;

        let up_bottom = if horizontal_has_heavy {
            h_heavy_bottom
        } else if lines.left != lines.right || lines.down == lines.up {
            if horizontal_has_double {
                h_double_bottom
            } else {
                h_light_bottom
            }
        } else if horizontal_is_empty {
            h_light_bottom
        } else {
            h_light_top
        };
        let down_top = if horizontal_has_heavy {
            h_heavy_top
        } else if lines.left != lines.right || lines.up == lines.down {
            if horizontal_has_double {
                h_double_top
            } else {
                h_light_top
            }
        } else if horizontal_is_empty {
            h_light_top
        } else {
            h_light_bottom
        };
        let left_right = if vertical_has_heavy {
            v_heavy_right
        } else if lines.up != lines.down || lines.left == lines.right {
            if vertical_has_double {
                v_double_right
            } else {
                v_light_right
            }
        } else if vertical_is_empty {
            v_light_right
        } else {
            v_light_left
        };
        let right_left = if vertical_has_heavy {
            v_heavy_left
        } else if lines.up != lines.down || lines.right == lines.left {
            if vertical_has_double {
                v_double_left
            } else {
                v_light_left
            }
        } else if vertical_is_empty {
            v_light_left
        } else {
            v_light_right
        };

        Self {
            rect,
            light,
            center_x,
            center_y,
            h_light_top,
            h_light_bottom,
            h_double_top,
            v_light_left,
            v_light_right,
            v_double_left,
            up_bottom,
            down_top,
            left_right,
            right_left,
        }
    }
    fn draw_up(&self, lines: BoxLines, commands: &mut Vec<SpriteCommand>) {
        let &Self {
            rect,
            light,
            center_x,
            h_light_top,
            v_light_right,
            v_double_left,
            up_bottom,
            ..
        } = self;
        match lines.up {
            BoxLineStyle::None => {}
            BoxLineStyle::Light | BoxLineStyle::Heavy => {
                let width = box_line_width(lines.up, rect);
                commands.push(fill_rect(
                    width.mul_add(-0.5, center_x),
                    rect.min_y,
                    width,
                    up_bottom - rect.min_y,
                ));
            }
            BoxLineStyle::Double => {
                let left_bottom = if lines.left == BoxLineStyle::Double {
                    h_light_top
                } else {
                    up_bottom
                };
                let right_bottom = if lines.right == BoxLineStyle::Double {
                    h_light_top
                } else {
                    up_bottom
                };
                commands.push(fill_rect(
                    v_double_left,
                    rect.min_y,
                    light,
                    left_bottom - rect.min_y,
                ));
                commands.push(fill_rect(
                    v_light_right,
                    rect.min_y,
                    light,
                    right_bottom - rect.min_y,
                ));
            }
        }
    }
    fn draw_down(&self, lines: BoxLines, commands: &mut Vec<SpriteCommand>) {
        let &Self {
            rect,
            light,
            center_x,
            h_light_bottom,
            v_light_right,
            v_double_left,
            down_top,
            ..
        } = self;
        match lines.down {
            BoxLineStyle::None => {}
            BoxLineStyle::Light | BoxLineStyle::Heavy => {
                let width = box_line_width(lines.down, rect);
                commands.push(fill_rect(
                    width.mul_add(-0.5, center_x),
                    down_top,
                    width,
                    rect.max_y - down_top,
                ));
            }
            BoxLineStyle::Double => {
                let left_top = if lines.left == BoxLineStyle::Double {
                    h_light_bottom
                } else {
                    down_top
                };
                let right_top = if lines.right == BoxLineStyle::Double {
                    h_light_bottom
                } else {
                    down_top
                };
                commands.push(fill_rect(
                    v_double_left,
                    left_top,
                    light,
                    rect.max_y - left_top,
                ));
                commands.push(fill_rect(
                    v_light_right,
                    right_top,
                    light,
                    rect.max_y - right_top,
                ));
            }
        }
    }
    fn draw_left(&self, lines: BoxLines, commands: &mut Vec<SpriteCommand>) {
        let &Self {
            rect,
            light,
            center_y,
            h_light_bottom,
            h_double_top,
            v_light_left,
            left_right,
            ..
        } = self;
        match lines.left {
            BoxLineStyle::None => {}
            BoxLineStyle::Light | BoxLineStyle::Heavy => {
                let width = left_right - rect.min_x;
                let height = box_line_width(lines.left, rect);
                commands.push(fill_rect(
                    rect.min_x,
                    height.mul_add(-0.5, center_y),
                    width,
                    height,
                ));
            }
            BoxLineStyle::Double => {
                let top_right = if lines.up == BoxLineStyle::Double {
                    v_light_left
                } else {
                    left_right
                };
                let bottom_right = if lines.down == BoxLineStyle::Double {
                    v_light_left
                } else {
                    left_right
                };
                commands.push(fill_rect(
                    rect.min_x,
                    h_double_top,
                    top_right - rect.min_x,
                    light,
                ));
                commands.push(fill_rect(
                    rect.min_x,
                    h_light_bottom,
                    bottom_right - rect.min_x,
                    light,
                ));
            }
        }
    }
    fn draw_right(&self, lines: BoxLines, commands: &mut Vec<SpriteCommand>) {
        let &Self {
            rect,
            light,
            center_y,
            h_light_bottom,
            h_double_top,
            v_light_right,
            right_left,
            ..
        } = self;
        match lines.right {
            BoxLineStyle::None => {}
            BoxLineStyle::Light | BoxLineStyle::Heavy => {
                let height = box_line_width(lines.right, rect);
                commands.push(fill_rect(
                    right_left,
                    height.mul_add(-0.5, center_y),
                    rect.max_x - right_left,
                    height,
                ));
            }
            BoxLineStyle::Double => {
                let top_left = if lines.up == BoxLineStyle::Double {
                    v_light_right
                } else {
                    right_left
                };
                let bottom_left = if lines.down == BoxLineStyle::Double {
                    v_light_right
                } else {
                    right_left
                };
                commands.push(fill_rect(
                    top_left,
                    h_double_top,
                    rect.max_x - top_left,
                    light,
                ));
                commands.push(fill_rect(
                    bottom_left,
                    h_light_bottom,
                    rect.max_x - bottom_left,
                    light,
                ));
            }
        }
    }
}

fn box_line_width(style: BoxLineStyle, rect: SurfaceRect) -> f32 {
    match style {
        BoxLineStyle::None => 0.0,
        BoxLineStyle::Light | BoxLineStyle::Double => line_width(rect),
        BoxLineStyle::Heavy => heavy_line_width(rect),
    }
}
