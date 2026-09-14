use num_traits::ToPrimitive as _;

use crate::gpui::{InputEvent, Modifiers, Point, PointerButton};
use bootty_terminal::geometry::{SurfacePoint, SurfaceRect, TerminalSurface, ViewTransform};

use bootty_terminal::terminal_engine::TerminalSelectionEvent;

const fn surface_point(pos: Point) -> SurfacePoint {
    SurfacePoint { x: pos.x, y: pos.y }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TerminalSelectionAction {
    Begin(TerminalSelectionEvent),
    Update(TerminalSelectionEvent),
    Scroll(isize),
    End(Option<TerminalSelectionEvent>),
}

#[derive(Clone, Copy)]
pub(super) struct TerminalSelectionRouteContext<'a> {
    pub(super) surface: Option<TerminalSurface>,
    pub(super) view: ViewTransform,
    pub(super) mouse_tracking: bool,
    pub(super) frame_modifiers: Modifiers,
    pub(super) chrome_handle_rects: &'a [SurfaceRect],
}

#[derive(Debug, Default)]
pub(super) struct TerminalSelectionRouter {
    active: bool,
    drag_pos: Option<Point>,
    pending_start: Option<TerminalSelectionEvent>,
    passthrough_active: bool,
}

impl TerminalSelectionRouter {
    pub(super) fn route_events(
        &mut self,
        events: Vec<InputEvent>,
        context: TerminalSelectionRouteContext<'_>,
    ) -> (Vec<InputEvent>, Vec<TerminalSelectionAction>) {
        let TerminalSelectionRouteContext {
            surface,
            view,
            mouse_tracking,
            frame_modifiers,
            chrome_handle_rects,
        } = context;
        let Some(surface) = surface else {
            *self = Self::default();
            return (events, Vec::new());
        };

        let mut terminal_events = Vec::with_capacity(events.len());
        let mut selection_actions = Vec::new();
        let update_drag = |actions: &mut Vec<TerminalSelectionAction>, pos| {
            if selection_drag_scroll_delta(surface, pos) == 0
                && let Some(selection_event) =
                    terminal_selection_event_clamped(surface, view, pos, frame_modifiers.alt)
            {
                actions.push(TerminalSelectionAction::Update(selection_event));
            }
        };
        for event in events {
            match event {
                event @ InputEvent::PointerButton {
                    position: pos,
                    button: PointerButton::Left,
                    pressed: true,
                    click_count,
                    modifiers,
                } if surface.rect.contains(surface_point(pos))
                    && !chrome_handle_rects
                        .iter()
                        .any(|rect| rect.contains(surface_point(pos))) =>
                {
                    if self.begin_pointer_selection(
                        context,
                        pos,
                        click_count,
                        modifiers,
                        &mut selection_actions,
                    ) {
                        continue;
                    }
                    terminal_events.push(event);
                }
                InputEvent::PointerMoved(pos) if self.active => {
                    self.drag_pos = Some(pos);
                    update_drag(&mut selection_actions, pos);
                    if self.passthrough_active {
                        terminal_events.push(InputEvent::PointerMoved(pos));
                    }
                }
                InputEvent::PointerMoved(pos) if self.pending_start.is_some() => {
                    if mouse_tracking {
                        self.pending_start = None;
                        terminal_events.push(InputEvent::PointerMoved(pos));
                    } else if let Some(start) = self.pending_start.take() {
                        self.active = true;
                        self.passthrough_active = true;
                        self.drag_pos = Some(pos);
                        selection_actions.push(TerminalSelectionAction::Begin(start));
                        update_drag(&mut selection_actions, pos);
                        terminal_events.push(InputEvent::PointerMoved(pos));
                    }
                }
                event @ InputEvent::PointerButton {
                    position: pos,
                    button: PointerButton::Left,
                    pressed: false,
                    modifiers,
                    ..
                } if self.active => {
                    let selection_event = terminal_selection_event_clamped(
                        surface,
                        view,
                        pos,
                        modifiers.alt || frame_modifiers.alt,
                    );
                    selection_actions.push(TerminalSelectionAction::End(selection_event));
                    self.drag_pos = None;
                    self.pending_start = None;
                    if self.passthrough_active {
                        terminal_events.push(event);
                    }
                    self.passthrough_active = false;
                    self.active = false;
                }
                event @ InputEvent::PointerButton {
                    button: PointerButton::Left,
                    pressed: false,
                    ..
                } if self.pending_start.is_some() => {
                    self.pending_start = None;
                    terminal_events.push(event);
                }
                event => terminal_events.push(event),
            }
        }

        (terminal_events, selection_actions)
    }

