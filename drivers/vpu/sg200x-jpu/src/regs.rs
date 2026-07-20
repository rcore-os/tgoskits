//! JPU MMIO registers and caller-addressed bring-up helpers.

#![allow(dead_code)]

use tock_registers::{
    fields::FieldValue,
    interfaces::{Readable, Writeable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite},
};

const TOP_DDR_ADDR_MODE_OFF: usize = 0x64;
const TOP_CLK_JPEG_OFF: usize = 0x2008;
const TOP_RST_JPEG_OFF: usize = 0x3000;

const TOP_CLK_JPEG_ENABLE: u32 = 0x3300;
const TOP_RST_JPEG_RELEASE_BIT: u32 = 1 << 4;
const TOP_DDR_VD_REMAP_BIT: u32 = 1 << 24;
const VC_BLOCK_ENABLE: u32 = 0x1F;
const JPU_WARMUP_BBC_BASE: u32 = 0x8026_C000;
const REGISTER_WAIT_POLLS: usize = 100_000;

/// JPEG sampling formats written to the MCU and DPB registers.
pub const FORMAT_420: u32 = 0;
pub const FORMAT_422: u32 = 1;
pub const FORMAT_224: u32 = 2;
pub const FORMAT_444: u32 = 3;
pub const FORMAT_400: u32 = 4;

register_bitfields! [
    u32,

    pub VALUE32 [
        VAL OFFSET(0) NUMBITS(32) []
    ],

    pub MJPEG_PIC_START [
        START_PIC OFFSET(0) NUMBITS(1) [],
        START_INIT OFFSET(1) NUMBITS(1) [],
    ],

    pub MJPEG_PIC_STATUS [
        DONE OFFSET(0) NUMBITS(1) [],
        ERROR OFFSET(1) NUMBITS(1) [],
    ],

    pub MJPEG_PIC_CTRL [
        USER_HUFF_TAB OFFSET(6) NUMBITS(1) [],
        HUFF_DC_IDX OFFSET(7) NUMBITS(3) [],
        HUFF_AC_IDX OFFSET(10) NUMBITS(3) [],
    ],

    pub MJPEG_PIC_SIZE [
        HEIGHT OFFSET(0) NUMBITS(16) [],
        WIDTH OFFSET(16) NUMBITS(16) [],
    ],

    pub MJPEG_SCL_INFO [
        ENABLE OFFSET(4) NUMBITS(1) [],
        HORIZONTAL_MODE OFFSET(2) NUMBITS(2) [
            Full = 0,
            Half = 1,
            Quarter = 2,
            Eighth = 3
        ],
        VERTICAL_MODE OFFSET(0) NUMBITS(2) [
            Full = 0,
            Half = 1,
            Quarter = 2,
            Eighth = 3
        ],
    ],

    pub MJPEG_BBC_STRM_CTRL [
        PAGES OFFSET(0) NUMBITS(31) [],
        END_FLAG OFFSET(31) NUMBITS(1) [],
    ],

    pub MJPEG_BBC_BUSY [
        BUSY OFFSET(0) NUMBITS(1) [],
    ],

    pub MJPEG_HUFF_CTRL [
        PHASE OFFSET(0) NUMBITS(12) [],
    ],

    pub MJPEG_QMAT_CTRL [
        PHASE OFFSET(0) NUMBITS(8) [],
    ],
];

