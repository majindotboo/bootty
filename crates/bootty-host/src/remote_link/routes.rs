//! Ordered routes with an injected monotonic clock. No pairing or learned LAN routes:
//! add discovery only when a stable remote catalog can own and validate those hints.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Route {
    Quic,
    Tls,
    Ssh,
}
impl Route {
    pub const ALL: [Self; 3] = [Self::Quic, Self::Tls, Self::Ssh];
}
pub struct Routes {
    active: Option<Route>,
    held: Option<(Route, Instant)>,
    preflight_at: Instant,
    cooldown: Duration,
}
impl Routes {
    #[must_use]
    pub const fn new(now: Instant, cooldown: Duration) -> Self {
        Self {
            active: None,
            held: None,
            preflight_at: now,
            cooldown,
        }
    }
    #[must_use]
    pub const fn active(&self) -> Option<Route> {
        self.active
    }
    pub fn connected(&mut self, route: Route, now: Instant) {
        self.active = Some(route);
        self.defer(now);
    }
    pub fn defer(&mut self, now: Instant) {
        self.preflight_at = now.checked_add(self.cooldown).unwrap_or(now);
    }
    pub fn disconnected(&mut self, now: Instant) {
        // A preferred route that passed preflight but immediately failed must not
        // bounce a working fallback on every reconnect.
        if let Some(route) = self.active.take() {
            self.held = Some((route, now.checked_add(self.cooldown).unwrap_or(now)));
        }
    }
    pub fn candidates(&self, now: Instant) -> impl Iterator<Item = Route> + use<> {
        let active = self.active;
        let held = self.held;
        let ready = now >= self.preflight_at;
        Route::ALL.into_iter().filter(move |route| {
            held.is_none_or(|(held, until)| held != *route || now >= until)
                && active.is_none_or(|active| ready && *route < active)
        })
    }
}
