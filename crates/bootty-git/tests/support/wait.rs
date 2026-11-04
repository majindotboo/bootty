use std::{
    thread,
    time::{Duration, Instant},
};

/// Bounds a wait on background work by time, not by spin count, so a loaded CI host cannot fail
/// a test that would have passed. A passing wait returns as soon as `done` holds.
const DEADLINE: Duration = Duration::from_secs(10);

pub fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now().checked_add(DEADLINE).expect("wait deadline");
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::yield_now();
    }
    true
}