register_structs! {
    pub JpuRegisters {
        (0x000 => pub pic_start: ReadWrite<u32, MJPEG_PIC_START::Register>),
        (0x004 => pub pic_status: ReadWrite<u32, MJPEG_PIC_STATUS::Register>),
        (0x008 => pub pic_errmb: ReadWrite<u32, VALUE32::Register>),
        (0x00C => _reserved_pic_setmb),
        (0x010 => pub pic_ctrl: ReadWrite<u32, MJPEG_PIC_CTRL::Register>),
        (0x014 => pub pic_size: ReadWrite<u32, MJPEG_PIC_SIZE::Register>),
        (0x018 => pub mcu_info: ReadWrite<u32, VALUE32::Register>),
        (0x01C => pub rot_info: ReadWrite<u32, VALUE32::Register>),
        (0x020 => pub scl_info: ReadWrite<u32, MJPEG_SCL_INFO::Register>),
        (0x024 => _reserved_if_info),
        (0x028 => pub clp_info: ReadWrite<u32, VALUE32::Register>),
        (0x02C => pub op_info: ReadWrite<u32, VALUE32::Register>),
        (0x030 => pub dpb_config: ReadWrite<u32, VALUE32::Register>),
        (0x034 => pub dpb_base_y: ReadWrite<u32, VALUE32::Register>),
        (0x038 => pub dpb_base_cb: ReadWrite<u32, VALUE32::Register>),
        (0x03C => pub dpb_base_cr: ReadWrite<u32, VALUE32::Register>),
        (0x040 => _reserved_dpb_extra: [u8; 0x24]),
        (0x064 => pub dpb_ystride: ReadWrite<u32, VALUE32::Register>),
        (0x068 => pub dpb_cstride: ReadWrite<u32, VALUE32::Register>),
        (0x06C => _reserved_wresp: [u8; 0x14]),
        (0x080 => pub huff_ctrl: ReadWrite<u32, MJPEG_HUFF_CTRL::Register>),
        (0x084 => pub huff_addr: ReadWrite<u32, VALUE32::Register>),
        (0x088 => pub huff_data: ReadWrite<u32, VALUE32::Register>),
        (0x08C => _reserved_huff_pad),
        (0x090 => pub qmat_ctrl: ReadWrite<u32, MJPEG_QMAT_CTRL::Register>),
        (0x094 => _reserved_qmat_addr),
        (0x098 => pub qmat_data: ReadWrite<u32, VALUE32::Register>),
        (0x09C => _reserved_coef: [u8; 0x14]),
        (0x0B0 => pub rst_intval: ReadWrite<u32, VALUE32::Register>),
        (0x0B4 => pub rst_index: ReadWrite<u32, VALUE32::Register>),
        (0x0B8 => pub rst_count: ReadWrite<u32, VALUE32::Register>),
        (0x0BC => _reserved_rst_pad: [u8; 0x34]),
        (0x0F0 => pub dpcm_diff_y: ReadWrite<u32, VALUE32::Register>),
        (0x0F4 => pub dpcm_diff_cb: ReadWrite<u32, VALUE32::Register>),
        (0x0F8 => pub dpcm_diff_cr: ReadWrite<u32, VALUE32::Register>),
        (0x0FC => _reserved_dpcm_pad),
        (0x100 => pub gbu_ctrl: ReadWrite<u32, VALUE32::Register>),
        (0x104 => _reserved_gbu_mid: [u8; 0x10]),
        (0x114 => pub gbu_wd_ptr: ReadWrite<u32, VALUE32::Register>),
        (0x118 => pub gbu_tt_cnt: ReadWrite<u32, VALUE32::Register>),
        (0x11C => pub gbu_tt_cnt_h: ReadWrite<u32, VALUE32::Register>),
        (0x120 => _reserved_gbu_pbit: [u8; 0x20]),
        (0x140 => pub gbu_bbsr: ReadWrite<u32, VALUE32::Register>),
        (0x144 => pub gbu_bber: ReadWrite<u32, VALUE32::Register>),
        (0x148 => pub gbu_bbir: ReadWrite<u32, VALUE32::Register>),
        (0x14C => pub gbu_bbhr: ReadWrite<u32, VALUE32::Register>),
        (0x150 => _reserved_gbu_tail: [u8; 0x10]),
        (0x160 => pub gbu_ff_rptr: ReadWrite<u32, VALUE32::Register>),
        (0x164 => _reserved_bbc_gap: [u8; 0xA4]),
        (0x208 => pub bbc_end_addr: ReadWrite<u32, VALUE32::Register>),
        (0x20C => pub bbc_wr_ptr: ReadWrite<u32, VALUE32::Register>),
        (0x210 => pub bbc_rd_ptr: ReadWrite<u32, VALUE32::Register>),
        (0x214 => pub bbc_ext_addr: ReadWrite<u32, VALUE32::Register>),
        (0x218 => pub bbc_int_addr: ReadWrite<u32, VALUE32::Register>),
        (0x21C => pub bbc_data_cnt: ReadWrite<u32, VALUE32::Register>),
        (0x220 => pub bbc_command: ReadWrite<u32, VALUE32::Register>),
        (0x224 => pub bbc_busy: ReadOnly<u32, MJPEG_BBC_BUSY::Register>),
        (0x228 => pub bbc_ctrl: ReadWrite<u32, VALUE32::Register>),
        (0x22C => pub bbc_cur_pos: ReadWrite<u32, VALUE32::Register>),
        (0x230 => pub bbc_bas_addr: ReadWrite<u32, VALUE32::Register>),
        (0x234 => pub bbc_strm_ctrl: ReadWrite<u32, MJPEG_BBC_STRM_CTRL::Register>),
        (0x238 => @END),
    }
}

