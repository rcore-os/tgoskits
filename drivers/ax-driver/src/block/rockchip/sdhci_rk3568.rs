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

use core::ptr::NonNull;

use log::{info, warn};
use rdrive::{
    probe::{OnProbeError, fdt::ClockLine},
    register::ProbeFdt,
};
use sdhci_host::{HostClock, HostResetHook, ResetHookRecoveryMode, Sdhci, rdif as sdhci_rdif};
use sdmmc_protocol::{
    Error,
    error::{ErrorContext, Phase},
    rdif::v13::SdmmcControllerActivator,
    sdio::{CardInitPreference, OwnedSdioInit, SdioSdmmc},
};

use crate::{block::ProbeFdtBlockActivation, mmio::iomap};

// RK3568 DWCMSHC uses the same SDHCI completion interrupt path as RK3588:
// the hard IRQ exclusively acknowledges controller status, then bounded
// task-side service consumes only the published snapshot.

const DWCMSHC_P_VENDOR_AREA1: usize = 0xe8;
const DWCMSHC_AREA1_MASK: u16 = 0x0fff;
const DWCMSHC_HOST_CTRL3: usize = 0x08;
const DWCMSHC_EMMC_CONTROL: usize = 0x2c;
const DWCMSHC_CARD_IS_EMMC: u16 = 1 << 0;
const DWCMSHC_EMMC_DLL_CTRL: usize = 0x800;
const DWCMSHC_EMMC_DLL_RXCLK: usize = 0x804;
const DWCMSHC_EMMC_DLL_TXCLK: usize = 0x808;
const DWCMSHC_EMMC_DLL_STRBIN: usize = 0x80c;
const DWCMSHC_EMMC_DLL_CMDOUT: usize = 0x810;
const DWCMSHC_EMMC_MISC_CON: usize = 0x81c;
const DWCMSHC_EMMC_DLL_BYPASS: u32 = 1 << 24;
const DWCMSHC_EMMC_DLL_START: u32 = 1 << 0;
const DWCMSHC_EMMC_DLL_DLYENA: u32 = 1 << 27;
const DLL_RXCLK_ORI_GATE: u32 = 1 << 31;
const DLL_STRBIN_DELAY_NUM_SEL: u32 = 1 << 26;
const DLL_STRBIN_DELAY_NUM_DEFAULT: u32 = 0x16;
const DLL_STRBIN_DELAY_NUM_OFFSET: u32 = 16;
const MISC_INTCLK_EN: u32 = 1 << 1;

const DWC_MSHC_PTR_PHY_R: usize = 0x300;
const PHY_CNFG_R: usize = DWC_MSHC_PTR_PHY_R;
const PHY_CMDPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x04;
const PHY_DATAPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x06;
const PHY_CLKPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x08;
const PHY_STBPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x0a;
const PHY_RSTNPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x0c;
const PHY_SDCLKDL_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x1d;
const PHY_SDCLKDL_DC_R: usize = DWC_MSHC_PTR_PHY_R + 0x1e;
const PHY_SMPLDL_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x20;
const PHY_DLL_CTRL_R: usize = DWC_MSHC_PTR_PHY_R + 0x24;
const PHY_DLL_CNFG2_R: usize = DWC_MSHC_PTR_PHY_R + 0x26;
const PHY_CNFG_RSTN_DEASSERT: u32 = 1 << 0;
const PHY_CNFG_PAD_SP: u32 = 0x0c;
const PHY_CNFG_PAD_SN: u32 = 0x0c;
const PHY_PAD_RXSEL_3V3: u16 = 0x2;
const PHY_PAD_WEAKPULL_PULLUP: u16 = 0x1;
const PHY_PAD_WEAKPULL_PULLDOWN: u16 = 0x2;
const PHY_PAD_TXSLEW_CTRL_P: u16 = 0x3;
const PHY_PAD_TXSLEW_CTRL_N: u16 = 0x3;
const PHY_SDCLKDL_CNFG_UPDATE: u8 = 1 << 4;
const PHY_SDCLKDL_DC_DEFAULT: u8 = 0x32;
const PHY_SMPLDL_CNFG_BYPASS_EN: u8 = 1 << 1;
const PHY_DLL_CTRL_ENABLE: u8 = 0x1;
const PHY_DLL_CNFG2_JUMPSTEP: u8 = 0x0a;

