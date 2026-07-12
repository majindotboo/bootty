use bootty_app::{
    geometry::SurfaceRect,
    paint_plan::{PlanColor, TextAttrs, TextRun},
    terminal_sprite::{SpriteCommand, SpriteRegistry, SpriteShape, WgpuSpriteBackend},
    terminal_text::{
        NativeSymbolClass, NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract,
        TerminalTextFragment,
    },
};

fn attrs() -> TextAttrs {
    TextAttrs {
        fg: PlanColor {
            r: 220,
            g: 221,
            b: 222,
            a: 255,
        },
        bold: false,
        italic: false,
        underline: libghostty_vt::style::Underline::None,
        strikethrough: false,
        overline: false,
    }
}

fn run(text: &str) -> TextRun {
    TextRun {
        cell_rect: SurfaceRect::from_min_size(0.0, 0.0, 30.0, 20.0),
        rect: SurfaceRect::from_min_size(0.0, 0.0, 30.0, 20.0),
        cells: 3,
        text: text.to_owned(),
        attrs: attrs(),
    }
}

#[test]
fn text_contract_fragments_only_registry_owned_terminal_sprites() {
    let contract = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );

    let shaped = contract.shape_run(&run("┌┃❯\u{E0B8}"));

    assert_eq!(
        shaped.fragments,
        vec![
            TerminalTextFragment::NativeSymbol {
                cell: 0,
                ch: '┌',
                class: NativeSymbolClass::BoxDrawing
            },
            TerminalTextFragment::NativeSymbol {
                cell: 1,
                ch: '┃',
                class: NativeSymbolClass::BoxDrawing
            },
            TerminalTextFragment::NativeSymbol {
                cell: 2,
                ch: '❯',
                class: NativeSymbolClass::Separator
            },
            TerminalTextFragment::NativeSymbol {
                cell: 3,
                ch: '\u{E0B8}',
                class: NativeSymbolClass::Powerline
            },
        ]
    );
}

#[test]
fn wgpu_sprite_backend_builds_primitive_buffers_for_task_shapes() {
    let registry = SpriteRegistry::prompt_graphics();
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 8.0, 24.0);
    let color = PlanColor {
        r: 10,
        g: 20,
        b: 30,
        a: 255,
    };
    let mut all_commands = Vec::new();

    for ch in ['┃', '\u{E0B8}', '\u{E0B1}', '\u{E0B4}'] {
        let glyph = registry.glyph_for(ch).expect("task sprite glyph");
        all_commands.extend(registry.commands_for(glyph, rect));
    }

    assert!(
        all_commands
            .iter()
            .any(|command| matches!(command, SpriteCommand::FillRect { .. }))
    );
    assert!(all_commands.iter().any(|command| matches!(
        command,
        SpriteCommand::FillPolygon {
            shape: SpriteShape::Triangle,
            ..
        }
    )));
    assert!(
        all_commands
            .iter()
            .any(|command| matches!(command, SpriteCommand::StrokePolyline { .. }))
    );

    let primitives = WgpuSpriteBackend::build_primitives(&all_commands, color);

    assert!(primitives.vertices.len() >= 18);
    assert!(primitives.indices.len() >= 24);
    assert!(primitives.indices.len().is_multiple_of(3));
}