/// Returns a register view over a caller-mapped JPU MMIO base.
#[inline]
pub fn jpu_regs_at(jpu_base: usize) -> &'static JpuRegisters {
    // SAFETY: JpuDecoder's constructor requires a valid mapped MMIO base.
    unsafe { &*(jpu_base as *const JpuRegisters) }
}

#[inline]
fn mmio_read32(addr: usize) -> u32 {
    // SAFETY: the address is derived from a caller-validated MMIO base.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

#[inline]
fn mmio_write32(addr: usize, value: u32) {
    // SAFETY: the address is derived from a caller-validated MMIO base.
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
}

#[inline]
fn mmio_modify32(addr: usize, update: impl FnOnce(u32) -> u32) {
    let value = mmio_read32(addr);
    mmio_write32(addr, update(value));
}

/// Enables the JPEG clock, reset, DDR remap, and VC block, then resets the JPU.
pub fn hardware_init_at(
    jpu_base: usize,
    top_base: usize,
    vc_base: usize,
) -> Result<(), &'static str> {
    mmio_modify32(top_base + TOP_CLK_JPEG_OFF, |v| v | TOP_CLK_JPEG_ENABLE);
    mmio_modify32(top_base + TOP_RST_JPEG_OFF, |v| {
        v | TOP_RST_JPEG_RELEASE_BIT
    });
    mmio_modify32(top_base + TOP_DDR_ADDR_MODE_OFF, |v| {
        v | TOP_DDR_VD_REMAP_BIT
    });
    mmio_modify32(vc_base, |v| v | VC_BLOCK_ENABLE);
    let _ = mmio_read32(vc_base);

    let regs = jpu_regs_at(jpu_base);
    let _ = regs.pic_status.get();
    regs.bbc_bas_addr
        .write(VALUE32::VAL.val(JPU_WARMUP_BBC_BASE));
    let _ = regs.bbc_bas_addr.get();

    wait_sw_reset_done_at(jpu_base)
}

#[inline]
pub fn clear_pic_status_at(jpu_base: usize, status: u32) {
    jpu_regs_at(jpu_base).pic_status.set(status);
}

pub fn wait_sw_reset_done_at(jpu_base: usize) -> Result<(), &'static str> {
    let regs = jpu_regs_at(jpu_base);
    regs.pic_start.write(MJPEG_PIC_START::START_INIT::SET);
    if poll_until(REGISTER_WAIT_POLLS, || {
        !regs.pic_start.is_set(MJPEG_PIC_START::START_INIT)
    }) {
        Ok(())
    } else {
        Err("JPU software reset timed out")
    }
}

