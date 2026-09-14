#![cfg(test)]

use bootty_terminal::geometry::SurfaceRect;
use bootty_ui::{
    gpui::GpuiTerminalElement,
    paint_plan::PlanColor,
    terminal_render::{SpriteCommandBatch, TerminalRenderCommand, TerminalRenderFrame},
    terminal_sprite::SpriteGlyph,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn gpui_prepares_every_subtractive_sprite_as_an_exact_cached_image() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 48.0, 24.0);
    let rects = [
        SurfaceRect::from_min_size(0.0, 0.0, 16.0, 24.0),
        SurfaceRect::from_min_size(16.0, 0.0, 16.0, 24.0),
        SurfaceRect::from_min_size(32.0, 0.0, 16.0, 24.0),
    ];
    let commands = ['\u{1FBBD}', '\u{1FBBE}', '\u{1FBBF}']
        .into_iter()
        .zip(rects)
        .map(|(ch, rect)| {
            TerminalRenderCommand::Sprite(SpriteCommandBatch {
                glyph: SpriteGlyph::from_char(ch).expect("subtractive sprite"),
                rect,
                color: PlanColor {
                    r: 0x12,
                    g: 0x34,
                    b: 0x56,
                    a: 255,
                },
            })
        })
        .collect();

    let element = GpuiTerminalElement::new(TerminalRenderFrame { surface, commands });

    assert_eq!(element.limits(), []);
    assert_eq!(element.glyph_sprite_rects(), rects);
    assert_eq!(element.frame().commands.len(), 3);
}
