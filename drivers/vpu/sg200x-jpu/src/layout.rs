//! Pure, checked layout rules for planar frames produced by the JPU.

const PLANE_ALIGNMENT: usize = 8;
const SCALED_STRIDE_ALIGNMENT: u32 = 16;
const MIN_SCALED_CODED_EXTENT: u32 = 128;

/// Isotropic downscale mode implemented by the SG2002 JPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JpuScale {
    /// Decode at the JPEG's full coded resolution.
    Full,
    /// Decode at one half of the coded width and height.
    Half,
    /// Decode at one quarter of the coded width and height.
    Quarter,
    /// Decode at one eighth of the coded width and height.
    Eighth,
}

impl JpuScale {
    /// Linear scale denominator.
    pub const fn factor(self) -> u32 {
        1 << self.register_mode()
    }

    /// Two-bit mode written to each axis in `MJPEG_SCL_INFO`.
    pub(crate) const fn register_mode(self) -> u32 {
        match self {
            Self::Full => 0,
            Self::Half => 1,
            Self::Quarter => 2,
            Self::Eighth => 3,
        }
    }

    pub(crate) const fn is_scaled(self) -> bool {
        !matches!(self, Self::Full)
    }
}

/// Planar pixel format produced by the JPU.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JpuPixelFormat {
    /// 4:2:0: chroma is subsampled in both axes.
    Yuv420,
    /// 4:2:2: chroma is subsampled horizontally.
    Yuv422Horizontal,
    /// 4:2:2: chroma is subsampled vertically (vendor name `FORMAT_224`).
    Yuv422Vertical,
    /// 4:4:4: luma and chroma have equal resolution.
    Yuv444,
    /// Luma only.
    Grayscale,
}

impl JpuPixelFormat {
    pub(crate) fn from_raw(raw: u32) -> Result<Self, FrameLayoutError> {
        match raw {
            super::regs::FORMAT_420 => Ok(Self::Yuv420),
            super::regs::FORMAT_422 => Ok(Self::Yuv422Horizontal),
            super::regs::FORMAT_224 => Ok(Self::Yuv422Vertical),
            super::regs::FORMAT_444 => Ok(Self::Yuv444),
            super::regs::FORMAT_400 => Ok(Self::Grayscale),
            _ => Err(FrameLayoutError::UnsupportedPixelFormat),
        }
    }

    const fn mcu_extent(self) -> Extent {
        match self {
            Self::Yuv420 => Extent::new(16, 16),
            Self::Yuv422Horizontal => Extent::new(16, 8),
            Self::Yuv422Vertical => Extent::new(8, 16),
            Self::Yuv444 | Self::Grayscale => Extent::new(8, 8),
        }
    }
}

/// Two-dimensional extent in pixels.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Extent {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Extent {
    const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// Byte layout of one planar component.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaneLayout {
    /// Offset from the start of the frame buffer.
    pub offset: usize,
    /// Component bytes including row-stride padding, but excluding the aligned
    /// gap before the next plane.
    pub len: usize,
    /// Distance in bytes between consecutive rows.
    pub stride: u32,
    /// Stored component dimensions in samples.
    pub storage: Extent,
}

/// Checked description of a planar JPU output buffer.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLayout {
    /// Sampling format.
    pub format: JpuPixelFormat,
    /// Selected hardware scale.
    pub scale: JpuScale,
    /// Dimensions from the JPEG SOF marker.
    pub source: Extent,
    /// Meaningful output dimensions; downstream processing should crop to this.
    pub visible: Extent,
    /// Source dimensions rounded up to whole JPEG MCU blocks.
    pub source_aligned: Extent,
    /// Dimensions of the hardware output buffer, including coded padding.
    pub coded: Extent,
    /// Stored dimensions after the scaler's even-size requirement.
    pub storage: Extent,
    /// Luma plane.
    pub y: PlaneLayout,
    /// Blue-difference chroma plane, absent for grayscale.
    pub cb: Option<PlaneLayout>,
    /// Red-difference chroma plane, absent for grayscale.
    pub cr: Option<PlaneLayout>,
    /// Total frame allocation, including inter-plane and trailing alignment.
    pub total_len: usize,
}

