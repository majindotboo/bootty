use bootty_config::{color::Color, config::BoottyConfig};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn default_terminal_themes_preserve_the_one_palette() {
    let config = BoottyConfig::default();
    let dark = &config.appearance.dark.colors;
    let light = &config.appearance.light.colors;

    assert_eq!(dark.background, color("#282c34"));
    assert_eq!(dark.foreground, color("#abb2bf"));
    assert_eq!(dark.cursor, color("#74ade8"));
    assert_eq!(dark.cursor_text, color("#111110"));
    assert_eq!(dark.selection_background, color("#74ade83d"));
    assert_eq!(dark.palette[0], color("#282c34").unwrap());
    assert_eq!(dark.palette[8], color("#636d83").unwrap());
    assert_eq!(dark.palette[12], color("#85c1ff").unwrap());
    assert_eq!(dark.palette[15], color("#fafafa").unwrap());

    assert_eq!(light.background, color("#fafafa"));
    assert_eq!(light.foreground, color("#2a2c33"));
    assert_eq!(light.cursor, color("#5c78e2"));
    assert_eq!(light.cursor_text, color("#fdfdfc"));
    assert_eq!(light.selection_background, color("#5c78e23d"));
    assert_eq!(light.palette[6], color("#0997b3").unwrap());
    assert_eq!(light.palette[13], color("#a00095").unwrap());
    assert_eq!(light.palette[14], color("#0bbcd6").unwrap());
    assert_eq!(light.palette[15], color("#ffffff").unwrap());
}

fn color(value: &str) -> Option<Color> {
    Color::from_hex(value).ok()
}
