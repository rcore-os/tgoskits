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
    // MODE_CTRL: render_mode bits[2:0], bitblt_mode bit3. Vendor RGA2_set_mode_ctrl only sets
    // gradient_sat (bit6) from `alpha_rop_flag >> 7` and intr_cf_e (bit7) from CMD_fin_int_enable;
    // both are 0 for a plain fill/copy/blit. The previous unconditional `(1<<6)` forced
    // SW_GRADIENT_SAT on (vendor rga2_reg_info.h m_RGA2_MODE_CTRL_SW_GRADIENT_SAT = 0x1<<6), which
    // is NOT a generic run-enable and corrupted fill/copy output.
    (render & 0x7) | ((bitblt & 0x1) << 3)
}

/// Format-portion of SRC_INFO/DST_INFO: format code + R/B-swap modifier.
/// CONFIRM ON BOARD: codes + modifier bit positions (vendor rga2.h).
const fn hw_format(fmt: crate::operation::PixelFormat) -> u32 {
    use crate::operation::PixelFormat;
    match fmt {
        PixelFormat::Rgba8888 => registers::FMT_RGBA8888,
        PixelFormat::Rgbx8888 => registers::FMT_RGBX8888,
        PixelFormat::Rgb888 => registers::FMT_RGB888,
        PixelFormat::Bgra8888 => registers::FMT_RGBA8888 | registers::INFO_RBSWAP,
        PixelFormat::Bgr888 => registers::FMT_RGB888 | registers::INFO_RBSWAP,
        PixelFormat::Rgb565 => registers::FMT_RGB565,
        // ABGR = RGBA byte-reversed → RGBA code + R/B-swap + alpha-swap (best effort).
        // CONFIRM ON BOARD: exact ABGR encoding (vendor rga2.h).
        PixelFormat::Abgr8888 => {
            registers::FMT_RGBA8888 | registers::INFO_RBSWAP | registers::INFO_ALPHA_SWAP
        }
        PixelFormat::Nv12 => registers::FMT_YCBCR_420_SP,
        PixelFormat::Nv21 => registers::FMT_YCRCB_420_SP,
        PixelFormat::Nv16 => registers::FMT_YCBCR_422_SP,
    }
}

/// SRC1 (foreground-constant) format field for DST_INFO, derived from a packed `hw_format` value.
///
/// A color FILL routes `fg_color` (SRC_FG_COLOR, 0x12c) through the engine's SRC1 / foreground
/// channel; the vendor programs that channel's format/rb_swap/alpha_swap into DST_INFO bits[11:7]
/// from `msg->src1.format` (rga2_reg_info.c RGA2_set_reg_dst_info:419-448), and the color_fill
/// dispatch (rga2_reg_info.c:1084) calls `RGA2_set_reg_dst_info` but NEVER `RGA2_set_reg_src_info` —
/// so SRC_INFO (0x104) is irrelevant to a fill and the FG color order is decided entirely by these
/// SRC1 bits. `fg_color` is written verbatim (RGA2_set_reg_color_fill:953, no normalization).
///
/// `RgaOperation::Fill { color }` is documented as packed in dst-format order (operation.rs), so we
/// set SRC1 == dst format/swap: the engine then interprets `color` in the SAME channel order it
/// writes to the dst, guaranteeing a verbatim round-trip (e.g. RGBA8888 dst, SRC1=RGBA8888 → memory
/// bytes [R,G,B,A]). Relying on the zeroed default (SRC1=0=RGBA8888/no-swap) only works for RGBA;
/// this makes BGRA/ABGR/RGB888 fills correct and deterministic too.
///
/// The dst-position layout from `hw_format` is `fmt[3:0] | rb_swap(1<<4) | alpha_swap(1<<5)`; remap
/// those into the SRC1 positions `fmt[9:7] | rb_swap(1<<10) | alpha_swap(1<<11)`. Format codes 0..6
/// fit the 3-bit SRC1_FMT field (RGBA=0, RGBX=1, RGB888=2, RGB565=4); a fill dst is always RGB-family.
const fn src1_fill_format(dst_fmt_bits: u32) -> u32 {
    let fmt = dst_fmt_bits & 0xf;
    let rb_swap = (dst_fmt_bits & registers::INFO_RBSWAP) != 0;
    let alpha_swap = (dst_fmt_bits & registers::INFO_ALPHA_SWAP) != 0;
    let mut reg = (fmt & registers::DST_INFO_SRC1_FMT_MASK) << registers::DST_INFO_SRC1_FMT_SHIFT;
    if rb_swap {
        reg |= registers::DST_INFO_SRC1_RB_SWAP;
    }
    if alpha_swap {
        reg |= registers::DST_INFO_SRC1_ALPHA_SWAP;
    }
    reg
}

