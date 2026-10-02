use std::ops::Range;

use gpui_kit::{FontStyle, FontWeight, HighlightStyle, Hsla, UnderlineStyle, px, rgb};
use vte::{Params, Perform};

/// Styles an already formatted desktop capture; the desktop owns VT state and cursor movement.
#[derive(Default)]
pub struct TerminalPresentation {
    pub text: String,
    pub highlights: Vec<(Range<usize>, HighlightStyle)>,
    style: HighlightStyle,
    concealed: bool,
    inverted: bool,
}

impl TerminalPresentation {
    #[must_use]
    pub fn parse(capture: &str) -> Self {
        let mut output = Self::default();
        vte::Parser::new().advance(&mut output, capture.as_bytes());
        output
    }

    fn append(&mut self, character: char) {
        let start = self.text.len();
        self.text.push(character);
        let mut style = self.style;
        if self.inverted {
            std::mem::swap(&mut style.color, &mut style.background_color);
            // Capture does not transmit the computer's default palette.
            style.color.get_or_insert(rgb(0x0000_0000).into());
            style
                .background_color
                .get_or_insert(rgb(0x00ff_ffff).into());
        }
        if let Some((range, previous)) = self.highlights.last_mut()
            && *previous == style
        {
            range.end = self.text.len();
        } else {
            self.highlights.push((start..self.text.len(), style));
        }
    }

    fn sgr(&mut self, code: u16) {
        match code {
            0 => {
                self.style = HighlightStyle::default();
                self.concealed = false;
                self.inverted = false;
            }
            1 => self.style.font_weight = Some(FontWeight::BOLD),
            2 => self.style.fade_out = Some(0.5),
            3 => self.style.font_style = Some(FontStyle::Italic),
            4 => {
                self.style.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    ..Default::default()
                });
            }
            7 => self.inverted = true,
            8 => self.concealed = true,
            22 => {
                self.style.font_weight = None;
                self.style.fade_out = None;
            }
            23 => self.style.font_style = None,
            24 => self.style.underline = None,
            27 => self.inverted = false,
            28 => self.concealed = false,
            30..=37 => self.style.color = indexed(code - 30),
            40..=47 => self.style.background_color = indexed(code - 40),
            90..=97 => self.style.color = indexed(code - 90 + 8),
            100..=107 => self.style.background_color = indexed(code - 100 + 8),
            39 => self.style.color = None,
            49 => self.style.background_color = None,
            _ => {}
        }
    }
}

impl Perform for TerminalPresentation {
    fn print(&mut self, character: char) {
        self.append(if self.concealed { ' ' } else { character });
    }

    fn execute(&mut self, byte: u8) {
        if byte == b'\n' {
            self.append('\n');
        }
        // Final-state captures expand tabs and use CRLF; CR is not a second line break.
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if action != 'm' || ignore || !intermediates.is_empty() {
            return;
        }
        let mut groups = params.iter();
        while let Some(group) = groups.next() {
            let Some(&code) = group.first() else {
                continue;
            };
            if code == 38 || code == 48 {
                let mut components = group
                    .iter()
                    .copied()
                    .skip(1)
                    .enumerate()
                    .filter_map(|(index, value)| (group.len() != 6 || index != 1).then_some(value));
                let color = if group.len() > 1 {
                    extended(&mut components)
                } else {
                    let mut semicolon = groups.by_ref().filter_map(|value| value.first().copied());
                    extended(&mut semicolon)
                };
                if code == 38 {
                    self.style.color = color;
                } else {
                    self.style.background_color = color;
                }
            } else {
                self.sgr(code);
            }
        }
    }
}

fn extended(values: &mut impl Iterator<Item = u16>) -> Option<Hsla> {
    match values.next()? {
        5 => indexed(values.next()?),
        2 => {
            let red = u8::try_from(values.next()?).ok()?;
            let green = u8::try_from(values.next()?).ok()?;
            let blue = u8::try_from(values.next()?).ok()?;
            Some(rgb((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)).into())
        }
        _ => None,
    }
}

// ANSI colors are terminal content, not UI theme roles. Use the xterm palette until
// the capture protocol supplies the computer's configured palette.
fn indexed(index: u16) -> Option<Hsla> {
    const BASE: [u32; 16] = [
        0x0000_0000,
        0x00cd_0000,
        0x0000_cd00,
        0x00cd_cd00,
        0x0000_00ee,
        0x00cd_00cd,
        0x0000_cdcd,
        0x00e5_e5e5,
        0x007f_7f7f,
        0x00ff_0000,
        0x0000_ff00,
        0x00ff_ff00,
        0x005c_5cff,
        0x00ff_00ff,
        0x0000_ffff,
        0x00ff_ffff,
    ];
    let color = match index {
        0..=15 => *BASE.get(usize::from(index))?,
        16..=231 => {
            let value = u32::from(index - 16);
            let channel = |level| if level == 0 { 0 } else { 55 + level * 40 };
            (channel(value / 36) << 16) | (channel(value / 6 % 6) << 8) | channel(value % 6)
        }
        232..=255 => u32::from(8 + (index - 232) * 10) * 0x0001_0101,
        _ => return None,
    };
    Some(rgb(color).into())
}
