use core::ptr::NonNull;

use log::{info, warn};
use rdrive::{probe::OnProbeError, register::FdtInfo};

use super::{
    RK3588_CRU_BASE, RK3588_CRU_SIZE, RK3588_SDMMC_CON0, RK3588_SDMMC_CON1,
    RK3588_SDMMC_DRV_PHASE_DEG, RK3588_SDMMC_PHASE_SHIFT, RK3588_SDMMC_SAMPLE_PHASE_DEG,
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