struct RockchipSdhciClock {
    clock: ClockLine,
}
struct RockchipSdhciResetHook;

impl HostClock for RockchipSdhciClock {
    fn set_clock(&self, target_hz: u32) -> Result<(), Error> {
        self.clock.enable().map_err(|_| clock_error())?;
        self.clock
            .set_rate(u64::from(target_hz))
            .map_err(|_| clock_error())?;
        let rate = self.clock.rate().map_err(|_| clock_error())?;
        info!("rockchip-rk3568-sdhci: core clock set to {} Hz", rate);
        Ok(())
    }
}

impl HostResetHook for RockchipSdhciResetHook {
    fn recovery_mode(&self) -> ResetHookRecoveryMode {
        ResetHookRecoveryMode::BoundedCallbacks
    }

    fn after_reset(&self, host: &mut Sdhci) -> Result<(), Error> {
        init_dwcmshc_after_reset(host)
    }
}

crate::model_register!(
    name: "Rockchip RK3568 sdhci",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3568-dwcmshc"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
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
        "rockchip-rk3568-sdhci probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;

    let mut host = unsafe { Sdhci::new(mmio_base) };
    if let Some(clock) = info.find_clock_line_by_name("core")? {
        info!("rockchip-rk3568-sdhci: staging external CRU clock");
        host.set_external_clock(RockchipSdhciClock { clock });
    } else {
        warn!("rockchip-rk3568-sdhci: no core clock found; using SDHCI internal clock divider");
    }
    host.set_reset_hook(RockchipSdhciResetHook);
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    host.set_dma(dma.clone());

    let card = SdioSdmmc::new_host2_timed_evidence(host);
    let activator = SdmmcControllerActivator::new(
        OwnedSdioInit::new(card, CardInitPreference::MmcFirst),
        sdhci_rdif::dma_config("rockchip-rk3568-sdhci", 0, dma),
    )
    .map_err(|error| {
        OnProbeError::other(alloc::format!(
            "failed to prepare rockchip-rk3568-sdhci activation owner: {error}"
        ))
    })?;
    let slot = probe
        .register_block_activator(activator)?
        .ok_or_else(|| OnProbeError::other("failed to register RK3568 SDHCI activation owner"))?;
    info!("rockchip-rk3568-sdhci activation owner registered slot={slot}");
    Ok(())
}

fn init_dwcmshc_after_reset(host: &mut Sdhci) -> Result<(), Error> {
    let base = NonNull::new(host.mmio_base() as *mut u8).ok_or(Error::InvalidArgument)?;
    let area1 = vendor_area1(base);

    // Match Linux rk35xx reset/set_clock setup for identification speed:
    // keep the internal clock ungated, disable command-conflict checking,
    // and put Rockchip's DLL path in bypass while the bus runs below 52 MHz.
    write_u32(
        base,
        DWCMSHC_EMMC_MISC_CON,
        read_u32(base, DWCMSHC_EMMC_MISC_CON) | MISC_INTCLK_EN,
    );
    write_u32(base, area1 + DWCMSHC_HOST_CTRL3, 0);
    write_u16(
        base,
        area1 + DWCMSHC_EMMC_CONTROL,
        read_u16(base, area1 + DWCMSHC_EMMC_CONTROL) | DWCMSHC_CARD_IS_EMMC,
    );
    write_u32(
        base,
        DWCMSHC_EMMC_DLL_CTRL,
        DWCMSHC_EMMC_DLL_BYPASS | DWCMSHC_EMMC_DLL_START,
    );
    write_u32(base, DWCMSHC_EMMC_DLL_RXCLK, DLL_RXCLK_ORI_GATE);
    write_u32(base, DWCMSHC_EMMC_DLL_TXCLK, 0);
    write_u32(base, DWCMSHC_EMMC_DLL_CMDOUT, 0);
    write_u32(
        base,
        DWCMSHC_EMMC_DLL_STRBIN,
        DWCMSHC_EMMC_DLL_DLYENA
            | DLL_STRBIN_DELAY_NUM_SEL
            | (DLL_STRBIN_DELAY_NUM_DEFAULT << DLL_STRBIN_DELAY_NUM_OFFSET),
    );
    init_dwcmshc_phy_3v3(base);
    info!(
        "rockchip-rk3568-sdhci: dwcmshc vendor init area1={:#x}",
        area1
    );
    Ok(())
}