/// Encode one isotropic scale mode for `MJPEG_SCL_INFO`.
pub(crate) fn scl_info_value(
    scale: super::layout::JpuScale,
) -> FieldValue<u32, MJPEG_SCL_INFO::Register> {
    match scale {
        super::layout::JpuScale::Full => {
            MJPEG_SCL_INFO::ENABLE::CLEAR
                + MJPEG_SCL_INFO::HORIZONTAL_MODE::Full
                + MJPEG_SCL_INFO::VERTICAL_MODE::Full
        }
        super::layout::JpuScale::Half => {
            MJPEG_SCL_INFO::ENABLE::SET
                + MJPEG_SCL_INFO::HORIZONTAL_MODE::Half
                + MJPEG_SCL_INFO::VERTICAL_MODE::Half
        }
        super::layout::JpuScale::Quarter => {
            MJPEG_SCL_INFO::ENABLE::SET
                + MJPEG_SCL_INFO::HORIZONTAL_MODE::Quarter
                + MJPEG_SCL_INFO::VERTICAL_MODE::Quarter
        }
        super::layout::JpuScale::Eighth => {
            MJPEG_SCL_INFO::ENABLE::SET
                + MJPEG_SCL_INFO::HORIZONTAL_MODE::Eighth
                + MJPEG_SCL_INFO::VERTICAL_MODE::Eighth
        }
    }
}

pub fn wait_bbc_idle_at(jpu_base: usize) -> Result<(), &'static str> {
    let regs = jpu_regs_at(jpu_base);
    if poll_until(REGISTER_WAIT_POLLS, || {
        !regs.bbc_busy.is_set(MJPEG_BBC_BUSY::BUSY)
    }) {
        Ok(())
    } else {
        Err("JPU BBC did not become idle")
    }
}

fn poll_until(max_polls: usize, mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..max_polls {
        if ready() {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

#[inline]
pub fn pic_ctrl_value(dc_idx: u32, ac_idx: u32) -> u32 {
    (MJPEG_PIC_CTRL::HUFF_AC_IDX.val(ac_idx)
        + MJPEG_PIC_CTRL::HUFF_DC_IDX.val(dc_idx)
        + MJPEG_PIC_CTRL::USER_HUFF_TAB::SET)
        .into()
}

#[inline]
pub fn bbc_strm_ctrl_value(pages: u32) -> u32 {
    (MJPEG_BBC_STRM_CTRL::END_FLAG::SET + MJPEG_BBC_STRM_CTRL::PAGES.val(pages)).into()
}

pub const HUFF_PHASE_MIN: u32 = 0x003;
pub const HUFF_PHASE_MAX: u32 = 0x403;
pub const HUFF_PHASE_PTR: u32 = 0x803;
pub const HUFF_PHASE_VAL: u32 = 0xC03;
pub const HUFF_ADDR_MAX: u32 = 0x440;
pub const HUFF_ADDR_PTR: u32 = 0x880;

pub const QMAT_PHASE_Y: u32 = 0x03;
pub const QMAT_PHASE_CB: u32 = 0x43;
pub const QMAT_PHASE_CR: u32 = 0x83;

#[cfg(test)]
mod tests {
    use super::{poll_until, scl_info_value};
    use crate::JpuScale;

    #[test]
    fn bounded_register_poll_distinguishes_ready_and_timeout() {
        let mut attempts = 0;
        assert!(poll_until(3, || {
            attempts += 1;
            attempts == 3
        }));
        assert!(!poll_until(3, || false));
    }

    #[test]
    fn scale_info_encoding_matches_the_documented_hardware_modes() {
        let cases = [
            (JpuScale::Full, 0x00),
            (JpuScale::Half, 0x15),
            (JpuScale::Quarter, 0x1a),
            (JpuScale::Eighth, 0x1f),
        ];

        for (scale, expected) in cases {
            assert_eq!(u32::from(scl_info_value(scale)), expected);
        }
    }
}
