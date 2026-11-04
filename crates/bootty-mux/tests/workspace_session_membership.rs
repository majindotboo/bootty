use bootty_mux::session_membership::{SessionMembership, SessionState, WorkspaceSession};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;

static_assertions::assert_impl_all!(WorkspaceSession: Clone, PartialEq, Send, Sync);
static_assertions::assert_impl_all!(SessionMembership: Clone, PartialEq, Send, Sync);

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
