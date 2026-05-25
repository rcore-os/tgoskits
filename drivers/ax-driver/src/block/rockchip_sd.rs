// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::{format, sync::Arc};
use core::time::Duration;

use ax_kspin::SpinNoIrq;
use dwmmc_host::DwMmc;
use log::{info, warn};
use rdif_clk::ClockId;
use rdrive::{PlatformDevice, probe::OnProbeError, register::FdtInfo};
use sdmmc_protocol::{
    Error, OperationPoll,
    error::Phase,
    sdio::{CardInfo, SdioInitScratch, SdioSdmmc},
};

use crate::{
    block::{PlatformDeviceBlock, decode_fdt_irq},
    mmio::iomap,
    soc::scmi,
};

const BLOCK_SIZE: usize = 512;
const DWMMC_STABLE_REFERENCE_CLOCK: u32 = 50_000_000;
const ENABLE_SD_SPEED_SELECTION: bool = true;
const RK3588_CRU_BASE: usize = 0xfd7c_0000;
const RK3588_CRU_SIZE: usize = 0x5c000;
const RK3588_SDMMC_CON0: usize = 0x0c30;
const RK3588_SDMMC_CON1: usize = 0x0c34;
const RK3588_SDMMC_PHASE_SHIFT: u32 = 1;
const RK3588_SDMMC_DRV_PHASE_DEG: u32 = 90;
const RK3588_SDMMC_SAMPLE_PHASE_DEG: u32 = 0;
const RK3588_SDMMC_SAMPLE_PHASE_CANDIDATES: [u32; 8] = [0, 45, 90, 135, 180, 225, 270, 315];

type RockchipDwMmc = SdioSdmmc<DwMmc>;

mod block;
mod phase;

use block::SdBlockDevice;
use phase::{init_rk3588_sdmmc_phase, tune_rk3588_sdmmc_sample_phase};

crate::model_register!(
    name: "Rockchip SD",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-dw-mshc", "rockchip,rk3288-dw-mshc"],
            on_probe: probe
        }
    ],
);

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let base_reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    info!(
        "rockchip-dwmmc probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;

    let mut host = unsafe { DwMmc::new(mmio_base) };
    let reference_clock = dwmmc_reference_clock(&info);
    if let Some(reference_clock) = reference_clock {
        info!(
            "rockchip-dwmmc: using ciu reference clock {} Hz",
            reference_clock
        );
        host.set_reference_clock(reference_clock);
        if is_rk3588_dwmmc(&info) {
            init_rk3588_sdmmc_phase(&info, reference_clock)?;
        }
    } else {
        warn!(
            "rockchip-dwmmc: ciu clock not found; leaving DWMMC divider bypassed and relying on \
             CRU rate"
        );
    }
    info!("rockchip-dwmmc: reset controller");
    host.reset_and_init()
        .map_err(|e| init_error(base_reg.address, mmio_size, e))?;
    host.set_dma(axklib::dma::device_with_mask(u32::MAX as u64));

    info!("rockchip-dwmmc: initialize card");
    let mut sd = SdioSdmmc::new(host);
    sd.set_sd_speed_selection_enabled(ENABLE_SD_SPEED_SELECTION);
    let card_info = poll_card_init(&mut sd).map_err(|e| {
        warn!("rockchip-dwmmc: card init failed: {:?}", e);
        card_init_error(base_reg.address, mmio_size, e)
    })?;
    info!(
        "rockchip-dwmmc card: kind={:?} high_capacity={} rca={} ocr={:#010x} capacity_blocks={:?} \
         cid={} ext_csd={}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
        card_info.cid.is_some(),
        card_info.ext_csd.is_some()
    );

    if let Some(reference_clock) = reference_clock
        && is_rk3588_dwmmc(&info)
    {
        tune_rk3588_sdmmc_sample_phase(&mut sd, reference_clock);
    }

    let irq_num = decode_fdt_irq(&info.interrupts());
    let raw = Arc::new(SpinNoIrq::new(sd));
    let dev = SdBlockDevice {
        raw: Some(raw.clone()),
        capacity_blocks: card_info.capacity_blocks.unwrap_or(0),
        irq_enabled: false,
        queue_created: false,
    };
    plat_dev.register_block_with_irq(dev, irq_num);
    info!("rockchip-sd block device registered irq={:?}", irq_num);
    Ok(())
}

