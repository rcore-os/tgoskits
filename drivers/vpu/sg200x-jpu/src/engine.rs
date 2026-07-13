//! Low-level JPU register programming and checked DMA address derivation.

use tock_registers::interfaces::{Readable, Writeable};

use super::{
    header::{HuffTable, JpegHeaderInfo},
    layout::{FrameLayout, JpuPixelFormat},
    regs::{
        HUFF_ADDR_MAX, HUFF_ADDR_PTR, HUFF_PHASE_MAX, HUFF_PHASE_MIN, HUFF_PHASE_PTR,
        HUFF_PHASE_VAL, MJPEG_HUFF_CTRL, MJPEG_PIC_SIZE, MJPEG_PIC_START, MJPEG_PIC_STATUS,
        MJPEG_QMAT_CTRL, QMAT_PHASE_CB, QMAT_PHASE_CR, QMAT_PHASE_Y, VALUE32, bbc_strm_ctrl_value,
        clear_pic_status_at, jpu_regs_at, pic_ctrl_value, scl_info_value, wait_bbc_idle_at,
    },
};

pub(super) const BBC_STREAM_PAGE_SIZE: usize = 256;
pub(super) const GRAM_PREFETCH_PAGES: usize = 2;

#[derive(Clone, Copy)]
pub(super) struct HardwareDecodeInfo {
    mcu_block_num: u32,
    comp_info: u32,
    bus_req_num: u32,
}