impl FrameLayout {
    /// Calculate the exact JPU output layout without touching hardware.
    pub fn new(
        source_width: u32,
        source_height: u32,
        format: JpuPixelFormat,
        scale: JpuScale,
    ) -> Result<Self, FrameLayoutError> {
        if source_width == 0 || source_height == 0 {
            return Err(FrameLayoutError::ZeroDimension);
        }

        let source = Extent::new(source_width, source_height);
        let mcu = format.mcu_extent();
        let source_aligned = Extent::new(
            align_u32(source.width, mcu.width)?,
            align_u32(source.height, mcu.height)?,
        );
        if source_aligned.width > u16::MAX as u32 || source_aligned.height > u16::MAX as u32 {
            return Err(FrameLayoutError::DimensionOverflow);
        }

        if scale.is_scaled()
            && (source_aligned.width < MIN_SCALED_CODED_EXTENT
                || source_aligned.height < MIN_SCALED_CODED_EXTENT)
        {
            return Err(FrameLayoutError::UnsupportedScaledDimensions);
        }

        let factor = scale.factor();
        let visible = Extent::new(
            ceil_div_u32(source.width, factor),
            ceil_div_u32(source.height, factor),
        );
        let coded = Extent::new(
            source_aligned.width >> scale.register_mode(),
            source_aligned.height >> scale.register_mode(),
        );
        let storage = if scale.is_scaled() {
            Extent::new(align_u32(coded.width, 2)?, align_u32(coded.height, 2)?)
        } else {
            coded
        };

        let stride_y = if scale.is_scaled() {
            align_u32(storage.width, SCALED_STRIDE_ALIGNMENT)?
        } else {
            storage.width
        };
        let y = PlaneLayout {
            offset: 0,
            len: plane_len(stride_y, storage.height)?,
            stride: stride_y,
            storage,
        };

        let (cb, cr, total_len) = match format {
            JpuPixelFormat::Grayscale => (None, None, align_usize(y.len, PLANE_ALIGNMENT)?),
            _ => {
                let chroma_storage = match format {
                    JpuPixelFormat::Yuv420 => Extent::new(storage.width / 2, storage.height / 2),
                    JpuPixelFormat::Yuv422Horizontal => {
                        Extent::new(storage.width / 2, storage.height)
                    }
                    JpuPixelFormat::Yuv422Vertical => {
                        Extent::new(storage.width, storage.height / 2)
                    }
                    JpuPixelFormat::Yuv444 => storage,
                    JpuPixelFormat::Grayscale => unreachable!(),
                };
                // The JPU's CSTRIDE is derived from the programmed YSTRIDE,
                // including scaler-added row padding, not from visible width.
                let stride_c = match format {
                    JpuPixelFormat::Yuv420 | JpuPixelFormat::Yuv422Horizontal => stride_y / 2,
                    JpuPixelFormat::Yuv422Vertical | JpuPixelFormat::Yuv444 => stride_y,
                    JpuPixelFormat::Grayscale => unreachable!(),
                };
                let chroma_len = plane_len(stride_c, chroma_storage.height)?;
                let cb_offset = align_usize(y.len, PLANE_ALIGNMENT)?;
                let cr_offset = align_usize(
                    cb_offset
                        .checked_add(chroma_len)
                        .ok_or(FrameLayoutError::BufferSizeOverflow)?,
                    PLANE_ALIGNMENT,
                )?;
                let total_len = align_usize(
                    cr_offset
                        .checked_add(chroma_len)
                        .ok_or(FrameLayoutError::BufferSizeOverflow)?,
                    PLANE_ALIGNMENT,
                )?;
                (
                    Some(PlaneLayout {
                        offset: cb_offset,
                        len: chroma_len,
                        stride: stride_c,
                        storage: chroma_storage,
                    }),
                    Some(PlaneLayout {
                        offset: cr_offset,
                        len: chroma_len,
                        stride: stride_c,
                        storage: chroma_storage,
                    }),
                    total_len,
                )
            }
        };

        Ok(Self {
            format,
            scale,
            source,
            visible,
            source_aligned,
            coded,
            storage,
            y,
            cb,
            cr,
            total_len,
        })
    }
}

