//! Typed, validated RGA operations (PR-1 scope: Copy and Fill on a single contiguous plane).
use crate::error::{Result, RgaError};

pub const MIN_DIMENSION: u32 = 2;
pub const MAX_DIMENSION: u32 = 8192;
/// RGA2 destination max (smaller than src's 8192 — the DST_ACT_INFO field is 12-bit). CONFIRM ON BOARD.
pub const DST_MAX_DIMENSION: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgba8888,
    Rgbx8888,
    Bgra8888,
    Abgr8888,
    Rgb888,
    Bgr888,
    Rgb565,
    Nv12,
    Nv21,
    Nv16,
}

impl PixelFormat {
    /// Bytes per pixel of the luma/packed plane (row-stride + base-offset math).
    /// Semiplanar YUV reports its Y-plane bpp (1).
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8888 | Self::Rgbx8888 | Self::Bgra8888 | Self::Abgr8888 => 4,
            Self::Rgb888 | Self::Bgr888 => 3,
            Self::Rgb565 => 2,
            Self::Nv12 | Self::Nv21 | Self::Nv16 => 1,
        }
    }

    pub const fn is_yuv(self) -> bool {
        matches!(self, Self::Nv12 | Self::Nv21 | Self::Nv16)
    }

    /// Semiplanar YUV (separate interleaved CbCr plane) — requires a UV plane base.
    pub const fn is_semiplanar(self) -> bool {
        matches!(self, Self::Nv12 | Self::Nv21 | Self::Nv16)
    }
}

/// A windowed region within a surface (mirrors librga `im_rect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// YUV↔RGB colour-space standard. Maps to RGA2 SRC_INFO.csc_mode 1/2/3.
/// CONFIRM ON BOARD: the csc_mode value↔standard map (vendor/TRM ambiguous).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CscStandard {
    Bt601Limited,
    Bt601Full,
    Bt709Limited,
}

/// One contiguous image plane backed by a DMA buffer (physical base supplied at submit time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDesc {
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub format: PixelFormat,
    /// Device (bus) physical address of the plane. 32-bit reachable (DMA mask = u32::MAX in PR-1).
    pub phys_addr: u64,
    /// Chroma plane base for semiplanar YUV formats (NV12/NV21/NV16). Must be `None` for packed formats.
    pub uv_phys_addr: Option<u64>,
}

impl ImageDesc {
    /// Packed single-plane (RGB/RGBA) surface.
    pub fn rgb(
        width: u32,
        height: u32,
        stride_bytes: u32,
        format: PixelFormat,
        phys_addr: u64,
    ) -> Self {
        Self {
            width,
            height,
            stride_bytes,
            format,
            phys_addr,
            uv_phys_addr: None,
        }
    }

    /// Contiguous NV12 surface: CbCr plane immediately follows the Y plane.
    pub fn nv12(width: u32, height: u32, stride_bytes: u32, y_phys: u64) -> Self {
        Self {
            width,
            height,
            stride_bytes,
            format: PixelFormat::Nv12,
            phys_addr: y_phys,
            uv_phys_addr: Some(y_phys + (stride_bytes as u64) * (height as u64)),
        }
    }

    pub fn validate(self) -> Result<()> {
        if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&self.width)
            || !(MIN_DIMENSION..=MAX_DIMENSION).contains(&self.height)
        {
            return Err(RgaError::Invalid);
        }
        let min_stride = self
            .width
            .checked_mul(self.format.bytes_per_pixel())
            .ok_or(RgaError::Overflow)?;
        if self.stride_bytes < min_stride || !self.stride_bytes.is_multiple_of(4) {
            return Err(RgaError::Invalid);
        }
        // Total byte extent must not overflow 32-bit reachable space (PR-1 contiguous + 32-bit mask).
        let extent = (self.stride_bytes as u64)
            .checked_mul(self.height as u64)
            .ok_or(RgaError::Overflow)?;
        let end = self
            .phys_addr
            .checked_add(extent)
            .ok_or(RgaError::Overflow)?;
        if end > u32::MAX as u64 {
            return Err(RgaError::Invalid);
        }

        // Semiplanar YUV requires a chroma plane base; its extent must also fit 32-bit.
        if self.format.is_semiplanar() {
            let uv = self.uv_phys_addr.ok_or(RgaError::Invalid)?;
            // 4:2:0 chroma is half-height; height must be even so chroma rows are exact.
            if !matches!(self.format, PixelFormat::Nv16) && !self.height.is_multiple_of(2) {
                return Err(RgaError::Invalid);
            }
            let uv_rows = if matches!(self.format, PixelFormat::Nv16) {
                self.height
            } else {
                self.height / 2
            };
            let uv_extent = (self.stride_bytes as u64)
                .checked_mul(uv_rows as u64)
                .ok_or(RgaError::Overflow)?;
            let uv_end = uv.checked_add(uv_extent).ok_or(RgaError::Overflow)?;
            if uv_end > u32::MAX as u64 {
                return Err(RgaError::Invalid);
            }
        } else if self.uv_phys_addr.is_some() {
            return Err(RgaError::Invalid); // packed format must not carry a UV base
        }

