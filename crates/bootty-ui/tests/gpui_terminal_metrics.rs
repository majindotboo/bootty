#![cfg(test)]

use std::sync::Arc;

use bootty_terminal::geometry::{
    CellMetrics, SurfaceRect, TerminalGeometry, TerminalPadding, TerminalSurface,
};
use bootty_terminal::{
    terminal_engine::TerminalEngine,
    terminal_frame::{FrameCopyMode, RenderFrame},
    terminal_image::{KittyImageLayer, KittyImagePlacement},
};
use bootty_ui::{
    gpui::{
        GpuiTerminalAdapter, GpuiTerminalElement, TerminalPrepaint, TerminalRenderMetrics,
        TerminalRenderMetricsSnapshot,
    },
    paint_plan::CursorBlinkPhase,
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
};
use gpui_kit::{
    App, Bounds, Element, ElementId, Font, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, TestAppContext, TextRun, TextStyleRefinement, VisualTestContext, Window,
    font, point, px, rgba, size,
};
use libghostty_vt::kitty::graphics::{ImageFormat, SourceRect};
use pretty_assertions::assert_eq;

fn surface() -> TerminalSurface {
    TerminalSurface::for_logical_size(
        40.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    )
}

fn draw_frame(
    cx: &mut VisualTestContext,
    adapter: &mut GpuiTerminalAdapter,
    surface: TerminalSurface,
    frame: &Arc<RenderFrame>,
    contract: &TerminalTextContract,
) {
    cx.draw(
        point(px(0.0), px(0.0)),
        size(px(surface.rect.width()), px(surface.rect.height())),
        |_, _| {
            adapter.element(
                surface,
                frame,
                14.0,
                surface.cell.height,
                1.0,
                contract,
                CursorBlinkPhase::visible(),
                true,
                "",
            )
        },
    );
}

fn assert_incremental_metrics(
    snapshot: TerminalRenderMetricsSnapshot,
    final_dirty_rows: u64,
    rows: u16,
    command_count: usize,
) {
    assert_eq!(snapshot.scene_builds, 1);
    assert_eq!(snapshot.full_plan_builds, 0);
    assert_eq!(snapshot.incremental_plan_builds, 1);
    assert_eq!(snapshot.incremental_scene_builds, 1);
    assert_eq!(snapshot.full_scene_builds, 0);
    assert_eq!(snapshot.render_frame_reuses, 1);
    assert_eq!(snapshot.render_frame_cold_builds, 0);
    assert_eq!(snapshot.planned_rows, final_dirty_rows);
    assert!(snapshot.reused_prepared_commands > snapshot.prepared_commands);
    assert!(final_dirty_rows < u64::from(rows));
    assert_eq!(
        snapshot
            .prepared_commands
            .checked_add(snapshot.reused_prepared_commands)
            .expect("prepared command count fits u64"),
        u64::try_from(command_count).expect("command count fits u64")
    );
}

