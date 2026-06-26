//! librga kernel ioctl ABI (RGA_BLIT_SYNC 0x5017) + translation to RgaOperation.
//! Struct mirrors verified 2026-06-24 against the MultiRGA v1.3.1 kernel driver
//! (radxa/kernel linux-6.1-stan-rkr1, drivers/video/rockchip/rga3/include/rga.h).
//! Sizes confirmed via LP64 C probe (same alignment as aarch64).

use crate::{
    error::{Result, RgaError},
    operation::{Blit, CscStandard, ImageDesc, PixelFormat, Rect, RgaOperation},
};

// --- ioctl commands (rga.h:27-40) ---

pub const RGA_BLIT_SYNC: u32 = 0x5017;
pub const RGA_BLIT_ASYNC: u32 = 0x5018;
pub const RGA_FLUSH: u32 = 0x5019;
pub const RGA_GET_RESULT: u32 = 0x501a;
pub const RGA_GET_VERSION: u32 = 0x501b;
pub const RGA_CACHE_FLUSH: u32 = 0x501c;

/// render_mode (rga.h process mode enum). CONFIRMED on the MultiRGA v1.3.1 driver.
pub const RENDER_BITBLT: u8 = 0;
pub const RENDER_COLOR_FILL: u8 = 2;

// --- New-style ioctl commands (rga.h, _IOC with magic 'r') ---
// RGA_IOC_MAGIC = 'r' (0x72). Computed from the kernel source:
//   _IOWR('r', 3, sizeof(struct rga_buffer_pool)) = _IOWR(0x72, 3, 16)
//   _IOW('r',  4, sizeof(struct rga_buffer_pool)) = _IOW(0x72, 4, 16)
// sizeof(rga_buffer_pool) = 16 (uint64_t + uint32_t, padded to 8-byte alignment).
pub const RGA_IOC_IMPORT_BUFFER: u32 = 0xC0107203;
pub const RGA_IOC_RELEASE_BUFFER: u32 = 0x40107204;
// Version queries librga calls at init (rga.h):
//   _IOR('r', 1, sizeof(rga_version_t)=28)      → driver version
//   _IOR('r', 2, sizeof(rga_hw_versions_t)=144) → per-core hw versions
pub const RGA_IOC_GET_DRVIER_VERSION: u32 = 0x801C_7201;
pub const RGA_IOC_GET_HW_VERSION: u32 = 0x8090_7202;
// Job-scheduler request API (librga ≥ v1.9 im2d path):
//   _IOR('r',  5, sizeof(uint32_t)=4)            → create, returns request id
//   _IOWR('r', 6, sizeof(rga_user_request)=152)  → submit (run)
//   _IOWR('r', 7, sizeof(rga_user_request)=152)  → config (stage, no run)
//   _IOWR('r', 8, sizeof(uint32_t)=4)            → cancel
pub const RGA_IOC_REQUEST_CREATE: u32 = 0x8004_7205;
pub const RGA_IOC_REQUEST_SUBMIT: u32 = 0xC098_7206;
pub const RGA_IOC_REQUEST_CONFIG: u32 = 0xC098_7207;
pub const RGA_IOC_REQUEST_CANCEL: u32 = 0xC004_7208;

/// Buffer type constants for rga_external_buffer.type (rga.h enum rga_memory_type).
pub const RGA_DMA_BUFFER: u32 = 0;
pub const RGA_VIRTUAL_ADDRESS: u32 = 1;
pub const RGA_PHYSICAL_ADDRESS: u32 = 2;
pub const RGA_DMA_BUFFER_PTR: u32 = 3;

// --- Sub-structs (rga.h, in declaration order) ---

