//! One terminal pane's content and host-owned drag handle. Dock owns all layout and hit testing.
use bootty_control::{Caller, CommandInvocation, CommandTarget};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::AnyDrag,
    menu::{ContextMenuExt as _, PopupMenuItem},
};
use gpui_kit::{
    AnyElement, App, Hsla, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window,
    div, prelude::*, px, rgba,
};
use num_traits::ToPrimitive as _;
use std::sync::Arc;

const PROGRESS_HEIGHT_PX: f32 = 2.;
const INDETERMINATE_PROGRESS_WIDTH: f32 = 0.25;
const INDETERMINATE_PROGRESS_CYCLE_SECONDS: f64 = 1.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneProgressState {
    Normal,
    Error,
    Indeterminate,
    Warning,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneProgress {
    pub state: PaneProgressState,
    pub value: Option<u8>,
}
impl PaneProgress {
    fn fraction(self) -> Option<f32> {
        self.value.map(|value| f32::from(value) / 100.)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GpuiPaneColors {
    pub background: Hsla,
    pub focus_border: Hsla,
    pub progress_track: Hsla,
    pub progress_normal: Hsla,
    pub progress_error: Hsla,
    pub progress_warning: Hsla,
}

#[derive(Clone, PartialEq)]
pub struct GpuiPaneSnapshot<T = AnyElement> {
    pub id: String,
    pub target: Option<CommandTarget>,
    pub terminal: T,
    pub focused: bool,
    pub progress: Option<PaneProgress>,
    pub width: f32,
    pub corner_radius: f32,
    pub focus_border_width: f32,
    pub inactive_dim: f32,
    pub window_dim: f32,
    pub animation_seconds: f64,
    pub colors: GpuiPaneColors,
}

type PaneCommandHandler = Arc<dyn Fn(CommandInvocation, &mut Window, &mut App)>;

pub struct GpuiPane<T = AnyElement> {
    snapshot: GpuiPaneSnapshot<T>,
    on_command: PaneCommandHandler,
}
impl<T> GpuiPane<T> {
    pub fn new(
        snapshot: GpuiPaneSnapshot<T>,
        on_command: impl Fn(CommandInvocation, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            snapshot,
            on_command: Arc::new(on_command),
        }
    }
}
impl<T: IntoElement + 'static> IntoElement for GpuiPane<T> {
    type Element = gpui_kit::ViewElement<Self>;
    fn into_element(self) -> Self::Element {
        gpui_kit::ViewElement::new(self)
    }
}
impl<T: IntoElement + 'static> gpui_kit::RenderOnce for GpuiPane<T> {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let pane = self.snapshot;
        let pane_id = pane.id.clone();
        let mut surface = div()
            .id(SharedString::from(format!("terminal-pane-{}", pane.id)))
            .debug_selector(move || format!("terminal-pane-{pane_id}"))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(pane.colors.background)
            .rounded(px(pane.corner_radius))
            .child(pane.terminal);
        let dim = if pane.focused {
            pane.window_dim
        } else {
            pane.inactive_dim.max(pane.window_dim)
        };
        if dim > 0. {
            surface = surface.child(div().absolute().inset_0().bg(rgba(black_alpha(dim))));
        }
        if let Some(progress) = pane.progress {
            surface = surface.child(progress_element(
                pane.width,
                progress,
                pane.animation_seconds,
                pane.colors,
            ));
        }
        if pane.focused && pane.focus_border_width > 0. {
            surface = surface.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(pane.corner_radius))
                    .border(px(pane.focus_border_width))
                    .border_color(pane.colors.focus_border),
            );
        }
        if let Some(target) = pane.target {
            let handle_id = format!("pane-drag-handle-{}", pane.id);
            let drag = DraggedPane {
                pane: pane.id.clone(),
                target: target.clone(),
            };
            let extract = pane_invocation("pane.extract", vec![pane.id], target);
            let on_command = self.on_command;
            surface = surface.child(
                div().id(SharedString::from(handle_id.clone())).debug_selector(move || handle_id)
                    .absolute().top_0().right_0().cursor_grab()
                    .child(Button::new("pane-drag-button").label("⋮⋮").ghost().xsmall()
                        .accessibility_label("Move terminal pane")
                        .tooltip("Drag to an edge to move; drop in the center to swap. Right-click to extract."))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_drag(AnyDrag::new(drag.clone()), move |_, _, _, cx| cx.new(|_| drag.clone()))
                    .context_menu(move |menu, _, _| {
                        let extract = extract.clone(); let on_command = on_command.clone();
                        menu.item(PopupMenuItem::new("Extract into a Tab").on_click(move |_, window, cx| on_command(extract.clone(), window, cx)))
                    })
            );
        }
        surface
    }
}

#[derive(Clone)]
pub struct DraggedPane {
    pub pane: String,
    pub target: CommandTarget,
}

#[must_use]
pub fn pane_invocation(
    command: &str,
    arguments: Vec<String>,
    target: CommandTarget,
) -> CommandInvocation {
    let mut invocation = CommandInvocation::new(command, arguments, Caller::Internal);
    invocation.target = Some(target);
    invocation
}

impl gpui_kit::Render for DraggedPane {
    fn render(&mut self, _: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .text_color(cx.theme().foreground)
            .text_sm()
            .shadow_md()
            .child("Terminal pane")
    }
}
fn progress_element(
    pane_width: f32,
    progress: PaneProgress,
    animation_seconds: f64,
    colors: GpuiPaneColors,
) -> gpui_kit::Div {
    let (left, width) = if progress.state == PaneProgressState::Indeterminate {
        let width = pane_width * INDETERMINATE_PROGRESS_WIDTH;
        let phase = (animation_seconds / INDETERMINATE_PROGRESS_CYCLE_SECONDS)
            .fract()
            .to_f32()
            .unwrap_or(0.0);
        let travel = 1.0 - phase.mul_add(2.0, -1.0).abs();
        ((pane_width - width).max(0.0) * travel, width)
    } else {
        let width = progress
            .fraction()
            .map_or(0.0, |fraction| pane_width * fraction.clamp(0.0, 1.0));
        (0.0, width)
    };
    let color = match progress.state {
        PaneProgressState::Normal | PaneProgressState::Indeterminate => colors.progress_normal,
        PaneProgressState::Error => colors.progress_error,
        PaneProgressState::Warning => colors.progress_warning,
    };
    div()
        .absolute()
        .left_0()
        .top_0()
        .w_full()
        .h(px(PROGRESS_HEIGHT_PX))
        .bg(colors.progress_track)
        .child(
            div()
                .absolute()
                .left(px(left))
                .top_0()
                .w(px(width))
                .h_full()
                .bg(color),
        )
}

fn black_alpha(alpha: f32) -> u32 {
    (alpha.clamp(0., 1.) * 255.).round().to_u32().unwrap_or(0)
}
