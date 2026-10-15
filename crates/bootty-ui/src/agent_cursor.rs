//! The agent cursor: a small pointer shown where an agent clicks, moves or scrolls, so computer use
//! is visible. Only macOS needs it, because input goes straight to the app there and the real
//! pointer stays put; on Windows and X11 the real pointer moves to each point. It never takes
//! focus, and its window starts just past the target so it never covers the pixel the agent acts
//! on.
//!
//! Deliberate limit: the cursor appears at each point instead of gliding, because GPUI cannot
//! move a window; add a native move when the cursor should animate.
use std::{
    sync::{Mutex, PoisonError},
    thread,
    time::{Duration, Instant},
};

use bootty_computer::ComputerAction;
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, Global, IntoElement, ParentElement as _, Render,
    Styled as _, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, component::ActiveTheme as _, div, point, px, size,
};

const SIDE: f32 = 24.0;
/// The arrow's tip sits this far inside the lucide icon box.
const TIP_INSET: f32 = 2.0;
const VISIBLE_FOR: Duration = Duration::from_millis(1500);
/// How long a shown cursor may stay on screen, including the frame that opens it and the
/// deferred native close.
const LINGER: Duration = Duration::from_millis(1800);

struct Points {
    /// The next point to show, in desktop points.
    pending: Option<(f64, f64)>,
    /// The latest point and when it was published.
    last: Option<(f64, f64, Instant)>,
}

static POINTS: Mutex<Points> = Mutex::new(Points {
    pending: None,
    last: None,
});

/// Run `act`, then show the cursor where a delivered pointer action landed. The window list counts
/// the cursor as cover, so a previous cursor that would cover the point is waited out first. Call
/// it from a worker thread, never the UI thread.
pub fn pointing<T, E>(
    action: &ComputerAction,
    repaint: &dyn Fn(),
    act: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let (ComputerAction::Click { x, y, .. }
    | ComputerAction::Move { x, y }
    | ComputerAction::Scroll { x, y, .. }) = *action
    else {
        return act();
    };
    if !cfg!(target_os = "macos") {
        return act();
    }
    let covering = POINTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .last
        .filter(|&(last_x, last_y, _)| covers(last_x, last_y, x, y))
        .and_then(|(_, _, at)| at.checked_add(LINGER))
        .and_then(|until| until.checked_duration_since(Instant::now()));
    if let Some(wait) = covering {
        thread::sleep(wait);
    }
    let result = act()?;
    let mut points = POINTS.lock().unwrap_or_else(PoisonError::into_inner);
    points.pending = Some((x, y));
    points.last = Some((x, y, Instant::now()));
    drop(points);
    repaint();
    Ok(result)
}

/// Whether the cursor shown for `(cursor_x, cursor_y)` covers `(x, y)`.
fn covers(cursor_x: f64, cursor_y: f64, x: f64, y: f64) -> bool {
    let side = f64::from(SIDE);
    let left = cursor_x + 1.0;
    let top = cursor_y + 1.0;
    (left..=left + side).contains(&x) && (top..=top + side).contains(&y)
}

/// Show the latest pending point. The workspace calls this every frame.
pub fn sync_pending(cx: &mut App) {
    let Some((x, y)) = POINTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .pending
        .take()
    else {
        return;
    };
    let existing = cx.try_global::<Cursor>().map(|cursor| cursor.0.clone());
    let cursor = existing.unwrap_or_else(|| {
        let cursor = cx.new(|_| AgentCursor::default());
        cx.set_global(Cursor(cursor.clone()));
        cursor
    });
    cursor.update(cx, |cursor, cx| cursor.show(x, y, cx));
}

struct Cursor(Entity<AgentCursor>);
impl Global for Cursor {}

#[derive(Default)]
struct AgentCursor {
    window: Option<WindowHandle<CursorView>>,
    shown: u64,
}

impl AgentCursor {
    fn show(&mut self, x: f64, y: f64, cx: &mut Context<Self>) {
        self.close(cx);
        let origin = point(px(gpui_points(x) + 1.0), px(gpui_points(y) + 1.0));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                origin,
                size(px(SIDE), px(SIDE)),
            ))),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..WindowOptions::default()
        };
        self.shown = self.shown.wrapping_add(1);
        let shown = self.shown;
        // A closing cursor lingers until GPUI's deferred native close, so each title is unique.
        let title = format!("Bootty Agent Cursor {shown}");
        self.window = cx
            .open_window(options, |window, cx| {
                window.set_window_title(&title);
                cx.new(|_| CursorView)
            })
            .ok();
        crate::window::macos_set_window_shadow(&title, false);
        cx.spawn(async move |cursor, cx| {
            cx.background_executor().timer(VISIBLE_FOR).await;
            _ = cursor.update(cx, |cursor, cx| {
                if cursor.shown == shown {
                    cursor.close(cx);
                }
            });
        })
        .detach();
    }

    fn close(&mut self, cx: &mut App) {
        if let Some(window) = self.window.take() {
            _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "Desktop coordinates fit GPUI's f32 points."
)]
const fn gpui_points(value: f64) -> f32 {
    value as f32
}

struct CursorView;

impl Render for CursorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // GPUI pins the root to the window origin, so the inset moves a child.
        div().size_full().child(
            div()
                .absolute()
                .left(px(-TIP_INSET))
                .top(px(-TIP_INSET))
                .child(crate::gpui::icon(
                    "mouse-pointer-2",
                    SIDE,
                    cx.theme().primary,
                )),
        )
    }
}
