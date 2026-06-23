//! Command buffer construction for the RGA2 hardware backend.
//!
//! The encoder builds the 32-word mode command block that old Rockchip RGA
//! hardware consumes through `RGA_CMD_BASE`, with the local MMU DISABLED
//! (direct physical base addresses). It does not submit the command to
//! hardware; board glue must still handle DMA allocation, cache sync, IRQ or
//! polling, clocks, and power.

use super::registers;
use crate::operation::{ImageDesc, RgaOperation};

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
        // Phase D Task D4 implements encode_blit
        RgaOperation::Blit(_) => Err(crate::error::RgaError::Unsupported),
    }
}

#[cfg(test)]
mod mmu_off_tests {
    use super::{encode_copy, encode_fill, encode_mode, registers};
    use crate::operation::{ImageDesc, PixelFormat};

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
}