impl HardwareDecodeInfo {
    pub(super) const fn for_format(format: JpuPixelFormat) -> Self {
        match format {
            JpuPixelFormat::Yuv420 => Self {
                mcu_block_num: 6,
                comp_info: (10 << 8) | (5 << 4) | 5,
                bus_req_num: 2,
            },
            JpuPixelFormat::Yuv422Horizontal => Self {
                mcu_block_num: 4,
                comp_info: (9 << 8) | (5 << 4) | 5,
                bus_req_num: 3,
            },
            JpuPixelFormat::Yuv422Vertical => Self {
                mcu_block_num: 4,
                comp_info: (6 << 8) | (5 << 4) | 5,
                bus_req_num: 3,
            },
            JpuPixelFormat::Yuv444 => Self {
                mcu_block_num: 3,
                comp_info: (5 << 8) | (5 << 4) | 5,
                bus_req_num: 4,
            },
            JpuPixelFormat::Grayscale => Self {
                mcu_block_num: 1,
                comp_info: 5 << 8,
                bus_req_num: 4,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DmaRegion {
    pub(super) start: u32,
    pub(super) end: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FrameDmaAddresses {
    pub(super) y: u32,
    pub(super) cb: u32,
    pub(super) cr: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PollError {
    Decode,
    Timeout,
}

pub(super) fn checked_dma_region(
    dma_start: u64,
    capacity: usize,
    used_len: usize,
) -> Result<DmaRegion, &'static str> {
    if used_len == 0 || used_len > capacity {
        return Err("invalid JPU DMA region length");
    }
    let mapped_end = dma_start
        .checked_add(used_len as u64)
        .ok_or("JPU DMA address range overflow")?;
    Ok(DmaRegion {
        start: u32::try_from(dma_start).map_err(|_| "JPU DMA start does not fit u32")?,
        end: u32::try_from(mapped_end).map_err(|_| "JPU DMA end does not fit u32")?,
    })
}

pub(super) fn checked_dma_offset(
    region: DmaRegion,
    offset: usize,
    allow_end: bool,
) -> Result<u32, &'static str> {
    let offset = u32::try_from(offset).map_err(|_| "JPU DMA offset does not fit u32")?;
    let address = region
        .start
        .checked_add(offset)
        .ok_or("JPU DMA offset address overflow")?;
    if address > region.end || (!allow_end && address == region.end) {
        return Err("JPU DMA offset is outside its region");
    }
    Ok(address)
}

pub(super) fn checked_frame_dma_addresses(
    region: DmaRegion,
    layout: &FrameLayout,
) -> Result<FrameDmaAddresses, &'static str> {
    let plane_address = |offset: usize| -> Result<u32, &'static str> {
        let address = (region.start as usize)
            .checked_add(offset)
            .ok_or("JPU frame plane address overflow")?;
        let address = u32::try_from(address).map_err(|_| "JPU frame plane does not fit u32")?;
        if address >= region.end {
            return Err("JPU frame plane starts outside DMA region");
        }
        Ok(address)
    };

    let y = plane_address(layout.y.offset)?;
    let cb = match layout.cb {
        Some(plane) => plane_address(plane.offset)?,
        None => y,
    };
    let cr = match layout.cr {
        Some(plane) => plane_address(plane.offset)?,
        None => y,
    };
    Ok(FrameDmaAddresses { y, cb, cr })
}

pub(super) fn configure_stream_regs(
    jpu_base: usize,
    stream_dma: DmaRegion,
    stream_data_end: u32,
    copy_len: usize,
    header: &JpegHeaderInfo,
    layout: &FrameLayout,
    hardware: HardwareDecodeInfo,
) {
    let r = jpu_regs_at(jpu_base);
    let stream_phys = stream_dma.start;
    let stream_end = stream_dma.end;

    r.bbc_bas_addr.write(VALUE32::VAL.val(stream_phys));
    r.bbc_end_addr.write(VALUE32::VAL.val(stream_end));
    r.bbc_rd_ptr.write(VALUE32::VAL.val(stream_phys));
    r.bbc_wr_ptr.write(VALUE32::VAL.val(stream_data_end));

    let strm_pages = copy_len.div_ceil(BBC_STREAM_PAGE_SIZE);
    r.bbc_strm_ctrl.set(bbc_strm_ctrl_value(strm_pages as u32));

    r.gbu_tt_cnt.write(VALUE32::VAL.val(0));
    r.gbu_tt_cnt_h.write(VALUE32::VAL.val(0));
    r.pic_errmb.write(VALUE32::VAL.val(0));

    let mut huff_dc_idx = 0u32;
    let mut huff_ac_idx = 0u32;
    for i in 0..3 {
        huff_dc_idx = (huff_dc_idx << 1) | header.dc_huff_tbl[i] as u32;
        huff_ac_idx = (huff_ac_idx << 1) | header.ac_huff_tbl[i] as u32;
    }
    r.pic_ctrl.set(pic_ctrl_value(huff_dc_idx, huff_ac_idx));

    r.pic_size.write(
        MJPEG_PIC_SIZE::WIDTH.val(layout.source_aligned.width)
            + MJPEG_PIC_SIZE::HEIGHT.val(layout.source_aligned.height),
    );
    r.rot_info.write(VALUE32::VAL.val(0));
    r.mcu_info.write(
        VALUE32::VAL.val(
            (hardware.mcu_block_num << 16) | (header.num_components << 12) | hardware.comp_info,
        ),
    );
    r.dpb_config.write(VALUE32::VAL.val(0));
    r.rst_intval
        .write(VALUE32::VAL.val(header.restart_interval));
    r.scl_info.write(scl_info_value(layout.scale));
    r.op_info.write(VALUE32::VAL.val(hardware.bus_req_num));
}

pub(super) fn upload_huff_tables(
    jpu_base: usize,
    header: &JpegHeaderInfo,
) -> Result<(), &'static str> {
    let r = jpu_regs_at(jpu_base);

    r.huff_ctrl
        .write(MJPEG_HUFF_CTRL::PHASE.val(HUFF_PHASE_MIN));
    for table_idx in [0, 2, 1, 3] {
        for j in 0..16 {
            let huff_data = header.huff_tables[table_idx].min_codes[j];
            let temp = HuffTable::sign_extend_16(huff_data);
            r.huff_data
                .write(VALUE32::VAL.val(((temp & 0xFFFF) << 16) | huff_data));
        }
    }

    r.huff_ctrl
        .write(MJPEG_HUFF_CTRL::PHASE.val(HUFF_PHASE_MAX));
    r.huff_addr.write(VALUE32::VAL.val(HUFF_ADDR_MAX));
    for table_idx in [0, 2, 1, 3] {
        for j in 0..16 {
            let huff_data = header.huff_tables[table_idx].max_codes[j];
            let temp = HuffTable::sign_extend_16(huff_data);
            r.huff_data
                .write(VALUE32::VAL.val(((temp & 0xFFFF) << 16) | huff_data));
        }
    }

    r.huff_ctrl
        .write(MJPEG_HUFF_CTRL::PHASE.val(HUFF_PHASE_PTR));
    r.huff_addr.write(VALUE32::VAL.val(HUFF_ADDR_PTR));
    for table_idx in [0, 2, 1, 3] {
        for j in 0..16 {
            let huff_data = header.huff_tables[table_idx].ptrs[j] as u32;
            let temp = HuffTable::sign_extend_8(huff_data);
            r.huff_data
                .write(VALUE32::VAL.val(((temp & 0xFFFFFF) << 8) | huff_data));
        }
    }

    r.huff_ctrl
        .write(MJPEG_HUFF_CTRL::PHASE.val(HUFF_PHASE_VAL));
    for &table_idx in &[0, 2, 1, 3] {
        let is_dc = table_idx == 0 || table_idx == 2;
        let max_count = if is_dc { 12 } else { 162 };
        let bits_len = if is_dc { 12 } else { 16 };
        let count: usize = header.huff_tables[table_idx].bits[..bits_len]
            .iter()
            .map(|&b| b as usize)
            .sum();

        for j in 0..count.min(header.huff_tables[table_idx].num_values) {
            let val = header.huff_tables[table_idx].values[j] as u32;
            let temp = HuffTable::sign_extend_8(val);
            r.huff_data
                .write(VALUE32::VAL.val(((temp & 0xFFFFFF) << 8) | val));
        }
        for _ in count..max_count {
            r.huff_data.write(VALUE32::VAL.val(0xFFFF_FFFF));
        }
    }

    r.huff_ctrl.write(MJPEG_HUFF_CTRL::PHASE.val(0));
    Ok(())
}

pub(super) fn upload_quant_tables(
    jpu_base: usize,
    header: &JpegHeaderInfo,
) -> Result<(), &'static str> {
    let r = jpu_regs_at(jpu_base);
    let qmat_phases = [QMAT_PHASE_Y, QMAT_PHASE_CB, QMAT_PHASE_CR];
    let comp_count = (header.num_components as usize).min(3);
    for (comp_idx, &phase) in qmat_phases.iter().enumerate().take(comp_count) {
        let table_idx = header.quant_tbl[comp_idx];
        if table_idx >= 4 || table_idx >= header.quant_table_count {
            continue;
        }

        r.qmat_ctrl.write(MJPEG_QMAT_CTRL::PHASE.val(phase));
        for j in 0..64 {
            r.qmat_data
                .write(VALUE32::VAL.val(header.quant_tables[table_idx].values[j] as u32));
        }
        r.qmat_ctrl.write(MJPEG_QMAT_CTRL::PHASE.val(0));
    }
    Ok(())
}

