use bootty_mux::session_membership::{SessionLifecycle, SessionState};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;

#[derive(Arbitrary, Clone, Debug)]
struct Inactivity {
    elapsed: u32,
    interval: u16,
    keep_active: bool,
    pinned: bool,
}

proptest! {
    #[test]
    fn inactive_sessions_settle_only_after_the_interval(input in any::<Inactivity>()) {
        let at: i64 = 100;
        let interval = i64::from(input.interval).saturating_mul(86_400);
        let state = SessionState { last_activity_at: Some(at), auto_settle_disabled: input.keep_active, pinned: input.pinned, ..SessionState::default() };
        let elapsed = i64::from(input.elapsed);
        prop_assert_eq!(state.should_auto_settle(at.saturating_add(elapsed), interval), interval != 0 && elapsed >= interval && !input.keep_active && !input.pinned);
    }
}

#[rstest]
#[case(SessionState { archived: true, ..SessionState::default() })]
#[case(SessionState { deleted: true, ..SessionState::default() })]
#[case(SessionState { hidden: true, ..SessionState::default() })]
#[case(SessionState { snoozed_until: Some(1), ..SessionState::default() })]
#[case(SessionState { lifecycle: SessionLifecycle::Settled, ..SessionState::default() })]
fn settlement_preserves_explicit_session_state(#[case] mut state: SessionState) {
    state.last_activity_at = Some(0);
    assert_eq!(state.should_auto_settle(1_000, 10), false);
}
