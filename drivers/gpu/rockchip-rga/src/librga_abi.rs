//! librga kernel ioctl ABI (RGA_BLIT_SYNC 0x5017, `struct rga_req`) + translation to RgaOperation.
//! `#[repr(C)]` mirrors use fixed-width types so the arm64 (LP64) layout is reproduced on the host.
//! CONFIRM ON BOARD: exact sizeof/offsets vs the real librga (strace) — reconstructed from
//! canonical Rockchip rga.h (amarula/rockchip-linux-rga).

use crate::{
    error::{Result, RgaError},
    operation::{Blit, CscStandard, ImageDesc, PixelFormat, Rect, RgaOperation},
};

pub const RGA_BLIT_SYNC: u32 = 0x5017;
pub const RGA_BLIT_ASYNC: u32 = 0x5018;
pub const RGA_FLUSH: u32 = 0x5019;
pub const RGA_GET_VERSION: u32 = 0x501b;

/// render_mode (rga_req). CONFIRM ON BOARD.
pub const RENDER_BITBLT: u8 = 0;
pub const RENDER_COLOR_FILL: u8 = 2;

/// Mirror of `rga_img_info_t` from kernel rga.h (arm64/LP64).
///
/// Addresses are `u64` (`unsigned long` on arm64 = 8 bytes). This gives:
///   3×u64 + u32 + 8×u16 = 24 + 4 + 16 = 44 raw bytes; padded to 48 by repr(C) alignment.
///
/// CONFIRM ON BOARD: `sizeof(rga_img_info_t)` via strace or a C probe. The Rust mirror
/// is 48 bytes (align=8, 44 raw + 4 pad) — the correct arm64/LP64 value for this layout.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgaImgInfo {
    pub yrgb_addr: u64,
    pub uv_addr: u64,
    pub v_addr: u64,
    pub format: u32,
    pub act_w: u16,
    pub act_h: u16,
    pub x_offset: u16,
    pub y_offset: u16,
    pub vir_w: u16,
    pub vir_h: u16,
    pub endian_mode: u16,
    pub alpha_swap: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RectT {
    pub xmin: u16,
    pub xmax: u16,
    pub ymin: u16,
    pub ymax: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorFill {
    pub v: [i16; 8],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PointT {
    pub x: u16,
    pub y: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct LineDraw {
    pub start: PointT,
    pub end: PointT,
    pub color: u32,
    pub flag: u32,
    pub line_width: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Fading {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub res: u8,
}

/// Mirror of `STRUCT_RGA_MMU` from kernel rga.h (arm64/LP64).
/// `base_addr` is `unsigned long*` (pointer-to-table) — stored as raw `u64`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MmuInfo {
    pub mmu_en: u8,
    pub base_addr: u64,
    pub mmu_flag: u32,
}

/// Mirror of `struct rga_req` — the ioctl argument for `RGA_BLIT_SYNC`/`RGA_BLIT_ASYNC`.
///
/// All sub-structs must match the arm64/LP64 ABI.  `sizeof::<RgaReq>()` == 296 on this host
/// (confirmed by `core::mem::size_of` — see drift guard below).
/// CONFIRM ON BOARD: verify 296 via strace or a C sizeof probe against the running kernel.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgaReq {
    pub render_mode: u8,
    pub src: RgaImgInfo,
    pub dst: RgaImgInfo,
    pub pat: RgaImgInfo,
    pub rop_mask_addr: u64,
    pub lut_addr: u64,
    pub clip: RectT,
    pub sina: i32,
    pub cosa: i32,
    pub alpha_rop_flag: u16,
    pub scale_mode: u8,
    pub color_key_max: u32,
    pub color_key_min: u32,
    pub fg_color: u32,
    pub bg_color: u32,
    pub gr_color: ColorFill,
    pub line_draw_info: LineDraw,
    pub fading: Fading,
    pub pd_mode: u8,
    pub alpha_global_value: u8,
    pub rop_code: u16,
    pub bsfilter_flag: u8,
    pub palette_mode: u8,
    pub yuv2rgb_mode: u8,
    pub endian_mode: u8,
    pub rotate_mode: u8,
    pub color_fill_mode: u8,
    pub mmu_info: MmuInfo,
    pub alpha_rop_mode: u8,
    pub src_trans_mode: u8,
    pub cmd_fin_int_enable: u8,
    pub complete: u64,
}

// Drift guard: `rga_img_info_t` on arm64/LP64 is 48 bytes.
//
// NOTE: the task spec cited 44 bytes — that under-counts the trailing alignment padding added
// by repr(C) (the struct has align=8 from the u64 address fields; raw sum is 44 bytes but
// sizeof rounds up to the next multiple of 8 = 48).  The 48-byte figure is the correct
// arm64/LP64 ABI value. CONFIRM ON BOARD via `sizeof(rga_img_info_t)` in a C probe.
const _: () = assert!(core::mem::size_of::<RgaImgInfo>() == 48);

// Drift guard (self-consistency). The absolute match to the real librga is CONFIRMED ON BOARD.
const _: () = assert!(core::mem::size_of::<RgaReq>() == 296);

// ---------------------------------------------------------------------------
// Parsed types
// ---------------------------------------------------------------------------

/// Decoded image buffer reference extracted from `RgaImgInfo`.
///
/// `addr`/`uv_addr` hold the raw values from the ioctl argument — these are dma-buf fds or
/// physical addresses depending on the MMU mode; the kernel layer resolves them before calling
/// `into_operation` (design §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgaBufferRef {
    pub addr: u64,
    pub uv_addr: u64,
    pub format: PixelFormat,
    pub act_w: u32,
    pub act_h: u32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub vir_w: u32,
    pub vir_h: u32,
}

/// Which Phase-D operation shape the parsed request maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedKind {
    Blit,
    Fill,
    Copy,
}

