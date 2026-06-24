//! Minimal RGA register offsets used by the bring-up path.

pub const CMD_BUFFER_WORDS: usize = 0x20;

pub const SYS_CTRL: usize = 0x0000;
pub const CMD_CTRL: usize = 0x0004;
pub const CMD_BASE: usize = 0x0008;
pub const INT: usize = 0x0010;
pub const STATUS: usize = 0x000c;
pub const VERSION_INFO: usize = 0x0028;

pub const MODE_BASE: usize = 0x0100;
pub const MODE_MAX: usize = 0x017c;

pub const MODE_CTRL: usize = 0x0100;
pub const SRC_INFO: usize = 0x0104;
pub const SRC_Y_RGB_BASE_ADDR: usize = 0x0108;
pub const SRC_CB_BASE_ADDR: usize = 0x010c;
pub const SRC_CR_BASE_ADDR: usize = 0x0110;
pub const SRC_VIR_INFO: usize = 0x0118;
pub const SRC_ACT_INFO: usize = 0x011c;
pub const SRC_X_FACTOR: usize = 0x0120;
pub const SRC_Y_FACTOR: usize = 0x0124;
pub const SRC_BG_COLOR: usize = 0x0128;
pub const SRC_FG_COLOR: usize = 0x012c;
pub const DST_INFO: usize = 0x0138;
pub const DST_Y_RGB_BASE_ADDR: usize = 0x013c;
pub const DST_CB_BASE_ADDR: usize = 0x0140;
pub const DST_CR_BASE_ADDR: usize = 0x0144;
pub const DST_VIR_INFO: usize = 0x0148;
pub const DST_ACT_INFO: usize = 0x014c;
pub const ALPHA_CTRL0: usize = 0x0150;
pub const ALPHA_CTRL1: usize = 0x0154;
// FADING_CTRL (cmd 0x58, word 22) and PAT_CON (cmd 0x5c, word 23): the pattern/fading registers
// the vendor `RGA2_set_pat_info` writes UNCONDITIONALLY for every render mode, including color fill
// (rga2_reg_info.c:1009-1030, called before the render-mode switch). Verbatim offsets from
// rga2_reg_info.h:307-308. A solid fill leaves them logically inert, but the vendor still programs
// them from a zeroed `pat` descriptor; our command block's omission of these two words is the SOLE
// deviation from the proven-working reference color-fill block (see encode_fill).
pub const FADING_CTRL: usize = 0x0158;
pub const PAT_CON: usize = 0x015c;
pub const MMU_CTRL1: usize = 0x016c;
pub const MMU_SRC_BASE: usize = 0x0170;
pub const MMU_SRC1_BASE: usize = 0x0174;
pub const MMU_DST_BASE: usize = 0x0178;

pub const MODE_RENDER_BITBLT: u32 = 0;
pub const MODE_RENDER_RECTANGLE_FILL: u32 = 2;
pub const MODE_BITBLT_SRC_TO_DST: u32 = 0;

pub const COLOR_FMT_ABGR8888: u32 = 0;
pub const COLOR_NONE_SWAP: u32 = 0;

pub const MMU_SRC_ENABLE: u32 = 1 << 0; // sw_src_mmu_en
pub const MMU_SRC1_ENABLE: u32 = 1 << 4; // sw_src1_mmu_en
pub const MMU_DST_ENABLE: u32 = 1 << 8; // sw_dst_mmu_en

