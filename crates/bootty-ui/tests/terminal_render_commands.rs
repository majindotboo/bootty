#![cfg(test)]

use bootty_terminal::geometry::{
    CellMetrics, SurfaceRect, TerminalGeometry, TerminalPadding, TerminalSurface,
};
use bootty_terminal::terminal_engine::TerminalEngine;
use bootty_terminal::terminal_frame::{CursorSnapshot, RenderFrame};
use bootty_terminal::terminal_image::{KittyImageFrame, KittyImageLayer, KittyImagePlacement};
use bootty_ui::{
    paint_plan::{
        BackgroundRect, CursorBlinkPhase, CursorPlan, CursorShape, CursorTextPlan, DecorationLine,
        PaintPlanner, PlanColor, TerminalPaintPlan, TextAttrs, TextRun, cursor_fill_rect,
    },
    terminal_render::{FillRole, TerminalRenderCommand, TerminalRenderFrame},
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
};
use proptest::prelude::*;
use std::sync::Arc;

const fn color(r: u8, g: u8, b: u8) -> PlanColor {
    PlanColor { r, g, b, a: 255 }
}

fn attrs() -> TextAttrs {
    TextAttrs {
        fg: color(220, 221, 222),
        bold: false,
        italic: false,
        underline: libghostty_vt::style::Underline::None,
        strikethrough: false,
        overline: false,
    }
}

fn text_run(rect: SurfaceRect, cells: u16, text: &str) -> TextRun {
    TextRun {
        cell_rect: rect,
        rect,
        cells,
        text: text.to_owned(),
        attrs: attrs(),
    }
}

fn plan_with_text_runs(surface: SurfaceRect, text_runs: Vec<TextRun>) -> TerminalPaintPlan {
    TerminalPaintPlan {
        surface,
        default_background: color(1, 2, 3),
        backgrounds: Vec::new(),
        text_runs,
        decorations: Vec::new(),
        cursor: None,
    }
}

fn text_renderer() -> TerminalTextContract {
    TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    )
}

fn image(layer: KittyImageLayer, id: u32) -> KittyImagePlacement {
    KittyImagePlacement {
        image_id: id,
        placement_id: id.checked_add(10).expect("fixture placement ID fits"),
        layer,
        image_width: 1,
        image_height: 1,
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Rgba,
        source: libghostty_vt::kitty::graphics::SourceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        destination: SurfaceRect::from_min_size(
            f32::from(u16::try_from(id).expect("fixture image ID fits")),
            0.0,
            1.0,
            1.0,
        ),
        data: Arc::new(vec![255, 0, 0, 255]),
    }
}

#[test]
fn image_layers_are_ordered_around_surface_text() {
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(300.0, 40.0, 40.0, 20.0),
        vec![text_run(
            SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0),
            1,
            "a",
        )],
    );
    let images = KittyImageFrame {
        placements: vec![
            image(KittyImageLayer::AboveText, 3),
            image(KittyImageLayer::BelowText, 2),
            image(KittyImageLayer::BelowBackground, 1),
        ],
        ..Default::default()
    };

    let frame = TerminalRenderFrame::from_plan_and_images(&plan, &text_renderer(), &images);

    assert!(matches!(
        frame.commands.as_slice(),
        [
            TerminalRenderCommand::FillRect(_),
            TerminalRenderCommand::Image(below_background),
            TerminalRenderCommand::Image(below_text),
            TerminalRenderCommand::Text(_),
            TerminalRenderCommand::Image(above_text),
        ] if below_background.layer == KittyImageLayer::BelowBackground
            && below_text.layer == KittyImageLayer::BelowText
            && below_text.destination == SurfaceRect::from_min_size(302.0, 40.0, 1.0, 1.0)
            && above_text.layer == KittyImageLayer::AboveText
    ));
}