/// A fully-parsed and validated `rga_req` ready for `into_operation`.
#[derive(Debug, Clone, Copy)]
pub struct ParsedRgaReq {
    pub kind: ParsedKind,
    pub src: RgaBufferRef,
    pub dst: RgaBufferRef,
    pub csc: Option<CscStandard>,
    pub fill_color: u32,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn img_ref(i: &RgaImgInfo) -> Result<RgaBufferRef> {
    Ok(RgaBufferRef {
        addr: i.yrgb_addr,
        uv_addr: i.uv_addr,
        format: rk_format_to_pixel(i.format)?,
        act_w: i.act_w as u32,
        act_h: i.act_h as u32,
        x_offset: i.x_offset as u32,
        y_offset: i.y_offset as u32,
        vir_w: i.vir_w as u32,
        vir_h: i.vir_h as u32,
    })
}

/// Select CSC standard when the src/dst format pair crosses YUV↔RGB.
///
/// CONFIRM ON BOARD: the `yuv2rgb_mode` value↔standard map (design §8).
/// PR-E2: any YUV↔RGB crossing picks BT.601 limited; refine once the board confirms the map.
fn csc_for(src: PixelFormat, dst: PixelFormat, _yuv2rgb_mode: u8) -> Option<CscStandard> {
    if src.is_yuv() != dst.is_yuv() {
        Some(CscStandard::Bt601Limited)
    } else {
        None
    }
}

/// Parse a librga `rga_req` into the supported op shape (PR-E2 subset). Rejects
/// rotation, blend/ROP, and unrecognised render modes.
pub fn parse(req: &RgaReq) -> Result<ParsedRgaReq> {
    if req.rotate_mode != 0 || req.sina != 0 || req.cosa != 0 {
        return Err(RgaError::Unsupported); // rotation not supported in PR-E2
    }
    if req.alpha_rop_flag != 0 {
        return Err(RgaError::Unsupported); // blend / ROP / fading / dither not supported
    }
    let src = img_ref(&req.src)?;
    let dst = img_ref(&req.dst)?;
    let kind = match req.render_mode {
        RENDER_COLOR_FILL => ParsedKind::Fill,
        RENDER_BITBLT => {
            if src.format == dst.format
                && src.act_w == dst.act_w
                && src.act_h == dst.act_h
                && !src.format.is_yuv()
            {
                ParsedKind::Copy
            } else {
                ParsedKind::Blit
            }
        }
        _ => return Err(RgaError::Unsupported),
    };
    let csc = if matches!(kind, ParsedKind::Blit) {
        csc_for(src.format, dst.format, req.yuv2rgb_mode)
    } else {
        None
    };
    Ok(ParsedRgaReq {
        kind,
        src,
        dst,
        csc,
        fill_color: req.bg_color,
    })
}

// ---------------------------------------------------------------------------
// RgaBufferRef / ParsedRgaReq → RgaOperation
// ---------------------------------------------------------------------------

impl RgaBufferRef {
    fn image_desc(&self, phys: u64, uv_phys: Option<u64>) -> ImageDesc {
        ImageDesc {
            width: self.vir_w,
            height: self.vir_h,
            stride_bytes: self.vir_w * self.format.bytes_per_pixel(),
            format: self.format,
            phys_addr: phys,
            uv_phys_addr: uv_phys,
        }
    }