pub(super) fn gram_setup(
    jpu_base: usize,
    stream_dma: DmaRegion,
    header: &JpegHeaderInfo,
) -> Result<(), &'static str> {
    let r = jpu_regs_at(jpu_base);
    let ecs_offset = header.ecs_offset;
    let page_ptr = ecs_offset >> 8;
    let mut word_ptr = (ecs_offset & 0xF0) >> 2;
    let bit_ptr = (ecs_offset & 0xF) << 3;

    if page_ptr & 1 != 0 {
        word_ptr += 64;
    }
    if word_ptr & 1 != 0 {
        word_ptr -= 1;
    }

    for i in 0..GRAM_PREFETCH_PAGES {
        let cur_page = page_ptr
            .checked_add(i)
            .ok_or("JPU GRAM page index overflow")?;
        let page_offset = cur_page
            .checked_mul(BBC_STREAM_PAGE_SIZE)
            .ok_or("JPU GRAM page offset overflow")?;
        let external_address = checked_dma_offset(stream_dma, page_offset, false)?;
        r.bbc_cur_pos.write(
            VALUE32::VAL
                .val(u32::try_from(cur_page).map_err(|_| "JPU GRAM page index does not fit u32")?),
        );
        r.bbc_ext_addr.write(VALUE32::VAL.val(external_address));
        r.bbc_int_addr
            .write(VALUE32::VAL.val(((cur_page & 1) as u32) << 6));
        r.bbc_data_cnt
            .write(VALUE32::VAL.val(BBC_STREAM_PAGE_SIZE as u32 / 4));
        r.bbc_command.write(VALUE32::VAL.val(0));
        wait_bbc_idle_at(jpu_base)?;
    }

    let next_page = page_ptr
        .checked_add(GRAM_PREFETCH_PAGES)
        .ok_or("JPU GRAM next page index overflow")?;
    r.bbc_cur_pos.write(
        VALUE32::VAL
            .val(u32::try_from(next_page).map_err(|_| "JPU GRAM next page does not fit u32")?),
    );
    r.bbc_ctrl.write(VALUE32::VAL.val(1));

    r.gbu_wd_ptr.write(VALUE32::VAL.val(word_ptr as u32));
    r.gbu_bbsr.write(VALUE32::VAL.val(0));
    r.gbu_bber.write(
        VALUE32::VAL.val(((BBC_STREAM_PAGE_SIZE as u32 / 4) * GRAM_PREFETCH_PAGES as u32) - 1),
    );

    if page_ptr & 1 != 0 {
        r.gbu_bbir.write(VALUE32::VAL.val(0));
        r.gbu_bbhr.write(VALUE32::VAL.val(0));
    } else {
        r.gbu_bbir
            .write(VALUE32::VAL.val(BBC_STREAM_PAGE_SIZE as u32 / 4));
        r.gbu_bbhr
            .write(VALUE32::VAL.val(BBC_STREAM_PAGE_SIZE as u32 / 4));
    }

    r.gbu_ctrl.write(VALUE32::VAL.val(4));
    r.gbu_ff_rptr.write(VALUE32::VAL.val(bit_ptr as u32));
    Ok(())
}

