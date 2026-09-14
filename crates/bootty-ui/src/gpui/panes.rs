//! GPUI presentation for Bootty's native split-pane workspace.
//!
//! The mux binding remains the owner of topology, focus, split ratios, terminal frames, and
//! progress. This module accepts one immutable, host-neutral projection of those facts and emits
//! typed intents. It deliberately does not reconstruct or mutate the host's split-tree owner.
//!
//! Pane composition needs no custom Metal stage: GPUI clips each pane and preserves the ordered
//! primitives produced by [`GpuiTerminalElement`]. The terminal adapter's documented image and
//! subtractive-sprite limits still apply; a Metal path is only warranted if GPUI cannot express
//! one of those terminal primitives without changing its blending semantics.

use bootty_control::{Caller, CommandInvocation, CommandTarget};
use gpui_kit::component::{
    menu::{ContextMenuExt as _, PopupMenuItem},
    tooltip::Tooltip,
};
use num_traits::ToPrimitive as _;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, Empty, Hsla, IntoElement, MouseButton, ParentElement, Pixels, Point,
    SharedString, Styled, Window, div, prelude::*, px, rgba,
};

const MIN_PANE_PX: f32 = 80.0;
const MIN_DIVIDER_GRAB_PX: f32 = 8.0;
const PROGRESS_HEIGHT_PX: f32 = 2.0;
const INDETERMINATE_PROGRESS_WIDTH: f32 = 0.25;
const INDETERMINATE_PROGRESS_CYCLE_SECONDS: f64 = 1.5;

/// Host-neutral orientation of a binary pane split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneSplitDirection {
    /// The second child is positioned to the right of the first.
    Right,
    /// The second child is positioned below the first.
    Down,
}

/// Host-neutral logical rectangle in window coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PaneRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PaneRect {
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width: width.max(0.0),
            height: height.max(0.0),
        }
    }

    fn relative_to(self, area: Self) -> Self {
        Self::new(self.x - area.x, self.y - area.y, self.width, self.height)
    }

    fn expanded_grab_area(self, direction: PaneSplitDirection) -> Self {
        match direction {
            PaneSplitDirection::Right => {
                let width = self.width.max(MIN_DIVIDER_GRAB_PX);
                Self::new(
                    self.x - (width - self.width) / 2.0,
                    self.y,
                    width,
                    self.height,
                )
            }
            PaneSplitDirection::Down => {
                let height = self.height.max(MIN_DIVIDER_GRAB_PX);
                Self::new(
                    self.x,
                    self.y - (height - self.height) / 2.0,
                    self.width,
                    height,
                )
            }
        }
    }
}

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
        self.value.map(|value| f32::from(value) / 100.0)
    }
}

/// One pane's already-published terminal frame and presentation facts.
#[derive(Clone, PartialEq)]
pub struct GpuiPaneSnapshot<T = AnyElement> {
    pub id: String,
    pub rect: PaneRect,
    pub terminal: T,
    pub focused: bool,
    pub progress: Option<PaneProgress>,
}

/// One split divider projected from the binding-owned split tree.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuiPaneDividerSnapshot {
    pub path: Vec<u8>,
    pub direction: PaneSplitDirection,
    pub rect: PaneRect,
    pub area: PaneRect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GpuiPaneColors {
    pub background: Hsla,
    pub divider: Hsla,
    pub divider_hover: Hsla,
    pub focus_border: Hsla,
    pub progress_track: Hsla,
    pub progress_normal: Hsla,
    pub progress_error: Hsla,
    pub progress_warning: Hsla,
    pub empty_text: Hsla,
}