    fn rect(&self) -> Rect {
        Rect {
            x: self.x_offset,
            y: self.y_offset,
            width: self.act_w,
            height: self.act_h,
        }
    }
}

impl ParsedRgaReq {
    /// Build the Phase D op from resolved physical addresses (the kernel supplies phys after
    /// resolving the dma-buf fds). `src_phys`/`src_uv` are ignored for `Fill`.
    pub fn into_operation(
        &self,
        src_phys: u64,
        src_uv: Option<u64>,
        dst_phys: u64,
        dst_uv: Option<u64>,
    ) -> Result<RgaOperation> {
        let dst_desc = self.dst.image_desc(dst_phys, dst_uv);
        match self.kind {
            ParsedKind::Fill => Ok(RgaOperation::Fill {
                dst: dst_desc,
                color: self.fill_color,
            }),
            ParsedKind::Copy => {
                let src_desc = self.src.image_desc(src_phys, src_uv);
                Ok(RgaOperation::Copy {
                    src: src_desc,
                    dst: dst_desc,
                })
            }
            ParsedKind::Blit => {
                let src_desc = self.src.image_desc(src_phys, src_uv);
                let op = Blit::new(
                    src_desc,
                    dst_desc,
                    self.src.rect(),
                    self.dst.rect(),
                    self.csc,
                );
                op.validate()?;
                Ok(RgaOperation::Blit(op))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RK_FORMAT mapper
// ---------------------------------------------------------------------------

/// Map a librga RK_FORMAT_* field (stored value = code << 8) to a core PixelFormat.
/// CONFIRM ON BOARD: the RK_FORMAT_* numeric codes vs the bundled rga.h.
pub fn rk_format_to_pixel(format_field: u32) -> Result<PixelFormat> {
    match (format_field >> 8) & 0xff {
        0x0 => Ok(PixelFormat::Rgba8888),
        0x1 => Ok(PixelFormat::Rgbx8888),
        0x2 => Ok(PixelFormat::Rgb888),
        0x3 => Ok(PixelFormat::Bgra8888),
        0x4 => Ok(PixelFormat::Rgb565),
        0x7 => Ok(PixelFormat::Bgr888),
        0x8 => Ok(PixelFormat::Nv16),
        0xa => Ok(PixelFormat::Nv12),
        0xe => Ok(PixelFormat::Nv21),
        _ => Err(RgaError::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{CscStandard, Rect, RgaOperation};

    #[test]
    fn img_info_is_48_bytes() {
        // arm64/LP64: 3×u64 (24) + u32 (4) + 8×u16 (16) = 44 raw, padded to 48 by align=8.
        assert_eq!(core::mem::size_of::<RgaImgInfo>(), 48);
    }

    #[test]
    fn rga_req_embeds_three_images() {
        assert!(core::mem::size_of::<RgaReq>() >= 1 + 3 * 48);
    }

    #[test]
    fn format_mapping() {
        assert_eq!(rk_format_to_pixel(0x2 << 8), Ok(PixelFormat::Rgb888));
        assert_eq!(rk_format_to_pixel(0xa << 8), Ok(PixelFormat::Nv12));
        assert_eq!(rk_format_to_pixel(0x7 << 8), Ok(PixelFormat::Bgr888));
        assert_eq!(rk_format_to_pixel(0x99 << 8), Err(RgaError::Unsupported));
    }

    #[test]
    fn req_default_zeroes() {
        let r = RgaReq::default();
        assert_eq!(r.render_mode, 0);
        assert_eq!(r.src.yrgb_addr, 0);
    }

    // -----------------------------------------------------------------------
    // E2 tests
    // -----------------------------------------------------------------------

    /// Helper: build an RgaImgInfo with the given RK_FORMAT code, dimensions and base address.
    fn img(fmt_code: u32, w: u16, h: u16, vir: u16, addr: u64) -> RgaImgInfo {
        RgaImgInfo {
            yrgb_addr: addr,
            format: fmt_code << 8,
            act_w: w,
            act_h: h,
            vir_w: vir,
            vir_h: h,
            ..Default::default()
        }
    }

    #[test]
    fn parse_resize_nv12_to_rgb888_is_blit_with_csc() {
        // NV12 (0xa) 1920×1080 → RGB888 (0x2) 640×640: different dims + YUV↔RGB → Blit + CSC
        let req = RgaReq {
            render_mode: RENDER_BITBLT,
            src: img(0xa, 1920, 1080, 1920, 0x1000),
            dst: img(0x2, 640, 640, 640, 0x2000),
            ..Default::default()
        };
        let p = parse(&req).expect("parse failed");
        assert_eq!(p.kind, ParsedKind::Blit);
        assert_eq!(p.csc, Some(CscStandard::Bt601Limited));
    }

    #[test]
    fn parse_color_fill_is_fill() {
        let req = RgaReq {
            render_mode: RENDER_COLOR_FILL,
            dst: img(0x2, 640, 640, 640, 0x2000),
            bg_color: 0x727272,
            ..Default::default()
        };
        let p = parse(&req).expect("parse failed");
        assert_eq!(p.kind, ParsedKind::Fill);
        assert_eq!(p.fill_color, 0x727272);
    }

    #[test]
    fn parse_same_rgb_is_copy() {
        let req = RgaReq {
            render_mode: RENDER_BITBLT,
            src: img(0x2, 64, 64, 64, 0x1000),
            dst: img(0x2, 64, 64, 64, 0x2000),
            ..Default::default()
        };
        let p = parse(&req).expect("parse failed");
        assert_eq!(p.kind, ParsedKind::Copy);
        assert_eq!(p.csc, None);
    }

    #[test]
    fn parse_rejects_rotation() {
        let req = RgaReq {
            render_mode: RENDER_BITBLT,
            rotate_mode: 1,
            src: img(0x2, 64, 64, 64, 0x1000),
            dst: img(0x2, 64, 64, 64, 0x2000),
            ..Default::default()
        };
        assert!(matches!(parse(&req), Err(RgaError::Unsupported)));
    }

    #[test]
    fn parse_rejects_blend() {
        let req = RgaReq {
            render_mode: RENDER_BITBLT,
            alpha_rop_flag: 1,
            src: img(0x2, 64, 64, 64, 0x1000),
            dst: img(0x2, 64, 64, 64, 0x2000),
            ..Default::default()
        };
        assert!(matches!(parse(&req), Err(RgaError::Unsupported)));
    }

    #[test]
    fn into_operation_blit_geometry() {
        // NV12 1920×1080 vir → RGB888 640×640 vir.
        // act dims == vir dims so rects are fully within surface.
        // Scale: 1920→640 (3× down-scale) — within the 16× limit.
        // dst vir_w=640 ≤ DST_MAX_DIMENSION=4096 — OK.
        // Phys addrs must keep the full surface extent within 32-bit:
        //   src Y: 0x0100_0000 + 1920*1080*1 = ~2 MiB → well under 4 GiB
        //   src UV: 0x0200_0000
        //   dst:    0x0300_0000 + 640*640*3  = ~1.2 MiB → OK
        let req = RgaReq {
            render_mode: RENDER_BITBLT,
            src: img(0xa, 1920, 1080, 1920, 0x0100_0000),
            dst: img(0x2, 640, 640, 640, 0x0300_0000),
            ..Default::default()
        };
        let p = parse(&req).expect("parse failed");
        assert_eq!(p.kind, ParsedKind::Blit);

        let src_phys: u64 = 0x0100_0000;
        let src_uv: Option<u64> = Some(0x0200_0000);
        let dst_phys: u64 = 0x0300_0000;

        let op = p
            .into_operation(src_phys, src_uv, dst_phys, None)
            .expect("into_operation failed");
        match op {
            RgaOperation::Blit(b) => {
                assert_eq!(b.src.phys_addr, src_phys);
                assert_eq!(b.src.uv_phys_addr, src_uv);
                // stride = vir_w * bpp = 1920 * 1 (NV12 Y-plane bpp)
                assert_eq!(b.src.stride_bytes, 1920 * 1);
                // dst rect: x_offset=0, y_offset=0, act_w=640, act_h=640
                assert_eq!(
                    b.dst_rect,
                    Rect {
                        x: 0,
                        y: 0,
                        width: 640,
                        height: 640
                    }
                );
                // validate must pass
                b.validate().expect("Blit::validate failed");
            }
            _ => panic!("expected Blit, got {:?}", op),
        }
    }

    #[test]
    fn into_operation_fill_uses_bg_color() {
        let req = RgaReq {
            render_mode: RENDER_COLOR_FILL,
            dst: img(0x2, 640, 640, 640, 0x0100_0000),
            bg_color: 0x727272,
            ..Default::default()
        };
        let p = parse(&req).expect("parse failed");
        let op = p
            .into_operation(0, None, 0x0100_0000, None)
            .expect("into_operation failed");
        match op {
            RgaOperation::Fill { color, .. } => assert_eq!(color, 0x727272),
            _ => panic!("expected Fill"),
        }
    }

    #[test]
    fn into_operation_nv12_sets_uv() {
        // NV12→RGB888 blit; src has explicit UV plane.
        let req = RgaReq {
            render_mode: RENDER_BITBLT,
            src: img(0xa, 640, 480, 640, 0x0100_0000),
            dst: img(0x2, 640, 480, 640, 0x0300_0000),
            ..Default::default()
        };
        let p = parse(&req).expect("parse failed");
        assert_eq!(p.kind, ParsedKind::Blit);

        let src_phys: u64 = 0x0100_0000;
        let uv: u64 = 0x0200_0000;
        let dst_phys: u64 = 0x0300_0000;

        let op = p
            .into_operation(src_phys, Some(uv), dst_phys, None)
            .expect("into_operation failed");
        match op {
            RgaOperation::Blit(b) => {
                assert_eq!(b.src.uv_phys_addr, Some(uv));
            }
            _ => panic!("expected Blit"),
        }
    }
}
