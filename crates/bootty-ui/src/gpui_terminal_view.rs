//! One focused GPUI entity per terminal pane.

use std::{cell::Cell, ops::Range, rc::Rc, sync::Arc, time::Duration};

use num_traits::ToPrimitive as _;

use crate::{
    gpui::{
        FrameInputSnapshot, GpuiTerminalAdapter, GpuiTerminalInteraction, GpuiTerminalZoom,
        InputAccumulator, TerminalZoomRequest,
    },
    paint_plan::CursorBlinkPhase,
    terminal_render::TerminalRenderCommand,
    terminal_text::TerminalTextContract,
};
use bootty_config::config::TerminalScrollbar;
use bootty_terminal::geometry::{TerminalSurface, ViewTransform};
use bootty_terminal::terminal_frame::RenderFrame;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarMode};
use gpui_kit::{
    App, Bounds, Context, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    InputHandler, IntoElement, ParentElement, Pixels, Render, Styled, UTF16Selection, Window,
    canvas, div, point, prelude::*, px, size,
};

use crate::frame_facts::RendererMetrics;

/// Reuse terminal paint when only sibling chrome changed.
#[derive(Clone, PartialEq, Eq)]
pub struct CachedTerminalView(pub Entity<GpuiTerminalView>);

impl IntoElement for CachedTerminalView {
    type Element = gpui_kit::AnyElement;

    fn into_element(self) -> Self::Element {
        self.0
            .cached(gpui_kit::StyleRefinement::default().size_full())
            .into_any_element()
    }
}

#[derive(Clone, Debug)]
pub struct TerminalViewInput(pub FrameInputSnapshot);

#[derive(Clone, Debug)]
pub struct TerminalScrollbarInput {
    pub transition_key: Option<String>,
    pub offset: usize,
}

/// Adapts immutable terminal scrollback to the shared scrollbar's pixel geometry.
#[derive(Clone, Default)]
struct TerminalScrollHandle {
    bounds: Rc<Cell<Bounds<Pixels>>>,
    offset: Rc<Cell<gpui_kit::Point<Pixels>>>,
    content: Rc<Cell<gpui_kit::Size<Pixels>>>,
    requested: Rc<Cell<Option<gpui_kit::Point<Pixels>>>>,
}

impl ScrollbarHandle for TerminalScrollHandle {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.bounds.get()
    }
    fn offset(&self) -> gpui_kit::Point<Pixels> {
        self.offset.get()
    }
    fn content_size(&self) -> gpui_kit::Size<Pixels> {
        self.content.get()
    }
    fn set_offset(&self, offset: gpui_kit::Point<Pixels>) {
        self.requested.set(Some(offset));
        self.offset.set(offset);
    }
}

struct TerminalPresentation {
    transition_key: Option<String>,
    surface: TerminalSurface,
    frame: Arc<RenderFrame>,
    font_size: f32,
    text_cell_height: f32,
    pixels_per_point: f32,
    text_contract: Arc<TerminalTextContract>,
    animate_cursor: bool,
    dim_inactive_cursor: bool,
}

const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(700);

/// Stateful terminal presentation and input owner, following Zed's `TerminalView` boundary.
#[allow(
    clippy::struct_excessive_bools,
    reason = "Focus, cursor animation, and frame dirtiness are independent presentation state."
)]
pub struct GpuiTerminalView {
    focus: FocusHandle,
    adapter: GpuiTerminalAdapter,
    zoom_renderer: Option<Entity<GpuiTerminalZoom>>,
    presentation: Option<TerminalPresentation>,
    interaction: Option<GpuiTerminalInteraction>,
    metrics: RendererMetrics,
    frame_facts_dirty: bool,
    input: InputAccumulator,
    marked_text: String,
    window_focused: bool,
    input_handler_focused: bool,
    cursor_blink_epoch: usize,
    cursor_blinking: bool,
    cursor_visible: bool,
    view_transform: ViewTransform,
    background_opacity: f32,
    scrollbar_mode: TerminalScrollbar,
    scrollbar: TerminalScrollHandle,
    scrollbar_epoch: usize,
}

