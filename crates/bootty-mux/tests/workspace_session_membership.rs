use bootty_mux::session_membership::{SessionMembership, SessionState, WorkspaceSession};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;

static_assertions::assert_impl_all!(WorkspaceSession: Clone, Eq, Send, Sync);
static_assertions::assert_impl_all!(SessionMembership: Clone, Eq, Send, Sync);

#[derive(Arbitrary, Clone, Copy, Debug)]
struct SessionSeed {
    identity: u16,
    backend_name: u16,
    cwd: u16,
}

fn session(identity: &str, name: &str) -> WorkspaceSession {
    WorkspaceSession {
        identity: identity.to_owned(),
        backend_name: name.to_owned(),
        display_name: String::new(),
        explicit: false,
        cwd: "/repo".to_owned(),
        state: SessionState::default(),
        terminal_snapshot: None,
    }
}

fn labels(membership: &SessionMembership) -> Vec<&str> {
    membership
        .sessions()
        .iter()
        .map(WorkspaceSession::label)
        .collect()
}

#[test]
fn a_rename_from_anywhere_leaves_the_claim_and_the_display_name_alone() {
    let mut membership = SessionMembership::default();
    membership.claim(session("id-1", "agents/main"));
    membership.set_display_name("id-1", "agents/main", true);

    assert!(membership.observe_backend_name("id-1", "renamed-elsewhere"));

    let claimed = membership.get("id-1").expect("the claim survives a rename");
    assert_eq!(claimed.backend_name, "renamed-elsewhere");
    assert_eq!(claimed.label(), "agents/main");
}

#[test]
fn two_sessions_can_share_a_display_name_when_the_backend_had_to_uniquify_one() {
    let mut membership = SessionMembership::default();
    membership.claim(session("id-1", "agents/main"));
    membership.claim(session("id-2", "agents/main-2"));
    membership.set_display_name("id-1", "agents/main", true);
    membership.set_display_name("id-2", "agents/main", true);

    assert_eq!(labels(&membership), ["agents/main", "agents/main"]);
    assert_eq!(
        membership.backend_names(),
        ["agents/main", "agents/main-2"],
        "the backend keeps the names it needs to tell them apart"
    );
}

#[test]
fn a_claimed_session_joins_its_group_rather_than_the_end_of_the_list() {
    let mut membership = SessionMembership::default();
    membership.claim(session("id-1", "agents/main"));
    membership.claim(session("id-2", "web/dev"));
    membership.claim(session("id-3", "agents/review"));

    assert_eq!(
        labels(&membership),
        ["agents/main", "agents/review", "web/dev"]
    );
}

#[test]
fn a_session_reorders_inside_its_group_and_carries_the_group_across_one() {
    let mut membership = SessionMembership::default();
    for (identity, name) in [
        ("id-1", "agents/main"),
        ("id-2", "agents/review"),
        ("id-3", "web/dev"),
    ] {
        membership.claim(session(identity, name));
    }

    assert!(membership.move_before("id-2", Some("id-1")));
    assert_eq!(
        labels(&membership),
        ["agents/review", "agents/main", "web/dev"]
    );

    assert!(
        !membership.move_before("id-1", Some("id-3")),
        "the agents block already sits before web/dev"
    );
    assert!(membership.move_before("id-3", Some("id-1")));
    assert_eq!(
        labels(&membership),
        ["web/dev", "agents/review", "agents/main"],
        "a session cannot leave its group, so the whole group travels"
    );
}

proptest! {
    /// Property: claim followed by release returns the same public session value.
    #[test]
    fn claim_then_release_round_trips_session(seed in any::<SessionSeed>()) {
        let mut expected = session(
            &format!("id-{}", seed.identity),
            &format!("session-{}", seed.backend_name),
        );
        expected.cwd = format!("/worktree/{}", seed.cwd);
        let identity = expected.identity.clone();
        let mut membership = SessionMembership::default();

        prop_assert!(membership.claim(expected.clone()));
        let released = membership.release(&identity);

        prop_assert_eq!(released, Some(expected));
        prop_assert!(membership.is_empty());
        prop_assert_eq!(membership.release(&identity), None);
    }
}

