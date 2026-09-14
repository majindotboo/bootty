use assert_fs::{TempDir, prelude::*};
use bootty_config::{
    color::Color,
    config::{
        ColorConfig, ResolvedTheme, ThemeInfo, parse_theme_source,
        theme_file::{encode_theme, import_theme, read_theme, save_theme},
    },
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn theme() -> ResolvedTheme {
    ResolvedTheme {
        info: ThemeInfo {
            name: "Original".to_owned(),
            source: "Local sample".to_owned(),
            license: "MIT".to_owned(),
        },
        colors: ColorConfig {
            background: Some(Color {
                r: 10,
                g: 20,
                b: 30,
                a: 128,
            }),
            ..ColorConfig::default()
        },
    }
}

#[rstest]
fn saving_a_theme_is_atomic_and_rejects_stale_edits() {
    let dir = TempDir::new().unwrap();
    let source = encode_theme(&theme()).unwrap();
    let saved = save_theme(dir.path(), "Edited", &source, None).unwrap();
    let file = dir.child("themes/Edited.toml");
    assert_eq!(std::fs::read_to_string(file.path()).unwrap(), saved.source);
    assert!(save_theme(dir.path(), "Edited", &source, None).is_err());
    let updated = saved.source.replace("#0a141e80", "#abcdef");
    save_theme(dir.path(), "Edited", &updated, saved.revision.as_deref()).unwrap();
    assert!(
        save_theme(dir.path(), "Edited", &source, saved.revision.as_deref())
            .unwrap_err()
            .contains("changed on disk")
    );
    let read = read_theme(dir.path(), "Edited").unwrap();
    let parsed = parse_theme_source(&read.source, "Edited").unwrap();
    assert_eq!(parsed.info.name, "Edited");
    assert_eq!(parsed.info.license, "MIT");
    assert_eq!(
        parsed.colors.background,
        Some(Color {
            r: 171,
            g: 205,
            b: 239,
            a: 255
        })
    );
}

#[rstest]
#[case("../outside")]
#[case(".")]
#[case("bad/name")]
#[case("bad\\name")]
#[case("C:outside")]
fn theme_names_cannot_escape_the_theme_directory(#[case] name: &str) {
    let dir = TempDir::new().unwrap();
    assert!(save_theme(dir.path(), name, &encode_theme(&theme()).unwrap(), None).is_err());
    assert!(!dir.child("themes").path().exists());
}

#[rstest]
fn builtins_can_be_copied_and_legacy_theme_files_keep_their_identity() {
    let dir = TempDir::new().unwrap();
    let builtin = read_theme(dir.path(), "One Dark").unwrap();
    assert_eq!(builtin.revision, None);
    let copy = save_theme(dir.path(), "My Dark", &builtin.source, None).unwrap();
    let original = parse_theme_source(&builtin.source, "One Dark").unwrap();
    assert_eq!(
        parse_theme_source(&copy.source, "My Dark").unwrap().colors,
        original.colors
    );
    dir.child("themes/Legacy").write_str(&copy.source).unwrap();
    let legacy = read_theme(dir.path(), "Legacy").unwrap();
    save_theme(
        dir.path(),
        "Legacy",
        &legacy.source,
        legacy.revision.as_deref(),
    )
    .unwrap();
    assert!(!dir.child("themes/Legacy.toml").path().exists());
}

fn iterm() -> plist::Dictionary {
    let channels = plist::Dictionary::from_iter([
        ("Red Component".to_owned(), plist::Value::Real(0.5)),
        ("Green Component".to_owned(), plist::Value::Real(1.0)),
        ("Blue Component".to_owned(), plist::Value::Real(0.0)),
    ]);
    (0..16)
        .map(|index| format!("Ansi {index} Color"))
        .chain([
            "Background Color".to_owned(),
            "Foreground Color".to_owned(),
            "Selection Color".to_owned(),
        ])
        .map(|key| (key, plist::Value::Dictionary(channels.clone())))
        .collect()
}

#[rstest]
#[case(false)]
#[case(true)]
fn iterm_import_preserves_palette_and_selection_in_xml_and_binary(#[case] binary: bool) {
    let dir = TempDir::new().unwrap();
    let path = dir.child("Sample.itermcolors");
    let value = plist::Value::Dictionary(iterm());
    if binary {
        value.to_file_binary(path.path()).unwrap();
    } else {
        value.to_file_xml(path.path()).unwrap();
    }
    let imported = import_theme(path.path()).unwrap();
    let parsed = parse_theme_source(&imported.source, "sample").unwrap();
    let expected = Color {
        r: 128,
        g: 255,
        b: 0,
        a: 255,
    };
    assert_eq!(parsed.colors.palette, vec![expected; 16]);
    assert_eq!(parsed.colors.selection_background, Some(expected));
    assert_eq!(parsed.colors.cursor, None);
}

#[rstest]
#[case(plist::Value::Real(-1.0))]
#[case(plist::Value::Real(1.1))]
#[case(plist::Value::String("invalid".to_owned()))]
fn malformed_alpha_is_not_silently_replaced(#[case] alpha: plist::Value) {
    let dir = TempDir::new().unwrap();
    let path = dir.child("Invalid.itermcolors");
    let mut dictionary = iterm();
    dictionary
        .get_mut("Background Color")
        .unwrap()
        .as_dictionary_mut()
        .unwrap()
        .insert("Alpha Component".to_owned(), alpha);
    plist::Value::Dictionary(dictionary)
        .to_file_xml(path.path())
        .unwrap();
    assert!(
        import_theme(path.path())
            .unwrap_err()
            .contains("Alpha Component")
    );
}

proptest::proptest! {
    #[test]
    fn authored_rgba_colors_round_trip_exactly(rgba in proptest::array::uniform4(proptest::prelude::any::<u8>())) {
        let mut theme = theme();
        theme.colors.background = Some(Color { r: rgba[0], g: rgba[1], b: rgba[2], a: rgba[3] });
        let source = encode_theme(&theme).unwrap();
        let decoded = parse_theme_source(&source, "roundtrip").unwrap();
        proptest::prop_assert_eq!(decoded, theme);
    }
}