/// Complete frame-local projection for the terminal workspace.
#[derive(Clone, PartialEq)]
pub struct GpuiPaneWorkspaceSnapshot<T = AnyElement> {
    pub area: PaneRect,
    pub arrangement_target: Option<CommandTarget>,
    pub panes: Vec<GpuiPaneSnapshot<T>>,
    pub dividers: Vec<GpuiPaneDividerSnapshot>,
    pub gap: f32,
    pub corner_radius: f32,
    pub focus_border_width: f32,
    pub inactive_dim: f32,
    pub window_dim: f32,
    pub animation_seconds: f64,
    pub colors: GpuiPaneColors,
    pub empty_message: Option<String>,
}

/// User interaction returned to the binding owner.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuiPaneIntent {
    Command(CommandInvocation),
    Focus(String),
    Resize {
        path: Vec<u8>,
        ratio: f32,
        min_fraction: f32,
    },
}

type PaneIntentHandler = Arc<dyn Fn(GpuiPaneIntent, &mut Window, &mut App)>;

/// A disposable GPUI element for one complete split-pane frame.
pub struct GpuiPaneWorkspace<T = AnyElement> {
    snapshot: GpuiPaneWorkspaceSnapshot<T>,
    on_intent: PaneIntentHandler,
    single_pane_handle: bool,
}

impl<T> GpuiPaneWorkspace<T> {
    pub fn new(
        snapshot: GpuiPaneWorkspaceSnapshot<T>,
        on_intent: impl Fn(GpuiPaneIntent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            snapshot,
            on_intent: Arc::new(on_intent),
            single_pane_handle: false,
        }
    }

    /// A terminal pane embedded as one Dock leaf still needs a mux drag affordance when its Dock
    /// tab is hidden. The drag continues to emit mux commands; Dock never adopts terminal topology.
    #[must_use]
    pub const fn with_single_pane_handle(mut self, visible: bool) -> Self {
        self.single_pane_handle = visible;
        self
    }
}

#[derive(Clone)]
struct DraggedPaneDivider {
    path: Vec<u8>,
    direction: PaneSplitDirection,
    area: PaneRect,
    gap: f32,
}

impl DraggedPaneDivider {
    fn intent_at(&self, pointer: Point<Pixels>) -> Option<GpuiPaneIntent> {
        let pointer_x: f32 = pointer.x.into();
        let pointer_y: f32 = pointer.y.into();
        let (extent, offset) = match self.direction {
            PaneSplitDirection::Right => (self.area.width, pointer_x - self.area.x),
            PaneSplitDirection::Down => (self.area.height, pointer_y - self.area.y),
        };
        let splittable_extent = extent - self.gap;
        if splittable_extent <= 1.0 {
            return None;
        }
        let min_fraction = (MIN_PANE_PX / splittable_extent).clamp(0.05, 0.45);
        Some(GpuiPaneIntent::Resize {
            path: self.path.clone(),
            ratio: (offset / splittable_extent).clamp(min_fraction, 1.0 - min_fraction),
            min_fraction,
        })
    }
}

impl<T: IntoElement + 'static> IntoElement for GpuiPaneWorkspace<T> {
    type Element = gpui_kit::ViewElement<Self>;
    fn into_element(self) -> Self::Element {
        gpui_kit::ViewElement::new(self)
    }
}