#[derive(Arbitrary, Clone, Copy, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Generate independent saved flags to prove restoration preserves every combination"
)]
struct SavedStateSeed {
    settled: bool,
    pinned: bool,
    archived: bool,
    hidden: bool,
    until: Option<u32>,
    activity: Option<u32>,
}

proptest! {
    #[test]
    fn deleting_and_restoring_retains_every_saved_value(seed in any::<SavedStateSeed>()) {
        use bootty_mux::session_membership::{SessionLifecycle, SessionState, SessionStateChange};
        let mut saved = session("saved", "backend");
        saved.state = SessionState {
            lifecycle: if seed.settled { SessionLifecycle::Settled } else { SessionLifecycle::Active },
            pinned: seed.pinned,
            archived: seed.archived,
            hidden: seed.hidden,
            snoozed_until: seed.until.map(i64::from),
            deleted: false,
            last_activity_at: seed.activity.map(i64::from),
        };
        let expected = saved.clone();
        let sibling = session("sibling", "backend-2");
        let mut membership = SessionMembership::from_sessions(vec![saved, sibling.clone()]);
        prop_assert!(membership.set_state("saved", SessionStateChange::Delete));
        prop_assert!(membership.set_state("saved", SessionStateChange::RestoreDeleted));
        prop_assert_eq!(membership.sessions(), &[expected, sibling]);
    }

    #[test]
    fn snooze_becomes_overdue_at_its_exact_deadline(until in 0_i64..i64::MAX) {
        use bootty_mux::session_membership::{SessionState, SessionView};
        let state = SessionState { snoozed_until: Some(until), ..SessionState::default() };
        let before = until.checked_sub(1).expect("deadline has a preceding UTC second");
        prop_assert_eq!(state.view(before), SessionView::Snoozed);
        prop_assert!(!state.is_visible(before));
        prop_assert!(!state.is_overdue(before));
        prop_assert_eq!(state.view(until), SessionView::Active);
        prop_assert!(state.is_visible(until));
        prop_assert!(state.is_overdue(until));
    }

    #[test]
    fn pinning_promotes_saved_work_without_changing_order_or_visibility(seed in any::<SavedStateSeed>()) {
        use bootty_mux::session_membership::{SessionLifecycle, SessionStateChange};
        let mut saved = session("saved", "backend");
        saved.state.lifecycle = if seed.settled { SessionLifecycle::Settled } else { SessionLifecycle::Active };
        saved.state.archived = seed.archived;
        saved.state.hidden = seed.hidden;
        saved.state.snoozed_until = seed.until.map(i64::from);
        let sibling = session("sibling", "backend-2");
        let mut membership = SessionMembership::from_sessions(vec![saved.clone(), sibling.clone()]);
        prop_assert!(membership.set_state("saved", SessionStateChange::SetPinned(true)));
        saved.state.pinned = true;
        saved.state.lifecycle = SessionLifecycle::Active;
        saved.state.snoozed_until = None;
        prop_assert_eq!(membership.sessions(), &[saved.clone(), sibling.clone()]);
        prop_assert!(!membership.set_state("saved", SessionStateChange::SetPinned(true)));
        prop_assert!(membership.set_state("saved", SessionStateChange::SetPinned(false)));
        saved.state.pinned = false;
        prop_assert_eq!(membership.sessions(), &[saved, sibling]);
    }
}

#[rstest::rstest]
#[case(bootty_mux::session_membership::SessionStateChange::Archive)]
#[case(bootty_mux::session_membership::SessionStateChange::Delete)]
#[case(bootty_mux::session_membership::SessionStateChange::SetHidden(true))]
#[case(bootty_mux::session_membership::SessionStateChange::SnoozeUntil(100))]
fn parked_saved_work_retains_its_pin_until_settled(
    #[case] change: bootty_mux::session_membership::SessionStateChange,
) {
    use bootty_mux::session_membership::{SessionLifecycle, SessionStateChange};
    let mut membership = SessionMembership::from_sessions(vec![session("saved", "backend")]);
    assert!(membership.set_state("saved", SessionStateChange::SetPinned(true)));
    assert!(membership.set_state("saved", change));
    assert!(membership.get("saved").unwrap().state.pinned);
    assert!(membership.set_state(
        "saved",
        SessionStateChange::SetLifecycle(SessionLifecycle::Settled)
    ));
    assert!(!membership.get("saved").unwrap().state.pinned);
}