impl GpuiTerminalView {
    pub(crate) fn set_background_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        if (self.background_opacity - opacity).abs() > f32::EPSILON {
            self.background_opacity = opacity;
            cx.notify();
        }
    }

    pub(crate) fn set_scrollbar_mode(&mut self, mode: TerminalScrollbar, cx: &mut Context<Self>) {
        if self.scrollbar_mode != mode {
            self.scrollbar_mode = mode;
            self.scrollbar.requested.set(None);
            self.scrollbar_epoch = self.scrollbar_epoch.wrapping_add(1);
            cx.notify();
        }
    }

    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            for image in this.adapter.take_render_images() {
                cx.drop_image(image, None);
            }
        })
        .detach();
        Self {
            focus: cx.focus_handle(),
            adapter: cx
                .try_global::<crate::gpui::TerminalPlatformTextSystem>()
                .map_or_else(GpuiTerminalAdapter::default, |provider| {
                    GpuiTerminalAdapter::with_platform_text_system(Arc::clone(&provider.0))
                }),
            zoom_renderer: None,
            presentation: None,
            interaction: None,
            metrics: RendererMetrics::default(),
            frame_facts_dirty: false,
            input: InputAccumulator::default(),
            marked_text: String::new(),
            window_focused: true,
            input_handler_focused: false,
            cursor_blink_epoch: 0,
            cursor_blinking: false,
            cursor_visible: true,
            view_transform: ViewTransform::IDENTITY,
            background_opacity: 1.0,
            scrollbar_mode: TerminalScrollbar::default(),
            scrollbar: TerminalScrollHandle::default(),
            scrollbar_epoch: 0,
        }
    }

    pub(crate) fn presents_frame(&self, frame: &Arc<RenderFrame>) -> bool {
        self.presentation
            .as_ref()
            .is_some_and(|presentation| Arc::ptr_eq(&presentation.frame, frame))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn matches_presentation(
        &self,
        transition_key: Option<&str>,
        surface: TerminalSurface,
        frame: &Arc<RenderFrame>,
        font_size: f32,
        text_cell_height: f32,
        pixels_per_point: f32,
        text_contract: &Arc<TerminalTextContract>,
        animate_cursor: bool,
        dim_inactive_cursor: bool,
    ) -> bool {
        self.presentation.as_ref().is_some_and(|presentation| {
            presentation.transition_key.as_deref() == transition_key
                && presentation.surface == surface
                && Arc::ptr_eq(&presentation.frame, frame)
                && presentation.font_size.to_bits() == font_size.to_bits()
                && presentation.text_cell_height.to_bits() == text_cell_height.to_bits()
                && presentation.pixels_per_point.to_bits() == pixels_per_point.to_bits()
                && Arc::ptr_eq(&presentation.text_contract, text_contract)
                && presentation.animate_cursor == animate_cursor
                && presentation.dim_inactive_cursor == dim_inactive_cursor
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        transition_key: Option<String>,
        surface: TerminalSurface,
        frame: Arc<RenderFrame>,
        font_size: f32,
        text_cell_height: f32,
        pixels_per_point: f32,
        text_contract: Arc<TerminalTextContract>,
        animate_cursor: bool,
        dim_inactive_cursor: bool,
        cx: &mut Context<Self>,
    ) {
        if self.matches_presentation(
            transition_key.as_deref(),
            surface,
            &frame,
            font_size,
            text_cell_height,
            pixels_per_point,
            &text_contract,
            animate_cursor,
            dim_inactive_cursor,
        ) {
            return;
        }
        if self
            .presentation
            .as_ref()
            .is_none_or(|presentation| presentation.transition_key != transition_key)
        {
            self.adapter.set_transition_key(transition_key.clone());
            self.scrollbar.requested.set(None);
        }
        self.metrics = RendererMetrics {
            dirty_rows: frame.stats.dirty_rows,
            text_runs: self.metrics.text_runs,
            cursor_blinking: frame.cursor.is_some_and(|cursor| cursor.blinking),
        };
        self.presentation = Some(TerminalPresentation {
            transition_key,
            surface,
            frame,
            font_size,
            text_cell_height,
            pixels_per_point,
            text_contract,
            animate_cursor,
            dim_inactive_cursor,
        });
        self.frame_facts_dirty = true;
        let cursor_blinking = self.metrics.cursor_blinking
            && animate_cursor
            && self.window_focused
            && self.marked_text.is_empty();
        if self.cursor_blinking == cursor_blinking && cursor_blinking {
            self.reset_cursor_blink(cx);
        } else {
            self.set_cursor_blinking(cursor_blinking, cx);
        }
        cx.notify();
    }

    fn set_cursor_blinking(&mut self, blinking: bool, cx: &Context<Self>) {
        if self.cursor_blinking == blinking {
            return;
        }
        self.cursor_blinking = blinking;
        if blinking {
            self.reset_cursor_blink(cx);
        } else {
            self.cursor_visible = true;
            self.cursor_blink_epoch = self.cursor_blink_epoch.wrapping_add(1);
        }
    }

    fn reset_cursor_blink(&mut self, cx: &Context<Self>) {
        self.cursor_visible = true;
        self.cursor_blink_epoch = self.cursor_blink_epoch.wrapping_add(1);
        Self::schedule_cursor_blink(self.cursor_blink_epoch, cx);
    }

    fn schedule_cursor_blink(epoch: usize, cx: &Context<Self>) {
        cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(CURSOR_BLINK_INTERVAL).await;
            let _ = weak.update(cx, |this, cx| {
                if !this.cursor_blinking || this.cursor_blink_epoch != epoch {
                    return;
                }
                this.cursor_visible = !this.cursor_visible;
                cx.notify();
                Self::schedule_cursor_blink(epoch, cx);
            });
        })
        .detach();
    }

    pub(crate) fn interaction(&self) -> Option<GpuiTerminalInteraction> {
        self.interaction
            .clone()
            .map(|interaction| interaction.with_view_transform(self.view_transform))
    }

    pub(crate) fn set_view_transform(&mut self, view: ViewTransform, cx: &mut Context<Self>) {
        if self.view_transform != view {
            self.view_transform = view;
            self.frame_facts_dirty = true;
            cx.notify();
        }
    }

    pub(crate) const fn metrics(&self) -> RendererMetrics {
        self.metrics
    }

    pub(crate) fn set_window_focused(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.window_focused == focused {
            return;
        }
        self.window_focused = focused;
        if !focused {
            self.scrollbar.requested.set(None);
            self.scrollbar_epoch = self.scrollbar_epoch.wrapping_add(1);
        }
        self.input.window_focused(focused);
        let animate_cursor = self
            .presentation
            .as_ref()
            .is_some_and(|presentation| presentation.animate_cursor);
        self.set_cursor_blinking(
            self.metrics.cursor_blinking
                && animate_cursor
                && focused
                && self.marked_text.is_empty(),
            cx,
        );
        cx.notify();
        self.emit_input(cx);
    }

    fn emit_input(&mut self, cx: &mut Context<Self>) {
        let input = self.input.drain_frame();
        if input.events.is_empty() && input.dropped_file_paths.is_empty() {
            return;
        }
        cx.emit(TerminalViewInput(input));
    }

    fn key_down(&mut self, event: &gpui_kit::KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.cursor_blinking {
            self.reset_cursor_blink(cx);
            cx.notify();
        }
        // Command/Super keys are queued by the workspace capture handler for the direct physical
        // path. Do not also publish the logical key: the legacy resolver would execute a bound
        // action (notably paste) twice from the same physical press.
        if crate::gpui_input::direct_key_input(event).is_none() {
            self.input.key_down(event);
        }
        self.emit_input(cx);
        if crate::gpui_input::terminal_owns_key_down(event) {
            cx.stop_propagation();
        }
    }

    fn key_up(&mut self, event: &gpui_kit::KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.input.key_up(event);
        self.emit_input(cx);
    }

    fn modifiers_changed(
        &mut self,
        event: &gpui_kit::ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.modifiers_changed(event);
        self.emit_input(cx);
    }
}