impl<T: IntoElement + 'static> gpui_kit::RenderOnce for GpuiPaneWorkspace<T> {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_empty = self.snapshot.panes.is_empty();
        let has_split_panes = self.snapshot.panes.len() > 1;
        let colors = self.snapshot.colors;
        let window_dim = self.snapshot.window_dim;
        let empty_message = self.snapshot.empty_message.take();
        let preview = window.use_keyed_state("terminal-pane-drop-preview", cx, |_, _| {
            None::<(String, Option<&'static str>)>
        });
        if !cx.has_active_drag() {
            preview.update(cx, |preview, _| *preview = None);
        }
        let pane_elements = std::mem::take(&mut self.snapshot.panes)
            .into_iter()
            .map(|pane| self.render_pane(pane, has_split_panes, &preview, window, cx))
            .collect::<Vec<_>>();

        let divider_elements = self
            .snapshot
            .dividers
            .iter()
            .filter_map(|divider| self.render_divider(divider, has_split_panes))
            .collect::<Vec<_>>();

        let drop_intent = Arc::clone(&self.on_intent);
        let drag_intent = Arc::clone(&self.on_intent);
        let mut root = div()
            .id("terminal-pane-workspace")
            .debug_selector(|| "terminal-pane-workspace".to_owned())
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(colors.background)
            .on_drag_move::<DraggedPaneDivider>(move |event, window, cx| {
                if let Some(intent) = event.drag(cx).intent_at(event.event.position) {
                    drag_intent(intent, window, cx);
                }
            })
            .on_drop(move |drag: &DraggedPaneDivider, window, cx| {
                if let Some(intent) = drag.intent_at(window.mouse_position()) {
                    drop_intent(intent, window, cx);
                }
            })
            .children(pane_elements)
            .children(divider_elements);

        if let Some(message) = empty_message.filter(|_| is_empty) {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(colors.empty_text)
                    .child(message),
            );
        }
        if window_dim > 0.0 {
            root = root.child(div().absolute().inset_0().bg(rgba(black_alpha(window_dim))));
        }
        root.into_any_element()
    }
}

type PaneDropHandler = Arc<dyn Fn(&DraggedPane, &mut Window, &mut App)>;