#[rstest::rstest]
#[case(
    false,
    false,
    false,
    bootty_mux::session_membership::SessionView::Snoozed
)]
#[case(
    false,
    false,
    true,
    bootty_mux::session_membership::SessionView::Hidden
)]
#[case(
    false,
    true,
    true,
    bootty_mux::session_membership::SessionView::Archived
)]
#[case(true, true, true, bootty_mux::session_membership::SessionView::Deleted)]
fn visibility_preserves_underlying_lifecycle_and_snooze(
    #[case] deleted: bool,
    #[case] archived: bool,
    #[case] hidden: bool,
    #[case] expected: bootty_mux::session_membership::SessionView,
) {
    use bootty_mux::session_membership::{SessionLifecycle, SessionState};
    let state = SessionState {
        lifecycle: SessionLifecycle::Settled,
        pinned: false,
        deleted,
        archived,
        hidden,
        snoozed_until: Some(100),
        last_activity_at: Some(90),
    };
    assert_eq!(state.view(99), expected);
    assert_eq!(state.lifecycle, SessionLifecycle::Settled);
    assert_eq!(state.snoozed_until, Some(100));
    assert!(!state.is_visible(99));
    if deleted || archived || hidden {
        assert!(!state.is_overdue(100));
    }
}

proptest! {
    #[test]
    fn accepted_activity_is_monotone_and_preserves_saved_state(initial in any::<Option<u32>>(), receipts in proptest::collection::vec(any::<u32>(), 0..32)) {
        use bootty_mux::session_membership::SessionStateChange;
        let mut saved = session("saved", "backend");
        saved.state.last_activity_at = initial.map(i64::from);
        saved.state.pinned = true;
        let sibling = session("sibling", "other");
        let mut membership = SessionMembership::from_sessions(vec![saved.clone(), sibling.clone()]);
        let mut expected = saved.state.last_activity_at;
        for at in receipts.into_iter().map(i64::from) {
            let next = Some(expected.map_or(at, |last| last.max(at)));
            prop_assert_eq!(membership.set_state("saved", SessionStateChange::RecordActivity { at, now: i64::from(u32::MAX) }), next != expected);
            expected = next;
            saved.state.last_activity_at = expected;
            prop_assert_eq!(membership.sessions(), &[saved.clone(), sibling.clone()]);
        }
    }
}

#[rstest::rstest]
#[case(-1, 100)]
#[case(101, 100)]
#[case(0, -1)]
fn invalid_activity_preserves_the_saved_record(#[case] at: i64, #[case] now: i64) {
    use bootty_mux::session_membership::SessionStateChange;
    let saved = session("saved", "backend");
    let mut membership = SessionMembership::from_sessions(vec![saved.clone()]);
    assert!(!membership.set_state("saved", SessionStateChange::RecordActivity { at, now }));
    assert_eq!(membership.sessions(), &[saved]);
}

#[rstest::rstest]
#[case::new_input(101, 101, true)]
#[case::same_second(100, 100, true)]
#[case::stale(99, 101, false)]
#[case::future(101, 100, false)]
#[case::negative(-1, 100, false)]
fn accepted_input_unsettles_atomically_and_preserves_other_saved_values(
    #[case] at: i64,
    #[case] now: i64,
    #[case] accepted: bool,
) {
    use bootty_mux::session_membership::{SessionLifecycle, SessionStateChange};
    let mut saved = session("saved", "backend");
    saved.state.lifecycle = SessionLifecycle::Settled;
    saved.state.last_activity_at = Some(100);
    saved.state.hidden = true;
    saved.state.archived = true;
    let mut membership = SessionMembership::from_sessions(vec![saved.clone()]);
    assert_eq!(
        membership.set_state("saved", SessionStateChange::AcceptInput { at, now }),
        accepted
    );
    if accepted {
        saved.state.lifecycle = SessionLifecycle::Active;
        saved.state.last_activity_at = Some(at);
    }
    assert_eq!(membership.sessions(), &[saved]);
}
