//! The image viewer: one image, or several to step through, fitted to the window, with zoom and
//! pan. Rows keep small thumbnails; the viewer decodes an image at full size only while it shows
//! it.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _,
    PinchEvent, Pixels, Render, RenderImage, ScrollWheelEvent, Size, Styled as _, StyledImage as _,
    Task, Window, canvas, div, img, px, size,
};
use image::{DynamicImage, ImageReader};
use num_traits::ToPrimitive as _;
use std::{cell::Cell, io::Cursor, path::Path, rc::Rc, sync::Arc};

/// Rows show images at most this many pixels across their shorter side: a 5rem thumbnail at
/// twice its size.
const THUMBNAIL: u32 = 256;
/// The closest the viewer zooms, as a multiple of the fitted size.
const MAX_ZOOM: f32 = 8.;
/// Image files larger than this get no preview; attachments can't be larger images anyway.
const SOURCE_BYTES: u64 = 16 * 1024 * 1024;

/// A small image for rows, and the encoded image it came from, which the viewer decodes at full
/// size when it shows it.
#[derive(Clone)]
pub struct ImagePreview {
    pub thumbnail: Arc<RenderImage>,
    source: Arc<[u8]>,
}

impl PartialEq for ImagePreview {
    fn eq(&self, other: &Self) -> bool {
        self.thumbnail == other.thumbnail
    }
}

impl ImagePreview {
    /// The thumbnail of `source`, an image in any format the image crate reads.
    #[must_use]
    pub fn new(source: Arc<[u8]>) -> Option<Self> {
        let image = decode(&source)?;
        let shorter = image.width().min(image.height()).max(1);
        let thumbnail = if shorter > THUMBNAIL {
            let scale = f64::from(THUMBNAIL) / f64::from(shorter);
            let scaled = |side: u32| (f64::from(side) * scale).ceil().to_u32().unwrap_or(1);
            image.thumbnail(scaled(image.width()), scaled(image.height()))
        } else {
            image
        };
        Some(Self {
            thumbnail: render_image(thumbnail),
            source,
        })
    }

    /// The image in the file at `path`.
    #[must_use]
    pub fn from_file(path: &Path) -> Option<Self> {
        if std::fs::metadata(path).ok()?.len() > SOURCE_BYTES {
            return None;
        }
        Self::new(std::fs::read(path).ok()?.into())
    }
}

fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().ok()
}

fn render_image(image: DynamicImage) -> Arc<RenderImage> {
    let mut pixels = image.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Arc::new(RenderImage::new([image::Frame::new(pixels)]))
}

/// One image the viewer can show, with the name shown under it.
#[derive(Clone)]
pub struct ViewedImage {
    pub name: String,
    pub preview: ImagePreview,
}

/// Opens the viewer on `images[index]`; the arrow keys step through the rest. Root's dialog gives
/// it Escape and focus restoration.
pub fn open(images: Vec<ViewedImage>, index: usize, window: &mut Window, cx: &mut App) {
    if images.is_empty() {
        return;
    }
    let viewer = cx.new(|cx| ImageViewer::new(images, index, cx));
    let focus = viewer.read(cx).focus.clone();
    window.open_dialog(cx, move |dialog, window, cx| {
        let viewport = window.viewport_size();
        let frame = viewer.read(cx).frame(window);
        dialog
            .w(frame.width)
            .close_button(false)
            .p_0()
            .bg(gpui_kit::Hsla::transparent_black())
            .border_0()
            .shadow_none()
            .title(controls(&viewer, cx))
            .margin_top(px(((f32::from(viewport.height) - f32::from(frame.height))
                / 2.
                - f32::from(gpui_kit::rems(2.).to_pixels(window.rem_size())))
            .max(0.)))
            .footer({
                let index = viewer.read(cx).index;
                div()
                    .debug_selector(move || format!("image-viewer-showing:{index}"))
                    .w_full()
                    .py_2()
                    .text_xs()
                    .text_center()
                    .text_color(cx.theme().foreground.opacity(0.8))
                    .child(viewer.read(cx).caption())
            })
            .content({
                let viewer = viewer.clone();
                move |content, _, _| content.child(viewer.clone())
            })
    });
    focus.focus(window, cx);
}

