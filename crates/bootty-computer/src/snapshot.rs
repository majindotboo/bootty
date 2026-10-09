use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct as _};

use crate::{ComputerAction, ComputerError, ComputerResult, ComputerTarget, DisplayBounds};

/// A validated, bounded PNG result. It contains no filesystem destination or authority grant.
#[derive(Clone, Debug)]
pub struct ComputerResultSnapshot {
    target: Box<ComputerTarget>,
    region: Option<DisplayBounds>,
    requested_region: Option<DisplayBounds>,
    pixel_width: u32,
    pixel_height: u32,
    png: Vec<u8>,
}

impl TryFrom<ComputerResult> for ComputerResultSnapshot {
    type Error = ComputerError;

    fn try_from(result: ComputerResult) -> Result<Self, Self::Error> {
        let ComputerResult::Snapshot {
            target,
            requested_region,
            pixel_width,
            pixel_height,
            png_base64,
            ..
        } = &result
        else {
            return Err(invalid_image());
        };
        target.bounds.validate()?;
        let action = requested_region
            .as_ref()
            .map_or(ComputerAction::Snapshot, |rect| {
                ComputerAction::SnapshotRegion { rect: rect.clone() }
            });
        target.validate_action(&action)?;
        result.validate(target, &action)?;
        let png = STANDARD.decode(png_base64).map_err(|_| invalid_image())?;
        if png.len() > 8 * 1024 * 1024 {
            return Err(invalid_image());
        }
        let decoder = png::Decoder::new_with_limits(
            Cursor::new(&png),
            png::Limits {
                bytes: 32 * 1024 * 1024,
            },
        );
        let mut reader = decoder.read_info().map_err(|_| invalid_image())?;
        if reader.info().width != *pixel_width
            || reader.info().height != *pixel_height
            || reader.info().animation_control.is_some()
        {
            return Err(invalid_image());
        }
        let size = reader
            .output_buffer_size()
            .filter(|size| *size <= 32 * 1024 * 1024)
            .ok_or_else(invalid_image)?;
        let mut pixels = vec![0; size];
        reader
            .next_frame(&mut pixels)
            .map_err(|_| invalid_image())?;
        reader.finish().map_err(|_| invalid_image())?;
        drop(reader);
        let ComputerResult::Snapshot {
            target,
            region,
            requested_region,
            pixel_width,
            pixel_height,
            ..
        } = result
        else {
            return Err(invalid_image());
        };
        Ok(Self {
            target,
            region,
            requested_region,
            pixel_width,
            pixel_height,
            png,
        })
    }
}

impl ComputerResultSnapshot {
    #[must_use]
    pub fn png(&self) -> &[u8] {
        &self.png
    }

    #[must_use]
    pub fn into_png(self) -> Vec<u8> {
        self.png
    }

    #[must_use]
    pub fn png_base64(&self) -> String {
        STANDARD.encode(&self.png)
    }

    #[must_use]
    pub fn geometry(&self) -> (&ComputerTarget, &DisplayBounds, u32, u32) {
        (
            &self.target,
            self.region.as_ref().unwrap_or(&self.target.bounds),
            self.pixel_width,
            self.pixel_height,
        )
    }
}

impl Serialize for ComputerResultSnapshot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let fields = 5_usize
            .saturating_add(usize::from(self.region.is_some()))
            .saturating_add(usize::from(self.requested_region.is_some()));
        let mut result = serializer.serialize_struct("Snapshot", fields)?;
        result.serialize_field("result", "snapshot")?;
        result.serialize_field("png_base64", &self.png_base64())?;
        result.serialize_field("pixel_width", &self.pixel_width)?;
        result.serialize_field("pixel_height", &self.pixel_height)?;
        result.serialize_field("target", &self.target)?;
        if let Some(region) = &self.region {
            result.serialize_field("region", region)?;
        }
        if let Some(region) = &self.requested_region {
            result.serialize_field("requested_region", region)?;
        }
        result.end()
    }
}

impl<'de> Deserialize<'de> for ComputerResultSnapshot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(ComputerResult::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn invalid_image() -> ComputerError {
    ComputerError::Helper("invalid or oversized screenshot PNG".into())
}
