//! Command buffer construction for the RGA2 hardware backend.
//!
//! The encoder builds the 32-word mode command block that old Rockchip RGA
//! hardware consumes through `RGA_CMD_BASE`, with the local MMU DISABLED
//! (direct physical base addresses). It does not submit the command to
//! hardware; board glue must still handle DMA allocation, cache sync, IRQ or
//! polling, clocks, and power.

use super::registers;
use crate::operation::{Blit, CscStandard, ImageDesc, RgaOperation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBuffer {
    words: [u32; registers::CMD_BUFFER_WORDS],
}

impl CommandBuffer {
    pub const fn zeroed() -> Self {
        Self {
            words: [0; registers::CMD_BUFFER_WORDS],
        }
    }

    pub fn words(&self) -> &[u32; registers::CMD_BUFFER_WORDS] {
        &self.words
    }

    pub fn register(&self, register: usize) -> Option<u32> {
        command_index(register).map(|index| self.words[index])
    }

    fn set_register(&mut self, register: usize, value: u32) {
        if let Some(index) = command_index(register) {
            self.words[index] = value;
        }
    }
}

fn command_index(register: usize) -> Option<usize> {
    let offset = register.checked_sub(registers::MODE_BASE)?;
    if offset % core::mem::size_of::<u32>() != 0 {
        return None;
    }
    let index = offset / core::mem::size_of::<u32>();
    (index < registers::CMD_BUFFER_WORDS).then_some(index)
}

const fn encode_mode(render: u32, bitblt: u32) -> u32 {
    (render & 0x7) | ((bitblt & 0x1) << 3) | (1 << 6)
}

/// Format-portion of SRC_INFO/DST_INFO: format code + R/B-swap modifier.
/// CONFIRM ON BOARD: codes + modifier bit positions (vendor rga2.h). Abgr8888 retains its PR-1 code.
const fn hw_format(fmt: crate::operation::PixelFormat) -> u32 {
    use crate::operation::PixelFormat;
    match fmt {
        PixelFormat::Rgba8888 => registers::FMT_RGBA8888,
        PixelFormat::Rgbx8888 => registers::FMT_RGBX8888,
        PixelFormat::Rgb888 => registers::FMT_RGB888,
        PixelFormat::Bgra8888 => registers::FMT_RGBA8888 | registers::INFO_RBSWAP,
        PixelFormat::Bgr888 => registers::FMT_RGB888 | registers::INFO_RBSWAP,
        PixelFormat::Rgb565 => registers::FMT_RGB565,
        PixelFormat::Abgr8888 => registers::COLOR_FMT_ABGR8888,
        PixelFormat::Nv12 => registers::FMT_YCBCR_420_SP,
        PixelFormat::Nv21 => registers::FMT_YCRCB_420_SP,
        PixelFormat::Nv16 => registers::FMT_YCBCR_422_SP,
    }
}

/// 16.16 fixed-point scale factor for one axis. CONFIRM ON BOARD: exact rounding (+1/-1 vendor forms).
/// `dim << 16` is overflow-safe in u32 because dims are bounded ≤ 8192 (8192<<16 < u32::MAX).
const fn scale_factor(src_dim: u32, dst_dim: u32) -> u32 {
    if dst_dim < src_dim {
        (src_dim << 16) / dst_dim // downscale: src/dst
    } else {
        (dst_dim << 16) / src_dim // upscale: dst/src
    }
}

const fn src_csc_bits(csc: Option<CscStandard>) -> u32 {
    match csc {
        Some(CscStandard::Bt601Limited) => {
            registers::CSC_BT601_LIMITED << registers::SRC_INFO_CSC_SHIFT
        }
        Some(CscStandard::Bt601Full) => registers::CSC_BT601_FULL << registers::SRC_INFO_CSC_SHIFT,
        Some(CscStandard::Bt709Limited) => {
            registers::CSC_BT709_LIMITED << registers::SRC_INFO_CSC_SHIFT
        }
        None => 0,
    }
}

/// Effective plane base for a windowed rect: base + y*stride + x*bpp (truncated to the 32-bit
/// register width; validation guarantees the surface + extent fit 32-bit).
fn rect_base(plane_base: u64, x: u32, y: u32, stride_bytes: u32, bpp: u32) -> u32 {
    (plane_base + (y as u64) * (stride_bytes as u64) + (x as u64) * (bpp as u64)) as u32 // validated to fit
}

/// Encode a general blit (crop/scale/place/CSC), MMU disabled. Assumes a validated `Blit`
/// (`RgaCore::start` calls `op.validate()` first); rect dims are non-zero per that contract.
pub fn encode_blit(blit: &Blit) -> crate::error::Result<CommandBuffer> {
    let mut buf = CommandBuffer::zeroed();
    let Blit {
        src,
        dst,
        src_rect,
        dst_rect,
        csc,
    } = blit;
    let src_bpp = src.format.bytes_per_pixel();
    let dst_bpp = dst.format.bytes_per_pixel();

    // --- destination ---
    buf.set_register(
        registers::DST_Y_RGB_BASE_ADDR,
        rect_base(
            dst.phys_addr,
            dst_rect.x,
            dst_rect.y,
            dst.stride_bytes,
            dst_bpp,
        ),
    );
    let dst_csc = if !src.format.is_yuv() && dst.format.is_yuv() {
        let mode = match csc {
            Some(CscStandard::Bt709Limited) => registers::CSC_BT709_LIMITED,
            Some(CscStandard::Bt601Full) => registers::CSC_BT601_FULL,
            _ => registers::CSC_BT601_LIMITED,
        };
        mode << registers::DST_INFO_CSC_SHIFT
    } else {
        0
    };
    buf.set_register(registers::DST_INFO, hw_format(dst.format) | dst_csc);
    buf.set_register(registers::DST_VIR_INFO, (dst.stride_bytes / 4) & 0x7fff);
    buf.set_register(
        registers::DST_ACT_INFO,
        ((dst_rect.width - 1) & 0x0fff) | (((dst_rect.height - 1) & 0x0fff) << 16),
    );
    if dst.format.is_semiplanar()
        && let Some(uv) = dst.uv_phys_addr
    {
        buf.set_register(
            registers::DST_CB_BASE_ADDR,
            rect_base(uv, dst_rect.x, dst_rect.y / 2, dst.stride_bytes, 1),
        );
    }

    // --- source ---
    buf.set_register(
        registers::SRC_Y_RGB_BASE_ADDR,
        rect_base(
            src.phys_addr,
            src_rect.x,
            src_rect.y,
            src.stride_bytes,
            src_bpp,
        ),
    );
    let scl_x = if dst_rect.width == src_rect.width {
        registers::SCL_NONE
    } else if dst_rect.width < src_rect.width {
        registers::SCL_DOWN
    } else {
        registers::SCL_UP
    };
    let scl_y = if dst_rect.height == src_rect.height {
        registers::SCL_NONE
    } else if dst_rect.height < src_rect.height {
        registers::SCL_DOWN
    } else {
        registers::SCL_UP
    };
    // SRC_INFO.csc_mode encodes YUV→RGB; only set when src is YUV (not RGB→YUV direction).
    let src_info = hw_format(src.format)
        | if src.format.is_yuv() {
            src_csc_bits(*csc)
        } else {
            0
        }
        | (scl_x << registers::SRC_INFO_HSCL_SHIFT)
        | (scl_y << registers::SRC_INFO_VSCL_SHIFT);
    buf.set_register(registers::SRC_INFO, src_info);
    buf.set_register(registers::SRC_VIR_INFO, (src.stride_bytes / 4) & 0x7fff);
    buf.set_register(
        registers::SRC_ACT_INFO,
        ((src_rect.width - 1) & 0x1fff) | (((src_rect.height - 1) & 0x1fff) << 16),
    );
    if scl_x != registers::SCL_NONE {
        buf.set_register(
            registers::SRC_X_FACTOR,
            scale_factor(src_rect.width, dst_rect.width),
        );
    }
    if scl_y != registers::SCL_NONE {
        buf.set_register(
            registers::SRC_Y_FACTOR,
            scale_factor(src_rect.height, dst_rect.height),
        );
    }
    if src.format.is_semiplanar()
        && let Some(uv) = src.uv_phys_addr
    {
        buf.set_register(
            registers::SRC_CB_BASE_ADDR,
            rect_base(uv, src_rect.x, src_rect.y / 2, src.stride_bytes, 1),
        );
    }

    buf.set_register(registers::MMU_CTRL1, 0); // MMU OFF
    buf.set_register(
        registers::MODE_CTRL,
        encode_mode(
            registers::MODE_RENDER_BITBLT,
            registers::MODE_BITBLT_SRC_TO_DST,
        ),
    );
    Ok(buf)
}

/// Encode a same-size copy with the local MMU DISABLED (direct physical base addresses).
pub fn encode_copy(src: ImageDesc, dst: ImageDesc) -> crate::error::Result<CommandBuffer> {
    let mut buf = CommandBuffer::zeroed();
    buf.set_register(registers::DST_Y_RGB_BASE_ADDR, dst.phys_addr as u32);
    buf.set_register(registers::DST_INFO, hw_format(dst.format));
    buf.set_register(registers::DST_VIR_INFO, (dst.stride_bytes / 4) & 0x7fff);
    buf.set_register(
        registers::DST_ACT_INFO,
        ((dst.width - 1) & 0x0fff) | (((dst.height - 1) & 0x0fff) << 16),
    );
    buf.set_register(registers::SRC_Y_RGB_BASE_ADDR, src.phys_addr as u32);
    buf.set_register(registers::SRC_INFO, hw_format(src.format));
    buf.set_register(registers::SRC_VIR_INFO, (src.stride_bytes / 4) & 0x7fff);
    buf.set_register(
        registers::SRC_ACT_INFO,
        ((src.width - 1) & 0x1fff) | (((src.height - 1) & 0x1fff) << 16),
    );
    buf.set_register(registers::MMU_CTRL1, 0); // MMU OFF
    buf.set_register(
        registers::MODE_CTRL,
        encode_mode(
            registers::MODE_RENDER_BITBLT,
            registers::MODE_BITBLT_SRC_TO_DST,
        ),
    );
    Ok(buf)
}

/// Encode a solid fill of the whole destination, MMU disabled.
pub fn encode_fill(dst: ImageDesc, color: u32) -> crate::error::Result<CommandBuffer> {
    let mut buf = CommandBuffer::zeroed();
    buf.set_register(registers::DST_Y_RGB_BASE_ADDR, dst.phys_addr as u32);
    buf.set_register(registers::DST_INFO, hw_format(dst.format));
    buf.set_register(registers::DST_VIR_INFO, (dst.stride_bytes / 4) & 0x7fff);
    buf.set_register(
        registers::DST_ACT_INFO,
        ((dst.width - 1) & 0x0fff) | (((dst.height - 1) & 0x0fff) << 16),
    );
    buf.set_register(registers::SRC_BG_COLOR, color);
    buf.set_register(registers::MMU_CTRL1, 0);
    buf.set_register(
        registers::MODE_CTRL,
        encode_mode(registers::MODE_RENDER_RECTANGLE_FILL, 0),
    );
    Ok(buf)
}

pub fn encode(op: &RgaOperation) -> crate::error::Result<CommandBuffer> {
    match op {
        RgaOperation::Copy { src, dst } => encode_copy(*src, *dst),
        RgaOperation::Fill { dst, color } => encode_fill(*dst, *color),
        RgaOperation::Blit(b) => encode_blit(b),
    }
}

#[cfg(test)]
mod mmu_off_tests {
    use super::{encode_blit, encode_copy, encode_fill, encode_mode, registers};
    use crate::operation::{Blit, CscStandard, ImageDesc, PixelFormat, Rect};

    fn img(w: u32, h: u32, addr: u64) -> ImageDesc {
        ImageDesc::rgb(w, h, w * 4, PixelFormat::Rgba8888, addr)
    }

    #[test]
    fn copy_programs_physical_bases_and_mmu_off() {
        let cmd = encode_copy(img(64, 48, 0x4000_0000), img(64, 48, 0x4010_0000)).unwrap();
        assert_eq!(
            cmd.register(registers::SRC_Y_RGB_BASE_ADDR),
            Some(0x4000_0000)
        );
        assert_eq!(
            cmd.register(registers::DST_Y_RGB_BASE_ADDR),
            Some(0x4010_0000)
        );
        assert_eq!(cmd.register(registers::MMU_CTRL1), Some(0));
        assert_eq!(cmd.register(registers::SRC_ACT_INFO), Some(63 | (47 << 16)));
        assert_eq!(cmd.register(registers::DST_VIR_INFO), Some(64));
        assert_eq!(
            cmd.register(registers::MODE_CTRL),
            Some(encode_mode(
                registers::MODE_RENDER_BITBLT,
                registers::MODE_BITBLT_SRC_TO_DST
            ))
        );
    }

    #[test]
    fn fill_programs_color_and_rect_fill_mode() {
        let cmd = encode_fill(img(32, 32, 0x4020_0000), 0x0000_00ff).unwrap();
        assert_eq!(
            cmd.register(registers::DST_Y_RGB_BASE_ADDR),
            Some(0x4020_0000)
        );
        assert_eq!(cmd.register(registers::SRC_BG_COLOR), Some(0x0000_00ff));
        assert_eq!(cmd.register(registers::MMU_CTRL1), Some(0));
        assert_eq!(
            cmd.register(registers::MODE_CTRL),
            Some(encode_mode(registers::MODE_RENDER_RECTANGLE_FILL, 0))
        );
    }

    #[test]
    fn hw_format_codes() {
        use crate::operation::{ImageDesc, PixelFormat};
        // RGB888 must now be 0x2 (the PR-1 stub returned 0).
        let cmd = encode_copy(
            ImageDesc::rgb(64, 48, 64 * 3, PixelFormat::Rgb888, 0x1000),
            ImageDesc::rgb(64, 48, 64 * 3, PixelFormat::Rgb888, 0x9000),
        )
        .unwrap();
        assert_eq!(
            cmd.register(registers::SRC_INFO),
            Some(registers::FMT_RGB888)
        );
        assert_eq!(
            cmd.register(registers::DST_INFO),
            Some(registers::FMT_RGB888)
        );
    }

    #[test]
    fn imported_backing_phys_flows_into_command() {
        let backing = crate::buffer::RgaBufferBacking::Imported {
            phys_addr: 0x4002_0000,
            len: 64 * 48 * 4,
        };
        let dst = ImageDesc::rgb(64, 48, 64 * 4, PixelFormat::Rgba8888, backing.phys_addr());
        let cmd = encode_fill(dst, 0xAABB_CCDD).unwrap();
        // `len` is for caller-side bounds checking, not command encoding — only phys_addr
        // flows into the hardware register, which is what this test verifies.
        assert_eq!(
            cmd.register(registers::DST_Y_RGB_BASE_ADDR),
            Some(0x4002_0000)
        );
    }

    #[test]
    fn blit_downscale_programs_factor_and_mode() {
        let src = ImageDesc::rgb(1920, 1080, 1920 * 3, PixelFormat::Rgb888, 0x4000_0000);
        let dst = ImageDesc::rgb(640, 360, 640 * 3, PixelFormat::Rgb888, 0x4100_0000);
        let cmd = encode_blit(&Blit::resize(src, dst)).unwrap();
        assert_eq!(
            cmd.register(registers::SRC_ACT_INFO),
            Some(1919 | (1079 << 16))
        );
        assert_eq!(
            cmd.register(registers::DST_ACT_INFO),
            Some(639 | (359 << 16))
        );
        assert_eq!(
            cmd.register(registers::SRC_X_FACTOR),
            Some((1920u32 << 16) / 640)
        );
        assert_eq!(
            cmd.register(registers::SRC_Y_FACTOR),
            Some((1080u32 << 16) / 360)
        );
        let src_info = cmd.register(registers::SRC_INFO).unwrap();
        assert_eq!(src_info & 0xf, registers::FMT_RGB888);
        assert_eq!(
            (src_info >> registers::SRC_INFO_HSCL_SHIFT) & 0x3,
            registers::SCL_DOWN
        );
        assert_eq!(
            (src_info >> registers::SRC_INFO_VSCL_SHIFT) & 0x3,
            registers::SCL_DOWN
        );
    }

    #[test]
    fn blit_dst_subrect_offsets_base() {
        let src = ImageDesc::rgb(320, 240, 320 * 4, PixelFormat::Rgba8888, 0x4000_0000);
        let dst = ImageDesc::rgb(640, 480, 640 * 4, PixelFormat::Rgba8888, 0x4100_0000);
        let b = Blit::new(
            src,
            dst,
            Rect {
                x: 0,
                y: 0,
                width: 320,
                height: 240,
            },
            Rect {
                x: 160,
                y: 120,
                width: 320,
                height: 240,
            },
            None,
        );
        let cmd = encode_blit(&b).unwrap();
        assert_eq!(
            cmd.register(registers::DST_Y_RGB_BASE_ADDR),
            Some(0x4100_0000 + 120 * 640 * 4 + 160 * 4)
        );
        let si = cmd.register(registers::SRC_INFO).unwrap();
        assert_eq!(
            (si >> registers::SRC_INFO_HSCL_SHIFT) & 0x3,
            registers::SCL_NONE
        );
        assert_eq!(cmd.register(registers::SRC_X_FACTOR), Some(0));
    }

    #[test]
    fn blit_nv12_to_rgb_sets_csc_and_uv_base() {
        let src = ImageDesc::nv12(640, 480, 640, 0x4000_0000);
        let dst = ImageDesc::rgb(640, 480, 640 * 4, PixelFormat::Rgba8888, 0x4100_0000);
        let mut b = Blit::resize(src, dst);
        b.csc = Some(CscStandard::Bt601Limited);
        let cmd = encode_blit(&b).unwrap();
        assert_eq!(
            cmd.register(registers::SRC_Y_RGB_BASE_ADDR),
            Some(0x4000_0000)
        );
        assert_eq!(
            cmd.register(registers::SRC_CB_BASE_ADDR),
            Some(0x4000_0000 + 640 * 480)
        );
        let si = cmd.register(registers::SRC_INFO).unwrap();
        assert_eq!(si & 0xf, registers::FMT_YCBCR_420_SP);
        assert_eq!(
            (si >> registers::SRC_INFO_CSC_SHIFT) & 0x3,
            registers::CSC_BT601_LIMITED
        );
    }

    #[test]
    fn blit_rgb_to_nv12_sets_dst_csc() {
        // RGB→YUV writes dst_csc in DST_INFO; src csc bits stay 0.
        let src = ImageDesc::rgb(640, 480, 640 * 4, PixelFormat::Rgba8888, 0x4000_0000);
        let dst = ImageDesc::nv12(640, 480, 640, 0x4100_0000);
        let mut b = Blit::resize(src, dst);
        b.csc = Some(CscStandard::Bt709Limited);
        let cmd = encode_blit(&b).unwrap();
        let di = cmd.register(registers::DST_INFO).unwrap();
        assert_eq!(di & 0xf, registers::FMT_YCBCR_420_SP);
        assert_eq!(
            (di >> registers::DST_INFO_CSC_SHIFT) & 0x3,
            registers::CSC_BT709_LIMITED
        );
        // YUV dst → DST_CB_BASE programmed
        assert_eq!(
            cmd.register(registers::DST_CB_BASE_ADDR),
            Some(0x4100_0000 + 640 * 480)
        );
        // src is RGB → no src csc bits
        let si = cmd.register(registers::SRC_INFO).unwrap();
        assert_eq!((si >> registers::SRC_INFO_CSC_SHIFT) & 0x3, 0);
    }

    #[test]
    fn letterbox_is_fill_plus_blit_into_centered_rect() {
        let dst = ImageDesc::rgb(640, 640, 640 * 3, PixelFormat::Rgb888, 0x4100_0000);
        let fill = encode_fill(dst, 0x0072_7272).unwrap();
        assert_eq!(fill.register(registers::SRC_BG_COLOR), Some(0x0072_7272));
        let src = ImageDesc::rgb(1920, 1080, 1920 * 3, PixelFormat::Rgb888, 0x4000_0000);
        let b = Blit::new(
            src,
            dst,
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            Rect {
                x: 0,
                y: 140,
                width: 640,
                height: 360,
            },
            None,
        );
        let cmd = encode_blit(&b).unwrap();
        assert_eq!(
            cmd.register(registers::DST_Y_RGB_BASE_ADDR),
            Some(0x4100_0000 + 140 * 640 * 3)
        );
        assert_eq!(
            cmd.register(registers::DST_ACT_INFO),
            Some(639 | (359 << 16))
        );
    }
}
