#![cfg(test)]

use std::sync::Arc;

use bootty_terminal::{
    geometry::{CellMetrics, TerminalPadding, TerminalSurface},
    terminal_engine::TerminalEngine,
};
use bootty_ui::{
    gpui::{
        GpuiTerminalAdapter, GpuiTerminalElement, GpuiTerminalZoom, TerminalRenderMetrics,
        TerminalZoomRequest,
    },
    paint_plan::CursorBlinkPhase,
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
};
use gpui_kit::{AppContext as _, TestAppContext};
use pretty_assertions::assert_eq;

static_assertions::assert_impl_all!(GpuiTerminalAdapter: Send);
static_assertions::assert_impl_all!(GpuiTerminalElement: Send);

fn request(text: &[u8]) -> TerminalZoomRequest {
    let surface = TerminalSurface::for_logical_size(
        200.0,
        80.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let mut engine = TerminalEngine::new(surface.geometry()).unwrap();
    engine.write_vt(text);
    TerminalZoomRequest {
        surface,
        frame: Arc::new(engine.extract_frame().unwrap().clone()),
        font_size: 15.0,
        text_cell_height: 20.0,
        pixels_per_point: 4.0,
        text_contract: Arc::new(TerminalTextContract::new(
            TerminalTextConfig::default(),
            NativeSymbolPolicy::default(),
        )),
        cursor_blink_phase: CursorBlinkPhase::visible(),
        cursor_focused: true,
        marked_text: String::new(),
    }
}

fn expected(request: &TerminalZoomRequest) -> GpuiTerminalElement {
    GpuiTerminalAdapter::default().element(
        request.surface,
        &request.frame,
        request.font_size,
        request.text_cell_height,
        request.pixels_per_point,
        &request.text_contract,
        request.cursor_blink_phase,
        request.cursor_focused,
        &request.marked_text,
    )
}

#[gpui_kit::test]
fn cold_zoom_is_deferred_and_requests_coalesce_to_the_latest_resolution(cx: &mut TestAppContext) {
    let metrics = TerminalRenderMetrics::default();
    let zoom = cx.new(|cx| {
        let mut adapter = GpuiTerminalAdapter::default();
        adapter.set_render_metrics(Some(metrics.clone()));
        GpuiTerminalZoom::new(adapter, cx)
    });
    let mut latest = request(b"first frame");
    for scale in [4.0, 6.0, 8.0, 10.0] {
        latest.pixels_per_point = scale;
        assert!(
            zoom.update(cx, |zoom, cx| zoom.element(latest.clone(), cx))
                .is_none()
        );
    }
    assert_eq!(
        metrics.snapshot().scene_builds,
        0,
        "requesting zoom must not rasterize on the UI thread"
    );
    cx.run_until_parked();
    assert_eq!(
        metrics.snapshot().scene_builds,
        2,
        "one running job and only the latest queued job"
    );
    let actual = zoom
        .update(cx, |zoom, cx| zoom.element(latest.clone(), cx))
        .unwrap();
    let expected = expected(&latest);
    assert_eq!(actual.frame(), expected.frame());
    assert_eq!(actual.glyph_sprite_rects(), expected.glyph_sprite_rects());
    assert_eq!(metrics.snapshot().scene_builds, 2);
    for scale in [8.0, 6.0, 4.0] {
        latest.pixels_per_point = scale;
        let actual = zoom
            .update(cx, |zoom, cx| zoom.element(latest.clone(), cx))
            .unwrap();
        assert_eq!(actual.glyph_sprite_rects(), expected.glyph_sprite_rects());
        assert_eq!(
            metrics.snapshot().scene_builds,
            2,
            "zooming out reuses the sharper resolution"
        );
    }
}

#[gpui_kit::test]
fn warm_zoom_keeps_output_cursor_and_ime_current_without_resolution_flashes(
    cx: &mut TestAppContext,
) {
    let zoom = cx.new(|cx| GpuiTerminalZoom::new(GpuiTerminalAdapter::default(), cx));
    let initial = request(b"old frame");
    assert!(
        zoom.update(cx, |zoom, cx| zoom.element(initial, cx))
            .is_none()
    );
    cx.run_until_parked();
    let mut latest = request(b"new frame");
    for (blink, marked) in [
        (CursorBlinkPhase::visible(), ""),
        (CursorBlinkPhase::hidden(), ""),
        (CursorBlinkPhase::visible(), "compose"),
    ] {
        latest.cursor_blink_phase = blink;
        latest.marked_text = marked.into();
        let actual = zoom
            .update(cx, |zoom, cx| zoom.element(latest.clone(), cx))
            .expect("warm resolution is immediately available for current content");
        let expected = expected(&latest);
        assert_eq!(actual.frame(), expected.frame());
        assert_eq!(actual.glyph_sprite_rects(), expected.glyph_sprite_rects());
    }
    latest.pixels_per_point = 10.0;
    zoom.update(cx, |zoom, cx| {
        zoom.element(latest.clone(), cx);
    });
    latest = request(b"different terminal");
    latest.pixels_per_point = 10.0;
    assert!(
        zoom.update(cx, |zoom, cx| zoom.element(latest.clone(), cx))
            .is_none(),
        "a pending job must not return an old terminal frame"
    );
    cx.run_until_parked();
    let actual = zoom
        .update(cx, |zoom, cx| zoom.element(latest.clone(), cx))
        .unwrap();
    assert_eq!(actual.frame(), expected(&latest).frame());
}

#[gpui_kit::test]
fn reset_and_pane_close_allow_an_inflight_zoom_to_finish_without_publication(
    cx: &mut TestAppContext,
) {
    let zoom = cx.new(|cx| GpuiTerminalZoom::new(GpuiTerminalAdapter::default(), cx));
    let request = request(b"closing");
    zoom.update(cx, |zoom, cx| {
        zoom.element(request.clone(), cx);
        zoom.clear();
    });
    cx.run_until_parked();
    assert!(
        zoom.update(cx, |zoom, cx| zoom.element(request, cx))
            .is_none(),
        "reset discards pending publication"
    );
    drop(zoom);
    cx.run_until_parked();
}