pub(super) fn start_decode(
    jpu_base: usize,
    frame_dma: FrameDmaAddresses,
    header: &JpegHeaderInfo,
    layout: &FrameLayout,
) {
    let r = jpu_regs_at(jpu_base);
    r.rst_index.write(VALUE32::VAL.val(0));
    r.rst_count.write(VALUE32::VAL.val(0));
    r.dpcm_diff_y.write(VALUE32::VAL.val(0));
    r.dpcm_diff_cb.write(VALUE32::VAL.val(0));
    r.dpcm_diff_cr.write(VALUE32::VAL.val(0));

    let bit_ptr = (header.ecs_offset & 0xF) << 3;
    r.gbu_ff_rptr.write(VALUE32::VAL.val(bit_ptr as u32));
    r.gbu_ctrl.write(VALUE32::VAL.val(3));

    r.dpb_base_y.write(VALUE32::VAL.val(frame_dma.y));
    r.dpb_base_cb.write(VALUE32::VAL.val(frame_dma.cb));
    r.dpb_base_cr.write(VALUE32::VAL.val(frame_dma.cr));

    r.dpb_ystride.write(VALUE32::VAL.val(layout.y.stride));
    let chroma_stride = layout.cb.map_or(0, |plane| plane.stride);
    r.dpb_cstride.write(VALUE32::VAL.val(chroma_stride));
    r.clp_info.write(VALUE32::VAL.val(0));

    clear_pic_status_at(jpu_base, r.pic_status.get());
    r.pic_start.write(MJPEG_PIC_START::START_PIC::SET);
}

pub(super) fn poll_decode_done(jpu_base: usize) -> Result<(), PollError> {
    let mut count = 0u32;
    const MAX_POLLS: u32 = 500_000;
    let r = jpu_regs_at(jpu_base);

    loop {
        if r.pic_status.is_set(MJPEG_PIC_STATUS::DONE) {
            clear_pic_status_at(jpu_base, r.pic_status.get());
            return Ok(());
        }

        if r.pic_status.is_set(MJPEG_PIC_STATUS::ERROR) {
            let status = r.pic_status.get();
            let err_mb = r.pic_errmb.get();
            log::warn!("[JPU] Error! status=0x{:x}, err_mb=0x{:x}", status, err_mb);
            clear_pic_status_at(jpu_base, status);
            return Err(PollError::Decode);
        }

        for _ in 0..1000 {
            core::hint::spin_loop();
        }
        count += 1;

        if count >= MAX_POLLS {
            let status = r.pic_status.get();
            log::warn!("[JPU] Timeout! status=0x{:x}, polls={}", status, count);
            return Err(PollError::Timeout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DmaRegion, checked_dma_offset, checked_dma_region, checked_frame_dma_addresses};
    use crate::{FrameLayout, JpuPixelFormat, JpuScale};

    #[test]
    fn dma_region_checks_capacity_and_32_bit_register_range() {
        assert_eq!(
            checked_dma_region(u32::MAX as u64 - 8, 16, 8),
            Ok(DmaRegion {
                start: u32::MAX - 8,
                end: u32::MAX,
            })
        );
        assert!(checked_dma_region(u32::MAX as u64 - 8, 16, 9).is_err());
        assert!(checked_dma_region(0x1000, 16, 17).is_err());
        assert!(checked_dma_region(u32::MAX as u64 + 1, 16, 1).is_err());
    }

    #[test]
    fn dma_offsets_reject_addresses_outside_the_registered_region() {
        let region = DmaRegion {
            start: 0x1000,
            end: 0x1100,
        };
        assert_eq!(checked_dma_offset(region, 0x100, true), Ok(0x1100));
        assert!(checked_dma_offset(region, 0x100, false).is_err());
        assert!(checked_dma_offset(region, 0x101, true).is_err());
    }

    #[test]
    fn frame_addresses_include_checked_plane_offsets() {
        let layout = FrameLayout::new(129, 129, JpuPixelFormat::Yuv420, JpuScale::Eighth)
            .expect("valid layout");
        let region = DmaRegion {
            start: 0x1000,
            end: 0x1000 + layout.total_len as u32,
        };

        let addresses = checked_frame_dma_addresses(region, &layout).expect("valid addresses");
        assert_eq!(addresses.y, 0x1000);
        assert_eq!(addresses.cb, 0x1000 + 576);
        assert_eq!(addresses.cr, 0x1000 + 720);
    }
}