#[test]
fn prompt_glyphs_render_as_sprites_while_ordinary_text_stays_text() {
    let prompt_color = color(125, 207, 255);
    let mut run = text_run(
        SurfaceRect::from_min_size(0.0, 0.0, 50.0, 20.0),
        5,
        "a┃b\u{E0B8}❯",
    );
    run.attrs.fg = prompt_color;
    let plan = plan_with_text_runs(SurfaceRect::from_min_size(0.0, 0.0, 50.0, 20.0), vec![run]);

    let frame = TerminalRenderFrame::from_plan(&plan, &text_renderer());

    assert!(matches!(
        frame.commands[0],
        TerminalRenderCommand::FillRect(ref fill) if fill.role == FillRole::SurfaceBackground
    ));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.text == "a"
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.text == "b"
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Sprite(sprite) if sprite.glyph.ch == '┃'
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Sprite(sprite) if sprite.glyph.ch == '\u{E0B8}'
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Sprite(sprite)
            if sprite.glyph.ch == '❯' && sprite.color == prompt_color
    )));
    assert!(!frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text)
            if text.text.contains('┃') || text.text.contains('\u{E0B8}') || text.text.contains('❯')
    )));
}

#[test]
fn block_shade_progress_row_renders_with_sprites() {
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(0.0, 0.0, 100.0, 20.0),
        vec![text_run(
            SurfaceRect::from_min_size(0.0, 0.0, 100.0, 20.0),
            10,
            "0▏▌█▓▒░1",
        )],
    );

    let frame = TerminalRenderFrame::from_plan(&plan, &text_renderer());

    assert!(matches!(
        frame.commands.as_slice(),
        [
            TerminalRenderCommand::FillRect(_),
            TerminalRenderCommand::Text(text_0),
            TerminalRenderCommand::Sprite(thin_block),
            TerminalRenderCommand::Sprite(half_block),
            TerminalRenderCommand::Sprite(full_block),
            TerminalRenderCommand::Sprite(dark_shade),
            TerminalRenderCommand::Sprite(medium_shade),
            TerminalRenderCommand::Sprite(light_shade),
            TerminalRenderCommand::Text(text_1),
        ] if text_0.text == "0"
            && text_0.rect == SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0)
            && thin_block.glyph.ch == '▏'
            && thin_block.rect == SurfaceRect::from_min_size(10.0, 0.0, 10.0, 20.0)
            && half_block.glyph.ch == '▌'
            && half_block.rect == SurfaceRect::from_min_size(20.0, 0.0, 10.0, 20.0)
            && full_block.glyph.ch == '█'
            && full_block.rect == SurfaceRect::from_min_size(30.0, 0.0, 10.0, 20.0)
            && dark_shade.glyph.ch == '▓'
            && dark_shade.rect == SurfaceRect::from_min_size(40.0, 0.0, 10.0, 20.0)
            && medium_shade.glyph.ch == '▒'
            && medium_shade.rect == SurfaceRect::from_min_size(50.0, 0.0, 10.0, 20.0)
            && light_shade.glyph.ch == '░'
            && light_shade.rect == SurfaceRect::from_min_size(60.0, 0.0, 10.0, 20.0)
            && text_1.text == "1"
            && text_1.rect == SurfaceRect::from_min_size(70.0, 0.0, 10.0, 20.0)
    ));
}

#[test]
fn frame_commands_represent_backgrounds_decorations_and_cursor() {
    let cursor_rect = SurfaceRect::from_min_size(20.0, 0.0, 10.0, 20.0);
    let plan = TerminalPaintPlan {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
        default_background: color(1, 2, 3),
        backgrounds: vec![BackgroundRect {
            rect: SurfaceRect::from_min_size(10.0, 0.0, 20.0, 20.0),
            color: color(4, 5, 6),
        }],
        text_runs: Vec::new(),
        decorations: vec![DecorationLine {
            start_x: 0.0,
            start_y: 18.0,
            end_x: 40.0,
            end_y: 18.0,
            color: color(7, 8, 9),
            style: bootty_ui::paint_plan::DecorationStyle::Single,
        }],
        cursor: Some(CursorPlan {
            rect: cursor_rect,
            color: color(10, 11, 12),
            shape: CursorShape::Block,
            text_under_cursor: Some(CursorTextPlan {
                rect: cursor_rect,
                text: "x".to_owned(),
                color: color(13, 14, 15),
            }),
        }),
    };

    let frame = TerminalRenderFrame::from_plan(&plan, &text_renderer());

    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::FillRect(fill)
            if fill.role == FillRole::CellBackground
                && fill.rect == SurfaceRect::from_min_size(10.0, 0.0, 20.0, 20.0)
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Decoration(line)
            if line.start_x.to_bits() == 0.0_f32.to_bits() && line.end_x.to_bits() == 40.0_f32.to_bits() && line.color == color(7, 8, 9)
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Cursor(cursor)
            if cursor.shape == CursorShape::Block
                && cursor.rect == cursor_rect
                && cursor.fill_rect == cursor_rect
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text)
            if text.text == "x" && text.rect == cursor_rect && text.attrs.fg == color(13, 14, 15)
    )));

    let cursor_index = frame
        .commands
        .iter()
        .position(|command| matches!(command, TerminalRenderCommand::Cursor(_)))
        .expect("cursor command");
    let cursor_text_index = frame
        .commands
        .iter()
        .position(|command| {
            matches!(
                command,
                TerminalRenderCommand::Text(text) if text.text == "x" && text.rect == cursor_rect
            )
        })
        .expect("text under cursor command");
    assert!(
        cursor_index < cursor_text_index,
        "block cursor fill must be emitted before text-under-cursor so backend compositing keeps the glyph legible"
    );
}

