//! Side docks slide open and shut when the user toggles them.

use std::time::{Duration, Instant};

use gpui_kit::component::dock::DockPlacement;

/// How long a side dock takes to slide open or shut.
const SLIDE: Duration = Duration::from_millis(200);

/// The left and right docks' slides in progress.
///
/// Kit draws a side dock at its full width or not at all, so a slide keeps the dock open in Kit,
/// moves its content out toward the window edge, and lets the centre grow over the gap. A dock
/// sliding shut closes in Kit when its slide ends.
#[derive(Default)]
pub struct DockMotion {
    left: Option<Slide>,
    right: Option<Slide>,
}

#[derive(Clone, Copy)]
struct Slide {
    /// The fraction of the dock hidden when the slide started, and when it ends.
    from: f32,
    to: f32,
    started: Instant,
    width: f32,
    /// A finished slide shut whose close is on its way to Kit.
    closing_sent: bool,
}

impl Slide {
    fn fraction(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        let t = (elapsed / SLIDE.as_secs_f32()).min(1.0);
        // Ease out: quick to start, gentle to land.
        let eased = 1.0 - (1.0 - t).powi(3);
        (self.to - self.from).mul_add(eased, self.from)
    }

    fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= SLIDE
    }

    fn shutting(&self) -> bool {
        self.to >= 1.0
    }
}

impl DockMotion {
    const fn slide(&self, placement: DockPlacement) -> Option<&Slide> {
        match placement {
            DockPlacement::Left => self.left.as_ref(),
            DockPlacement::Right => self.right.as_ref(),
            DockPlacement::Bottom | DockPlacement::Center => None,
        }
    }

    const fn slot(&mut self, placement: DockPlacement) -> Option<&mut Option<Slide>> {
        match placement {
            DockPlacement::Left => Some(&mut self.left),
            DockPlacement::Right => Some(&mut self.right),
            DockPlacement::Bottom | DockPlacement::Center => None,
        }
    }

    /// Slides a dock `width` wide in, or out when `shut`, from wherever a slide in progress has
    /// it.
    pub fn start(&mut self, placement: DockPlacement, shut: bool, width: f32, now: Instant) {
        let Some(slot) = self.slot(placement) else {
            return;
        };
        let from = slot.map_or(if shut { 0.0 } else { 1.0 }, |slide| slide.fraction(now));
        *slot = Some(Slide {
            from,
            to: if shut { 1.0 } else { 0.0 },
            started: now,
            width,
            closing_sent: false,
        });
    }

    /// Whether a dock is sliding shut, still open in Kit until the slide ends.
    pub fn closing(&self, placement: DockPlacement) -> bool {
        self.slide(placement).is_some_and(Slide::shutting)
    }

    /// How many pixels of a dock its slide hides at `now`.
    pub fn hidden(&self, placement: DockPlacement, now: Instant) -> f32 {
        self.slide(placement)
            .map_or(0.0, |slide| slide.fraction(now) * slide.width)
    }

    /// Whether a slide still needs frames.
    pub const fn moving(&self) -> bool {
        self.left.is_some() || self.right.is_some()
    }

    /// Ends the slides done at `now`. Returns the docks that slid shut, once each, for Kit to
    /// close; each stays hidden until [`Self::clear`] follows that close.
    pub fn finish(&mut self, now: Instant) -> Vec<DockPlacement> {
        let mut closed = Vec::new();
        for placement in [DockPlacement::Left, DockPlacement::Right] {
            let Some(slot) = self.slot(placement) else {
                continue;
            };
            match slot {
                Some(slide) if slide.done(now) && slide.shutting() => {
                    if !slide.closing_sent {
                        slide.closing_sent = true;
                        closed.push(placement);
                    }
                }
                Some(slide) if slide.done(now) => *slot = None,
                _ => {}
            }
        }
        closed
    }

    pub const fn clear(&mut self, placement: DockPlacement) {
        if let Some(slot) = self.slot(placement) {
            *slot = None;
        }
    }
}
