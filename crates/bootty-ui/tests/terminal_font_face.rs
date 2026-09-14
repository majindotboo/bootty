#![cfg(test)]

use bootty_ui::terminal_font_face::{
    FontFaceMetrics, GlyphConstraint, GlyphConstraintAlign, GlyphConstraintHeight,
    GlyphConstraintSize, GlyphSize, nerd_font_constraint, terminal_glyph_constraint,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

const fn metrics() -> FontFaceMetrics {
    FontFaceMetrics {
        cell_width: 10,
        cell_height: 20,
        cell_baseline: 15,
        icon_height: 18.0,
        icon_height_single: 16.0,
        face_width: 10.0,
        face_height: 20.0,
        face_y: 0.0,
    }
}

#[rstest]
fn unconstrained_text_keeps_its_exact_glyph_box() {
    let glyph = GlyphSize {
        width: 7.0,
        height: 12.0,
        x: 1.5,
        y: 2.5,
    };

    assert_eq!(GlyphConstraint::NONE.constrain(glyph, metrics(), 1), glyph);
}

#[rstest]
fn ordinary_symbols_fit_the_cell_without_becoming_nerd_icons() {
    let constraint = terminal_glyph_constraint(u32::from('◆'));

    assert_eq!(constraint.size, GlyphConstraintSize::Fit);
    assert_eq!(constraint.height, GlyphConstraintHeight::Cell);
    assert_eq!(constraint.align_horizontal, GlyphConstraintAlign::None);
}

#[rstest]
fn nerd_icons_keep_the_libghostty_multi_cell_contract() {
    let constraint = nerd_font_constraint(0xF000).expect("Nerd Font icon constraint");

    assert_eq!(constraint.size, GlyphConstraintSize::FitCover1);
    assert_eq!(constraint.height, GlyphConstraintHeight::Icon);
    assert_eq!(constraint.align_horizontal, GlyphConstraintAlign::Center1);
    assert_eq!(constraint.align_vertical, GlyphConstraintAlign::Center1);
    assert_eq!(constraint.max_constraint_width, 2);
}

#[rstest]
fn powerline_joiners_stretch_past_cell_edges_to_avoid_seams() {
    let constraint = nerd_font_constraint(0xE0C0).expect("Powerline constraint");

    assert_eq!(constraint.size, GlyphConstraintSize::Stretch);
    assert_eq!(constraint.align_horizontal, GlyphConstraintAlign::Start);
    assert!(constraint.pad_left < 0.0);
    assert!(constraint.pad_right < 0.0);
}