#[test]
fn bar_cursor_is_one_logical_unit_wide() {
    let cell = SurfaceRect::from_min_size(20.0, 4.0, 10.0, 20.0);

    assert_eq!(
        cursor_fill_rect(CursorShape::Bar, cell),
        SurfaceRect::from_min_size(20.0, 4.0, 1.0, 20.0)
    );
}

#[test]
fn cursor_blink_phase_hides_only_blinking_cursors_and_resets_visible() {
    let frame = RenderFrame {
        cols: 1,
        rows: 1,
        cursor: Some(CursorSnapshot {
            x: 0,
            y: 0,
            at_wide_tail: false,
            style: libghostty_vt::render::CursorVisualStyle::Block,
            blinking: true,
            color: None,
        }),
        ..Default::default()
    };
    let surface = TerminalSurface::for_logical_size(
        10.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let mut planner = PaintPlanner::default();

    assert!(
        planner
            .plan_with_cursor_blink_phase_and_text_cell_height(
                surface,
                &frame,
                14.0,
                20.0,
                CursorBlinkPhase::visible(),
            )
            .cursor
            .is_some()
    );
    assert!(
        planner
            .plan_with_cursor_blink_phase_and_text_cell_height(
                surface,
                &frame,
                14.0,
                20.0,
                CursorBlinkPhase::hidden(),
            )
            .cursor
            .is_none()
    );
    assert!(
        planner
            .plan_with_cursor_blink_phase_and_text_cell_height(
                surface,
                &frame,
                14.0,
                20.0,
                CursorBlinkPhase::visible(),
            )
            .cursor
            .is_some()
    );

    let steady_frame = RenderFrame {
        cursor: Some(CursorSnapshot {
            blinking: false,
            ..frame.cursor.expect("blinking cursor")
        }),
        ..frame
    };
    assert!(
        planner
            .plan_with_cursor_blink_phase_and_text_cell_height(
                surface,
                &steady_frame,
                14.0,
                20.0,
                CursorBlinkPhase::hidden(),
            )
            .cursor
            .is_some()
    );
}

#[test]
fn underlines_and_overlines_stay_below_glyphs_while_strikethrough_stays_above() {
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0);
    let plan = TerminalPaintPlan {
        surface: rect,
        default_background: color(1, 2, 3),
        backgrounds: Vec::new(),
        text_runs: vec![text_run(rect, 1, "g")],
        decorations: [
            bootty_ui::paint_plan::DecorationStyle::Strikethrough,
            bootty_ui::paint_plan::DecorationStyle::Single,
            bootty_ui::paint_plan::DecorationStyle::Overline,
        ]
        .into_iter()
        .map(|style| DecorationLine {
            start_x: rect.min_x,
            start_y: rect.max_y - 2.0,
            end_x: rect.max_x,
            end_y: rect.max_y - 2.0,
            color: color(7, 8, 9),
            style,
        })
        .collect(),
        cursor: None,
    };

    let frame = TerminalRenderFrame::from_plan(&plan, &text_renderer());
    let text = frame
        .commands
        .iter()
        .position(|command| matches!(command, TerminalRenderCommand::Text(_)))
        .expect("text command");
    let decoration = |style| {
        frame
            .commands
            .iter()
            .position(|command| {
                matches!(command, TerminalRenderCommand::Decoration(line) if line.style == style)
            })
            .expect("decoration command")
    };

    assert!(decoration(bootty_ui::paint_plan::DecorationStyle::Single) < text);
    assert!(decoration(bootty_ui::paint_plan::DecorationStyle::Overline) < text);
    assert!(decoration(bootty_ui::paint_plan::DecorationStyle::Strikethrough) > text);
}