/// One axis of SRC_X_FACTOR / SRC_Y_FACTOR (16.16 fixed point), matching the vendor
/// `RGA2_reg_get_param` (rga2_reg_info.c):
///
/// - downscale (src > dst): `(dst << 16) / src`, written into the LOW half (bits[15:0]).
/// - upscale (src < dst): `((src - 1) << 16) / (dst - 1)`, written into the HIGH half (bits[31:16]).
/// - equal: 0.
///
/// The factor is the smaller dimension over the larger, so it is always <= 1.0 (<= 0x10000); the
/// previous `(src<<16)/dst` was the reciprocal (>1.0 for downscale), which steps the source pointer
/// far past SRC_ACT_INFO and stalls the scaler (the run-8 BUSY-50ms resize hang). `dim << 16` is
/// overflow-safe in u32 because dims are bounded <= 8192 (8192<<16 < u32::MAX).
const fn scale_factor(src_dim: u32, dst_dim: u32) -> u32 {
    if src_dim > dst_dim {
        // downscale: coefficient in low half [15:0]
        ((dst_dim << 16) / src_dim) & 0xffff
    } else if src_dim < dst_dim {
        // upscale: coefficient in high half [31:16]
        (((src_dim - 1) << 16) / (dst_dim - 1)) << 16
    } else {
        0
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
    // When either axis scales, also select the scaler filter (SCL_FILTER bits[25:24] =
    // scale_bicu_mode). The vendor bring-up uses bicubic (=2) for any scaling op
    // (rga2_drv.c req.scale_bicu_mode=2); leaving it 0 with HSCL/VSCL set is a degenerate config.
    let scl_filter = if scl_x != registers::SCL_NONE || scl_y != registers::SCL_NONE {
        registers::SCL_FILTER_BICUBIC << registers::SRC_INFO_SCL_FILTER_SHIFT
    } else {
        0
    };
    let src_info = hw_format(src.format)
        | if src.format.is_yuv() {
            src_csc_bits(*csc)
        } else {
            0
        }
        | (scl_x << registers::SRC_INFO_HSCL_SHIFT)
        | (scl_y << registers::SRC_INFO_VSCL_SHIFT)
        | scl_filter;
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
    // DST_INFO carries BOTH the dst format (bits[5:0]) AND the SRC1/foreground-constant format
    // (bits[11:7]). The fill's FG color is interpreted through the SRC1 channel — the vendor
    // color_fill path programs DST_INFO but never SRC_INFO (rga2_reg_info.c:1084), so the FG channel
    // order is governed solely by these SRC1 bits. We set SRC1 == dst format so the engine reads
    // `color` (packed in dst-format order) in the same channel order it writes back, giving a
    // verbatim round-trip. run-9 fill=FAIL (engine done, wrong color) was this missing SRC1 field:
    // SRC1 was left implicitly 0 and never tied to the dst format.
    let dst_fmt = hw_format(dst.format);
    buf.set_register(registers::DST_INFO, dst_fmt | src1_fill_format(dst_fmt));
    buf.set_register(registers::DST_VIR_INFO, (dst.stride_bytes / 4) & 0x7fff);
    buf.set_register(
        registers::DST_ACT_INFO,
        ((dst.width - 1) & 0x0fff) | (((dst.height - 1) & 0x0fff) << 16),
    );
    // Solid fill color lives in SRC_FG_COLOR (vendor RGA2_set_reg_color_fill:
    // `*bRGA_SRC_FG_COLOR = msg->fg_color`, RGA2_SRC_FG_COLOR_OFFSET = 0x2c), written verbatim with
    // no driver-side normalization (the SRC1 format above tells the engine how to read it). The
    // earlier SRC_BG_COLOR (0x28) write left FG=0; this writes the requested color into FG.
    // The vendor solid-color arm also writes CF_GR_A/B/G/R and SRC_VIR_INFO=mask_stride<<16; for a
    // plain (non-gradient, non-ROP-mask) fill `gr_color` and `rop_mask_stride` are 0, which our
    // zeroed command buffer already provides — so no extra writes are required.
    buf.set_register(registers::SRC_FG_COLOR, color);
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
        // Vendor solid-fill color lives in SRC_FG_COLOR (0x2c), not SRC_BG_COLOR (0x28).
        assert_eq!(cmd.register(registers::SRC_FG_COLOR), Some(0x0000_00ff));
        assert_eq!(cmd.register(registers::SRC_BG_COLOR), Some(0));
        // DST_INFO carries the dst format (RGBA8888=0) in bits[5:0] AND the SRC1/foreground format in
        // bits[11:7]. For an RGBA8888 dst both are 0, so DST_INFO is 0 — but the SRC1 field MUST be
        // explicitly tied to the dst format (the fill FG color is interpreted through SRC1, vendor
        // color_fill path never programs SRC_INFO). See fill_src1_format_tracks_dst.
        assert_eq!(cmd.register(registers::DST_INFO), Some(0));
        // The fill must NOT touch SRC_INFO (the vendor color_fill dispatch never calls
        // RGA2_set_reg_src_info; SRC_INFO is irrelevant to FG-color interpretation).
        assert_eq!(cmd.register(registers::SRC_INFO), Some(0));
        assert_eq!(cmd.register(registers::MMU_CTRL1), Some(0));
        assert_eq!(
            cmd.register(registers::MODE_CTRL),
            Some(encode_mode(registers::MODE_RENDER_RECTANGLE_FILL, 0))
        );
    }

    #[test]
    fn fill_src1_format_tracks_dst() {
        // The FG-color channel order is set by the SRC1 format field in DST_INFO (bits[11:7]); it
        // must mirror the dst format/swap so `color` (packed in dst order) round-trips verbatim.
        // RGBA8888 dst → SRC1 fmt=0, no swaps → SRC1 field 0.
        let rgba = encode_fill(
            ImageDesc::rgb(16, 16, 16 * 4, PixelFormat::Rgba8888, 0x4000_0000),
            0x1122_33ff,
        )
        .unwrap()
        .register(registers::DST_INFO)
        .unwrap();
        assert_eq!((rgba >> registers::DST_INFO_SRC1_FMT_SHIFT) & 0x7, 0);
        assert_eq!(rgba & registers::DST_INFO_SRC1_RB_SWAP, 0);
        assert_eq!(rgba & registers::DST_INFO_SRC1_ALPHA_SWAP, 0);
        // BGRA8888 dst → hw_format = RGBA(0) | R/B-swap → SRC1 fmt=0 + SRC1_RB_SWAP set, mirroring the
        // dst R/B-swap so the FG color is read with R/B swapped exactly as the dst writes it.
        let bgra = encode_fill(
            ImageDesc::rgb(16, 16, 16 * 4, PixelFormat::Bgra8888, 0x4000_0000),
            0x1122_33ff,
        )
        .unwrap()
        .register(registers::DST_INFO)
        .unwrap();
        // dst bits: RGBA fmt 0 + dst R/B swap (bit4).
        assert_eq!(bgra & 0xf, registers::FMT_RGBA8888);
        assert_ne!(bgra & registers::INFO_RBSWAP, 0);
        // SRC1 bits: fmt 0 + SRC1 R/B swap (bit10), alpha swap clear.
        assert_eq!((bgra >> registers::DST_INFO_SRC1_FMT_SHIFT) & 0x7, 0);
        assert_ne!(bgra & registers::DST_INFO_SRC1_RB_SWAP, 0);
        assert_eq!(bgra & registers::DST_INFO_SRC1_ALPHA_SWAP, 0);
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
        // Vendor downscale factor = (dst<<16)/src in the LOW half (bits[15:0]):
        // (640<<16)/1920 = 0x5555, (360<<16)/1080 = 0x5555.
        assert_eq!(
            cmd.register(registers::SRC_X_FACTOR),
            Some(((640u32 << 16) / 1920) & 0xffff)
        );
        assert_eq!(
            cmd.register(registers::SRC_Y_FACTOR),
            Some(((360u32 << 16) / 1080) & 0xffff)
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
        // Scaler filter selected (bicubic) when scaling.
        assert_eq!(
            (src_info >> registers::SRC_INFO_SCL_FILTER_SHIFT) & 0x3,
            registers::SCL_FILTER_BICUBIC
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
        assert_eq!(fill.register(registers::SRC_FG_COLOR), Some(0x0072_7272));
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

    #[test]
    fn hw_format_abgr_distinct_from_rgba() {
        let abgr = encode_copy(
            ImageDesc::rgb(8, 8, 8 * 4, PixelFormat::Abgr8888, 0x1000),
            ImageDesc::rgb(8, 8, 8 * 4, PixelFormat::Abgr8888, 0x9000),
        )
        .unwrap()
        .register(registers::SRC_INFO)
        .unwrap();
        let rgba = encode_copy(
            ImageDesc::rgb(8, 8, 8 * 4, PixelFormat::Rgba8888, 0x1000),
            ImageDesc::rgb(8, 8, 8 * 4, PixelFormat::Rgba8888, 0x9000),
        )
        .unwrap()
        .register(registers::SRC_INFO)
        .unwrap();
        assert_ne!(abgr, rgba, "ABGR must not encode identically to RGBA");
    }

    #[test]
    fn blit_upscale_programs_factor_and_mode() {
        // 320x240 → 640x480 RGBA upscale.
        let src = ImageDesc::rgb(320, 240, 320 * 4, PixelFormat::Rgba8888, 0x4000_0000);
        let dst = ImageDesc::rgb(640, 480, 640 * 4, PixelFormat::Rgba8888, 0x4100_0000);
        let cmd = encode_blit(&Blit::resize(src, dst)).unwrap();
        // Vendor upscale factor = ((src-1)<<16)/(dst-1) in the HIGH half (bits[31:16]):
        // ((320-1)<<16)/(640-1) << 16, ((240-1)<<16)/(480-1) << 16.
        assert_eq!(
            cmd.register(registers::SRC_X_FACTOR),
            Some((((320u32 - 1) << 16) / (640 - 1)) << 16)
        );
        assert_eq!(
            cmd.register(registers::SRC_Y_FACTOR),
            Some((((240u32 - 1) << 16) / (480 - 1)) << 16)
        );
        let si = cmd.register(registers::SRC_INFO).unwrap();
        assert_eq!(
            (si >> registers::SRC_INFO_HSCL_SHIFT) & 0x3,
            registers::SCL_UP
        );
        assert_eq!(
            (si >> registers::SRC_INFO_VSCL_SHIFT) & 0x3,
            registers::SCL_UP
        );
    }

    #[test]
    fn blit_nv12_cropped_src_uv_offset() {
        // Crop a 16x16 window at (8,4) from a 64x48 NV12 src into a 16x16 RGBA dst.
        let src = ImageDesc::nv12(64, 48, 64, 0x4000_0000); // uv at +64*48
        let dst = ImageDesc::rgb(16, 16, 16 * 4, PixelFormat::Rgba8888, 0x4100_0000);
        let mut b = Blit::crop(
            src,
            Rect {
                x: 8,
                y: 4,
                width: 16,
                height: 16,
            },
            dst,
        );
        b.csc = Some(CscStandard::Bt601Limited);
        let cmd = encode_blit(&b).unwrap();
        // Y base: 0x4000_0000 + 4*64 + 8*1
        assert_eq!(
            cmd.register(registers::SRC_Y_RGB_BASE_ADDR),
            Some(0x4000_0000 + 4 * 64 + 8)
        );
        // UV base: (0x4000_0000 + 64*48) + (4/2)*64 + 8*1
        assert_eq!(
            cmd.register(registers::SRC_CB_BASE_ADDR),
            Some(0x4000_0000 + 64 * 48 + 2 * 64 + 8)
        );
    }
}