/// Kernel `rga_img_info_t` (rga.h). arm64/LP64 sizeof == 56 (verified via C probe).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgaImgInfo {
    pub yrgb_addr: u64,     // offset 0
    pub uv_addr: u64,       // offset 8
    pub v_addr: u64,        // offset 16
    pub format: u32,        // offset 24  (kernel enum rga_surf_format, NOT librga RK_FORMAT)
    pub act_w: u16,         // offset 28
    pub act_h: u16,         // offset 30
    pub x_offset: u16,      // offset 32
    pub y_offset: u16,      // offset 34
    pub vir_w: u16,         // offset 36
    pub vir_h: u16,         // offset 38
    pub endian_mode: u16,   // offset 40
    pub alpha_swap: u16,    // offset 42
    pub rotate_mode: u16,   // offset 44  (NEW — was missing in old mirror)
    pub rd_mode: u16,       // offset 46  (NEW)
    pub compact_mode: u16,  // offset 48  (NEW)
    pub is_10b_endian: u16, // offset 50  (NEW)
    pub enable: u16,        // offset 52  (NEW)
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
    pub gr_x_a: i16,
    pub gr_y_a: i16,
    pub gr_x_b: i16,
    pub gr_y_b: i16,
    pub gr_x_g: i16,
    pub gr_y_g: i16,
    pub gr_x_r: i16,
    pub gr_y_r: i16,
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
    pub start_point: PointT,
    pub end_point: PointT,
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

