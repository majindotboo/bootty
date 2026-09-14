use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};
use windows::{
    Win32::{
        Foundation::E_POINTER,
        Graphics::DirectWrite::{
            DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN, DWRITE_GLYPH_RUN_DESCRIPTION, DWRITE_MATRIX,
            DWRITE_MEASURING_MODE, DWRITE_STRIKETHROUGH, DWRITE_UNDERLINE, IDWriteFontFace3,
            IDWriteInlineObject, IDWritePixelSnapping_Impl, IDWriteTextRenderer,
            IDWriteTextRenderer_Impl,
        },
    },
    core::{BOOL, Error, IUnknown, Interface, Ref, Result, implement},
};

pub(super) struct NativeGlyph {
    pub id: u16,
    pub advance: f32,
    pub offset: DWRITE_GLYPH_OFFSET,
}

pub(super) struct NativeRun {
    pub face: IDWriteFontFace3,
    pub glyphs: Vec<NativeGlyph>,
    pub indices: Vec<usize>,
    pub text_start: u32,
    pub x: f32,
    pub y: f32,
    pub rtl: bool,
}

#[implement(IDWriteTextRenderer)]
pub(super) struct LayoutRenderer(pub Arc<Mutex<Vec<NativeRun>>>);

impl IDWritePixelSnapping_Impl for LayoutRenderer_Impl {
    fn IsPixelSnappingDisabled(&self, _: *const c_void) -> Result<BOOL> {
        Ok(true.into())
    }
    fn GetCurrentTransform(&self, _: *const c_void, transform: *mut DWRITE_MATRIX) -> Result<()> {
        let transform = unsafe { transform.as_mut() }.ok_or_else(|| Error::from(E_POINTER))?;
        *transform = DWRITE_MATRIX {
            m11: 1.0,
            m22: 1.0,
            ..DWRITE_MATRIX::default()
        };
        Ok(())
    }
    fn GetPixelsPerDip(&self, _: *const c_void) -> Result<f32> {
        Ok(1.0)
    }
}

fn copied_slice<T: Copy>(pointer: *const T, count: usize) -> Result<Vec<T>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if pointer.is_null() {
        return Err(Error::from(E_POINTER));
    }
    // SAFETY: these buffers are supplied by DirectWrite for the duration of DrawGlyphRun;
    // their documented counts are used, and no borrowed pointer escapes the callback.
    Ok(unsafe { std::slice::from_raw_parts(pointer, count) }.to_vec())
}

impl IDWriteTextRenderer_Impl for LayoutRenderer_Impl {
    fn DrawGlyphRun(
        &self,
        _: *const c_void,
        x: f32,
        y: f32,
        _: DWRITE_MEASURING_MODE,
        glyphs: *const DWRITE_GLYPH_RUN,
        description: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _: Ref<IUnknown>,
    ) -> Result<()> {
        let glyphs = unsafe { glyphs.as_ref() }.ok_or_else(|| Error::from(E_POINTER))?;
        let description = unsafe { description.as_ref() }.ok_or_else(|| Error::from(E_POINTER))?;
        let face: IDWriteFontFace3 = glyphs
            .fontFace
            .as_ref()
            .ok_or_else(|| Error::from(E_POINTER))?
            .cast()?;
        let count = glyphs.glyphCount as usize;
        let ids = copied_slice(glyphs.glyphIndices, count)?;
        let advances = copied_slice(glyphs.glyphAdvances, count)?;
        let offsets = copied_slice(glyphs.glyphOffsets, count)?;
        let clusters = copied_slice(description.clusterMap, description.stringLength as usize)?;
        let mut indices = vec![None; count];
        for (index, &glyph) in clusters.iter().enumerate() {
            let slot = indices
                .get_mut(usize::from(glyph))
                .ok_or_else(|| Error::from(E_POINTER))?;
            slot.get_or_insert(description.textPosition as usize + index);
        }
        // Multiple glyphs can belong to one cluster; ligatures can cover multiple UTF16 units.
        let mut cluster = description.textPosition as usize;
        let indices = indices
            .into_iter()
            .map(|index| {
                if let Some(index) = index {
                    cluster = index;
                }
                cluster
            })
            .collect();
        let rtl = glyphs.bidiLevel % 2 == 1;
        let glyphs = ids
            .into_iter()
            .zip(advances)
            .zip(offsets)
            .map(|((id, advance), offset)| NativeGlyph {
                id,
                advance,
                offset,
            })
            .collect();
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(NativeRun {
                face,
                glyphs,
                indices,
                text_start: description.textPosition,
                x,
                y,
                rtl,
            });
        Ok(())
    }
    fn DrawUnderline(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_UNDERLINE,
        _: Ref<IUnknown>,
    ) -> Result<()> {
        Ok(())
    }
    fn DrawStrikethrough(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_STRIKETHROUGH,
        _: Ref<IUnknown>,
    ) -> Result<()> {
        Ok(())
    }
    fn DrawInlineObject(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: Ref<IDWriteInlineObject>,
        _: BOOL,
        _: BOOL,
        _: Ref<IUnknown>,
    ) -> Result<()> {
        Ok(())
    }
}