impl<T: IntoElement + 'static> GpuiPaneWorkspace<T> {
    fn render_pane(
        &self,
        pane: GpuiPaneSnapshot<T>,
        has_split_panes: bool,
        preview: &gpui_kit::Entity<Option<(String, Option<&'static str>)>>,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let area = self.snapshot.area;
        let arrangement_target = &self.snapshot.arrangement_target;
        let on_intent = &self.on_intent;
        let single_pane_handle = self.single_pane_handle;
        let pane_rect = pane.rect.relative_to(area);
        let id = pane.id.clone();
        let focus = Arc::clone(on_intent);
        let pane_id = pane.id.clone();
        let drop_rect = pane.rect;
        let pane_hover = SharedString::from(format!("terminal-pane-hover-{pane_id}"));
        let mut surface = self.pane_surface(pane, has_split_panes);
        let pointer = window.mouse_position();
        let inside = f32::from(pointer.x) >= drop_rect.x
            && f32::from(pointer.x) < drop_rect.x + drop_rect.width
            && f32::from(pointer.y) >= drop_rect.y
            && f32::from(pointer.y) < drop_rect.y + drop_rect.height;
        if inside
            && let Some((target, edge)) = preview.read(cx).clone()
            && target == pane_id
        {
            surface = surface.child(pane_drop_preview(&pane_id, pane_rect, edge, window, cx));
        }
        let drag_preview = preview.clone();
        let preview_pane = pane_id.clone();
        let preview_target = arrangement_target.clone();
        let handle_drop = self.pane_drop_handler(&pane_id, drop_rect);
        if let Some(target) = &arrangement_target
            && (has_split_panes || single_pane_handle)
        {
            surface = surface.child(self.pane_grip(&pane_id, pane_hover, target, &handle_drop));
        }
        div()
            .id(SharedString::from(format!("terminal-pane-{pane_id}")))
            .absolute()
            .left(px(pane_rect.x))
            .top(px(pane_rect.y))
            .w(px(pane_rect.width))
            .h(px(pane_rect.height))
            // Focus at press time so terminal selection/input targets the clicked pane on
            // the same event. The workspace host owns terminal input; this intent owns pane
            // selection, so it must not stop propagation here.
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                focus(GpuiPaneIntent::Focus(id.clone()), window, cx);
            })
            .on_mouse_down(MouseButton::Middle, {
                let focus = Arc::clone(on_intent);
                let pane_id = pane_id.clone();
                move |_, window, cx| focus(GpuiPaneIntent::Focus(pane_id.clone()), window, cx)
            })
            .on_mouse_down(MouseButton::Right, {
                let focus = Arc::clone(on_intent);
                move |_, window, cx| focus(GpuiPaneIntent::Focus(pane_id.clone()), window, cx)
            })
            .on_drag_move::<DraggedPane>(move |event, _, cx| {
                let drag = event.drag(cx);
                if drag.pane == preview_pane || preview_target.as_ref() != Some(&drag.target) {
                    return;
                }
                let x = (f32::from(event.event.position.x) - drop_rect.x) / drop_rect.width.max(1.);
                let y =
                    (f32::from(event.event.position.y) - drop_rect.y) / drop_rect.height.max(1.);
                if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
                    return;
                }
                let next = Some((preview_pane.clone(), pane_drop_direction(x, y)));
                drag_preview.update(cx, |preview, cx| {
                    if *preview != next {
                        *preview = next;
                        cx.notify();
                    }
                });
            })
            .on_drop(move |drag: &DraggedPane, window, cx| {
                handle_drop(drag, window, cx);
            })
            .child(surface)
    }
    fn pane_surface(&self, pane: GpuiPaneSnapshot<T>, has_split_panes: bool) -> gpui_kit::Div {
        let pane_rect = pane.rect.relative_to(self.snapshot.area);
        let corner_radius = self.snapshot.corner_radius;
        let focus_border_width = self.snapshot.focus_border_width;
        let inactive_dim = self.snapshot.inactive_dim;
        let animation_seconds = self.snapshot.animation_seconds;
        let colors = self.snapshot.colors;
        let surface_id = pane.id.clone();
        let radius = corner_radius.clamp(0.0, pane_rect.width.min(pane_rect.height) / 2.0);
        let border_width =
            focus_border_width.clamp(0.0, pane_rect.width.min(pane_rect.height) / 2.0);
        // Each pane owns its terminal background and the inactive tint. The root background
        // only fills the gaps between panes; it must not replace either pane's surface.
        let pane_hover = SharedString::from(format!("terminal-pane-hover-{}", pane.id));
        let mut surface = div()
            .group(pane_hover)
            .debug_selector(move || format!("terminal-pane-surface-{surface_id}"))
            .relative()
            .size_full()
            .overflow_hidden()
            .rounded(px(radius))
            .bg(colors.background)
            .child(pane.terminal);

        if !pane.focused && inactive_dim > 0.0 {
            let dim_id = pane.id.clone();
            surface = surface.child(
                div()
                    .debug_selector(move || format!("terminal-pane-inactive-dim-{dim_id}"))
                    .absolute()
                    .inset_0()
                    .bg(rgba(black_alpha(inactive_dim))),
            );
        }
        if let Some(progress) = pane.progress {
            surface = surface.child(progress_element(
                pane_rect.width,
                progress,
                animation_seconds,
                colors,
            ));
        }
        if has_split_panes && pane.focused && border_width > 0.0 {
            let focus_border_id = pane.id;
            surface = surface.child(
                div()
                    .debug_selector(move || format!("terminal-pane-focus-border-{focus_border_id}"))
                    .absolute()
                    .inset_0()
                    .rounded(px(radius))
                    .border(px(border_width))
                    .border_color(colors.focus_border),
            );
        }
        surface
    }
    fn pane_drop_handler(&self, pane_id: &str, drop_rect: PaneRect) -> PaneDropHandler {
        let drop_handler = Arc::clone(&self.on_intent);
        let drop_target = self.snapshot.arrangement_target.clone();
        let drop_pane = pane_id.to_owned();

        // A drop anywhere on the pane lands on the pane, including on its own grip: the
        // centered grip overlaps the pane's top-edge drop zone.
        Arc::new(
            move |drag: &DraggedPane, window: &mut Window, cx: &mut App| {
                if drop_target.as_ref() != Some(&drag.target) || drag.pane == drop_pane {
                    return;
                }
                let pointer = window.mouse_position();
                let x = (f32::from(pointer.x) - drop_rect.x) / drop_rect.width.max(1.);
                let y = (f32::from(pointer.y) - drop_rect.y) / drop_rect.height.max(1.);
                let edge = pane_drop_direction(x, y);
                let mut args = vec![drag.pane.clone(), drop_pane.clone()];
                if let Some(direction) = edge {
                    args.push(direction.to_owned());
                }
                let command = if edge.is_some() {
                    "pane.move"
                } else {
                    "pane.swap"
                };
                drop_handler(
                    GpuiPaneIntent::Command(pane_invocation(command, args, drag.target.clone())),
                    window,
                    cx,
                );
                cx.stop_propagation();
            },
        )
    }
    fn pane_grip(
        &self,
        pane_id: &str,
        pane_hover: SharedString,
        target: &CommandTarget,
        handle_drop: &PaneDropHandler,
    ) -> impl IntoElement + use<T> {
        let colors = self.snapshot.colors;
        let drag = DraggedPane {
            pane: pane_id.to_owned(),
            target: target.clone(),
        };
        let extract = pane_invocation("pane.extract", vec![pane_id.to_owned()], target.clone());
        let handler = Arc::clone(&self.on_intent);
        let grip_drop = Arc::clone(handle_drop);
        let grip_id = format!("pane-grip-{pane_id}");
        div()
            .id(SharedString::from(grip_id.clone()))
            .debug_selector(move || grip_id)
            .absolute()
            .left(gpui_kit::relative(0.5))
            .top(px(3.))
            .ml(px(-15.))
            .w(px(30.))
            .h(px(14.))
            .rounded(px(4.))
            .text_color(colors.empty_text)
            .opacity(0.)
            .group_hover(pane_hover, |style| style.opacity(0.72))
            .hover(|style| style.bg(colors.background).opacity(1.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_grab()
            .child("•••")
            .tooltip(|window, cx| {
                Tooltip::new(
                    "Drag to an edge to move; drop in the center to swap. Right-click to extract.",
                )
                .build(window, cx)
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
            .occlude()
            .on_drop(move |drag: &DraggedPane, window, cx| {
                grip_drop(drag, window, cx);
            })
            .context_menu(move |menu, _, _| {
                let extract = extract.clone();
                let handler = Arc::clone(&handler);
                menu.item(PopupMenuItem::new("Extract into a Tab").on_click(
                    move |_, window, cx| {
                        handler(GpuiPaneIntent::Command(extract.clone()), window, cx);
                    },
                ))
            })
    }
    fn render_divider(
        &self,
        divider: &GpuiPaneDividerSnapshot,
        has_split_panes: bool,
    ) -> Option<gpui_kit::Stateful<gpui_kit::Div>> {
        let area = self.snapshot.area;
        let gap = self.snapshot.gap;
        let colors = self.snapshot.colors;
        let divider_intent = &self.on_intent;
        if !has_split_panes {
            return None;
        }
        let visual = divider.rect.relative_to(area);
        let handle = divider
            .rect
            .expanded_grab_area(divider.direction)
            .relative_to(area);
        let direction = divider.direction;
        let reset = Arc::clone(divider_intent);
        let reset_path = divider.path.clone();
        let drag = DraggedPaneDivider {
            path: divider.path.clone(),
            direction,
            area: divider.area,
            gap,
        };
        let group = SharedString::from(format!(
            "terminal-divider-{}",
            divider_path_id(&divider.path)
        ));
        let visual_id = format!("terminal-divider-visual-{}", divider_path_id(&divider.path));
        // Keep the visual separator on the root layer, above both pane surfaces. The wider
        // handle is interaction-only and must not change the separator's visual bounds.
        let handle = div()
            .id(group.clone())
            .group(group.clone())
            .absolute()
            .left(px(handle.x))
            .top(px(handle.y))
            .w(px(handle.width))
            .h(px(handle.height))
            .child(
                div()
                    .debug_selector(move || visual_id)
                    .absolute()
                    .left(px(visual.x - handle.x))
                    .top(px(visual.y - handle.y))
                    .w(px(visual.width))
                    .h(px(visual.height))
                    .bg(colors.divider)
                    .group_hover(group, move |style| style.bg(colors.divider_hover)),
            )
            .on_drag(drag, |_, _, _, cx| cx.new(|_| Empty))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, move |event, window, cx| {
                if event.click_count >= 2 {
                    reset(
                        GpuiPaneIntent::Resize {
                            path: reset_path.clone(),
                            ratio: 0.5,
                            min_fraction: 0.05,
                        },
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }
            })
            .occlude();
        Some(match direction {
            PaneSplitDirection::Right => handle.cursor_col_resize(),
            PaneSplitDirection::Down => handle.cursor_row_resize(),
        })
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

fn divider_path_id(path: &[u8]) -> String {
    if path.is_empty() {
        return "root".to_owned();
    }
    path.iter().map(u8::to_string).collect::<Vec<_>>().join("-")
}

fn black_alpha(alpha: f32) -> u32 {
    (alpha.clamp(0.0, 1.0) * 255.0)
        .round()
        .to_u32()
        .unwrap_or(0)
}

#[derive(Clone)]
struct DraggedPane {
    pane: String,
    target: CommandTarget,
}

fn pane_invocation(
    command: &str,
    arguments: Vec<String>,
    target: CommandTarget,
) -> CommandInvocation {
    let mut invocation = CommandInvocation::new(command, arguments, Caller::Keybinding);
    invocation.target = Some(target);
    invocation
}

impl gpui_kit::Render for DraggedPane {
    fn render(&mut self, _: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        use gpui_kit::component::ActiveTheme as _;
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

/// The center swaps panes; the nearest outer quarter inserts beside the target.
fn pane_drop_preview(
    pane: &str,
    rect: PaneRect,
    edge: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    use gpui_kit::component::ActiveTheme as _;
    let (x, y, width, height) = match edge {
        Some("left") => (0., 0., 0.5, 1.),
        Some("right") => (0.5, 0., 0.5, 1.),
        Some("up") => (0., 0., 1., 0.5),
        Some("down") => (0., 0.5, 1., 0.5),
        _ => (0., 0., 1., 1.),
    };
    let motion = cx.theme().motion_tokens().spring_move.with_epsilon(0.5);
    let channel = format!("pane-drop-{pane}");
    let left = gpui_kit::base::spring(
        (channel.clone(), "left"),
        px(rect.width * x),
        motion,
        window,
        cx,
    );
    let top = gpui_kit::base::spring(
        (channel.clone(), "top"),
        px(rect.height * y),
        motion,
        window,
        cx,
    );
    let width = gpui_kit::base::spring(
        (channel.clone(), "width"),
        px(rect.width * width),
        motion,
        window,
        cx,
    );
    let height = gpui_kit::base::spring(
        (channel, "height"),
        px(rect.height * height),
        motion,
        window,
        cx,
    );
    div()
        .absolute()
        .left(left)
        .top(top)
        .w(width)
        .h(height)
        .bg(cx.theme().tokens.drop_target)
        .border_1()
        .border_color(cx.theme().primary)
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().foreground)
        .child(if edge.is_some() {
            "Move pane"
        } else {
            "Swap panes"
        })
}

fn pane_drop_direction(x: f32, y: f32) -> Option<&'static str> {
    [(x, "left"), (1. - x, "right"), (y, "up"), (1. - y, "down")]
        .into_iter()
        .filter(|(distance, _)| *distance < 0.25)
        .min_by(|(a, _), (b, _)| a.total_cmp(b))
        .map(|(_, direction)| direction)
}