impl EventEmitter<TerminalViewInput> for GpuiTerminalView {}
impl EventEmitter<TerminalScrollbarInput> for GpuiTerminalView {}

impl Focusable for GpuiTerminalView {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for GpuiTerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.marked_text.is_empty()).then(|| 0..self.marked_text.encode_utf16().count())
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text.clear();
        self.input.ime_preedit(String::new(), None);
        self.emit_input(cx);
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text.clear();
        if !text.is_empty() {
            if self.cursor_blinking {
                self.reset_cursor_blink(cx);
            }
            self.input.ime_commit(text);
        }
        self.emit_input(cx);
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        new_text.clone_into(&mut self.marked_text);
        self.input.ime_preedit(new_text, new_selected_range);
        self.emit_input(cx);
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let mut bounds = self
            .interaction
            .as_ref()
            .and_then(GpuiTerminalInteraction::cursor_bounds)
            .map_or(element_bounds, super::gpui_workspace::surface_bounds);
        let cell_width = self.presentation.as_ref()?.surface.cell.width;
        bounds.origin.x =
            px(cell_width.mul_add(range_utf16.start.to_f32()?, f32::from(bounds.origin.x)));
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: gpui_kit::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// GPUI's generic `ElementInputHandler` cannot override Apple press-and-hold. Terminals need
/// raw repeats, so use the same dedicated input-handler boundary as Zed's terminal view.
struct TerminalInputHandler {
    view: Entity<GpuiTerminalView>,
    element_bounds: Bounds<Pixels>,
}

