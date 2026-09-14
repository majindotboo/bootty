use std::sync::Arc;

use bootty_terminal::{geometry::TerminalSurface, terminal_frame::RenderFrame};
use gpui_kit::{AppContext as _, Context};

use super::{GpuiTerminalAdapter, GpuiTerminalElement};
use crate::{paint_plan::CursorBlinkPhase, terminal_text::TerminalTextContract};

/// Immutable inputs for one magnified terminal scene. Geometry remains unscaled.
#[derive(Clone)]
pub struct TerminalZoomRequest {
    pub surface: TerminalSurface,
    pub frame: Arc<RenderFrame>,
    pub font_size: f32,
    pub text_cell_height: f32,
    pub pixels_per_point: f32,
    pub text_contract: Arc<TerminalTextContract>,
    pub cursor_blink_phase: CursorBlinkPhase,
    pub cursor_focused: bool,
    pub marked_text: String,
}

impl TerminalZoomRequest {
    fn same_content(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.frame, &other.frame)
            && self.surface == other.surface
            && self.font_size.to_bits() == other.font_size.to_bits()
            && self.text_cell_height.to_bits() == other.text_cell_height.to_bits()
            && self.text_contract == other.text_contract
            && self.cursor_blink_phase == other.cursor_blink_phase
            && self.cursor_focused == other.cursor_focused
            && self.marked_text == other.marked_text
    }

    fn covers(&self, other: &Self) -> bool {
        self.surface == other.surface
            && self.font_size.to_bits() == other.font_size.to_bits()
            && self.text_cell_height.to_bits() == other.text_cell_height.to_bits()
            && self.text_contract == other.text_contract
            && self.pixels_per_point >= other.pixels_per_point
    }

    fn prepare(&self, adapter: &mut GpuiTerminalAdapter) -> GpuiTerminalElement {
        adapter.element(
            self.surface,
            &self.frame,
            self.font_size,
            self.text_cell_height,
            self.pixels_per_point,
            &self.text_contract,
            self.cursor_blink_phase,
            self.cursor_focused,
            &self.marked_text,
        )
    }
}

/// One worker and one replaceable request per pane; cold zoom rasterization never blocks the UI.
pub struct GpuiTerminalZoom {
    adapter: Option<GpuiTerminalAdapter>,
    requested: Option<TerminalZoomRequest>,
    prepared: Option<(TerminalZoomRequest, GpuiTerminalElement)>,
}

impl GpuiTerminalZoom {
    pub fn new(adapter: GpuiTerminalAdapter, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            if let Some(adapter) = &mut this.adapter {
                for image in adapter.take_render_images() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        Self {
            adapter: Some(adapter),
            requested: None,
            prepared: None,
        }
    }

    /// Return matching content immediately, using its previous resolution until the worker
    /// catches up. Never present an old frame, font, geometry, selection, or IME composition.
    pub fn element(
        &mut self,
        mut request: TerminalZoomRequest,
        cx: &mut Context<Self>,
    ) -> Option<GpuiTerminalElement> {
        if let Some((prepared, _)) = self.prepared.as_ref()
            && prepared.covers(&request)
            && let Some(adapter) = &mut self.adapter
        {
            // Once warm, retain the normal incremental renderer for output, cursor blinking,
            // selection and IME. Sending every frame through a worker would flash low-res text.
            request.pixels_per_point = prepared.pixels_per_point;
            self.prepared = None;
            let element = request.prepare(adapter);
            self.prepared = Some((request.clone(), element.clone()));
            self.requested = Some(request);
            return Some(element);
        }
        let element = self.prepared.as_ref().and_then(|(prepared, element)| {
            prepared.same_content(&request).then(|| element.clone())
        });
        self.requested = Some(request);
        self.start(cx);
        element
    }

    pub fn clear(&mut self) {
        self.requested = None;
    }

    fn start(&mut self, cx: &Context<Self>) {
        let Some(request) = self.requested.as_ref() else {
            return;
        };
        if self
            .prepared
            .as_ref()
            .is_some_and(|(prepared, _)| prepared.covers(request))
        {
            return;
        }
        let Some(mut adapter) = self.adapter.take() else {
            return;
        };
        let request = request.clone();
        let task = cx.background_spawn(async move {
            let element = request.prepare(&mut adapter);
            (adapter, request, element)
        });
        cx.spawn(async move |weak, cx| {
            let mut completed = Some(task.await);
            _ = weak.update(cx, |this, cx| {
                let Some((adapter, request, element)) = completed.take() else {
                    return;
                };
                for image in adapter.take_retired_images() {
                    cx.drop_image(image, None);
                }
                this.adapter = Some(adapter);
                if this
                    .requested
                    .as_ref()
                    .is_some_and(|latest| latest.covers(&request))
                {
                    this.prepared = Some((request, element));
                    cx.notify();
                } else {
                    this.prepared = None;
                }
                this.start(cx);
            });
            // A pane can close during a raster job. Its uploaded images still need GPUI
            // retirement even though the entity's release callback no longer owns the adapter.
            if let Some((mut adapter, _, _)) = completed {
                cx.update(|cx| {
                    for image in adapter.take_render_images() {
                        cx.drop_image(image, None);
                    }
                });
            }
        })
        .detach();
    }
}
