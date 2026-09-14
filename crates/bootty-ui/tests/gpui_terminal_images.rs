#![cfg(test)]

use std::sync::Arc;

use bootty_terminal::geometry::{CellMetrics, SurfaceRect, TerminalPadding, TerminalSurface};
use bootty_terminal::{
    terminal_frame::RenderFrame,
    terminal_image::{KittyImageFrame, KittyImageLayer, KittyImagePlacement},
};
use bootty_ui::{
    gpui::GpuiTerminalAdapter,
    paint_plan::CursorBlinkPhase,
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
};
use libghostty_vt::kitty::graphics::{ImageFormat, SourceRect};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn kitty_placements_reuse_one_prepared_crop_across_placement_ids() {
    let data = Arc::new(vec![255, 0, 0, 255, 0, 255, 0, 255]);
    let placement = |placement_id, destination| KittyImagePlacement {
        image_id: 7,
        placement_id,
        layer: KittyImageLayer::AboveText,
        image_width: 2,
        image_height: 1,
        image_format: ImageFormat::Rgba,
        source: SourceRect {
            x: 1,
            y: 0,
            width: 1,
            height: 1,
        },
        destination,
        data: Arc::clone(&data),
    };
    let frame = Arc::new(RenderFrame {
        cols: 2,
        rows: 1,
        row_dirty: vec![true],
        row_wraps: vec![false],
        images: KittyImageFrame {
            placements: vec![
                placement(11, SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0)),
                placement(12, SurfaceRect::from_min_size(10.0, 0.0, 10.0, 20.0)),
            ],
            ..Default::default()
        },
        ..Default::default()
    });
    let surface = TerminalSurface::for_logical_size(
        20.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let mut adapter = GpuiTerminalAdapter::default();

    let element = adapter.element(
        surface,
        &frame,
        14.0,
        20.0,
        1.0,
        &contract,
        CursorBlinkPhase::visible(),
        true,
        "",
    );
    assert_eq!(element.limits(), []);
    drop(element);

    let images = adapter.take_render_images();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].as_bytes(0), Some([0, 255, 0, 255].as_slice()));
}