impl InputHandler for TerminalInputHandler {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        self.view.update(cx, |view, cx| {
            view.text_for_range(range, adjusted_range, window, cx)
        })
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        self.view.update(cx, |view, cx| {
            view.selected_text_range(ignore_disabled_input, window, cx)
        })
    }

    fn marked_text_range(&mut self, window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view
            .update(cx, |view, cx| view.marked_text_range(window, cx))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut App) {
        self.view
            .update(cx, |view, cx| view.unmark_text(window, cx));
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.replace_text_in_range(range, text, window, cx);
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(range, new_text, new_selected_range, window, cx);
        });
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        self.view.update(cx, |view, cx| {
            view.bounds_for_range(range_utf16, self.element_bounds, window, cx)
        })
    }

    fn character_index_for_point(
        &mut self,
        point: gpui_kit::Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<usize> {
        self.view.update(cx, |view, cx| {
            view.character_index_for_point(point, window, cx)
        })
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }
}

impl GpuiTerminalView {
    fn update_cursor_focus(&mut self, window: &Window, cx: &Context<Self>) -> bool {
        let cursor_focused = self.window_focused
            && (self.focus.is_focused(window)
                || self
                    .presentation
                    .as_ref()
                    .is_some_and(|presentation| !presentation.dim_inactive_cursor));
        if self.input_handler_focused != cursor_focused {
            self.input_handler_focused = cursor_focused;
            window.invalidate_character_coordinates();
        }
        let cursor_should_blink = self.metrics.cursor_blinking
            && self
                .presentation
                .as_ref()
                .is_some_and(|presentation| presentation.animate_cursor)
            && cursor_focused;
        let cursor_should_blink = cursor_should_blink && self.marked_text.is_empty();
        self.set_cursor_blinking(cursor_should_blink, cx);
        cursor_focused
    }

    fn render_scrollbar(
        &self,
        presentation: &TerminalPresentation,
        cx: &mut Context<Self>,
    ) -> Option<Scrollbar> {
        let scroll = presentation.frame.scrollbar;
        // The scrollbar maps the whole viewport to logical rows, independent of renderer zoom.
        let cell_height = scroll.map_or(1.0, |scroll| {
            presentation.surface.rect.height() / scroll.len.max(1).to_f32().unwrap_or(f32::MAX)
        });
        if let Some(requested) = self.scrollbar.requested.take()
            && let Some(scroll) = scroll
        {
            let offset = (-f32::from(requested.y) / cell_height)
                .round()
                .max(0.0)
                .to_usize()
                .unwrap_or(usize::MAX)
                .min(
                    usize::try_from(scroll.total.saturating_sub(scroll.len)).unwrap_or(usize::MAX),
                );
            let transition_key = presentation.transition_key.clone();
            let scrollbar_epoch = self.scrollbar_epoch;
            let entity = cx.weak_entity();
            // ScrollbarHandle has no context; dispatch its request after layout, scoped to
            // the terminal that produced it. The terminal owner resolves absolute targets.
            cx.defer(move |cx| {
                _ = entity.update(cx, |this, cx| {
                    if this.scrollbar_epoch == scrollbar_epoch
                        && this.presentation.as_ref().is_some_and(|presentation| {
                            presentation.transition_key == transition_key
                        })
                    {
                        cx.emit(TerminalScrollbarInput {
                            transition_key,
                            offset,
                        });
                    }
                });
            });
        }
        if let Some(scroll) = scroll {
            self.scrollbar.offset.set(point(
                px(0.),
                px(-(scroll.offset.to_f32().unwrap_or(f32::MAX)) * cell_height),
            ));
            self.scrollbar.content.set(size(
                px(0.),
                px(scroll.total.to_f32().unwrap_or(f32::MAX) * cell_height),
            ));
        }
        scroll
            .filter(|scroll| {
                scroll.total > scroll.len && self.scrollbar_mode != TerminalScrollbar::Never
            })
            .map(|_| {
                Scrollbar::vertical(&self.scrollbar)
                    .id(gpui_kit::SharedString::from(format!(
                        "terminal-scrollback-{}-{}",
                        presentation.transition_key.as_deref().unwrap_or_default(),
                        self.scrollbar_epoch,
                    )))
                    .viewport_from_layout()
                    .mode(match self.scrollbar_mode {
                        TerminalScrollbar::Auto | TerminalScrollbar::Never => {
                            ScrollbarMode::Scrolling
                        }
                        TerminalScrollbar::Hover => ScrollbarMode::Hover,
                        TerminalScrollbar::Always => ScrollbarMode::Always,
                    })
            })
    }

