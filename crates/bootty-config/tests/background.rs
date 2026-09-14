use bootty_config::config::{BackgroundMaterial, load_config_from_path};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

proptest! {
    #[test]
    fn opacity_and_gradient_settings_round_trip(opacity in 0.0f32..=1.0, angle in 0.0f32..=360.0) {
        let dir = assert_fs::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, format!("[window]\nbackground-opacity={opacity}\nbackground-gradient-angle={angle}\nbackground-material='blurred'\nbackground-image='wallpaper.png'\nbackground-gradient-start='#12345678'\nbackground-gradient-end='#ABCDEF'\n")).unwrap();
        let config = load_config_from_path(&path).unwrap();
        prop_assert!((config.window.background_opacity - opacity).abs() < f32::EPSILON);
        prop_assert!((config.window.background_gradient_angle - angle).abs() < f32::EPSILON * 360.0);
        prop_assert_eq!(config.window.background_material, BackgroundMaterial::Blurred);
        prop_assert_eq!(config.window.background_image, Some(std::path::PathBuf::from("wallpaper.png")));
        prop_assert_eq!(config.window.background_gradient_start.unwrap().a, 120);
    }
}
#[rstest]
#[case("background-opacity", "1.1")]
#[case("background-opacity", "nan")]
#[case("background-image-opacity", "-0.1")]
#[case("background-gradient-angle", "361")]
#[case("background-gradient-start", "'invalid'")]
#[case("background-material", "'unknown'")]
fn invalid_background_setting_rejects_config(#[case] key: &str, #[case] value: &str) {
    let dir = assert_fs::TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, format!("[window]\n{key}={value}\n")).unwrap();
    assert!(load_config_from_path(&path).is_err());
}
#[rstest]
fn defaults_remain_opaque() {
    let config = bootty_config::config::BoottyConfig::default();
    assert!((config.window.background_opacity - 1.0).abs() < f32::EPSILON);
    assert_eq!(
        config.window.background_material,
        BackgroundMaterial::Opaque
    );
    assert!(config.window.background_image.is_none());
}