/// Kernel `struct rga_mmu_t` (rga.h). arm64/LP64 sizeof == 24.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MmuInfo {
    pub mmu_en: u8,
    /// 7 bytes implicit padding to align base_addr at offset 8.
    pub _pad: [u8; 7],
    pub base_addr: u64,
    pub mmu_flag: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CscCoe {
    pub r_v: i16,
    pub g_y: i16,
    pub b_u: i16,
    pub off: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FullCsc {
    pub flag: u8,
    pub coe_y: CscCoe,
    pub coe_u: CscCoe,
    pub coe_v: CscCoe,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CscRange {
    pub max: u16,
    pub min: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CscClip {
    pub y: CscRange,
    pub uv: CscRange,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MosaicInfo {
    pub enable: u8,
    pub mode: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct OsdInvertFactor {
    pub alpha_max: u8,
    pub alpha_min: u8,
    pub yg_max: u8,
    pub yg_min: u8,
    pub crb_max: u8,
    pub crb_min: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgaColor {
    pub value: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct OsdBpp2 {
    pub ac_swap: u8,
    pub endian_swap: u8,
    pub _pad: [u8; 2], // align RgaColor (u32) to 4
    pub color0: RgaColor,
    pub color1: RgaColor,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct OsdModeCtrl {
    pub mode: u8,              // offset 0
    pub direction_mode: u8,    // offset 1
    pub width_mode: u8,        // offset 2
    pub _pad0: u8,             // offset 3 (align u16)
    pub block_fix_width: u16,  // offset 4
    pub block_num: u8,         // offset 6
    pub _pad1: u8,             // offset 7 (align u16)
    pub flags_index: u16,      // offset 8
    pub color_mode: u8,        // offset 10
    pub invert_flags_mode: u8, // offset 11
    pub default_color_sel: u8, // offset 12
    pub invert_enable: u8,     // offset 13
    pub invert_mode: u8,       // offset 14
    pub invert_thresh: u8,     // offset 15
    pub unfix_index: u8,       // offset 16
    pub _pad_end: u8,          // offset 17 (align struct to 2)
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct OsdInfo {
    pub enable: u8,
    pub _pad0: u8,                   // align mode_ctrl to 2
    pub mode_ctrl: OsdModeCtrl,      // offset 2 (sizeof=18)
    pub cal_factor: OsdInvertFactor, // offset 20 (sizeof=6)
    pub _pad1: [u8; 2],              // offset 26, align bpp2_info to 4
    pub bpp2_info: OsdBpp2,          // offset 28 (sizeof=12)
    pub last_flags0: u32,            // offset 40
    pub last_flags1: u32,            // offset 44
    pub cur_flags0: u32,             // offset 48
    pub cur_flags1: u32,             // offset 52
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PreIntrInfo {
    pub enable: u8,
    pub read_intr_en: u8,
    pub write_intr_en: u8,
    pub read_hold_en: u8,
    pub read_threshold: u32,
    pub write_start: u32,
    pub write_step: u32,
}

/// Kernel `struct rga_feature` — bitfield u32.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgaFeature {
    pub bits: u32,
}

// --- Import/release buffer structs (rga.h) ---

/// Kernel `struct rga_memory_parm` (rga.h). sizeof == 16.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgaMemoryParm {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub size: u32,
}

/// Kernel `struct rga_external_buffer` (rga.h). sizeof == 288 on LP64.
/// Userspace fills memory + type + memory_parm; kernel fills handle.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RgaExternalBuffer {
    pub memory: u64,                // offset 0: dma-buf fd or phys addr
    pub r#type: u32,                // offset 8: RGA_DMA_BUFFER / RGA_DMA_BUFFER_PTR
    pub handle: u32,                // offset 12: output handle (filled by kernel)
    pub memory_parm: RgaMemoryParm, // offset 16
    pub _reserve: [u8; 252],        // offset 32  (kernel: uint8_t reserve[252])
}

// Default manually; [u8; 252] > 32
impl Default for RgaExternalBuffer {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

/// Kernel `struct rga_buffer_pool` (rga.h). sizeof == 16 on LP64.
/// Passed as the ioctl argument for IMPORT/RELEASE_BUFFER.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgaBufferPool {
    pub buffers_ptr: u64, // userspace pointer to RgaExternalBuffer array
    pub size: u32,        // number of buffers
}

// --- Version-query structs (rga.h) ---

/// Kernel `struct rga_version_t` (rga.h). sizeof == 28.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgaVersionT {
    pub major: u32,
    pub minor: u32,
    pub revision: u32,
    pub string: [u8; 16],
}

/// Kernel `struct rga_hw_versions_t` (rga.h). sizeof == 144. `size` = number of cores.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgaHwVersions {
    pub version: [RgaVersionT; 5],
    pub size: u32,
}

/// Kernel `struct rga_user_request` (rga.h). sizeof == 152. The ioctl argument for
/// RGA_IOC_REQUEST_SUBMIT / RGA_IOC_REQUEST_CONFIG. `task_ptr` points to a `task_num`-long
/// array of `RgaReq`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RgaUserRequest {
    pub task_ptr: u64,
    pub task_num: u32,
    pub id: u32,
    pub sync_mode: u32,
    pub release_fence_fd: u32,
    pub mpi_config_flags: u32,
    pub acquire_fence_fd: u32,
    pub reservr: [u8; 120],
}

impl Default for RgaUserRequest {
    fn default() -> Self {
        // SAFETY: all-zero is valid for this repr(C) struct (no pointers/invariants).
        unsafe { core::mem::zeroed() }
    }
}

const _: () = assert!(core::mem::size_of::<RgaVersionT>() == 28);
const _: () = assert!(core::mem::size_of::<RgaHwVersions>() == 144);
const _: () = assert!(core::mem::size_of::<RgaUserRequest>() == 152);
const _: () = assert!(core::mem::size_of::<RgaExternalBuffer>() == 288);
const _: () = assert!(core::mem::size_of::<RgaBufferPool>() == 16);

// ---------------------------------------------------------------------------
// Main ioctl argument struct
// ---------------------------------------------------------------------------

/// Mirror of `struct rga_req` (kernel rga.h). sizeof == 504 on arm64/LP64
/// (verified 2026-06-24 via C probe from the exact kernel struct definitions).
///
/// Default is implemented manually because several fields are arrays larger than 32 elements
/// (Rust's Default blanket only covers [T; N] for N ≤ 32).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RgaReq {
    pub render_mode: u8,
    pub _pad0: [u8; 7],             // align rga_img_info_t to 8
    pub src: RgaImgInfo,            // offset 8
    pub dst: RgaImgInfo,            // offset 64
    pub pat: RgaImgInfo,            // offset 120
    pub rop_mask_addr: u64,         // offset 176
    pub lut_addr: u64,              // offset 184
    pub clip: RectT,                // offset 192
    pub sina: i32,                  // offset 200
    pub cosa: i32,                  // offset 204
    pub alpha_rop_flag: u16,        // offset 208
    pub scale_mode: u8,             // offset 210
    pub _pad1: u8,                  // offset 211 (padding to align u32)
    pub color_key_max: u32,         // offset 212
    pub color_key_min: u32,         // offset 216
    pub fg_color: u32,              // offset 220
    pub bg_color: u32,              // offset 224
    pub gr_color: ColorFill,        // offset 228
    pub line_draw_info: LineDraw,   // offset 244
    pub fading: Fading,             // offset 264
    pub pd_mode: u8,                // offset 268
    pub alpha_global_value: u8,     // offset 269
    pub rop_code: u16,              // offset 270
    pub bsfilter_flag: u8,          // offset 272
    pub palette_mode: u8,           // offset 273
    pub yuv2rgb_mode: u8,           // offset 274
    pub endian_mode: u8,            // offset 275
    pub rotate_mode: u8,            // offset 276
    pub color_fill_mode: u8,        // offset 277
    pub _pad2: [u8; 2],             // offset 278 (padding to align mmu_info.base_addr at 8)
    pub mmu_info: MmuInfo,          // offset 280  (1+7pad+8+4 = 20, padded to 24)
    pub alpha_rop_mode: u8,         // offset 304
    pub src_trans_mode: u8,         // offset 305
    pub dither_mode: u8,            // offset 306
    pub _pad3: [u8; 1],             // offset 307 (padding to align full_csc)
    pub full_csc: FullCsc,          // offset 308
    pub in_fence_fd: i32,           // offset 348  (full_csc is 40 bytes: flag + 3x csc_coe_t@12)
    pub core: u8,                   // offset 352
    pub priority: u8,               // offset 353
    pub _pad4: [u8; 2],             // offset 354
    pub out_fence_fd: i32,          // offset 356
    pub handle_flag: u8,            // offset 360
    pub mosaic_info: MosaicInfo,    // offset 361
    pub uvhds_mode: u8,             // offset 363
    pub uvvds_mode: u8,             // offset 364
    pub osd_info: OsdInfo,          // offset 368
    pub pre_intr_info: PreIntrInfo, // offset 424
    pub fg_global_alpha: u8,        // offset 440
    pub bg_global_alpha: u8,        // offset 441
    pub feature: RgaFeature,        // offset 444
    pub full_csc_clip: CscClip,     // offset 448
    pub _reserved_tail: [u8; 48],   // kernel reservr[43] + trailing pad → 504 total
}

// Drift guards — MUST match the arm64/LP64 ABI verified via C probe.
const _: () = assert!(core::mem::size_of::<RgaImgInfo>() == 56);
const _: () = assert!(core::mem::size_of::<RgaReq>() == 504);

impl Default for RgaReq {
    fn default() -> Self {
        // SAFETY: all-zero is valid for this repr(C) struct (no pointers/invariants).
        unsafe { core::mem::zeroed() }
    }
}

// ---------------------------------------------------------------------------
// Parsed types
// ---------------------------------------------------------------------------

/// Decoded image buffer reference extracted from `RgaImgInfo`.
///
/// `addr`/`uv_addr` hold the raw values from the ioctl argument — these are dma-buf fds or
/// physical addresses; the kernel layer resolves them before calling `into_operation`.
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

/// Which operation shape the parsed request maps to.
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
fn csc_for(src: PixelFormat, dst: PixelFormat, _yuv2rgb_mode: u8) -> Option<CscStandard> {
    if src.is_yuv() != dst.is_yuv() {
        Some(CscStandard::Bt601Limited)
    } else {
        None
    }
}

/// Parse a librga `rga_req` into the supported op shape. Rejects rotation, blend/ROP,
/// and unrecognised render modes.
pub fn parse(req: &RgaReq) -> Result<ParsedRgaReq> {
    // Reject only ACTUAL rotation. librga always populates the rotation matrix with
    // the identity (sina=0, cosa=0x10000 == cos 0° in 16.16 fixed point) for a
    // non-rotated blit, so `cosa != 0` is NOT a rotation — gating on it rejected
    // every librga blit. The engine only applies the matrix when rotate_mode != 0.
    if req.rotate_mode != 0 {
        return Err(RgaError::Unsupported);
    }
    if req.alpha_rop_flag != 0 {
        return Err(RgaError::Unsupported);
    }
    // Features the RGA2 op model can't honour and which would silently MIS-RENDER
    // (change the produced pixels, not merely drop a quality hint) if ignored.
    // Reject them so librga gets EINVAL instead of wrong output. `scale_mode` (the
    // scaling-filter quality) and `dither_mode` are quality hints the engine may
    // safely ignore, so they are deliberately NOT gated here.
    if req.color_key_min != 0 || req.color_key_max != 0 {
        return Err(RgaError::Unsupported); // chroma/colour-key compositing
    }
    if req.palette_mode != 0 {
        return Err(RgaError::Unsupported); // indexed / palette colour
    }
    if req.pd_mode != 0 {
        return Err(RgaError::Unsupported); // Porter-Duff alpha blend
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
        fill_color: req.fg_color,
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
    /// Build the Phase D op from resolved physical addresses.
    /// `src_phys`/`src_uv` are ignored for `Fill`.
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
// RK_FORMAT → PixelFormat mapper
// ---------------------------------------------------------------------------

/// Map a `rga_img_info_t.format` value (kernel `RGA_FORMAT_*` enum, NOT shifted) to
/// a core PixelFormat.
///
/// CONFIRM ON BOARD: whether librga passes shifted RK_FORMAT or un-shifted RGA_FORMAT
/// in the kernel rga_req. The current implementation handles both.
pub fn rk_format_to_pixel(format_field: u32) -> Result<PixelFormat> {
    // The kernel RGA_FORMAT values are 0x0..0x3f (un-shifted).
    // librga RK_FORMAT values are shifted left by 8 (0x000..0x3f00).
    // Normalise: if the high byte is set, it's a shifted librga value.
    let code = if format_field > 0xff {
        (format_field >> 8) & 0xff
    } else {
        format_field & 0xff
    };
    match code {
        0x0 => Ok(PixelFormat::Rgba8888),
        0x1 => Ok(PixelFormat::Rgbx8888),
        0x2 => Ok(PixelFormat::Rgb888),
        0x3 => Ok(PixelFormat::Bgra8888),
        0x4 => Ok(PixelFormat::Rgb565),
        0x7 => Ok(PixelFormat::Bgr888),
        // Packed YUV 4:2:2 — format-FIELD codes (kernel enum rga_surf_format), NOT hw registers.
        0x1c => Ok(PixelFormat::Yuyv422), // RGA_FORMAT_YUYV_422
        0x1e => Ok(PixelFormat::Uyvy422), // RGA_FORMAT_UYVY_422
        0x8 => Ok(PixelFormat::Nv16),     // YCbCr_422_SP
        0xa => Ok(PixelFormat::Nv12),     // YCbCr_420_SP
        0xe => Ok(PixelFormat::Nv21),     // YCrCb_420_SP
        _ => Err(RgaError::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{CscStandard, Rect, RgaOperation};

    #[test]
    fn img_info_is_56_bytes() {
        assert_eq!(core::mem::size_of::<RgaImgInfo>(), 56);
    }

    #[test]
    fn rga_req_is_504_bytes() {
        assert_eq!(core::mem::size_of::<RgaReq>(), 504);
    }

    #[test]
    fn rga_req_embeds_three_images() {
        assert!(core::mem::size_of::<RgaReq>() >= 3 * core::mem::size_of::<RgaImgInfo>());
    }

    #[test]
    fn key_field_offsets() {
        // Confirmed via C probe against the real kernel struct
        assert_eq!(core::mem::offset_of!(RgaReq, render_mode), 0);
        assert_eq!(core::mem::offset_of!(RgaReq, src), 8);
        assert_eq!(core::mem::offset_of!(RgaReq, dst), 64);
        assert_eq!(core::mem::offset_of!(RgaReq, pat), 120);
        assert_eq!(core::mem::offset_of!(RgaReq, fg_color), 220);
        assert_eq!(core::mem::offset_of!(RgaReq, bg_color), 224);
        assert_eq!(core::mem::offset_of!(RgaReq, gr_color), 228);
        assert_eq!(core::mem::offset_of!(RgaReq, color_fill_mode), 277);
        assert_eq!(core::mem::offset_of!(RgaReq, alpha_rop_flag), 208);
        assert_eq!(core::mem::offset_of!(RgaReq, scale_mode), 210);
        assert_eq!(core::mem::offset_of!(RgaReq, yuv2rgb_mode), 274);
        assert_eq!(core::mem::offset_of!(RgaReq, rotate_mode), 276);
        // Tail offsets, verified against the librga userspace header (csc_coe_t is 12 bytes
        // and full_csc_t is 40, so everything after full_csc sits 8 bytes later than a naive
        // 32-byte full_csc would place it). handle_flag@360 is the field the kernel reads to
        // decide whether src/dst addresses are import handles or raw fds; a wrong offset here
        // silently flips that decision and breaks every handle-based blit.
        assert_eq!(core::mem::offset_of!(RgaReq, full_csc), 308);
        assert_eq!(core::mem::offset_of!(RgaReq, in_fence_fd), 348);
        assert_eq!(core::mem::offset_of!(RgaReq, core), 352);
        assert_eq!(core::mem::offset_of!(RgaReq, out_fence_fd), 356);
        assert_eq!(core::mem::offset_of!(RgaReq, handle_flag), 360);
    }

    #[test]
    fn format_mapping() {
        // Un-shifted kernel RGA_FORMAT values
        assert_eq!(rk_format_to_pixel(0x2), Ok(PixelFormat::Rgb888));
        assert_eq!(rk_format_to_pixel(0xa), Ok(PixelFormat::Nv12));
        // Shifted librga RK_FORMAT values
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

    /// Helper: build an RgaImgInfo with the given RGA_FORMAT code, dimensions and base address.
    fn img(fmt_code: u32, w: u16, h: u16, vir: u16, addr: u64) -> RgaImgInfo {
        RgaImgInfo {
            yrgb_addr: addr,
            format: fmt_code, // un-shifted RGA_FORMAT
            act_w: w,
            act_h: h,
            vir_w: vir,
            vir_h: h,
            ..Default::default()
        }
    }

    #[test]
    fn parse_resize_nv12_to_rgb888_is_blit_with_csc() {
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
            fg_color: 0x727272,
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
    fn parse_rejects_unhonourable_features() {
        // Each correctness-changing feature must be rejected, not silently ignored.
        let base = || RgaReq {
            render_mode: RENDER_BITBLT,
            src: img(0x2, 64, 64, 64, 0x1000),
            dst: img(0x2, 64, 64, 64, 0x2000),
            ..Default::default()
        };
        assert!(matches!(
            parse(&RgaReq {
                color_key_min: 1,
                ..base()
            }),
            Err(RgaError::Unsupported)
        ));
        assert!(matches!(
            parse(&RgaReq {
                color_key_max: 1,
                ..base()
            }),
            Err(RgaError::Unsupported)
        ));
        assert!(matches!(
            parse(&RgaReq {
                palette_mode: 1,
                ..base()
            }),
            Err(RgaError::Unsupported)
        ));
        assert!(matches!(
            parse(&RgaReq {
                pd_mode: 1,
                ..base()
            }),
            Err(RgaError::Unsupported)
        ));
        // A scaling-filter hint is NOT a correctness change: a plain copy still parses.
        assert!(
            parse(&RgaReq {
                scale_mode: 1,
                ..base()
            })
            .is_ok()
        );
    }

    #[test]
    fn into_operation_blit_geometry() {
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
                assert_eq!(b.src.stride_bytes, 1920);
                assert_eq!(
                    b.dst_rect,
                    Rect {
                        x: 0,
                        y: 0,
                        width: 640,
                        height: 640
                    }
                );
                b.validate().expect("Blit::validate failed");
            }
            _ => panic!("expected Blit, got {:?}", op),
        }
    }

    #[test]
    fn into_operation_fill_uses_fg_color() {
        let req = RgaReq {
            render_mode: RENDER_COLOR_FILL,
            dst: img(0x2, 640, 640, 640, 0x0100_0000),
            fg_color: 0x727272,
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

#[test]
fn sub_struct_sizes() {
    // Cross-check against C probe on LP64 (2026-06-24)
    assert_eq!(core::mem::size_of::<RgaImgInfo>(), 56);
    assert_eq!(core::mem::size_of::<RectT>(), 8);
    assert_eq!(core::mem::size_of::<ColorFill>(), 16);
    assert_eq!(core::mem::size_of::<LineDraw>(), 20);
    assert_eq!(core::mem::size_of::<Fading>(), 4);
    assert_eq!(core::mem::size_of::<MmuInfo>(), 24);
    assert_eq!(core::mem::size_of::<FullCsc>(), 40);
    assert_eq!(core::mem::size_of::<CscClip>(), 8);
    assert_eq!(core::mem::size_of::<MosaicInfo>(), 2);
    assert_eq!(core::mem::size_of::<OsdInfo>(), 56);
    assert_eq!(core::mem::size_of::<PreIntrInfo>(), 16);
    assert_eq!(core::mem::size_of::<RgaFeature>(), 4);
}
