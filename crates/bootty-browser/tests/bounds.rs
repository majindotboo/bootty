use bootty_browser::BrowserBounds;
use proptest::prelude::*;
use wry::dpi::{Position, Size};

proptest! {
    #[test]
    fn browser_geometry_uses_the_host_grid(
        x in -2048_i16..2048, y in -2048_i16..2048,
        width in 0_u16..2048, height in 0_u16..2048,
        scale_halves in 2_u8..9,
    ) {
        let scale = f64::from(scale_halves) / 2.0;
        let rect = wry::Rect::from(BrowserBounds {
            x: f64::from(x), y: f64::from(y),
            width: f64::from(width), height: f64::from(height),
            scale_factor: scale,
        });
        if cfg!(target_os = "linux") {
            prop_assert!(matches!(rect.position, Position::Physical(_)));
            prop_assert!(matches!(rect.size, Size::Physical(_)));
            // Wry stores device geometry as integers, so fractional scales round by at most half a pixel.
            prop_assert!(f64::from(x).mul_add(-scale, rect.position.to_physical::<f64>(1.0).x).abs() <= 0.5);
            prop_assert!(f64::from(y).mul_add(-scale, rect.position.to_physical::<f64>(1.0).y).abs() <= 0.5);
            prop_assert!(f64::from(width.max(1)).mul_add(-scale, rect.size.to_physical::<f64>(1.0).width).abs() <= 0.5);
            prop_assert!(f64::from(height.max(1)).mul_add(-scale, rect.size.to_physical::<f64>(1.0).height).abs() <= 0.5);
        } else {
            prop_assert!(matches!(rect.position, Position::Logical(_)));
            prop_assert!(matches!(rect.size, Size::Logical(_)));
            prop_assert!((rect.position.to_logical::<f64>(2.0).x - f64::from(x)).abs() < f64::EPSILON);
            prop_assert!((rect.size.to_logical::<f64>(2.0).width - f64::from(width.max(1))).abs() < f64::EPSILON);
        }
    }
}