fn init_dwcmshc_phy_3v3(base: NonNull<u8>) {
    let phy_cfg = PHY_CNFG_RSTN_DEASSERT | (PHY_CNFG_PAD_SP << 16) | (PHY_CNFG_PAD_SN << 20);
    write_u32(base, PHY_CNFG_R, phy_cfg);
    write_u8(base, PHY_SDCLKDL_CNFG_R, PHY_SDCLKDL_CNFG_UPDATE);
    write_u8(base, PHY_SDCLKDL_DC_R, PHY_SDCLKDL_DC_DEFAULT);
    write_u8(base, PHY_DLL_CNFG2_R, PHY_DLL_CNFG2_JUMPSTEP);
    write_u8(base, PHY_SDCLKDL_CNFG_R, 0);

    let pad_pullup = PHY_PAD_RXSEL_3V3
        | (PHY_PAD_WEAKPULL_PULLUP << 3)
        | (PHY_PAD_TXSLEW_CTRL_P << 5)
        | (PHY_PAD_TXSLEW_CTRL_N << 9);
    write_u16(base, PHY_CMDPAD_CNFG_R, pad_pullup);
    write_u16(base, PHY_DATAPAD_CNFG_R, pad_pullup);
    write_u16(base, PHY_RSTNPAD_CNFG_R, pad_pullup);

    let clk_pad = (PHY_PAD_TXSLEW_CTRL_P << 5) | (PHY_PAD_TXSLEW_CTRL_N << 9);
    write_u16(base, PHY_CLKPAD_CNFG_R, clk_pad);

    let strobe_pad = PHY_PAD_RXSEL_3V3
        | (PHY_PAD_WEAKPULL_PULLDOWN << 3)
        | (PHY_PAD_TXSLEW_CTRL_P << 5)
        | (PHY_PAD_TXSLEW_CTRL_N << 9);
    write_u16(base, PHY_STBPAD_CNFG_R, strobe_pad);
    write_u8(base, PHY_SMPLDL_CNFG_R, PHY_SMPLDL_CNFG_BYPASS_EN);
    write_u8(base, PHY_DLL_CTRL_R, PHY_DLL_CTRL_ENABLE);
}

fn vendor_area1(base: NonNull<u8>) -> usize {
    (read_u16(base, DWCMSHC_P_VENDOR_AREA1) & DWCMSHC_AREA1_MASK) as usize
}

fn read_u32(base: NonNull<u8>, off: usize) -> u32 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u32) }
}

fn write_u32(base: NonNull<u8>, off: usize, val: u32) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u32, val) }
}

fn read_u16(base: NonNull<u8>, off: usize) -> u16 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u16) }
}

fn write_u16(base: NonNull<u8>, off: usize, val: u16) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u16, val) }
}

fn write_u8(base: NonNull<u8>, off: usize, val: u8) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off), val) }
}

fn clock_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Init))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rk3568_block_io_uses_dma_irq_queue() {
        let config = sdhci_rdif::dma_config(
            "rockchip-rk3568-sdhci",
            8,
            axklib::dma::device_with_mask(u32::MAX as u64),
        );

        assert!(config.uses_dma());
        assert!(config.supports_runtime_queue());
    }

    #[test]
    fn rk3568_dma_queue_limits_multi_block_requests() {
        let config = sdhci_rdif::dma_config(
            "rockchip-rk3568-sdhci",
            8,
            axklib::dma::device_with_mask(u32::MAX as u64),
        );
        let limits = sdmmc_protocol::rdif::config::queue_limits(&config, u32::MAX as u64);

        assert!(limits.max_blocks_per_request > 1);
        assert!(limits.max_segment_size > sdmmc_protocol::rdif::config::BLOCK_SIZE);
        assert_eq!(limits.max_segments, 1);
    }
}
