use bootty_control::short_name::{full_name, is_short_name, names, shortest};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use std::path::Path;

#[rstest]
#[case("/Users/me/src/Bootty App", "bootty-app-")]
#[case("/srv/work_tree.v2", "work-tree-v2-")]
#[case("/", "session-")]
#[case("/tmp/日本", "session-")]
fn names_read_as_the_project_and_a_hash(#[case] project: &str, #[case] start: &str) {
    let name = full_name(Path::new(project), "identity");
    assert!(name.starts_with(start), "{name}");
    assert_eq!(name.len(), start.len().saturating_add(8), "{name}");
    assert!(is_short_name(&name), "{name}");
}

#[rstest]
#[case(r#"["process","window",1]"#)]
#[case("native:codex:26")]
#[case("Bootty-7c")]
fn issued_handles_are_not_short_names(#[case] handle: &str) {
    assert!(!is_short_name(handle));
}

proptest! {
    /// Each shortest name still names its own session and no other in the same set, and the
    /// full name is the same every time it is asked for.
    #[test]
    fn shortest_names_are_unambiguous(identities in prop::collection::btree_set("[a-z0-9:-]{1,24}", 1..40)) {
        let project = Path::new("/src/bootty");
        let full: Vec<String> = identities.iter().map(|identity| full_name(project, identity)).collect();
        prop_assert_eq!(&full, &identities.iter().map(|identity| full_name(project, identity)).collect::<Vec<_>>());
        for name in &full {
            let short = shortest(name, &full);
            prop_assert!(names(&short, name), "{} names {}", short, name);
            for other in full.iter().filter(|other| *other != name) {
                prop_assert!(!names(&short, other), "{} also names {}", short, other);
            }
        }
    }
}
