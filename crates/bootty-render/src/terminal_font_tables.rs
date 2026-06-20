use std::str;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontTableError {
    Truncated,
    InvalidTag,
    UnsupportedSvgVersion,
    InstructionsNotSupported,
    CompositeNotSupported,
    EndPointsOutOfOrder,
    TooManyPoints,
}

type Result<T> = std::result::Result<T, FontTableError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sfnt<'a> {
    data: &'a [u8],
    records: &'a [u8],
}

impl<'a> Sfnt<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < 12 {
            return Err(FontTableError::Truncated);
        }
        let num_tables = u16_at(data, 4)? as usize;
        let directory_len = num_tables
            .checked_mul(16)
            .and_then(|len| 12_usize.checked_add(len))
            .ok_or(FontTableError::Truncated)?;
        if data.len() < directory_len {
            return Err(FontTableError::Truncated);
        }
        Ok(Self {
            data,
            records: &data[12..directory_len],
        })
    }

    pub fn num_tables(self) -> usize {
        self.records.len() / 16
    }

    pub fn table_tag(self, index: usize) -> Result<&'a str> {
        let record = self.record(index)?;
        str::from_utf8(&record[..4]).map_err(|_| FontTableError::InvalidTag)
    }

    pub fn table(self, tag: &[u8; 4]) -> Result<Option<&'a [u8]>> {
        for index in 0..self.num_tables() {
            let record = self.record(index)?;
            if &record[..4] == tag {
                let offset = u32_at(record, 8)? as usize;
                let len = u32_at(record, 12)? as usize;
                let end = offset.checked_add(len).ok_or(FontTableError::Truncated)?;
                return if end <= self.data.len() {
                    Ok(Some(&self.data[offset..end]))
                } else {
                    Err(FontTableError::Truncated)
                };
            }
        }
        Ok(None)
    }

    fn record(self, index: usize) -> Result<&'a [u8]> {
        let start = index.checked_mul(16).ok_or(FontTableError::Truncated)?;
        let end = start.checked_add(16).ok_or(FontTableError::Truncated)?;
        self.records
            .get(start..end)
            .ok_or(FontTableError::Truncated)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyfTable<'a> {
    data: &'a [u8],
}

impl<'a> GlyfTable<'a> {
    pub fn parse(data: &'a [u8]) -> Self {
        Self { data }
    }