        Ok(())
    }
}

fn rect_within(r: &Rect, w: u32, h: u32) -> Result<()> {
    if r.width == 0 || r.height == 0 {
        return Err(RgaError::Invalid);
    }
    let x_end = r.x.checked_add(r.width).ok_or(RgaError::Overflow)?;
    let y_end = r.y.checked_add(r.height).ok_or(RgaError::Overflow)?;
    if x_end > w || y_end > h {
        return Err(RgaError::Invalid);
    }
    Ok(())
}

fn check_scale(src_dim: u32, dst_dim: u32) -> Result<()> {
    if dst_dim > src_dim.saturating_mul(16) || src_dim > dst_dim.saturating_mul(16) {
        return Err(RgaError::Unsupported);
    }
    Ok(())
}

/// General single-pass blit: crop (`src_rect`) → scale → place (`dst_rect`), with optional CSC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blit {
    pub src: ImageDesc,
    pub dst: ImageDesc,
    pub src_rect: Rect,
    pub dst_rect: Rect,
    /// `Some` only when the format pair crosses YUV↔RGB.
    pub csc: Option<CscStandard>,
}

impl Blit {
    pub fn new(
        src: ImageDesc,
        dst: ImageDesc,
        src_rect: Rect,
        dst_rect: Rect,
        csc: Option<CscStandard>,
    ) -> Self {
        Self {
            src,
            dst,
            src_rect,
            dst_rect,
            csc,
        }
    }

    /// Full-surface src → full-surface dst (resize if dims differ), no CSC.
    pub fn resize(src: ImageDesc, dst: ImageDesc) -> Self {
        Self::new(
            src,
            dst,
            Rect {
                x: 0,
                y: 0,
                width: src.width,
                height: src.height,
            },
            Rect {
                x: 0,
                y: 0,
                width: dst.width,
                height: dst.height,
            },
            None,
        )
    }

    /// `src_rect` window → full dst.
    pub fn crop(src: ImageDesc, src_rect: Rect, dst: ImageDesc) -> Self {
        Self::new(
            src,
            dst,
            src_rect,
            Rect {
                x: 0,
                y: 0,
                width: dst.width,
                height: dst.height,
            },
            None,
        )
    }

