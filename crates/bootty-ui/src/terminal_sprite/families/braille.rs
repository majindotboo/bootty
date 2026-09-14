use crate::terminal_sprite::SpriteCommand;
use crate::terminal_sprite::families::primitives::{fill_rect, placeholder_commands};
use bootty_terminal::geometry::SurfaceRect;

pub(super) fn commands_for(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let Some(dots) = u32::from(ch)
        .checked_sub(0x2800)
        .filter(|dots| *dots <= 0xff)
    else {
        return placeholder_commands(rect);
    };
    if dots == 0 {
        return Vec::new();
    }
    let mut commands = Vec::with_capacity(8);
    let layout = braille_dot_layout(rect);
    let [left, right] = layout.x;
    let [first, second, third, fourth] = layout.y;
    let positions = [
        (0x01, left, first),
        (0x02, left, second),
        (0x04, left, third),
        (0x08, right, first),
        (0x10, right, second),
        (0x20, right, third),
        (0x40, left, fourth),
        (0x80, right, fourth),
    ];
    for (mask, x, y) in positions {
        if dots & mask != 0 {
            commands.push(fill_rect(
                rect.min_x + x,
                rect.min_y + y,
                layout.dot_width,
                layout.dot_width,
            ));
        }
    }
    if commands.is_empty() {
        placeholder_commands(rect)
    } else {
        commands
    }
}

struct BrailleDotLayout {
    dot_width: f32,
    x: [f32; 2],
    y: [f32; 4],
}

fn braille_dot_layout(rect: SurfaceRect) -> BrailleDotLayout {
    let width = rect.width().round();
    let height = rect.height().round();

    let mut dot_width = (width / 4.0).trunc().min((height / 8.0).trunc());
    let mut x_spacing = (width / 4.0).trunc();
    let mut y_spacing = (height / 8.0).trunc();
    let mut x_margin = (x_spacing / 2.0).trunc();
    let mut y_margin = (y_spacing / 2.0).trunc();

    let mut x_px_left =
        (-2.0_f32).mul_add(dot_width, (-2.0_f32).mul_add(x_margin, width) - x_spacing);
    let mut y_px_left = (-4.0_f32).mul_add(
        dot_width,
        (-3.0_f32).mul_add(y_spacing, (-2.0_f32).mul_add(y_margin, height)),
    );

    if x_px_left >= 2.0 && y_px_left >= 4.0 && dot_width == 0.0 {
        dot_width += 1.0;
        x_px_left -= 2.0;
        y_px_left -= 4.0;
    }

    if x_px_left >= 2.0 && x_margin == 0.0 {
        x_margin += 1.0;
        x_px_left -= 2.0;
    }
    if y_px_left >= 2.0 && y_margin == 0.0 {
        y_margin += 1.0;
        y_px_left -= 2.0;
    }

    if x_px_left >= 1.0 {
        x_spacing += 1.0;
        x_px_left -= 1.0;
    }
    if y_px_left >= 3.0 {
        y_spacing += 1.0;
        y_px_left -= 3.0;
    }

    if x_px_left >= 2.0 {
        x_margin += 1.0;
        x_px_left -= 2.0;
    }
    if y_px_left >= 2.0 {
        y_margin += 1.0;
        y_px_left -= 2.0;
    }

    if x_px_left >= 2.0 && y_px_left >= 4.0 {
        dot_width += 1.0;
    }

    let dot_width = dot_width.max(0.0);

    BrailleDotLayout {
        dot_width,
        x: [x_margin, x_margin + dot_width + x_spacing],
        y: [
            y_margin,
            y_margin + dot_width + y_spacing,
            2.0f32.mul_add(dot_width + y_spacing, y_margin),
            3.0f32.mul_add(dot_width + y_spacing, y_margin),
        ],
    }
}
