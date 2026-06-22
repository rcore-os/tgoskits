//! Typed, validated RGA operations (PR-1 scope: Copy and Fill on a single contiguous plane).
use crate::error::{Result, RgaError};

pub const MIN_DIMENSION: u32 = 2;
pub const MAX_DIMENSION: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgba8888,
    Bgra8888,
    Abgr8888,
    Rgb888,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8888 | Self::Bgra8888 | Self::Abgr8888 => 4,
            Self::Rgb888 => 3,
        }
    }
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
}

impl ImageDesc {
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
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgaOperation {
    /// Same-size, same-format blit.
    Copy { src: ImageDesc, dst: ImageDesc },
    /// Solid-color rectangle fill of the whole destination. `color` is packed in dst format order.
    Fill { dst: ImageDesc, color: u32 },
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, fmt: PixelFormat, addr: u64) -> ImageDesc {
        ImageDesc {
            width: w,
            height: h,
            stride_bytes: w * fmt.bytes_per_pixel(),
            format: fmt,
            phys_addr: addr,
        }
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
        };
        assert_eq!(d.validate(), Err(RgaError::Invalid));
    }
}