/// Error returned while deriving a hardware frame layout.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FrameLayoutError {
    /// A JPEG dimension is zero.
    #[error("JPEG dimensions must be non-zero")]
    ZeroDimension,
    /// Rounding a source dimension would overflow `u32`.
    #[error("aligned JPEG dimensions overflow")]
    DimensionOverflow,
    /// A scaled decode was requested for a coded extent below 128 pixels.
    #[error("JPU scaling requires both aligned dimensions to be at least 128")]
    UnsupportedScaledDimensions,
    /// The JPEG sampling value is unknown to this driver.
    #[error("unsupported JPU pixel format")]
    UnsupportedPixelFormat,
    /// A plane length, offset, or total allocation overflowed `usize`.
    #[error("JPU frame layout size overflow")]
    BufferSizeOverflow,
}

fn align_u32(value: u32, alignment: u32) -> Result<u32, FrameLayoutError> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded & !(alignment - 1))
        .ok_or(FrameLayoutError::DimensionOverflow)
}

fn align_usize(value: usize, alignment: usize) -> Result<usize, FrameLayoutError> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded & !(alignment - 1))
        .ok_or(FrameLayoutError::BufferSizeOverflow)
}

const fn ceil_div_u32(value: u32, divisor: u32) -> u32 {
    value / divisor + if value.is_multiple_of(divisor) { 0 } else { 1 }
}

fn plane_len(stride: u32, rows: u32) -> Result<usize, FrameLayoutError> {
    let stride = usize::try_from(stride).map_err(|_| FrameLayoutError::BufferSizeOverflow)?;
    let rows = usize::try_from(rows).map_err(|_| FrameLayoutError::BufferSizeOverflow)?;
    stride
        .checked_mul(rows)
        .ok_or(FrameLayoutError::BufferSizeOverflow)
}

#[cfg(test)]
mod tests {
    use super::{FrameLayout, FrameLayoutError, JpuPixelFormat, JpuScale};

    #[test]
    fn half_scale_yuv420_distinguishes_visible_and_storage_extents() {
        let layout = FrameLayout::new(1279, 1706, JpuPixelFormat::Yuv420, JpuScale::Half)
            .expect("valid scaled layout");

        assert_eq!((layout.source.width, layout.source.height), (1279, 1706));
        assert_eq!((layout.visible.width, layout.visible.height), (640, 853));
        assert_eq!(
            (layout.source_aligned.width, layout.source_aligned.height),
            (1280, 1712)
        );
        assert_eq!((layout.storage.width, layout.storage.height), (640, 856));
        assert_eq!((layout.y.stride, layout.y.len), (640, 547_840));
        let cb = layout.cb.expect("YUV420 has Cb");
        let cr = layout.cr.expect("YUV420 has Cr");
        assert_eq!((cb.offset, cb.stride, cb.len), (547_840, 320, 136_960));
        assert_eq!((cr.offset, cr.stride, cr.len), (684_800, 320, 136_960));
        assert_eq!(layout.total_len, 821_760);
    }

    #[test]
    fn scaled_yuv420_chroma_stride_tracks_half_the_luma_stride() {
        let layout = FrameLayout::new(129, 129, JpuPixelFormat::Yuv420, JpuScale::Eighth)
            .expect("valid scaled layout");

        assert_eq!((layout.storage.width, layout.storage.height), (18, 18));
        assert_eq!((layout.y.stride, layout.y.len), (32, 576));
        let cb = layout.cb.expect("YUV420 has Cb");
        let cr = layout.cr.expect("YUV420 has Cr");
        assert_eq!((cb.offset, cb.stride, cb.len), (576, 16, 144));
        assert_eq!((cr.offset, cr.stride, cr.len), (720, 16, 144));
        assert_eq!(layout.total_len, 864);
    }

