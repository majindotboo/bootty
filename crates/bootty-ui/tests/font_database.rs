#![cfg(test)]

use bootty_ui::assets::{
    IBM_PLEX_SANS_ITALIC, IBM_PLEX_SANS_REGULAR, IBM_PLEX_SANS_SEMIBOLD,
    IBM_PLEX_SANS_SEMIBOLD_ITALIC, LILEX_BOLD, LILEX_BOLD_ITALIC, LILEX_ITALIC, LILEX_REGULAR,
};
use bootty_ui::font_database::{
    font_name_with_fallbacks, installed_family_names, system_font_database,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn installed_family_names_are_sorted_and_unique() {
    let names = installed_family_names();
    let mut expected = names.clone();
    expected.sort_unstable();
    expected.dedup();

    assert_ne!(names, Vec::<std::string::String>::new());
    assert_eq!(names, expected);
}

#[rstest]
fn generic_monospace_resolves_to_a_real_system_face() {
    let database = system_font_database();
    let id = database
        .query(&fontdb::Query {
            families: &[fontdb::Family::Monospace],
            ..fontdb::Query::default()
        })
        .expect("system monospace face");

    assert!(database.face(id).is_some());
}

#[rstest]
#[case(".ZedMono", "Lilex")]
#[case("Zed Plex Mono", "Lilex")]
#[case(".ZedSans", "IBM Plex Sans")]
#[case("Zed Plex Sans", "IBM Plex Sans")]
fn zed_virtual_font_names_resolve_to_bundled_families(
    #[case] virtual_name: &str,
    #[case] concrete_name: &str,
) {
    let resolved = font_name_with_fallbacks(virtual_name, "system");
    assert_eq!(resolved, concrete_name);
    assert_eq!(
        resolved,
        gpui_kit::font_name_with_fallbacks(virtual_name, "system")
    );

    let database = system_font_database();
    let id = database
        .query(&fontdb::Query {
            families: &[fontdb::Family::Name(resolved)],
            ..fontdb::Query::default()
        })
        .expect("Zed-compatible family is bundled");
    assert!(database.face(id).is_some());
}

#[rstest]
#[case(
    ".ZedSans",
    fontdb::Weight::NORMAL,
    fontdb::Style::Normal,
    IBM_PLEX_SANS_REGULAR
)]
#[case(
    ".ZedSans",
    fontdb::Weight::NORMAL,
    fontdb::Style::Italic,
    IBM_PLEX_SANS_ITALIC
)]
#[case(
    ".ZedSans",
    fontdb::Weight::SEMIBOLD,
    fontdb::Style::Normal,
    IBM_PLEX_SANS_SEMIBOLD
)]
#[case(
    ".ZedSans",
    fontdb::Weight::SEMIBOLD,
    fontdb::Style::Italic,
    IBM_PLEX_SANS_SEMIBOLD_ITALIC
)]
#[case(
    ".ZedMono",
    fontdb::Weight::NORMAL,
    fontdb::Style::Normal,
    LILEX_REGULAR
)]
#[case(
    ".ZedMono",
    fontdb::Weight::NORMAL,
    fontdb::Style::Italic,
    LILEX_ITALIC
)]
#[case(".ZedMono", fontdb::Weight::BOLD, fontdb::Style::Normal, LILEX_BOLD)]
#[case(
    ".ZedMono",
    fontdb::Weight::BOLD,
    fontdb::Style::Italic,
    LILEX_BOLD_ITALIC
)]
fn zed_virtual_fonts_select_the_exact_bundled_face(
    #[case] virtual_name: &str,
    #[case] weight: fontdb::Weight,
    #[case] style: fontdb::Style,
    #[case] expected_bytes: &[u8],
) {
    let database = system_font_database();
    let family = font_name_with_fallbacks(virtual_name, "system");
    let id = database
        .query(&fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight,
            style,
            ..fontdb::Query::default()
        })
        .expect("Zed virtual font resolves");
    let exact_match = database
        .with_face_data(id, |bytes, face_index| {
            face_index == 0 && bytes == expected_bytes
        })
        .expect("resolved face has byte data");

    assert!(exact_match, "{virtual_name} resolved to unexpected bytes");
}
