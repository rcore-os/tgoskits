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

// SRC_INFO/DST_INFO format codes (bits[3:0]). CONFIRM ON BOARD (vendor rga2.h).
pub const FMT_RGBA8888: u32 = 0x0;
pub const FMT_RGBX8888: u32 = 0x1;
pub const FMT_RGB888: u32 = 0x2;
pub const FMT_RGB565: u32 = 0x4;
pub const FMT_YCBCR_420_SP: u32 = 0xa; // NV12
pub const FMT_YCRCB_420_SP: u32 = 0xe; // NV21
pub const FMT_YCBCR_422_SP: u32 = 0x8; // NV16
// SRC_INFO modifier bits. CONFIRM ON BOARD.
pub const INFO_RBSWAP: u32 = 1 << 4;
pub const INFO_ALPHA_SWAP: u32 = 1 << 5;
pub const INFO_UVSWAP: u32 = 1 << 6;
// SRC_INFO csc_mode (bits[9:8]) — YUV→RGB. CONFIRM ON BOARD value↔standard map.
pub const SRC_INFO_CSC_SHIFT: u32 = 8;
pub const CSC_BT601_LIMITED: u32 = 1;
pub const CSC_BT601_FULL: u32 = 2;
pub const CSC_BT709_LIMITED: u32 = 3;
// DST_INFO dst_csc (bits[17:16]) — RGB→YUV. CONFIRM ON BOARD.
pub const DST_INFO_CSC_SHIFT: u32 = 16;
// SRC_INFO scale mode (h=bits[17:16], v=bits[15:14]); 00=none,01=down,10=up. CONFIRM ON BOARD split.
pub const SRC_INFO_HSCL_SHIFT: u32 = 16;
pub const SRC_INFO_VSCL_SHIFT: u32 = 14;
pub const SCL_NONE: u32 = 0;
pub const SCL_DOWN: u32 = 1;
pub const SCL_UP: u32 = 2;
// Width alignment required for the RGA path. CONFIRM ON BOARD (bench gates on %16).
// Not enforced in validate() yet — the 4-vs-16 requirement is board-confirmed first (the bench's %16 is conservative).
pub const WIDTH_ALIGN: u32 = 16;