#[rstest::rstest]
fn adapter_rebuilds_erased_rows_when_publications_are_skipped() {
    let surface = TerminalSurface::for_logical_size(
        400.0,
        100.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut engine = TerminalEngine::new(surface.geometry()).unwrap();
    let mut adapter = GpuiTerminalAdapter::default();
    let mut draw = |frame: &Arc<RenderFrame>| {
        let actual = adapter.element(
            surface,
            frame,
            15.0,
            20.0,
            1.0,
            &contract,
            CursorBlinkPhase::visible(),
            true,
            "",
        );
        let expected = GpuiTerminalAdapter::default().element(
            surface,
            frame,
            15.0,
            20.0,
            1.0,
            &contract,
            CursorBlinkPhase::visible(),
            true,
            "",
        );
        assert_eq!(actual.frame(), expected.frame());
        assert_eq!(actual.glyph_sprite_rects(), expected.glyph_sprite_rects());
    };
    for text in [b"\x1b[2;1HThinking".as_slice(), b"\x1b[4;1HWorking"] {
        engine.write_vt(text);
        draw(&Arc::new(engine.extract_frame().unwrap().clone()));
    }
    engine.write_vt(b"\x1b[2;1H\x1b[2K");
    engine.extract_frame().unwrap();
    draw(&Arc::new(engine.extract_frame().unwrap().clone()));
}

#[gpui_kit::test]
fn padded_terminal_layout_preserves_the_outer_surface(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let surface = TerminalSurface::new(
        SurfaceRect::from_min_size(50.0, 30.0, 208.0, 168.0),
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::uniform(4.0),
    );
    let mut engine = TerminalEngine::new(surface.geometry()).expect("terminal engine");
    engine.write_vt(b"A");
    let frame = Arc::new(engine.extract_frame().expect("frame").clone());
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    cx.draw(
        point(px(0.0), px(0.0)),
        size(px(208.0), px(168.0)),
        |window, app| {
            let mut element = GpuiTerminalAdapter::default().element(
                surface,
                &frame,
                15.0,
                20.0,
                1.0,
                &contract,
                CursorBlinkPhase::visible(),
                true,
                "",
            );
            let (layout, ()) = element.request_layout(None, None, window, app);
            window.compute_layout(layout, size(px(208.0), px(168.0)).into(), app);
            assert_eq!(
                window.layout_bounds(layout).size,
                size(px(208.0), px(168.0))
            );
            assert_eq!(
                element
                    .interaction()
                    .expect("interaction")
                    .surface()
                    .content_origin(),
                surface.content_origin()
            );
            element
        },
    );
}

fn frame(image: Arc<Vec<u8>>) -> Arc<RenderFrame> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 4,
        rows: 1,
        cell_width: 10,
        cell_height: 20,
    })
    .expect("terminal engine");
    engine.write_vt(b"AB");
    let mut frame = engine.extract_frame().expect("render frame").clone();
    frame.images.placements.push(KittyImagePlacement {
        image_id: 7,
        placement_id: 11,
        layer: KittyImageLayer::AboveText,
        image_width: 1,
        image_height: 1,
        image_format: ImageFormat::Rgba,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        destination: SurfaceRect::from_min_size(20.0, 0.0, 10.0, 20.0),
        data: image,
    });
    Arc::new(frame)
}

struct InheritedFontTerminal {
    terminal: GpuiTerminalElement,
    font: Font,
}

fn with_font_style<R>(font: &Font, window: &mut Window, f: impl FnOnce(&mut Window) -> R) -> R {
    window.with_text_style(
        Some(TextStyleRefinement {
            font_family: Some(font.family.clone()),
            font_features: Some(font.features.clone()),
            font_fallbacks: font.fallbacks.clone(),
            font_weight: Some(font.weight),
            font_style: Some(font.style),
            ..Default::default()
        }),
        f,
    )
}

impl IntoElement for InheritedFontTerminal {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InheritedFontTerminal {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let font = self.font.clone();
        with_font_style(&font, window, |window| {
            self.terminal.request_layout(id, inspector_id, window, cx)
        })
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let font = self.font.clone();
        with_font_style(&font, window, |window| {
            self.terminal
                .prepaint(id, inspector_id, bounds, request_layout, window, cx)
        })
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let font = self.font.clone();
        with_font_style(&font, window, |window| {
            self.terminal.paint(
                id,
                inspector_id,
                bounds,
                request_layout,
                prepaint,
                window,
                cx,
            );
        });
    }
}

