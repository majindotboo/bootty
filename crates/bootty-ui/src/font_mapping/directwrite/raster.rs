use super::Fonts;
use anyhow::{Context as _, Result};
use gpui_kit::{
    Bounds, DevicePixels, RenderGlyphParams, SUBPIXEL_VARIANTS_X, SUBPIXEL_VARIANTS_Y, Size, point,
    size,
};
use std::mem::ManuallyDrop;
use windows::{
    Win32::{
        Foundation::{DWRITE_E_NOCOLOR, HMODULE, RECT},
        Graphics::{
            Direct2D::Common::{
                D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
            },
            Direct2D::{
                D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ,
                D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
                D2D1_COLOR_BITMAP_GLYPH_SNAP_OPTION_DEFAULT, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
                D2D1_MAP_OPTIONS_READ, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateDevice,
                ID2D1DeviceContext4, ID2D1Image, ID2D1SvgGlyphStyle,
            },
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP},
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
                ID3D11Device,
            },
            DirectWrite::{
                DWRITE_GLYPH_IMAGE_FORMATS_CFF, DWRITE_GLYPH_IMAGE_FORMATS_COLR,
                DWRITE_GLYPH_IMAGE_FORMATS_JPEG, DWRITE_GLYPH_IMAGE_FORMATS_PNG,
                DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8, DWRITE_GLYPH_IMAGE_FORMATS_SVG,
                DWRITE_GLYPH_IMAGE_FORMATS_TIFF, DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE,
                DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN, DWRITE_GRID_FIT_MODE, DWRITE_MATRIX,
                DWRITE_MEASURING_MODE_NATURAL, DWRITE_OUTLINE_THRESHOLD_ANTIALIASED,
                DWRITE_RENDERING_MODE1, DWRITE_RENDERING_MODE1_NATURAL_SYMMETRIC,
                DWRITE_RENDERING_MODE1_OUTLINE, DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE, DWRITE_TEXTURE_ALIASED_1x1,
                DWRITE_TEXTURE_CLEARTYPE_3x1, DWRITE_TEXTURE_TYPE, IDWriteFactory4,
                IDWriteFontFace3, IDWriteGlyphRunAnalysis,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Dxgi::{IDXGIAdapter, IDXGIDevice},
        },
    },
    core::Interface,
};
use windows_numerics::{Matrix3x2, Vector2};

struct RetainedRun(DWRITE_GLYPH_RUN);
impl Drop for RetainedRun {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.0.fontFace);
        }
    }
}

fn with_run<T>(
    face: &IDWriteFontFace3,
    params: &RenderGlyphParams,
    action: impl FnOnce(&DWRITE_GLYPH_RUN) -> Result<T>,
) -> Result<T> {
    let glyph = u16::try_from(params.glyph_id.0)?;
    let advance = 0.0;
    let offset = DWRITE_GLYPH_OFFSET::default();
    let run = RetainedRun(DWRITE_GLYPH_RUN {
        fontFace: ManuallyDrop::new(Some(face.cast()?)),
        fontEmSize: params.font_size.into(),
        glyphCount: 1,
        glyphIndices: &glyph,
        glyphAdvances: &advance,
        glyphOffsets: &offset,
        isSideways: false.into(),
        bidiLevel: 0,
    });
    action(&run.0)
}

fn origin(params: &RenderGlyphParams) -> Vector2 {
    Vector2 {
        X: f32::from(params.subpixel_variant.x)
            / f32::from(SUBPIXEL_VARIANTS_X)
            / params.scale_factor,
        Y: f32::from(params.subpixel_variant.y)
            / f32::from(SUBPIXEL_VARIANTS_Y)
            / params.scale_factor,
    }
}

fn analysis(
    factory: &IDWriteFactory4,
    face: &IDWriteFontFace3,
    params: &RenderGlyphParams,
) -> Result<IDWriteGlyphRunAnalysis> {
    let matrix = DWRITE_MATRIX {
        m11: params.scale_factor,
        m22: params.scale_factor,
        ..DWRITE_MATRIX::default()
    };
    let mut mode = DWRITE_RENDERING_MODE1::default();
    let mut fitting = DWRITE_GRID_FIT_MODE::default();
    unsafe {
        face.GetRecommendedRenderingMode(
            params.font_size.into(),
            96.0,
            96.0,
            Some(&matrix),
            false,
            DWRITE_OUTLINE_THRESHOLD_ANTIALIASED,
            DWRITE_MEASURING_MODE_NATURAL,
            None,
            &mut mode,
            &mut fitting,
        )?;
    }
    if mode == DWRITE_RENDERING_MODE1_OUTLINE {
        mode = DWRITE_RENDERING_MODE1_NATURAL_SYMMETRIC;
    }
    let origin = origin(params);
    with_run(face, params, |run| {
        Ok(unsafe {
            factory.CreateGlyphRunAnalysis(
                run,
                Some(&matrix),
                mode,
                DWRITE_MEASURING_MODE_NATURAL,
                fitting,
                if params.subpixel_rendering {
                    DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE
                } else {
                    DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE
                },
                origin.X,
                origin.Y,
            )?
        })
    })
}