    pub fn validate(&self) -> Result<()> {
        self.src.validate()?;
        self.dst.validate()?;
        if self.dst.width > DST_MAX_DIMENSION || self.dst.height > DST_MAX_DIMENSION {
            return Err(RgaError::Invalid);
        }
        rect_within(&self.src_rect, self.src.width, self.src.height)?;
        rect_within(&self.dst_rect, self.dst.width, self.dst.height)?;
        check_scale(self.src_rect.width, self.dst_rect.width)?;
        check_scale(self.src_rect.height, self.dst_rect.height)?;
        let s = self.src.format;
        let d = self.dst.format;
        let crosses = s.is_yuv() != d.is_yuv();
        match (s.is_yuv(), d.is_yuv()) {
            (false, false) => {}
            (true, false) | (false, true) => {}
            (true, true) if s == d => {}
            (true, true) => return Err(RgaError::Unsupported),
        }
        if crosses && self.csc.is_none() {
            return Err(RgaError::Invalid);
        }
        if !crosses && self.csc.is_some() {
            return Err(RgaError::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgaOperation {
    /// Same-size, same-format blit.
    Copy { src: ImageDesc, dst: ImageDesc },
    /// Solid-color rectangle fill of the whole destination. `color` is packed in dst format order.
    Fill { dst: ImageDesc, color: u32 },
    /// General blit: crop/scale/place with optional CSC (encoding implemented in Phase D Task D4).
    Blit(Blit),
}

impl RgaOperation {
    pub fn validate(&self) -> Result<()> {
        match self {
            RgaOperation::Copy { src, dst } => {
                src.validate()?;
                dst.validate()?;
                if src.width != dst.width || src.height != dst.height {
                    return Err(RgaError::Invalid);
                }
                if src.format != dst.format {
                    return Err(RgaError::Unsupported); // CSC/format-convert is a later PR
                }
                Ok(())
            }
            RgaOperation::Fill { dst, .. } => dst.validate(),
            RgaOperation::Blit(b) => b.validate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, fmt: PixelFormat, addr: u64) -> ImageDesc {
        ImageDesc::rgb(w, h, w * fmt.bytes_per_pixel(), fmt, addr)
    }

    #[test]
    fn copy_same_size_same_format_ok() {
        let op = RgaOperation::Copy {
            src: img(64, 48, PixelFormat::Rgba8888, 0x1000),
            dst: img(64, 48, PixelFormat::Rgba8888, 0x9000),
        };
        assert_eq!(op.validate(), Ok(()));
    }

    #[test]
    fn copy_size_mismatch_is_invalid() {
        let op = RgaOperation::Copy {
            src: img(64, 48, PixelFormat::Rgba8888, 0x1000),
            dst: img(80, 48, PixelFormat::Rgba8888, 0x9000),
        };
        assert_eq!(op.validate(), Err(RgaError::Invalid));
    }

    #[test]
    fn copy_format_mismatch_is_unsupported() {
        let op = RgaOperation::Copy {
            src: img(64, 48, PixelFormat::Rgba8888, 0x1000),
            dst: img(64, 48, PixelFormat::Bgra8888, 0x9000),
        };
        assert_eq!(op.validate(), Err(RgaError::Unsupported));
    }

    #[test]
    fn zero_dimension_is_invalid() {
        let mut d = img(64, 48, PixelFormat::Rgb888, 0x1000);
        d.width = 0;
        assert_eq!(d.validate(), Err(RgaError::Invalid));
    }

    #[test]
    fn stride_below_row_is_invalid() {
        let d = ImageDesc {
            width: 64,
            height: 48,
            stride_bytes: 128,
            format: PixelFormat::Rgba8888,
            phys_addr: 0x1000,
            uv_phys_addr: None,
        };
        assert_eq!(d.validate(), Err(RgaError::Invalid));
    }

    #[test]
    fn extent_beyond_32bit_is_invalid() {
        let d = ImageDesc {
            width: 64,
            height: 48,
            stride_bytes: 256,
            format: PixelFormat::Rgba8888,
            phys_addr: 0xFFFF_FF00,
            uv_phys_addr: None,
        };
        assert_eq!(d.validate(), Err(RgaError::Invalid));
    }

    #[test]
    fn bytes_per_pixel_and_yuv_helpers() {
        assert_eq!(PixelFormat::Rgb888.bytes_per_pixel(), 3);
        assert_eq!(PixelFormat::Rgb565.bytes_per_pixel(), 2);
        assert_eq!(PixelFormat::Nv12.bytes_per_pixel(), 1);
        assert!(PixelFormat::Nv12.is_semiplanar() && PixelFormat::Nv12.is_yuv());
        assert!(!PixelFormat::Rgb888.is_yuv());
    }

    #[test]
    fn nv12_constructor_derives_uv_base() {
        let d = ImageDesc::nv12(64, 48, 64, 0x4000_0000);
        assert_eq!(d.uv_phys_addr, Some(0x4000_0000 + 64 * 48));
        assert_eq!(d.validate(), Ok(()));
    }

    #[test]
    fn semiplanar_without_uv_base_is_invalid() {
        let mut d = ImageDesc::nv12(64, 48, 64, 0x4000_0000);
        d.uv_phys_addr = None;
        assert_eq!(d.validate(), Err(RgaError::Invalid));
    }

    #[test]
    fn packed_with_uv_base_is_invalid() {
        let mut d = ImageDesc::rgb(64, 48, 64 * 4, PixelFormat::Rgba8888, 0x4000_0000);
        d.uv_phys_addr = Some(0x5000_0000);
        assert_eq!(d.validate(), Err(RgaError::Invalid));
    }

    fn rgb(w: u32, h: u32, a: u64) -> ImageDesc {
        ImageDesc::rgb(w, h, w * 4, PixelFormat::Rgba8888, a)
    }

    #[test]
    fn blit_resize_rgb_ok() {
        assert_eq!(
            Blit::resize(rgb(1920, 1080, 0x1000), rgb(640, 360, 0x9000)).validate(),
            Ok(())
        );
    }

    #[test]
    fn blit_dst_over_4096_invalid() {
        assert_eq!(
            Blit::resize(rgb(64, 64, 0x1000), rgb(5000, 64, 0x9000)).validate(),
            Err(RgaError::Invalid)
        );
    }

    #[test]
    fn blit_scale_beyond_16x_unsupported() {
        assert_eq!(
            Blit::resize(rgb(16, 16, 0x1000), rgb(512, 16, 0x9000)).validate(),
            Err(RgaError::Unsupported)
        );
    }

    #[test]
    fn blit_src_rect_out_of_bounds_invalid() {
        let mut b = Blit::resize(rgb(64, 64, 0x1000), rgb(64, 64, 0x9000));
        b.src_rect = Rect {
            x: 40,
            y: 0,
            width: 40,
            height: 64,
        };
        assert_eq!(b.validate(), Err(RgaError::Invalid));
    }

    #[test]
    fn blit_yuv_to_rgb_requires_csc() {
        let src = ImageDesc::nv12(640, 480, 640, 0x1000);
        let dst = rgb(640, 480, 0x9000);
        let mut b = Blit::resize(src, dst);
        assert_eq!(b.validate(), Err(RgaError::Invalid));
        b.csc = Some(CscStandard::Bt601Limited);
        assert_eq!(b.validate(), Ok(()));
    }

    #[test]
    fn blit_rgb_to_rgb_with_csc_invalid() {
        let mut b = Blit::resize(rgb(64, 64, 0x1000), rgb(64, 64, 0x9000));
        b.csc = Some(CscStandard::Bt601Limited);
        assert_eq!(b.validate(), Err(RgaError::Invalid));
    }

    #[test]
    fn imagedesc_odd_height_nv12_invalid() {
        let d = ImageDesc::nv12(64, 47, 64, 0x4000_0000); // odd height
        assert_eq!(d.validate(), Err(RgaError::Invalid));
    }
}