    #[test]
    fn scaled_chroma_strides_follow_each_sampling_format() {
        let cases = [
            (JpuPixelFormat::Yuv420, Some((16, 144))),
            (JpuPixelFormat::Yuv422Horizontal, Some((16, 288))),
            (JpuPixelFormat::Yuv422Vertical, Some((32, 288))),
            (JpuPixelFormat::Yuv444, Some((32, 576))),
            (JpuPixelFormat::Grayscale, None),
        ];

        for (format, expected_chroma) in cases {
            let layout =
                FrameLayout::new(129, 129, format, JpuScale::Eighth).expect("valid scaled layout");
            assert_eq!((layout.storage.width, layout.storage.height), (18, 18));
            assert_eq!((layout.y.stride, layout.y.len), (32, 576));
            match expected_chroma {
                Some((stride, len)) => {
                    let cb = layout.cb.expect("color format has Cb");
                    let cr = layout.cr.expect("color format has Cr");
                    assert_eq!((cb.stride, cb.len), (stride, len));
                    assert_eq!((cr.stride, cr.len), (stride, len));
                    assert_eq!(cb.offset % 8, 0);
                    assert_eq!(cr.offset % 8, 0);
                    assert!(cb.offset + cb.len <= cr.offset);
                    assert!(cr.offset + cr.len <= layout.total_len);
                }
                None => {
                    assert!(layout.cb.is_none());
                    assert!(layout.cr.is_none());
                    assert_eq!(layout.total_len, 576);
                }
            }
        }
    }

    #[test]
    fn scale_requires_both_aligned_axes_to_be_at_least_128() {
        FrameLayout::new(113, 113, JpuPixelFormat::Yuv420, JpuScale::Half)
            .expect("113 aligns to 128 for YUV420");

        assert_eq!(
            FrameLayout::new(112, 113, JpuPixelFormat::Yuv420, JpuScale::Half),
            Err(FrameLayoutError::UnsupportedScaledDimensions)
        );
        assert_eq!(
            FrameLayout::new(129, 119, JpuPixelFormat::Yuv422Horizontal, JpuScale::Half),
            Err(FrameLayoutError::UnsupportedScaledDimensions)
        );
    }

    #[test]
    fn visible_coded_and_storage_extents_are_not_conflated() {
        let layout = FrameLayout::new(129, 129, JpuPixelFormat::Yuv422Horizontal, JpuScale::Eighth)
            .expect("valid scaled layout");

        assert_eq!((layout.visible.width, layout.visible.height), (17, 17));
        assert_eq!((layout.coded.width, layout.coded.height), (18, 17));
        assert_eq!((layout.storage.width, layout.storage.height), (18, 18));
    }

    #[test]
    fn rejects_empty_and_overflowing_dimensions() {
        assert_eq!(
            FrameLayout::new(0, 64, JpuPixelFormat::Grayscale, JpuScale::Full),
            Err(FrameLayoutError::ZeroDimension)
        );
        assert_eq!(
            FrameLayout::new(u32::MAX, u32::MAX, JpuPixelFormat::Yuv420, JpuScale::Full),
            Err(FrameLayoutError::DimensionOverflow)
        );
        assert_eq!(
            FrameLayout::new(u16::MAX as u32, 128, JpuPixelFormat::Yuv420, JpuScale::Full,),
            Err(FrameLayoutError::DimensionOverflow)
        );
    }

    #[test]
    fn scale_factors_and_register_modes_are_explicit() {
        let cases = [
            (JpuScale::Full, 1, 0),
            (JpuScale::Half, 2, 1),
            (JpuScale::Quarter, 4, 2),
            (JpuScale::Eighth, 8, 3),
        ];

        for (scale, factor, mode) in cases {
            assert_eq!(scale.factor(), factor);
            assert_eq!(scale.register_mode(), mode);
        }
    }
}
