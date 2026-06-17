//! Minimal RGA register offsets used by the bring-up path.

pub const CMD_BUFFER_WORDS: usize = 0x20;

pub const SYS_CTRL: usize = 0x0000;
pub const CMD_CTRL: usize = 0x0004;
pub const CMD_BASE: usize = 0x0008;
pub const INT: usize = 0x0010;
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

pub const MMU_SRC_ENABLE: u32 = 0x7;
pub const MMU_SRC1_ENABLE: u32 = 0x7 << 4;
pub const MMU_DST_ENABLE: u32 = 0x7 << 8;
