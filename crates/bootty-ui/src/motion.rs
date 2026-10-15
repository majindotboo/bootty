//! Working animations share one 30 Hz clock. Every view that asks for a frame redraws on the same
//! tick, so motion costs one frame per tick instead of one per display refresh per animation.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use gpui_kit::{
    App, EntityId, Global, Hsla, IntoElement, Styled as _, Task, Transformation,
    component::{Icon, IconName, Sizable as _},
    radians,
};
use num_traits::ToPrimitive as _;

/// Deliberate limit: 30 Hz keeps rotation readable at a quarter of a 120 Hz display's frames.
/// Raise it only with a measured frame budget.
const FRAME: Duration = Duration::from_millis(33);

#[derive(Default)]
struct Clock {
    views: Vec<EntityId>,
    tick: Option<Task<()>>,
}
impl Global for Clock {}

/// Redraws `view` on the next shared tick. Ask while painting motion; a view that stops asking
/// stops redrawing.
pub fn request_frame(view: EntityId, cx: &mut App) {
    let clock = cx.default_global::<Clock>();
    if !clock.views.contains(&view) {
        clock.views.push(view);
    }
    if clock.tick.is_some() {
        return;
    }
    let tick = cx.spawn(async move |cx| {
        cx.background_executor().timer(FRAME).await;
        cx.update(|cx| {
            let clock = cx.default_global::<Clock>();
            clock.tick = None;
            for view in std::mem::take(&mut clock.views) {
                cx.notify(view);
            }
        });
    });
    cx.default_global::<Clock>().tick = Some(tick);
}

/// Where a `period`-long cycle is now, from 0 up to 1. Animations share one origin, so two
/// spinners turn together.
#[must_use]
pub fn phase(period: Duration) -> f32 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let elapsed = ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64();
    (elapsed / period.as_secs_f64())
        .fract()
        .to_f32()
        .unwrap_or_default()
}

/// A loading circle turned to the shared clock's phase. Its view asks for frames.
#[must_use]
pub fn spinner(color: Hsla) -> impl IntoElement {
    let turn = phase(Duration::from_millis(800)) * std::f32::consts::TAU;
    Icon::new(IconName::LoaderCircle)
        .small()
        .text_color(color)
        .transform(Transformation::rotate(radians(turn)))
}

/// Text opacity that rises and falls once per two seconds, for labels of running work.
#[must_use]
pub fn pulse() -> f32 {
    let wave = (phase(Duration::from_secs(2)) * std::f32::consts::TAU).cos();
    wave.mul_add(0.2, 0.8)
}
