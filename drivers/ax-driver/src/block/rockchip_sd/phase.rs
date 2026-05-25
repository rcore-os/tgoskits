use core::ptr::NonNull;

use log::{info, warn};
use rdrive::{probe::OnProbeError, register::FdtInfo};
use sdmmc_protocol::{DataCommandPoll, Error};

use super::{
    BLOCK_SIZE, RK3588_CRU_BASE, RK3588_CRU_SIZE, RK3588_SDMMC_CON0, RK3588_SDMMC_CON1,
    RK3588_SDMMC_DRV_PHASE_DEG, RK3588_SDMMC_PHASE_SHIFT, RK3588_SDMMC_SAMPLE_PHASE_CANDIDATES,
    RK3588_SDMMC_SAMPLE_PHASE_DEG, RockchipDwMmc,
};
use crate::mmio::iomap;

pub(super) fn init_rk3588_sdmmc_phase(
    info: &FdtInfo<'_>,
    parent_rate: u32,
) -> Result<(), OnProbeError> {
    let has_drive_clk = info.find_clk_by_name("ciu-drive").is_some();
    let has_sample_clk = info.find_clk_by_name("ciu-sample").is_some();
    if !has_drive_clk || !has_sample_clk {
        warn!(
            "[{}] RK3588 SDMMC phase clocks missing: ciu-drive={} ciu-sample={}",
            info.node.name(),
            has_drive_clk,
            has_sample_clk
        );
        return Ok(());
    }

    let cru = iomap(RK3588_CRU_BASE, RK3588_CRU_SIZE)?;
    set_rk3588_mmc_phase(
        cru,
        RK3588_SDMMC_CON0,
        parent_rate,
        RK3588_SDMMC_DRV_PHASE_DEG,
    );
    set_rk3588_mmc_phase(
        cru,
        RK3588_SDMMC_CON1,
        parent_rate,
        RK3588_SDMMC_SAMPLE_PHASE_DEG,
    );
    info!(
        "rockchip-dwmmc: RK3588 SDMMC phase configured: drive={}deg sample={}deg parent={}Hz",
        RK3588_SDMMC_DRV_PHASE_DEG, RK3588_SDMMC_SAMPLE_PHASE_DEG, parent_rate
    );
    Ok(())
}

pub(super) fn tune_rk3588_sdmmc_sample_phase(sd: &mut RockchipDwMmc, parent_rate: u32) {
    let Ok(cru) = iomap(RK3588_CRU_BASE, RK3588_CRU_SIZE) else {
        warn!("rockchip-dwmmc: failed to map RK3588 CRU for sample phase scan");
        return;
    };

    for sample_phase in RK3588_SDMMC_SAMPLE_PHASE_CANDIDATES {
        set_rk3588_mmc_phase(cru, RK3588_SDMMC_CON1, parent_rate, sample_phase);

        let mut block0 = [0; BLOCK_SIZE];
        let block0_result = read_block_sync(sd, 0, &mut block0);
        let block0_valid = block0_result.is_ok() && has_mbr_signature(&block0);

        let mut block1 = [0; BLOCK_SIZE];
        let block1_result = read_block_sync(sd, 1, &mut block1);
        let block1_valid = block1_result.is_ok() && has_gpt_header(&block1);

        info!(
            "rockchip-dwmmc: sample phase probe {}deg: block0_ok={} mbr_sig={:02x}{:02x} \
             block0_head={:02x?} block1_ok={} gpt_head={:02x?}",
            sample_phase,
            block0_result.is_ok(),
            block0[511],
            block0[510],
            &block0[..16],
            block1_result.is_ok(),
            &block1[..8]
        );

        if block0_valid || block1_valid {
            set_rk3588_mmc_phase(cru, RK3588_SDMMC_CON1, parent_rate, sample_phase);
            info!(
                "rockchip-dwmmc: selected RK3588 SDMMC sample phase {}deg",
                sample_phase
            );
            return;
        }
    }

    set_rk3588_mmc_phase(
        cru,
        RK3588_SDMMC_CON1,
        parent_rate,
        RK3588_SDMMC_SAMPLE_PHASE_DEG,
    );
    warn!(
        "rockchip-dwmmc: no valid RK3588 SDMMC sample phase found; restored {}deg",
        RK3588_SDMMC_SAMPLE_PHASE_DEG
    );
}

fn read_block_sync(
    sd: &mut RockchipDwMmc,
    addr: u32,
    buf: &mut [u8; BLOCK_SIZE],
) -> Result<(), Error> {
    let mut request = sd.submit_read_blocks_into(addr, buf)?;
    loop {
        match sd.poll_data_request(&mut request)? {
            DataCommandPoll::Pending => core::hint::spin_loop(),
            DataCommandPoll::Complete(_) => return Ok(()),
            _ => core::hint::spin_loop(),
        }
    }
}

fn set_rk3588_mmc_phase(cru: NonNull<u8>, offset: usize, parent_rate: u32, degrees: u32) {
    let delay_num = rk3588_mmc_delay_num(parent_rate, degrees);
    let raw_value = if delay_num != 0 { 1 << 10 } else { 0 }
        | ((delay_num & 0xff) << 2)
        | ((degrees / 90) & 0x03);
    let reg_value =
        ((0x07ff_u32 << RK3588_SDMMC_PHASE_SHIFT) << 16) | (raw_value << RK3588_SDMMC_PHASE_SHIFT);
    unsafe {
        (cru.as_ptr().add(offset) as *mut u32).write_volatile(reg_value);
    }
}

fn rk3588_mmc_delay_num(parent_rate: u32, degrees: u32) -> u32 {
    let degree = degrees % 360;
    let remainder = degree % 90;
    if parent_rate == 0 {
        0
    } else {
        div_round_closest(
            10_000_000_u64 * remainder as u64,
            (parent_rate as u64 / 1_000) * 36 * 6,
        )
        .min(255) as u32
    }
}

fn div_round_closest(numerator: u64, denominator: u64) -> u64 {
    numerator
        .saturating_add(denominator / 2)
        .checked_div(denominator)
        .unwrap_or(0)
}

fn has_mbr_signature(block: &[u8; BLOCK_SIZE]) -> bool {
    block[510] == 0x55 && block[511] == 0xaa
}

fn has_gpt_header(block: &[u8; BLOCK_SIZE]) -> bool {
    &block[..8] == b"EFI PART"
}