fn texture(params: &RenderGlyphParams) -> DWRITE_TEXTURE_TYPE {
    if params.subpixel_rendering {
        DWRITE_TEXTURE_CLEARTYPE_3x1
    } else {
        DWRITE_TEXTURE_ALIASED_1x1
    }
}

impl Fonts {
    fn color_raster(&mut self) -> Result<&ColorRaster> {
        if self.color.is_none() {
            self.color = Some(ColorRaster::new()?);
        }
        Ok(self.color.as_ref().expect("initialized color raster"))
    }

    pub(super) fn bounds(&mut self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let face = self.faces[params.font_id.0].native.clone();
        if params.is_emoji {
            return self.color_raster()?.bounds(&face, params);
        }
        let rect = unsafe {
            analysis(&self.factory, &face, params)?.GetAlphaTextureBounds(texture(params))?
        };
        Ok(Bounds::new(
            point(DevicePixels(rect.left), DevicePixels(rect.top)),
            size(
                DevicePixels((rect.right - rect.left).max(0)),
                DevicePixels((rect.bottom - rect.top).max(0)),
            ),
        ))
    }

    pub(super) fn raster(
        &mut self,
        params: &RenderGlyphParams,
        bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        let face = self.faces[params.font_id.0].native.clone();
        if params.is_emoji {
            let factory = self.factory.clone();
            let result = self.color_raster()?.draw(&factory, &face, params, bounds);
            // A lost device is retried with a fresh context on the next glyph request.
            if result.is_err() {
                self.color = None;
            }
            return result.map(|pixels| (bounds.size, pixels));
        }
        let width = usize::try_from(bounds.size.width.0)?;
        let height = usize::try_from(bounds.size.height.0)?;
        let count = width
            .checked_mul(height)
            .context("Glyph bitmap size overflow")?;
        let channels = if params.subpixel_rendering { 3 } else { 1 };
        let mut bitmap = vec![
            0;
            count
                .checked_mul(channels)
                .context("Glyph bitmap size overflow")?
        ];
        let rect = RECT {
            left: bounds.origin.x.0,
            top: bounds.origin.y.0,
            right: bounds.origin.x.0 + bounds.size.width.0,
            bottom: bounds.origin.y.0 + bounds.size.height.0,
        };
        unsafe {
            analysis(&self.factory, &face, params)?.CreateAlphaTexture(
                texture(params),
                &rect,
                &mut bitmap,
            )?;
        }
        if params.subpixel_rendering {
            bitmap = bitmap
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 0])
                .collect();
        }
        Ok((bounds.size, bitmap))
    }
}

/// One lazy color context serves every ambiguous face; ordinary/color fallbacks reuse GPUI.
pub(super) struct ColorRaster {
    context: ID2D1DeviceContext4,
}