    fn render_input_handler(
        &self,
        presentation: &TerminalPresentation,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let focus = self.focus.clone();
        let entity = cx.entity();
        let scrollbar_bounds = self.scrollbar.bounds.clone();
        let benchmark = crate::switch_benchmark::enabled().then(|| {
            (
                presentation.transition_key.clone(),
                presentation.frame.text.iter().any(|ch| !ch.is_whitespace()),
            )
        });
        canvas(
            |_, _, _| (),
            move |bounds, (), window, cx| {
                scrollbar_bounds.set(bounds);
                window.handle_input(
                    &focus,
                    TerminalInputHandler {
                        view: entity.clone(),
                        element_bounds: bounds,
                    },
                    cx,
                );
                if let Some((target, has_text)) = &benchmark {
                    crate::switch_benchmark::painted(
                        target.as_deref(),
                        focus.is_focused(window) && window.is_window_active(),
                        *has_text,
                    );
                }
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }
}

impl Render for GpuiTerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cursor_focused = self.update_cursor_focus(window, cx);
        let Some(presentation) = &self.presentation else {
            self.scrollbar.requested.set(None);
            return div().size_full().into_any_element();
        };
        let scrollbar = self.render_scrollbar(presentation, cx);
        let blink = if self.cursor_visible {
            CursorBlinkPhase::visible()
        } else {
            CursorBlinkPhase::hidden()
        };
        // Keep the normal scene current: it owns transition placeholders and provides
        // the current frame while a cold zoom resolution is being prepared.
        let base = self.adapter.element(
            presentation.surface,
            &presentation.frame,
            presentation.font_size,
            presentation.text_cell_height,
            presentation.pixels_per_point,
            &presentation.text_contract,
            blink,
            cursor_focused,
            &self.marked_text,
        );
        for image in self.adapter.take_retired_images() {
            cx.drop_image(image, Some(window));
        }
        let zoomed = if self.view_transform.is_zoomed() {
            let renderer = self.zoom_renderer.get_or_insert_with(|| {
                let renderer = cx.new(|cx| {
                    let adapter = cx
                        .try_global::<crate::gpui::TerminalPlatformTextSystem>()
                        .map_or_else(GpuiTerminalAdapter::default, |provider| {
                            GpuiTerminalAdapter::with_platform_text_system(Arc::clone(&provider.0))
                        });
                    GpuiTerminalZoom::new(adapter, cx)
                });
                cx.observe(&renderer, |_, _, cx| cx.notify()).detach();
                renderer
            });
            renderer.update(cx, |renderer, cx| {
                renderer.element(
                    TerminalZoomRequest {
                        surface: presentation.surface,
                        frame: Arc::clone(base.source_frame().unwrap_or(&presentation.frame)),
                        font_size: presentation.font_size,
                        text_cell_height: presentation.text_cell_height,
                        pixels_per_point: presentation.pixels_per_point
                            * self.view_transform.raster_supersample(),
                        text_contract: Arc::clone(&presentation.text_contract),
                        cursor_blink_phase: blink,
                        cursor_focused,
                        marked_text: self.marked_text.clone(),
                    },
                    cx,
                )
            })
        } else {
            if let Some(renderer) = &self.zoom_renderer {
                renderer.update(cx, |renderer, _| renderer.clear());
            }
            None
        };
        let terminal = zoomed
            .map(|zoomed| zoomed.with_search_pulse_from(&base))
            .unwrap_or(base)
            .with_view_transform(self.view_transform)
            .with_background_opacity(self.background_opacity);
        if self.frame_facts_dirty {
            self.interaction = terminal.interaction();
            self.metrics.text_runs = terminal
                .frame()
                .commands
                .iter()
                .filter(|command| matches!(command, TerminalRenderCommand::Text(_)))
                .count();
            self.frame_facts_dirty = false;
            window.invalidate_character_coordinates();
        }

        let ime = self.render_input_handler(presentation, cx);

        div()
            .id("terminal-view")
            .relative()
            .size_full()
            // Renderer zoom may extend content past a pane, never into an adjacent terminal.
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            )
            .child(terminal)
            .child(ime)
            .children(scrollbar.map(|scrollbar| div().absolute().inset_0().child(scrollbar)))
            .into_any_element()
    }
}
