use bootty_host::remote_link::routes::{Route, Routes};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use std::time::{Duration, Instant};

#[rstest]
fn fallback_order_preflight_and_failed_promotion_cooldown() {
    let now = Instant::now();
    let cooldown = Duration::from_secs(30);
    let mut routes = Routes::new(now, cooldown);
    assert_eq!(routes.candidates(now).collect::<Vec<_>>(), Route::ALL);
    routes.connected(Route::Ssh, now);
    assert_eq!(routes.candidates(now).count(), 0);
    let later = now.checked_add(cooldown).unwrap();
    assert_eq!(
        routes.candidates(later).collect::<Vec<_>>(),
        vec![Route::Quic, Route::Tls]
    );
    routes.connected(Route::Quic, later);
    routes.disconnected(later);
    assert_eq!(
        routes.candidates(later).collect::<Vec<_>>(),
        vec![Route::Tls, Route::Ssh]
    );
    routes.connected(Route::Tls, later);
    assert_eq!(routes.candidates(later).count(), 0);
    assert_eq!(
        routes
            .candidates(later.checked_add(cooldown).unwrap())
            .collect::<Vec<_>>(),
        vec![Route::Quic]
    );
}
proptest! {
    #[test]
    fn a_later_route_never_preflights_during_cooldown(elapsed in 0_u64..30, fallback in 1_usize..3) {
        let now = Instant::now();
        let route = Route::ALL.get(fallback).copied().unwrap();
        let mut routes = Routes::new(now, Duration::from_secs(30));
        routes.connected(route, now);
        prop_assert_eq!(routes.candidates(now.checked_add(Duration::from_secs(elapsed)).unwrap()).count(), 0);
        let candidates: Vec<_> = routes.candidates(now.checked_add(Duration::from_secs(30)).unwrap()).collect();
        prop_assert!(candidates.iter().all(|candidate| *candidate < route));
        prop_assert_eq!(candidates.first(), Some(&Route::Quic));
    }
}