#[gpui_kit::test]
fn opt_in_metrics_count_actual_terminal_prepaint_and_paint(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let metrics = TerminalRenderMetrics::default();
    let mut adapter = GpuiTerminalAdapter::default();
    adapter.set_render_metrics(Some(metrics.clone()));
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let first_image = Arc::new(vec![255, 0, 0, 255]);

    for image in [
        Arc::clone(&first_image),
        Arc::clone(&first_image),
        Arc::new(vec![0, 255, 0, 255]),
    ] {
        let frame = frame(image);
        cx.draw(point(px(0.0), px(0.0)), size(px(40.0), px(20.0)), |_, _| {
            adapter.element(
                surface(),
                &frame,
                14.0,
                20.0,
                1.0,
                &contract,
                CursorBlinkPhase::visible(),
                true,
                "",
            )
        });
    }

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.scene_builds, 3);
    assert_eq!(snapshot.full_plan_builds, 3);
    assert_eq!(snapshot.incremental_plan_builds, 0);
    assert_eq!(snapshot.incremental_scene_builds, 0);
    assert_eq!(snapshot.full_scene_builds, 3);
    assert_eq!(snapshot.reused_prepared_commands, 0);
    assert!(snapshot.prepared_commands > 0);
    assert_eq!(snapshot.planned_rows, 3);
    assert_eq!(snapshot.prepaint_calls, 3);
    assert_eq!(snapshot.paint_calls, 3);
    assert_eq!(snapshot.image_primitives, 3);
    assert!(snapshot.glyph_primitives >= 6);
    assert!(snapshot.cache_creations >= 3);
    assert!(snapshot.cache_hits > 0);
    assert!(snapshot.cache_retirements > 0);
}

#[gpui_kit::test]
fn terminal_glyph_tiles_apply_baseline_changes_without_reusing_stale_geometry(
    cx: &mut TestAppContext,
) {
    let cx = cx.add_empty_window();
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 1,
        rows: 1,
        cell_width: 10,
        cell_height: 20,
    })
    .expect("terminal engine");
    engine.write_vt(b"A");
    let frame = Arc::new(engine.extract_frame().expect("render frame").clone());
    let surface = TerminalSurface::for_logical_size(
        10.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let font_size = TerminalTextConfig::default().font_size;
    for pixels_per_point in [1.0, 2.0] {
        let mut adapter = GpuiTerminalAdapter::default();
        for adjustment in [0.0, 3.0, -2.0, 0.0] {
            let contract = TerminalTextContract::new(
                TerminalTextConfig {
                    baseline_adjustment: adjustment,
                    ..TerminalTextConfig::default()
                },
                NativeSymbolPolicy::default(),
            );
            let mut output = None;

            cx.draw(point(px(0.0), px(0.0)), size(px(10.0), px(20.0)), |_, _| {
                let element = adapter.element(
                    surface,
                    &frame,
                    font_size,
                    surface.cell.height,
                    pixels_per_point,
                    &contract,
                    CursorBlinkPhase::visible(),
                    true,
                    "",
                );
                let text_rect = element
                    .frame()
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        bootty_ui::terminal_render::TerminalRenderCommand::Text(text) => {
                            Some(text.rect)
                        }
                        _ => None,
                    })
                    .expect("text command");
                output = Some((text_rect, element.glyph_sprite_rects()));
                element
            });

            let (text_rect, glyph_rects) = output.expect("terminal element output");
            assert_ne!(
                glyph_rects,
                Vec::<bootty_terminal::geometry::SurfaceRect>::new()
            );
            let expected = SurfaceRect::from_min_size(
                text_rect.min_x,
                text_rect.min_y - adjustment,
                text_rect.width(),
                text_rect.height(),
            );
            assert!(glyph_rects.iter().all(|rect| *rect == expected));
        }
    }
}

