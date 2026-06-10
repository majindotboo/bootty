use std::{ffi::c_void, ptr};

use libghostty_vt::style::RgbColor;

use crate::terminal_palette::{Palette, generate_256_palette};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ColorRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

const GHOSTTY_NO_VALUE: i32 = -4;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ghostty_color_palette_generate_256(
    base: *const ColorRgb,
    skip: *const bool,
    bg: *const ColorRgb,
    fg: *const ColorRgb,
    harmonious: bool,
    out: *mut ColorRgb,
) {
    if base.is_null() || out.is_null() {
        return;
    }

    let mut base_palette = [RgbColor { r: 0, g: 0, b: 0 }; 256];
    let mut skip_palette = [false; 256];
    for index in 0..256 {
        base_palette[index] = unsafe { (*base.add(index)).into() };
        if !skip.is_null() {
            skip_palette[index] = unsafe { *skip.add(index) };
        }
    }

    let bg = pointed_color_or(bg, base_palette[0]);
    let fg = pointed_color_or(fg, base_palette[7]);
    let palette = generate_256_palette(&base_palette, &skip_palette, bg, fg, harmonious);
    write_palette(out, &palette);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ghostty_osc_semantic_prompt_write_command_line(
    _command: *mut c_void,
    _out: *mut u8,
    _out_len: usize,
    out_written: *mut usize,
) -> i32 {
    if !out_written.is_null() {
        unsafe { ptr::write(out_written, 0) };
    }
    GHOSTTY_NO_VALUE
}

fn pointed_color_or(color: *const ColorRgb, fallback: RgbColor) -> RgbColor {
    if color.is_null() {
        fallback
    } else {
        unsafe { (*color).into() }
    }
}

fn write_palette(out: *mut ColorRgb, palette: &Palette) {
    for (index, color) in palette.iter().copied().enumerate() {
        unsafe { ptr::write(out.add(index), color.into()) };
    }
}

impl From<ColorRgb> for RgbColor {
    fn from(color: ColorRgb) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}

impl From<RgbColor> for ColorRgb {
    fn from(color: RgbColor) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}