/// Stepping, zoom and close, above the image.
fn controls(viewer: &Entity<ImageViewer>, cx: &App) -> gpui_kit::Div {
    let state = viewer.read(cx);
    let several = state.images.len() > 1;
    let zoomed = state.zoom > 1.;
    let button = |id: &'static str, icon: IconName, label: &'static str| {
        Button::new(id)
            .small()
            .ghost()
            .icon(icon)
            .accessibility_label(label)
            .tooltip(label)
            .debug_selector(move || id.into())
    };
    let act = |apply: fn(&mut ImageViewer, &mut Context<ImageViewer>)| {
        let viewer = viewer.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            viewer.update(cx, |viewer, cx| {
                apply(viewer, cx);
                cx.notify();
            });
        }
    };
    div()
        .flex()
        .items_center()
        .gap_1()
        .when(several, |row| {
            row.child(
                button(
                    "image-viewer-previous",
                    IconName::ChevronLeft,
                    "Previous image",
                )
                .on_click(act(|viewer, cx| viewer.step(false, cx))),
            )
            .child(
                button("image-viewer-next", IconName::ChevronRight, "Next image")
                    .on_click(act(|viewer, cx| viewer.step(true, cx))),
            )
        })
        .child(div().flex_1())
        .child(
            button("image-viewer-zoom-out", IconName::Minus, "Zoom out")
                .disabled(!zoomed)
                .on_click(act(|viewer, _| viewer.zoom_centered(0.8))),
        )
        .child(
            button("image-viewer-zoom-in", IconName::Plus, "Zoom in")
                .disabled(state.zoom >= MAX_ZOOM)
                .on_click(act(|viewer, _| viewer.zoom_centered(1.25))),
        )
        .child(
            Button::new("close-image-preview")
                .small()
                .ghost()
                .icon(IconName::Close)
                .accessibility_label("Close image preview")
                .debug_selector(|| "close-image-preview".into())
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
}

struct ImageViewer {
    images: Vec<ViewedImage>,
    index: usize,
    /// The shown image decoded at full size, once ready.
    full: Option<(usize, Arc<RenderImage>)>,
    /// A multiple of the fitted size, from 1 to `MAX_ZOOM`.
    zoom: f32,
    /// Where the image's top left sits in the frame, in pixels; never past the frame's edges.
    pan: (f32, f32),
    /// The frame's fitted size, as last laid out.
    fitted: (f32, f32),
    /// The frame's place in the window, as last painted, so pointer positions map onto it.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Where a drag that pans the image last was.
    drag: Option<(f32, f32)>,
    focus: FocusHandle,
    decode: Task<()>,
}

impl ImageViewer {
    fn new(images: Vec<ViewedImage>, index: usize, cx: &mut Context<Self>) -> Self {
        cx.on_release(|viewer: &mut Self, cx| {
            if let Some((_, image)) = viewer.full.take() {
                cx.drop_image(image, None);
            }
        })
        .detach();
        let mut viewer = Self {
            index: index.min(images.len().saturating_sub(1)),
            images,
            full: None,
            zoom: 1.,
            pan: (0., 0.),
            fitted: (1., 1.),
            bounds: Rc::default(),
            drag: None,
            focus: cx.focus_handle(),
            decode: Task::ready(()),
        };
        viewer.load(cx);
        viewer
    }

    fn current(&self) -> Option<&ViewedImage> {
        self.images.get(self.index)
    }

    fn caption(&self) -> String {
        let name = self.current().map_or("", |image| image.name.as_str());
        if self.images.len() > 1 {
            format!(
                "{name} · {} of {}",
                self.index.saturating_add(1),
                self.images.len()
            )
        } else {
            name.to_owned()
        }
    }

    /// The largest size with the image's proportions that fits the window, leaving room for the
    /// controls and caption.
    fn frame(&self, window: &Window) -> Size<Pixels> {
        let pixels = self
            .current()
            .map_or_else(Size::default, |image| image.preview.thumbnail.size(0));
        let ratio =
            pixels.width.0.to_f32().unwrap_or(1.) / pixels.height.0.to_f32().unwrap_or(1.).max(1.);
        let rem = f32::from(gpui_kit::rems(1.).to_pixels(window.rem_size()));
        let viewport = window.viewport_size();
        let width = 4f32
            .mul_add(-rem, f32::from(viewport.width))
            .min(8f32.mul_add(-rem, f32::from(viewport.height)) * ratio)
            .max(1.);
        size(px(width), px(width / ratio.max(0.001)))
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.images.len();
        if count < 2 {
            return;
        }
        self.index = if forward {
            self.index.saturating_add(1).checked_rem(count).unwrap_or(0)
        } else {
            self.index
                .checked_sub(1)
                .unwrap_or_else(|| count.saturating_sub(1))
        };
        self.zoom = 1.;
        self.pan = (0., 0.);
        self.load(cx);
    }

    /// Decodes the shown image at full size, letting the previous one go.
    fn load(&mut self, cx: &mut Context<Self>) {
        let index = self.index;
        if self.full.as_ref().is_some_and(|(shown, _)| *shown == index) {
            return;
        }
        if let Some((_, image)) = self.full.take() {
            cx.drop_image(image, None);
        }
        let Some(source) = self.current().map(|image| image.preview.source.clone()) else {
            return;
        };
        self.decode = cx.spawn(async move |viewer, cx| {
            let image = cx
                .background_executor()
                .spawn(async move { decode(&source).map(render_image) })
                .await;
            _ = viewer.update(cx, |viewer, cx| {
                if let Some(image) = image.filter(|_| viewer.index == index) {
                    viewer.full = Some((index, image));
                    cx.notify();
                }
            });
        });
    }