    fn begin_pointer_selection(
        &mut self,
        context: TerminalSelectionRouteContext<'_>,
        pos: Point,
        click_count: usize,
        modifiers: Modifiers,
        selection_actions: &mut Vec<TerminalSelectionAction>,
    ) -> bool {
        let TerminalSelectionRouteContext {
            surface,
            view,
            mouse_tracking,
            frame_modifiers,
            ..
        } = context;
        let Some(surface) = surface else {
            return false;
        };
        let rectangle = modifiers.alt || frame_modifiers.alt;
        let selecting_with_modifier = modifiers.shift || frame_modifiers.shift;
        if selecting_with_modifier {
            if let Some(selection_event) = terminal_selection_event(surface, view, pos, rectangle) {
                self.drag_pos = None;
                self.pending_start = None;
                self.passthrough_active = false;
                self.active = true;
                selection_actions.push(TerminalSelectionAction::Begin(selection_event));
                return true;
            }
        } else if click_count >= 2 && !mouse_tracking {
            // GPUI already resolved the platform click sequence. Replay the
            // sequence into Ghostty's gesture state so double- and triple-clicks
            // select words and lines while the first click remains a normal terminal
            // click. Applications that report the mouse own every click; only
            // shift-click overrides them.
            if let Some(selection_event) = terminal_selection_event(surface, view, pos, rectangle) {
                self.drag_pos = None;
                self.pending_start = None;
                self.passthrough_active = false;
                self.active = true;
                for _ in 0..click_count.min(3) {
                    selection_actions.push(TerminalSelectionAction::Begin(selection_event));
                }
                return true;
            }
        } else if !mouse_tracking {
            self.pending_start = terminal_selection_event(surface, view, pos, rectangle);
        }
        false
    }

    pub(super) fn autoscroll_actions(
        &self,
        surface: Option<TerminalSurface>,
        view: ViewTransform,
        modifiers: Modifiers,
    ) -> Vec<TerminalSelectionAction> {
        if !self.active {
            return Vec::new();
        }
        let (Some(surface), Some(pos)) = (surface, self.drag_pos) else {
            return Vec::new();
        };

        let delta = selection_drag_scroll_delta(surface, pos);
        if delta == 0 {
            return Vec::new();
        }

        let mut actions = vec![TerminalSelectionAction::Scroll(delta)];
        if let Some(selection_event) =
            terminal_selection_event_clamped(surface, view, pos, modifiers.alt)
        {
            actions.push(TerminalSelectionAction::Update(selection_event));
        }
        actions
    }
}

fn terminal_selection_event(
    surface: TerminalSurface,
    view: ViewTransform,
    pos: Point,
    rectangle: bool,
) -> Option<TerminalSelectionEvent> {
    let position = surface.relative_position(view.inverse_point(surface_point(pos)))?;
    Some(TerminalSelectionEvent {
        surface,
        position,
        rectangle,
    })
}

fn previous_inside_coordinate(min: f32, max: f32) -> f32 {
    if max <= min {
        return min;
    }

    let inset = max.abs().max(1.0) * f32::EPSILON * 8.0;
    (max - inset).max(min)
}

fn terminal_grid_edge(surface: TerminalSurface) -> SurfacePoint {
    let geometry = surface.geometry();
    let right = f32::mul_add(
        f32::from(geometry.cols),
        surface.cell.width,
        surface.rect.min_x + surface.padding.left,
    );
    let bottom = f32::mul_add(
        f32::from(geometry.rows),
        surface.cell.height,
        surface.rect.min_y + surface.padding.top,
    );
    SurfacePoint {
        x: right.min(surface.rect.max_x),
        y: bottom.min(surface.rect.max_y),
    }
}

pub(super) fn terminal_selection_event_clamped(
    surface: TerminalSurface,
    view: ViewTransform,
    pos: Point,
    rectangle: bool,
) -> Option<TerminalSelectionEvent> {
    let pos = view.inverse_point(surface_point(pos));
    let grid_edge = terminal_grid_edge(surface);
    let max_x = previous_inside_coordinate(surface.rect.min_x, grid_edge.x);
    let max_y = previous_inside_coordinate(surface.rect.min_y, grid_edge.y);
    let pos = Point {
        x: pos.x.clamp(surface.rect.min_x, max_x),
        y: pos.y.clamp(surface.rect.min_y, max_y),
    };
    terminal_selection_event(surface, ViewTransform::IDENTITY, pos, rectangle)
}

pub(super) fn selection_drag_scroll_delta(surface: TerminalSurface, pos: Point) -> isize {
    let top = surface.rect.min_y;
    let bottom = terminal_grid_edge(surface).y;
    let hot_zone = (surface.cell.height * 0.35)
        .clamp(4.0, 12.0)
        .min(((bottom - top) / 2.0).max(0.0));

    if pos.y < top {
        selection_drag_scroll_rows(surface, top - pos.y).saturating_neg()
    } else if pos.y <= top + hot_zone {
        selection_drag_scroll_rows(surface, top + hot_zone - pos.y).saturating_neg()
    } else if pos.y > bottom {
        selection_drag_scroll_rows(surface, pos.y - bottom)
    } else if pos.y >= bottom - hot_zone {
        selection_drag_scroll_rows(surface, pos.y - (bottom - hot_zone))
    } else {
        0
    }
}

fn selection_drag_scroll_rows(surface: TerminalSurface, distance: f32) -> isize {
    let rows = (distance / surface.cell.height)
        .ceil()
        .max(1.0)
        .to_isize()
        .unwrap_or(isize::MAX);
    rows.min(isize::try_from(surface.geometry().rows.max(1)).unwrap_or(isize::MAX))
}