#[test]
fn background_only_frame_does_not_generate_text_or_sprite_commands() {
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
        vec![text_run(
            SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
            4,
            "a┃b\u{E0B8}",
        )],
    );

    let frame = TerminalRenderFrame::background_from_plan(&plan);

    assert!(
        frame
            .commands
            .iter()
            .all(|command| matches!(command, TerminalRenderCommand::FillRect(_)))
    );
}

#[rstest::rstest]
#[case::joined_family("👨‍👩‍👧‍👦", 2)]
#[case::skin_tone("👍🏽", 2)]
#[case::flag("🇺🇸", 2)]
#[case::text_presentation("☔\u{fe0e}", 1)]
#[case::emoji_presentation("⚠\u{fe0f}", 2)]
#[case::wide_emoji_selector("😀\u{fe0f}", 2)]
#[case::repeated_selector("😀\u{fe0f}\u{fe0f}", 2)]
fn graphemes_keep_the_engine_grid_through_render_commands(#[case] emoji: &str, #[case] cells: u16) {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 20,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })
    .expect("terminal engine");
    engine.write_vt(format!("{emoji}█A").as_bytes());
    let frame = engine.extract_frame().expect("terminal frame");
    // The engine normalizes redundant selectors; render its published grapheme payload.
    let native_emoji = frame
        .cell_text(frame.cells.first().expect("emoji cell"))
        .iter()
        .collect::<String>();
    let following = frame
        .cells
        .iter()
        .find(|cell| frame.cell_text(cell) == ['A'])
        .expect("following A");
    pretty_assertions::assert_eq!(
        following.x,
        cells.checked_add(1).expect("following cell fits")
    );
    let surface = TerminalSurface::for_logical_size(
        200.0,
        80.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let mut planner = PaintPlanner::default();
    let plan = planner.plan(surface, frame, 16.0);
    let rendered = TerminalRenderFrame::from_plan(plan, &text_renderer());
    let fragments = rendered
        .commands
        .iter()
        .filter_map(|command| match command {
            TerminalRenderCommand::Text(text) => Some((text.text.as_str(), text.rect)),
            TerminalRenderCommand::Sprite(sprite) => Some(("█", sprite.rect)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let emoji_width = f32::from(cells) * 10.0;
    pretty_assertions::assert_eq!(
        fragments,
        vec![
            (
                native_emoji.as_str(),
                SurfaceRect::from_min_size(0.0, 0.0, emoji_width, 20.0)
            ),
            (
                "█",
                SurfaceRect::from_min_size(emoji_width, 0.0, 10.0, 20.0)
            ),
            (
                "A",
                SurfaceRect::from_min_size(emoji_width + 10.0, 0.0, 10.0, 20.0)
            ),
        ]
    );
}

proptest! {
    /// Property: ordinary ASCII text survives command generation without loss or reordering.
    #[test]
    fn ordinary_text_is_preserved(text in "[a-z0-9 ]{1,64}") {
        let cells = u16::try_from(text.len()).expect("generated text length fits in u16");
        let run_rect = SurfaceRect::from_min_size(0.0, 0.0, f32::from(cells) * 10.0, 20.0);
        let plan = plan_with_text_runs(run_rect, vec![text_run(run_rect, cells, &text)]);

        let frame = TerminalRenderFrame::from_plan(&plan, &text_renderer());
        let text_commands = frame.commands.iter().filter_map(|command| match command {
            TerminalRenderCommand::Text(text) => Some(text),
            _ => None,
        }).collect::<Vec<_>>();

        let rendered_text = text_commands.iter().map(|command| command.text.as_str()).collect::<String>();
        prop_assert_eq!(rendered_text, text);
        prop_assert!(frame.commands.iter().all(|command|
            !matches!(command, TerminalRenderCommand::Sprite(_))));
        let rendered_width = text_commands.iter().map(|command| command.rect.width()).sum::<f32>();
        prop_assert_eq!(rendered_width.to_bits(), run_rect.width().to_bits());
        prop_assert_eq!(
            text_commands.first().map(|command| (command.rect.min_x, command.rect.min_y)),
            Some((run_rect.min_x, run_rect.min_y)),
        );
        prop_assert_eq!(
            text_commands.last().map(|command| (command.rect.max_x, command.rect.max_y)),
            Some((run_rect.max_x, run_rect.max_y)),
        );
    }
}
