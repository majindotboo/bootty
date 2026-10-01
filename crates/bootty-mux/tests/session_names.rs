use bootty_mux::session_names::{portable_session_name, unique_session_name};
use bootty_mux::workspace::SESSION_NAME_MAX_BYTES;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case("claude .project:branch#1\\file", "claude _project_branch_1__file")]
#[case("-project", "_-project")]
#[case("$project", "_$project")]
#[case("@project", "_@project")]
#[case("%project", "_%project")]
#[case("=project", "_=project")]
#[case("/$project", "_/$project")]
#[case("", "bootty")]
fn generated_labels_are_exact_backend_names(#[case] label: &str, #[case] expected: &str) {
    let name = portable_session_name(label);
    assert_eq!(name, expected);
    assert_eq!(
        rmux_proto::SessionName::new(name.clone()).unwrap().as_str(),
        name
    );
}

proptest! {
    #[test]
    fn generated_unicode_labels_remain_bounded_and_backend_exact_when_uniquified(
        characters in prop::collection::vec(any::<char>(), 0..400),
        collisions in 0_u16..64,
    ) {
        let input = characters.into_iter().collect::<String>();
        let base = portable_session_name(&input);
        let mut names = vec![base.clone()];
        for suffix in 2..collisions.saturating_add(2) {
            names.push(format!("{base}-{suffix}"));
        }
        let unique = unique_session_name(&base, names.iter().map(String::as_str));
        prop_assert!(!names.contains(&unique));
        prop_assert!(unique.len() <= SESSION_NAME_MAX_BYTES);
        prop_assert!(!unique.contains('#'));
        prop_assert!(!unique.starts_with(['-', '$', '@', '%', '=']));
        let backend = rmux_proto::SessionName::new(unique.clone()).expect("portable backend name");
        prop_assert_eq!(backend.as_str(), unique.as_str());
    }
}