    /// Zooms by `factor` keeping the image point under `anchor`, in frame pixels, where it is.
    fn zoom_at(&mut self, factor: f32, anchor: (f32, f32)) {
        let zoom = (self.zoom * factor).clamp(1., MAX_ZOOM);
        if !zoom.is_finite() {
            return;
        }
        let ratio = zoom / self.zoom;
        self.pan = (
            (anchor.0 - self.pan.0).mul_add(-ratio, anchor.0),
            (anchor.1 - self.pan.1).mul_add(-ratio, anchor.1),
        );
        self.zoom = zoom;
        self.clamp_pan();
    }

    fn zoom_centered(&mut self, factor: f32) {
        self.zoom_at(factor, (self.fitted.0 / 2., self.fitted.1 / 2.));
    }

    fn pan_by(&mut self, x: f32, y: f32) {
        self.pan = (self.pan.0 + x, self.pan.1 + y);
        self.clamp_pan();
    }

    fn clamp_pan(&mut self) {
        let (width, height) = self.fitted;
        self.pan = (
            self.pan.0.clamp(width.mul_add(-self.zoom, width), 0.),
            self.pan.1.clamp(height.mul_add(-self.zoom, height), 0.),
        );
    }

    /// `position` in the window, in frame pixels.
    fn local(&self, position: gpui_kit::Point<Pixels>) -> (f32, f32) {
        let origin = self.bounds.get().origin;
        (
            f32::from(position.x) - f32::from(origin.x),
            f32::from(position.y) - f32::from(origin.y),
        )
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "left" => self.step(false, cx),
            "right" => self.step(true, cx),
            "=" | "+" => self.zoom_centered(1.25),
            "-" => self.zoom_centered(0.8),
            "0" => {
                self.zoom = 1.;
                self.pan = (0., 0.);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Control or Command with the wheel zooms; the wheel alone pans a zoomed image.
    fn scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(window.line_height());
        if event.modifiers.control || event.modifiers.platform {
            self.zoom_at(
                (f32::from(delta.y) * 0.01).exp(),
                self.local(event.position),
            );
        } else if self.zoom > 1. {
            self.pan_by(f32::from(delta.x), f32::from(delta.y));
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn pinch(&mut self, event: &PinchEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.delta.is_finite() {
            self.zoom_at((1. + event.delta).max(0.01), self.local(event.position));
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// A press starts a drag; a double click zooms in where it lands, or back out to fit.
    fn press(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let at = self.local(event.position);
        if event.click_count == 2 {
            if self.zoom > 1. {
                self.zoom = 1.;
                self.pan = (0., 0.);
            } else {
                self.zoom_at(2., at);
            }
            cx.notify();
        }
        self.drag = Some(at);
        cx.stop_propagation();
    }

    fn drag(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(last) = self.drag.filter(|_| event.dragging()) else {
            self.drag = None;
            return;
        };
        let at = self.local(event.position);
        self.drag = Some(at);
        if self.zoom > 1. {
            self.pan_by(at.0 - last.0, at.1 - last.1);
            cx.notify();
        }
    }
}

impl Render for ImageViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.frame(window);
        self.fitted = (f32::from(frame.width), f32::from(frame.height));
        self.clamp_pan();
        let image = self
            .full
            .as_ref()
            .filter(|(shown, _)| *shown == self.index)
            .map(|(_, image)| image.clone())
            .or_else(|| self.current().map(|image| image.preview.thumbnail.clone()));
        let bounds = self.bounds.clone();
        div()
            .id("attachment-image-viewer")
            .debug_selector(|| "attachment-image-viewer".into())
            .track_focus(&self.focus)
            .relative()
            .overflow_hidden()
            .w(frame.width)
            .h(frame.height)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_pinch(cx.listener(Self::pinch))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press))
            .on_mouse_move(cx.listener(Self::drag))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|viewer, _, _, _| viewer.drag = None),
            )
            .child(
                canvas(move |painted, _, _| bounds.set(painted), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .debug_selector(|| "image-viewer-image".into())
                    .absolute()
                    .left(px(self.pan.0))
                    .top(px(self.pan.1))
                    .w(px(self.fitted.0 * self.zoom))
                    .h(px(self.fitted.1 * self.zoom))
                    .children(image.map(|image| {
                        img(image)
                            .size_full()
                            .object_fit(gpui_kit::ObjectFit::Contain)
                    })),
            )
    }
}
