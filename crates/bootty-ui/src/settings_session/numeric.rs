use std::ops::RangeInclusive;

/// Convert a number shown in the settings UI back to its stored schema value.
#[must_use]
pub fn parse_display_number(
    text: &str,
    range: &RangeInclusive<f32>,
    display_scale: f32,
) -> Option<f32> {
    let display_scale = valid_display_scale(display_scale)?;
    normalize_number(text.trim().parse::<f32>().ok()? / display_scale, range)
}

/// Validate and clamp a stored settings number before writeback.
#[must_use]
pub fn normalize_number(value: f32, range: &RangeInclusive<f32>) -> Option<f32> {
    let start = *range.start();
    let end = *range.end();
    (value.is_finite() && start.is_finite() && end.is_finite() && start <= end)
        .then(|| value.clamp(start, end))
}

fn valid_display_scale(display_scale: f32) -> Option<f32> {
    (display_scale.is_finite() && display_scale > 0.0).then_some(display_scale)
}