fn poll_card_init(sd: &mut RockchipDwMmc) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request = sd.submit_init(&mut scratch)?;
    loop {
        match sd.poll_init_request(&mut request)? {
            OperationPoll::Pending => {
                if request.take_needs_pace() {
                    axklib::time::busy_wait(Duration::from_millis(10));
                } else {
                    core::hint::spin_loop();
                }
            }
            OperationPoll::Complete(info) => return Ok(info),
            _ => return Err(Error::UnsupportedCommand),
        }
    }
}

fn init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    OnProbeError::other(format!(
        "failed to initialize DWMMC device at [PA:{:?}, SZ:0x{:x}): {err:?}",
        address, size
    ))
}

fn card_init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "rockchip-dwmmc: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping controller: \
             {err:?}",
            address, size
        );
        return OnProbeError::NotMatch;
    }

    init_error(address, size, err)
}

fn is_absent_card_init_error(err: Error) -> bool {
    match err {
        Error::NoCard => true,
        Error::Timeout(ctx) | Error::Crc(ctx) | Error::BadResponse(ctx) => {
            ctx.cmd.is_some()
                && matches!(
                    ctx.phase,
                    Phase::CommandSend | Phase::ResponseWait | Phase::Init
                )
        }
        _ => false,
    }
}

fn dwmmc_reference_clock(info: &FdtInfo<'_>) -> Option<u32> {
    let clk = info.find_clk_by_name("ciu")?;
    let Some(device_id) = info.phandle_to_device_id(clk.phandle) else {
        warn!(
            "[{}] ciu clock phandle {} has no device id",
            info.node.name(),
            clk.phandle
        );
        return None;
    };
    let clk_dev = match rdrive::get::<rdif_clk::Clk>(device_id) {
        Ok(clk_dev) => clk_dev,
        Err(_) => {
            let clock_id = clk.select().unwrap_or(0);
            if scmi::set_clock_rate(clk.phandle, clock_id, DWMMC_STABLE_REFERENCE_CLOCK as u64)
                .is_some()
            {
                return Some(DWMMC_STABLE_REFERENCE_CLOCK);
            }
            if let Some(rate) = scmi::clock_rate(clk.phandle, clock_id) {
                return validate_reference_clock(info, rate);
            }
            warn!(
                "[{}] ciu clock device {:?} is not registered",
                info.node.name(),
                device_id
            );
            return None;
        }
    };
    let mut clk_guard = match clk_dev.lock() {
        Ok(clk_guard) => clk_guard,
        Err(_) => {
            warn!(
                "[{}] ciu clock device {:?} is locked",
                info.node.name(),
                device_id
            );
            return None;
        }
    };
    let clock_id = ClockId::from(clk.select().unwrap_or(0) as usize);
    if let Err(err) = clk_guard.set_rate(clock_id, DWMMC_STABLE_REFERENCE_CLOCK as u64) {
        warn!(
            "[{}] failed to set ciu clock {:?} to {} Hz: {:?}",
            info.node.name(),
            clock_id,
            DWMMC_STABLE_REFERENCE_CLOCK,
            err
        );
    }
    let rate = match clk_guard.get_rate(clock_id) {
        Ok(rate) => rate,
        Err(err) => {
            warn!(
                "[{}] failed to read ciu clock {:?}: {:?}",
                info.node.name(),
                clock_id,
                err
            );
            return None;
        }
    };
    validate_reference_clock(info, rate)
}

fn is_rk3588_dwmmc(info: &FdtInfo<'_>) -> bool {
    info.node
        .as_node()
        .compatibles()
        .any(|compatible| compatible == "rockchip,rk3588-dw-mshc")
}

fn validate_reference_clock(info: &FdtInfo<'_>, rate: u64) -> Option<u32> {
    if rate == 0 || rate > u32::MAX as u64 {
        warn!("[{}] invalid ciu clock rate {} Hz", info.node.name(), rate);
        return None;
    }
    Some(rate as u32)
}

#[cfg(test)]
mod tests {
    use sdmmc_protocol::error::ErrorContext;

    use super::*;

    #[test]
    fn command_timeout_during_card_init_is_absent_card() {
        let err = Error::Timeout(ErrorContext::for_cmd(Phase::ResponseWait, 1));

        assert!(is_absent_card_init_error(err));
    }

    #[test]
    fn data_timeout_after_card_init_is_not_absent_card() {
        let err = Error::Timeout(ErrorContext::for_cmd(Phase::DataRead, 17));

        assert!(!is_absent_card_init_error(err));
    }
}