#[gpui_kit::test]
fn copy_mode_label_uses_the_inherited_root_font(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 8,
        rows: 1,
        cell_width: 10,
        cell_height: 20,
    })
    .expect("terminal engine");
    engine.write_vt(b"copy");
    let mut render_frame = engine.extract_frame().expect("render frame").clone();
    render_frame.copy_mode = Some(FrameCopyMode::default());
    let frame = Arc::new(render_frame);
    let surface = TerminalSurface::for_logical_size(
        80.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let root_font = font("serif");
    let mut adapter = GpuiTerminalAdapter::default();
    let ((), prepaint) = cx.draw(point(px(0.0), px(0.0)), size(px(80.0), px(20.0)), |_, _| {
        InheritedFontTerminal {
            terminal: adapter.element(
                surface,
                &frame,
                14.0,
                20.0,
                1.0,
                &contract,
                CursorBlinkPhase::visible(),
                true,
                "",
            ),
            font: root_font.clone(),
        }
    });
    let actual_width = prepaint
        .copy_mode_width()
        .expect("copy-mode label is shaped");
    let expected_width = cx.update(|window, _| {
        window
            .text_system()
            .shape_line(
                "[1/1]".into(),
                px(12.0),
                &[TextRun {
                    len: 5,
                    font: root_font,
                    color: rgba(0xffff_ffe6).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
            .width
    });
    assert_eq!(actual_width, expected_width);
}

#[gpui_kit::test]
fn adapter_rebuilds_only_dirty_rows_after_incremental_cache_warmup(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let metrics = TerminalRenderMetrics::default();
    let mut adapter = GpuiTerminalAdapter::default();
    adapter.set_render_metrics(Some(metrics.clone()));
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let (cols, rows) = (120, 40);
    let surface = TerminalSurface::for_logical_size(
        f32::from(cols) * 10.0,
        f32::from(rows) * 20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 10,
        cell_height: 20,
    })
    .expect("terminal engine");
    for row in 0..rows {
        let row_number = row.checked_add(1).expect("terminal row number fits u16");
        engine.write_vt(format!("\x1b[{row_number};1Hrow {row:03}").as_bytes());
    }

    let initial = Arc::new(engine.extract_frame().expect("initial frame").clone());
    draw_frame(cx, &mut adapter, surface, &initial, &contract);

    engine.write_vt(b"\x1b[10;1Hfirst edit");
    let first_edit = Arc::new(engine.extract_frame().expect("first edit").clone());
    draw_frame(cx, &mut adapter, surface, &first_edit, &contract);

    engine.write_vt(b"\x1b[20;1Hsecond edit");
    let second_edit = Arc::new(engine.extract_frame().expect("second edit").clone());
    let final_dirty_rows =
        u64::try_from(second_edit.stats.dirty_rows).expect("dirty row count fits u64");
    metrics.reset();
    let mut incremental_output = None;
    cx.draw(
        point(px(0.0), px(0.0)),
        size(px(surface.rect.width()), px(surface.rect.height())),
        |_, _| {
            let element = adapter.element(
                surface,
                &second_edit,
                14.0,
                surface.cell.height,
                1.0,
                &contract,
                CursorBlinkPhase::visible(),
                true,
                "",
            );
            incremental_output = Some((
                element.frame().clone(),
                element.glyph_sprite_rects(),
                element.limits().to_vec(),
            ));
            element
        },
    );

    let snapshot = metrics.snapshot();
    let (incremental_frame, incremental_glyphs, incremental_limits) =
        incremental_output.expect("incremental output");
    assert_incremental_metrics(
        snapshot,
        final_dirty_rows,
        rows,
        incremental_frame.commands.len(),
    );

    let mut cold_adapter = GpuiTerminalAdapter::default();
    let mut cold_output = None;
    cx.draw(
        point(px(0.0), px(0.0)),
        size(px(surface.rect.width()), px(surface.rect.height())),
        |_, _| {
            let element = cold_adapter.element(
                surface,
                &second_edit,
                14.0,
                surface.cell.height,
                1.0,
                &contract,
                CursorBlinkPhase::visible(),
                true,
                "",
            );
            cold_output = Some((
                element.frame().clone(),
                element.glyph_sprite_rects(),
                element.limits().to_vec(),
            ));
            element
        },
    );
    let (cold_frame, cold_glyphs, cold_limits) = cold_output.expect("cold output");
    assert_eq!(incremental_frame, cold_frame);
    assert_eq!(incremental_glyphs, cold_glyphs);
    assert_eq!(incremental_limits, cold_limits);
}