    pub fn entry(self, offset: usize) -> Result<GlyfEntry<'a>> {
        GlyfEntry::parse(self.data.get(offset..).ok_or(FontTableError::Truncated)?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyfEntry<'a> {
    pub number_of_contours: i16,
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
    data: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyfEntryType {
    Simple,
    Composite,
}

impl<'a> GlyfEntry<'a> {
    pub const HEADER_SIZE: usize = 10;

    pub fn parse(data: &'a [u8]) -> Result<Self> {
        Ok(Self {
            number_of_contours: i16_at(data, 0)?,
            x_min: i16_at(data, 2)?,
            y_min: i16_at(data, 4)?,
            x_max: i16_at(data, 6)?,
            y_max: i16_at(data, 8)?,
            data: data
                .get(Self::HEADER_SIZE..)
                .ok_or(FontTableError::Truncated)?,
        })
    }

    pub fn entry_type(self) -> GlyfEntryType {
        if self.number_of_contours >= 0 {
            GlyfEntryType::Simple
        } else {
            GlyfEntryType::Composite
        }
    }

    pub fn size(self) -> Result<usize> {
        match self.entry_type() {
            GlyfEntryType::Composite => Err(FontTableError::CompositeNotSupported),
            GlyfEntryType::Simple => self.simple_size(),
        }
    }

    fn simple_size(self) -> Result<usize> {
        let num_contours =
            usize::try_from(self.number_of_contours).map_err(|_| FontTableError::Truncated)?;
        if num_contours == 0 && self.data.len() < 2 {
            return Ok(Self::HEADER_SIZE);
        }

        let mut pos = 0;
        let mut max_point_index: Option<u16> = None;
        for _ in 0..num_contours {
            let index = u16_at(self.data, pos)?;
            if max_point_index.is_some_and(|prev| index <= prev) {
                return Err(FontTableError::EndPointsOutOfOrder);
            }
            max_point_index = Some(index);
            pos += 2;
        }

        let instructions_length = u16_at(self.data, pos)? as usize;
        pos += 2;
        if instructions_length > 0 {
            return Err(FontTableError::InstructionsNotSupported);
        }

        let max_point_index = max_point_index.ok_or(FontTableError::Truncated)? as usize;
        let mut point_index = 0;
        let mut x_coords_len = 0usize;
        let mut y_coords_len = 0usize;
        while point_index <= max_point_index {
            let flag = *self.data.get(pos).ok_or(FontTableError::Truncated)?;
            pos += 1;

            let x_bytes = coord_bytes(flag, 0x02, 0x10);
            let y_bytes = coord_bytes(flag, 0x04, 0x20);
            x_coords_len += x_bytes;
            y_coords_len += y_bytes;

            if flag & 0x08 != 0 {
                let repeat = *self.data.get(pos).ok_or(FontTableError::Truncated)? as usize;
                pos += 1;
                point_index += repeat;
                x_coords_len += repeat * x_bytes;
                y_coords_len += repeat * y_bytes;
                if point_index > max_point_index {
                    return Err(FontTableError::TooManyPoints);
                }
            }
            point_index += 1;
        }

        let coords_len = x_coords_len
            .checked_add(y_coords_len)
            .ok_or(FontTableError::Truncated)?;
        let end = pos
            .checked_add(coords_len)
            .ok_or(FontTableError::Truncated)?;
        self.data.get(pos..end).ok_or(FontTableError::Truncated)?;
        Ok(Self::HEADER_SIZE + end)
    }
}

fn coord_bytes(flag: u8, short_mask: u8, repeat_or_sign_mask: u8) -> usize {
    if flag & short_mask != 0 {
        1
    } else if flag & repeat_or_sign_mask != 0 {
        0
    } else {
        2
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadTable {
    pub major_version: u16,
    pub minor_version: u16,
    pub font_revision: Fixed,
    pub checksum_adjustment: u32,
    pub magic_number: u32,
    pub flags: u16,
    pub units_per_em: u16,
    pub created: i64,
    pub modified: i64,
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
    pub mac_style: u16,
    pub lowest_rec_ppem: u16,
    pub font_direction_hint: i16,
    pub index_to_loc_format: i16,
    pub glyph_data_format: i16,
}

impl HeadTable {
    pub fn parse(data: &[u8]) -> Result<Self> {
        Ok(Self {
            major_version: u16_at(data, 0)?,
            minor_version: u16_at(data, 2)?,
            font_revision: Fixed(i32_at(data, 4)?),
            checksum_adjustment: u32_at(data, 8)?,
            magic_number: u32_at(data, 12)?,
            flags: u16_at(data, 16)?,
            units_per_em: u16_at(data, 18)?,
            created: i64_at(data, 20)?,
            modified: i64_at(data, 28)?,
            x_min: i16_at(data, 36)?,
            y_min: i16_at(data, 38)?,
            x_max: i16_at(data, 40)?,
            y_max: i16_at(data, 42)?,
            mac_style: u16_at(data, 44)?,
            lowest_rec_ppem: u16_at(data, 46)?,
            font_direction_hint: i16_at(data, 48)?,
            index_to_loc_format: i16_at(data, 50)?,
            glyph_data_format: i16_at(data, 52)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HheaTable {
    pub major_version: u16,
    pub minor_version: u16,
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    pub advance_width_max: u16,
    pub min_left_side_bearing: i16,
    pub min_right_side_bearing: i16,
    pub x_max_extent: i16,
    pub caret_slope_rise: i16,
    pub caret_slope_run: i16,
    pub caret_offset: i16,
    pub metric_data_format: i16,
    pub number_of_h_metrics: u16,
}

impl HheaTable {
    pub fn parse(data: &[u8]) -> Result<Self> {
        Ok(Self {
            major_version: u16_at(data, 0)?,
            minor_version: u16_at(data, 2)?,
            ascender: i16_at(data, 4)?,
            descender: i16_at(data, 6)?,
            line_gap: i16_at(data, 8)?,
            advance_width_max: u16_at(data, 10)?,
            min_left_side_bearing: i16_at(data, 12)?,
            min_right_side_bearing: i16_at(data, 14)?,
            x_max_extent: i16_at(data, 16)?,
            caret_slope_rise: i16_at(data, 18)?,
            caret_slope_run: i16_at(data, 20)?,
            caret_offset: i16_at(data, 22)?,
            metric_data_format: i16_at(data, 32)?,
            number_of_h_metrics: u16_at(data, 34)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostTable {
    pub version: u32,
    pub italic_angle: Fixed,
    pub underline_position: i16,
    pub underline_thickness: i16,
    pub is_fixed_pitch: u32,
    pub min_mem_type42: u32,
    pub max_mem_type42: u32,
    pub min_mem_type1: u32,
    pub max_mem_type1: u32,
}

impl PostTable {
    pub fn parse(data: &[u8]) -> Result<Self> {
        Ok(Self {
            version: u32_at(data, 0)?,
            italic_angle: Fixed(i32_at(data, 4)?),
            underline_position: i16_at(data, 8)?,
            underline_thickness: i16_at(data, 10)?,
            is_fixed_pitch: u32_at(data, 12)?,
            min_mem_type42: u32_at(data, 16)?,
            max_mem_type42: u32_at(data, 20)?,
            min_mem_type1: u32_at(data, 24)?,
            max_mem_type1: u32_at(data, 28)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Os2Table {
    pub version: u16,
    pub x_avg_char_width: i16,
    pub us_weight_class: u16,
    pub us_width_class: u16,
    pub fs_type: u16,
    pub panose: [u8; 10],
    pub ach_vend_id: [u8; 4],
    pub fs_selection: FsSelection,
    pub typo_ascender: i16,
    pub typo_descender: i16,
    pub typo_line_gap: i16,
    pub win_ascent: u16,
    pub win_descent: u16,
    pub sx_height: i16,
    pub cap_height: i16,
    pub max_context: u16,
}

impl Os2Table {
    pub fn parse(data: &[u8]) -> Result<Self> {
        Ok(Self {
            version: u16_at(data, 0)?,
            x_avg_char_width: i16_at(data, 2)?,
            us_weight_class: u16_at(data, 4)?,
            us_width_class: u16_at(data, 6)?,
            fs_type: u16_at(data, 8)?,
            panose: bytes_at::<10>(data, 32)?,
            ach_vend_id: bytes_at::<4>(data, 58)?,
            fs_selection: FsSelection(u16_at(data, 62)?),
            typo_ascender: i16_at(data, 68)?,
            typo_descender: i16_at(data, 70)?,
            typo_line_gap: i16_at(data, 72)?,
            win_ascent: u16_at(data, 74)?,
            win_descent: u16_at(data, 76)?,
            sx_height: i16_at(data, 86)?,
            cap_height: i16_at(data, 88)?,
            max_context: u16_at(data, 94)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SvgTable<'a> {
    pub start_glyph_id: u16,
    pub end_glyph_id: u16,
    records: &'a [u8],
    count: usize,
}

impl<'a> SvgTable<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if u16_at(data, 0)? != 0 {
            return Err(FontTableError::UnsupportedSvgVersion);
        }
        let offset = u32_at(data, 2)? as usize;
        let count = u16_at(data, offset)? as usize;
        let records_start = offset.checked_add(2).ok_or(FontTableError::Truncated)?;
        let records_len = count.checked_mul(12).ok_or(FontTableError::Truncated)?;
        let records_end = records_start
            .checked_add(records_len)
            .ok_or(FontTableError::Truncated)?;
        let records = data
            .get(records_start..records_end)
            .ok_or(FontTableError::Truncated)?;
        let start_glyph_id = u16_at(records, 0)?;
        let last = records_len
            .checked_sub(12)
            .ok_or(FontTableError::Truncated)?;
        let end_glyph_id = u16_at(records, last + 2)?;
        Ok(Self {
            start_glyph_id,
            end_glyph_id,
            records,
            count,
        })
    }

    pub fn has_glyph(self, glyph_id: u16) -> bool {
        if glyph_id < self.start_glyph_id || glyph_id > self.end_glyph_id {
            return false;
        }
        if glyph_id == self.start_glyph_id || glyph_id == self.end_glyph_id {
            return true;
        }
        for index in 0..self.count {
            let record = &self.records[index * 12..][..12];
            let start = u16_at(record, 0).unwrap_or_default();
            let end = u16_at(record, 2).unwrap_or_default();
            if glyph_id >= start && glyph_id <= end {
                return true;
            }
        }
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fixed(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsSelection(u16);

impl FsSelection {
    pub fn regular(self) -> bool {
        self.0 & (1 << 6) != 0
    }

    pub fn use_typo_metrics(self) -> bool {
        self.0 & (1 << 7) != 0
    }
}

fn bytes_at<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    data.get(offset..offset + N)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(FontTableError::Truncated)
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(bytes_at(data, offset)?))
}
fn i16_at(data: &[u8], offset: usize) -> Result<i16> {
    Ok(i16::from_be_bytes(bytes_at(data, offset)?))
}
fn u32_at(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(bytes_at(data, offset)?))
}
fn i32_at(data: &[u8], offset: usize) -> Result<i32> {
    Ok(i32::from_be_bytes(bytes_at(data, offset)?))
}
fn i64_at(data: &[u8], offset: usize) -> Result<i64> {
    Ok(i64::from_be_bytes(bytes_at(data, offset)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opentype_glyf_ports_zero_contour_header_only() {
        let glyph = GlyfEntry::parse(&[0; GlyfEntry::HEADER_SIZE]).unwrap();
        assert_eq!(glyph.number_of_contours, 0);
        assert_eq!(glyph.size(), Ok(GlyfEntry::HEADER_SIZE));
    }
}