impl ColorRaster {
    fn new() -> Result<Self> {
        let mut device: Option<ID3D11Device> = None;
        let create = |driver, device: &mut Option<ID3D11Device>| unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(device),
                None,
                None,
            )
        };
        create(D3D_DRIVER_TYPE_HARDWARE, &mut device)
            .or_else(|_| create(D3D_DRIVER_TYPE_WARP, &mut device))?;
        let dxgi: IDXGIDevice = device.context("Direct3D returned no device")?.cast()?;
        let context = unsafe {
            D2D1CreateDevice(&dxgi, None)?
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?
                .cast()?
        };
        Ok(Self { context })
    }

    fn bounds(
        &self,
        face: &IDWriteFontFace3,
        params: &RenderGlyphParams,
    ) -> Result<Bounds<DevicePixels>> {
        unsafe {
            self.context
                .SetTransform(&Matrix3x2::scale(params.scale_factor, params.scale_factor));
        }
        let rect = with_run(face, params, |run| {
            Ok(unsafe {
                self.context.GetGlyphRunWorldBounds(
                    origin(params),
                    run,
                    DWRITE_MEASURING_MODE_NATURAL,
                )?
            })
        })?;
        let left = rect.left.floor() as i32 - 1;
        let top = rect.top.floor() as i32 - 1;
        Ok(Bounds::new(
            point(DevicePixels(left), DevicePixels(top)),
            size(
                DevicePixels(rect.right.ceil() as i32 + 1 - left),
                DevicePixels(rect.bottom.ceil() as i32 + 1 - top),
            ),
        ))
    }

    fn draw(
        &self,
        factory: &IDWriteFactory4,
        face: &IDWriteFontFace3,
        params: &RenderGlyphParams,
        bounds: Bounds<DevicePixels>,
    ) -> Result<Vec<u8>> {
        let dimensions = D2D_SIZE_U {
            width: u32::try_from(bounds.size.width.0)?,
            height: u32::try_from(bounds.size.height.0)?,
        };
        let properties = |options| D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: options,
            colorContext: ManuallyDrop::new(None),
        };
        let target = unsafe {
            self.context.CreateBitmap(
                dimensions,
                None,
                0,
                &properties(D2D1_BITMAP_OPTIONS_TARGET),
            )?
        };
        let staging = unsafe {
            self.context.CreateBitmap(
                dimensions,
                None,
                0,
                &properties(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW),
            )?
        };
        unsafe {
            self.context.SetTarget(&target);
            self.context.SetTransform(&Matrix3x2 {
                M11: params.scale_factor,
                M22: params.scale_factor,
                M31: -(bounds.origin.x.0 as f32),
                M32: -(bounds.origin.y.0 as f32),
                ..Matrix3x2::default()
            });
            self.context
                .SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            self.context.BeginDraw();
            self.context.Clear(None);
        }
        let draw_result = with_run(face, params, |run| {
            self.draw_layers(factory, run, origin(params))
        });
        let finish = unsafe { self.context.EndDraw(None, None) };
        unsafe {
            self.context.SetTarget(None::<&ID2D1Image>);
        }
        draw_result?;
        finish?;
        unsafe {
            staging.CopyFromBitmap(None, &target, None)?;
        }
        let stride = usize::try_from(dimensions.width)?
            .checked_mul(4)
            .context("Glyph row overflow")?;
        let count = stride
            .checked_mul(usize::try_from(dimensions.height)?)
            .context("Glyph bitmap overflow")?;
        let mut pixels = Vec::with_capacity(count);
        let mapped = unsafe { staging.Map(D2D1_MAP_OPTIONS_READ)? };
        if mapped.bits.is_null() || (mapped.pitch as usize) < stride {
            unsafe {
                staging.Unmap()?;
            }
            anyhow::bail!("Direct2D returned an invalid glyph bitmap");
        }
        for row in 0..dimensions.height as usize {
            // SAFETY: Direct2D owns the mapped image until Unmap. Each copy fits one row.
            pixels.extend_from_slice(unsafe {
                std::slice::from_raw_parts(mapped.bits.add(row * mapped.pitch as usize), stride)
            });
        }
        unsafe {
            staging.Unmap()?;
        }
        for pixel in pixels.as_chunks_mut::<4>().0 {
            if pixel[3] > 0 {
                let alpha = f32::from(pixel[3]) / 255.0;
                for channel in &mut pixel[..3] {
                    *channel = (f32::from(*channel) / alpha) as u8;
                }
            }
        }
        Ok(pixels)
    }

    fn draw_layers(
        &self,
        factory: &IDWriteFactory4,
        run: &DWRITE_GLYPH_RUN,
        baseline: Vector2,
    ) -> Result<()> {
        let formats = DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE
            | DWRITE_GLYPH_IMAGE_FORMATS_CFF
            | DWRITE_GLYPH_IMAGE_FORMATS_COLR
            | DWRITE_GLYPH_IMAGE_FORMATS_SVG
            | DWRITE_GLYPH_IMAGE_FORMATS_PNG
            | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
            | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
            | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8;
        let default = unsafe {
            self.context.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
                None,
            )?
        };
        let layers = match unsafe {
            factory.TranslateColorGlyphRun(
                baseline,
                run,
                None,
                formats,
                DWRITE_MEASURING_MODE_NATURAL,
                None,
                0,
            )
        } {
            Ok(layers) => layers,
            Err(error) if error.code() == DWRITE_E_NOCOLOR => {
                unsafe {
                    self.context.DrawGlyphRun(
                        baseline,
                        run,
                        None,
                        &default,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        while unsafe { layers.MoveNext()?.as_bool() } {
            let layer = unsafe { layers.GetCurrentRun()?.as_ref() }
                .context("DirectWrite returned a null color layer")?;
            let glyphs = &layer.Base.glyphRun;
            let origin = Vector2 {
                X: layer.Base.baselineOriginX,
                Y: layer.Base.baselineOriginY,
            };
            unsafe {
                match layer.glyphImageFormat {
                    DWRITE_GLYPH_IMAGE_FORMATS_SVG => self.context.DrawSvgGlyphRun(
                        origin,
                        glyphs,
                        &default,
                        None::<&ID2D1SvgGlyphStyle>,
                        0,
                        layer.measuringMode,
                    ),
                    DWRITE_GLYPH_IMAGE_FORMATS_PNG
                    | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
                    | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
                    | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8 => {
                        self.context.DrawColorBitmapGlyphRun(
                            layer.glyphImageFormat,
                            origin,
                            glyphs,
                            layer.measuringMode,
                            D2D1_COLOR_BITMAP_GLYPH_SNAP_OPTION_DEFAULT,
                        )
                    }
                    _ => {
                        let color = layer.Base.runColor;
                        let brush = if layer.Base.paletteIndex == 0xffff {
                            default.clone()
                        } else {
                            self.context.CreateSolidColorBrush(
                                &D2D1_COLOR_F {
                                    r: color.r,
                                    g: color.g,
                                    b: color.b,
                                    a: color.a,
                                },
                                None,
                            )?
                        };
                        self.context.DrawGlyphRun(
                            origin,
                            glyphs,
                            None,
                            &brush,
                            layer.measuringMode,
                        );
                    }
                }
            }
        }
        Ok(())
    }
}