// SRC_INFO/DST_INFO format codes (bits[3:0]). Board-validated 2026-06-24 — run 12
// copy+resize produce correct pixels on RGA2 (OrangePi-5-Plus).
pub const FMT_RGBA8888: u32 = 0x0;
pub const FMT_RGBX8888: u32 = 0x1;
pub const FMT_RGB888: u32 = 0x2;
pub const FMT_RGB565: u32 = 0x4;
pub const FMT_YCBCR_420_SP: u32 = 0xa; // NV12
pub const FMT_YCRCB_420_SP: u32 = 0xe; // NV21
pub const FMT_YCBCR_422_SP: u32 = 0x8; // NV16
/// Packed YUV 4:2:2 — single plane, interleaved. Format code 0x7 per kernel
/// rga2_reg_info.c:266-269. The byte-order variants (YUYV/UYVY/VYUY/YVYU) are
/// distinguished by cbcr_swap / rb_swap flags in SRC_INFO, set in `hw_format`.
pub const FMT_YUV422_PACKED: u32 = 0x7;
// SRC_INFO modifier bits. R/B+alpha swaps validated (copy+resize correct pixels, run 12).
// YUYV swap (rb_swp=1, cbcr_swp via INFO_UVSWAP) NOT YET board-tested.
pub const INFO_RBSWAP: u32 = 1 << 4;
pub const INFO_ALPHA_SWAP: u32 = 1 << 5;
pub const INFO_UVSWAP: u32 = 1 << 6; // aka cbcr_swp — swap Cb/Cr order
// DST_INFO SRC1 (foreground constant / second-source) format field. A color FILL feeds fg_color
// through the SRC1 path; the vendor sets its format/swap from `msg->src1.format` into DST_INFO, NOT
// into SRC_INFO (rga2_reg_info.c RGA2_set_reg_dst_info, lines 446-448; color_fill dispatch at 1084
// calls dst_info but never src_info). Masks (rga2_reg_info.h:137-152):
//   SW_SRC1_FMT       0x7<<7   SW_SRC1_RB_SWP 0x1<<10   SW_SRC1_ALPHA_SWP 0x1<<11
pub const DST_INFO_SRC1_FMT_SHIFT: u32 = 7;
pub const DST_INFO_SRC1_FMT_MASK: u32 = 0x7;
pub const DST_INFO_SRC1_RB_SWAP: u32 = 1 << 10;
pub const DST_INFO_SRC1_ALPHA_SWAP: u32 = 1 << 11;
// SRC_INFO csc_mode (bits[9:8]) — YUV→RGB. CONFIRM ON BOARD value↔standard map.
pub const SRC_INFO_CSC_SHIFT: u32 = 8;
pub const CSC_BT601_LIMITED: u32 = 1;
pub const CSC_BT601_FULL: u32 = 2;
pub const CSC_BT709_LIMITED: u32 = 3;
// DST_INFO dst_csc (bits[17:16]) — RGB→YUV. CONFIRM ON BOARD.
pub const DST_INFO_CSC_SHIFT: u32 = 16;
// SRC_INFO scale mode: HSCL=bits[15:14], VSCL=bits[17:16]; 00=none,01=down,10=up
// (vendor rga2_reg_info.h m_RGA2_SRC_INFO_SW_SW_SRC_HSCL_MODE=0x3<<14, VSCL=0x3<<16; mode value
// from RGA2_reg_get_param x_flag/y_flag: 1=down, 2=up). The shifts were previously transposed
// (HSCL=16, VSCL=14); harmless for a symmetric square resize but wrong for any axis-asymmetric scale.
pub const SRC_INFO_HSCL_SHIFT: u32 = 14;
pub const SRC_INFO_VSCL_SHIFT: u32 = 16;
pub const SCL_NONE: u32 = 0;
pub const SCL_DOWN: u32 = 1;
pub const SCL_UP: u32 = 2;
// SRC_INFO scaler filter (SCL_FILTER bits[25:24] = scale_bicu_mode). Vendor bring-up uses bicubic
// (=2) for any scaling op (rga2_drv.c req.scale_bicu_mode=2); 0 = bypass.
pub const SRC_INFO_SCL_FILTER_SHIFT: u32 = 24;
pub const SCL_FILTER_BICUBIC: u32 = 2;
// Width alignment required for the RGA path. CONFIRM ON BOARD (bench gates on %16).
// Not enforced in validate() yet — the 4-vs-16 requirement is board-confirmed first (the bench's %16 is conservative).
pub const WIDTH_ALIGN: u32 = 16;
