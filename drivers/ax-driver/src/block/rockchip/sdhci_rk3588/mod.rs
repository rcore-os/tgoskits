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

use alloc::vec::Vec;
use core::{ptr::NonNull, time::Duration};

use log::{info, warn};
use rdrive::{
    probe::{
        OnProbeError,
        fdt::{ClockLine, ResetLine},
    },
    register::{FdtInfo, ProbeFdt},
};
use sdhci_host::{HostClock, HostResetHook, HostTimer, Sdhci, rdif as sdhci_rdif};
use sdmmc_protocol::{
    Error,
    error::{ErrorContext, Phase},
    sdio::{SdMmcIrqHost, native::SdMmcCard},
};

use super::{
    card_init_preference, clock::enable_node_clocks, media_name, supports_block_card_protocol,
};
use crate::{block::ProbeFdtBlock, mmio::iomap};

// RK3588 DWCMSHC follows Linux's normal SDHCI completion path: hard IRQ only
// acknowledges and caches status; the bound hctx advances command/data state.
const DWCMSHC_P_VENDOR_AREA1: usize = 0xe8;
const DWCMSHC_AREA1_MASK: u16 = 0x0fff;
const DWCMSHC_HOST_CTRL3: usize = 0x08;
const DWCMSHC_HOST_CTRL3_CMD_CONFLICT: u32 = 1 << 0;
const DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE: u32 = 1 << 4;
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
const DLL_STRBIN_DELAY_NUM_DEFAULT: u32 = 16;
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

struct AxKlibHostTimer;

static HOST_TIMER: AxKlibHostTimer = AxKlibHostTimer;

impl HostTimer for AxKlibHostTimer {
    fn now_ms(&self) -> u64 {
        axklib::time::monotonic_nanos() / 1_000_000
    }
}

struct RockchipSdhciClock {
    clock: ClockLine,
}
struct RockchipSdhciResetHook {
    resets: Vec<ResetLine>,
}

impl HostClock for RockchipSdhciClock {
    fn effective_clock_hz(&self, target_hz: u32) -> u32 {
        if target_hz <= 400_000 {
            375_000
        } else {
            target_hz
        }
    }

    fn clock_div_zero_broken(&self) -> bool {
        true
    }

    fn set_clock(&self, target_hz: u32) -> Result<(), Error> {
        self.clock
            .set_rate(u64::from(target_hz))
            .map_err(|_| clock_error())?;
        let rate = self.clock.rate().map_err(|_| clock_error())?;
        info!("rockchip-sdhci: core clock set to {} Hz", rate);
        Ok(())
    }

    fn prepare_host_clock(&self, host: &mut Sdhci, target_hz: u32) -> Result<(), Error> {
        configure_rk3588_dwcmshc_clock(host, target_hz)
    }
}

impl HostResetHook for RockchipSdhciResetHook {
    fn before_reset_all(&self, _host: &mut Sdhci) -> Result<(), Error> {
        assert_resets(&self.resets).map_err(|_| reset_error())?;
        axklib::time::busy_wait(Duration::from_micros(1));
        deassert_resets(&self.resets).map_err(|_| reset_error())?;
        Ok(())
    }

    fn after_reset(&self, host: &mut Sdhci) -> Result<(), Error> {
        init_rk3588_dwcmshc_after_reset(host)
    }
}

crate::model_register!(
    name: "Rockchip sdhci",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-dwcmshc"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    if !supports_block_card_protocol(info.node.as_node()) {
        info!(
            "rockchip-sdhci: skip SDIO-only controller {}",
            info.node.path()
        );
        return Ok(());
    }
    let resets = apply_rockchip_sdhci_resources(info)?;
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
        "rockchip-sdhci probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;

    let mut host = unsafe { Sdhci::new(mmio_base) };
    // A previous owner (for example the hypervisor host) may leave completion
    // IRQ signaling enabled. Keep signal delivery masked until the runtime
    // owns the boxed handler; command/data initialization cannot advance until
    // that IRQ endpoint is registered and enabled.
    host.disable_completion_irq();
    if let Some(clock) = sdhci_core_clock(info)? {
        info!("rockchip-sdhci: using external CRU clock");
        host.set_external_clock(RockchipSdhciClock { clock });
    } else {
        warn!("rockchip-sdhci: no core clock found; using SDHCI internal clock divider");
    }
    host.set_reset_hook(RockchipSdhciResetHook { resets });
    host.set_timer(&HOST_TIMER);
    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        crate::binding_resolver::dma_coherency_from_fdt(info),
        dma_api::DmaConstraints::new(u32::MAX as u64),
    ));
    let config = rockchip_sdhci_rdif_config(0, &dma);
    host.configure_dma(dma).map_err(|err| {
        OnProbeError::other(alloc::format!(
            "rockchip-sdhci ADMA2 configuration failed: {err:?}"
        ))
    })?;

    let preference = card_init_preference(info.node.as_node());
    let identity = alloc::format!("rockchip-sdhci:{}", info.node.path());
    info!(
        "rockchip-sdhci: defer protocol initialization controller={} media={} preference={:?}",
        identity,
        media_name(preference),
        preference
    );
    let parts = host.into_parts();
    let mut card = SdMmcCard::new(parts.bus);
    card.set_diagnostic_identity(identity);
    let dev = sdhci_rdif::BlockDevice::new_initializing(card, parts.irq, config, preference);
    let irq = probe.register_block(dev)?;
    info!("rockchip-sdhci block device registered irq={:?}", irq);
    Ok(())
}

fn apply_rockchip_sdhci_resources(info: &FdtInfo<'_>) -> Result<Vec<ResetLine>, OnProbeError> {
    let resets = info.reset_lines()?;
    enable_node_clocks(info, "SDHCI")?;
    Ok(resets)
}

fn assert_resets(resets: &[ResetLine]) -> Result<(), OnProbeError> {
    for reset in resets {
        reset.assert()?;
    }
    Ok(())
}

fn deassert_resets(resets: &[ResetLine]) -> Result<(), OnProbeError> {
    for reset in resets {
        reset.deassert()?;
    }
    Ok(())
}

fn init_rk3588_dwcmshc_after_reset(host: &mut Sdhci) -> Result<(), Error> {
    let base = NonNull::new(host.mmio_base() as *mut u8).ok_or(Error::InvalidArgument)?;
    let area1 = dwcmshc_vendor_area1(base);

    // Match Linux rk3588 DWCMSHC low-speed setup before CMD1/CMD8:
    // keep internal clock alive, mark the vendor area as eMMC, disable
    // command-conflict checks, and bypass DLL while identification runs.
    write_u32(
        base,
        DWCMSHC_EMMC_MISC_CON,
        read_u32(base, DWCMSHC_EMMC_MISC_CON) | MISC_INTCLK_EN,
    );
    write_u16(
        base,
        area1 + DWCMSHC_EMMC_CONTROL,
        read_u16(base, area1 + DWCMSHC_EMMC_CONTROL) | DWCMSHC_CARD_IS_EMMC,
    );
    configure_rk3588_dwcmshc_clock_regs(base, area1, 400_000);
    init_rk3588_dwcmshc_phy_3v3(base);
    info!("rockchip-sdhci: RK3588 DWCMSHC vendor reset area1={area1:#x}");
    Ok(())
}

fn configure_rk3588_dwcmshc_clock(host: &mut Sdhci, target_hz: u32) -> Result<(), Error> {
    let base = NonNull::new(host.mmio_base() as *mut u8).ok_or(Error::InvalidArgument)?;
    let area1 = dwcmshc_vendor_area1(base);
    configure_rk3588_dwcmshc_clock_regs(base, area1, target_hz);
    Ok(())
}

fn configure_rk3588_dwcmshc_clock_regs(base: NonNull<u8>, area1: usize, target_hz: u32) {
    // Linux's rk35xx set_clock path disables command-conflict checks and
    // keeps the internal clock ungated while SDHCI clock output is gated.
    // Preserve unrelated vendor bits because this register also carries
    // controller-specific state outside the two Rockchip workarounds.
    let host_ctrl3 = read_u32(base, area1 + DWCMSHC_HOST_CTRL3);
    write_u32(
        base,
        area1 + DWCMSHC_HOST_CTRL3,
        (host_ctrl3 & !DWCMSHC_HOST_CTRL3_CMD_CONFLICT) | DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE,
    );
    if target_hz <= 52_000_000 {
        write_u32(base, DWCMSHC_EMMC_DLL_CTRL, 0);
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
    }
}

fn init_rk3588_dwcmshc_phy_3v3(base: NonNull<u8>) {
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

fn reset_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Init))
}

fn dwcmshc_vendor_area1(base: NonNull<u8>) -> usize {
    (read_u16(base, DWCMSHC_P_VENDOR_AREA1) & DWCMSHC_AREA1_MASK) as usize
}

fn read_u32(base: NonNull<u8>, off: usize) -> u32 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u32) }
}

#[cfg(test)]
fn read_u8(base: NonNull<u8>, off: usize) -> u8 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off)) }
}

fn read_u16(base: NonNull<u8>, off: usize) -> u16 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u16) }
}

fn write_u32(base: NonNull<u8>, off: usize, val: u32) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u32, val) }
}

fn write_u16(base: NonNull<u8>, off: usize, val: u16) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u16, val) }
}

fn write_u8(base: NonNull<u8>, off: usize, val: u8) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off), val) }
}

fn rockchip_sdhci_rdif_config(
    capacity_blocks: u64,
    dma: &dma_api::DeviceDma,
) -> sdhci_rdif::BlockConfig {
    sdhci_rdif::dma_config("rockchip-sdhci", capacity_blocks, dma)
}

fn sdhci_core_clock(info: &FdtInfo<'_>) -> Result<Option<ClockLine>, OnProbeError> {
    info.find_clock_line_by_name("core")
}

fn clock_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Init))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rk3588_block_io_uses_adma_config_with_irq_completion() {
        let dma = test_dma();
        let config = rockchip_sdhci_rdif_config(8, &dma);

        assert_eq!(config.name(), "rockchip-sdhci");
        assert_eq!(config.capacity_blocks(), 8);
        assert!(config.uses_dma());
    }

    #[test]
    fn rk3588_adma_queue_limits_expose_sdhci_window() {
        let dma = test_dma();
        let config = rockchip_sdhci_rdif_config(8, &dma);
        let limits = sdmmc_protocol::rdif::config::queue_limits(&config);

        assert_eq!(limits.max_blocks_per_request, sdhci_host::ADMA2_MAX_BLOCKS);
        assert_eq!(
            limits.dma.constraints().max_segment_size,
            Some(sdhci_host::ADMA2_MAX_TRANSFER_SIZE)
        );
        assert_eq!(
            limits.dma.constraints().boundary,
            Some(sdhci_host::DWC_MSHC_ADMA_BOUNDARY)
        );
        assert_eq!(limits.max_segments, 1);
    }

    #[test]
    fn dwcmshc_vendor_area_masks_pointer_register() {
        let mut mmio = [0u8; 0x1000];
        let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
        unsafe {
            core::ptr::write_volatile(
                mmio.as_mut_ptr().add(DWCMSHC_P_VENDOR_AREA1) as *mut u16,
                0xfabc,
            );
        }

        assert_eq!(dwcmshc_vendor_area1(base), 0x0abc);
    }

    #[test]
    fn rk3588_dwcmshc_after_reset_programs_low_speed_vendor_and_phy_defaults() {
        let mut mmio = [0xff_u8; 0x1000];
        let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
        write_u16(base, DWCMSHC_P_VENDOR_AREA1, 0x0500);
        write_u32(base, DWCMSHC_EMMC_MISC_CON, 0);
        write_u16(base, 0x0500 + DWCMSHC_EMMC_CONTROL, 0);

        let mut host = unsafe { Sdhci::new(base) };
        init_rk3588_dwcmshc_after_reset(&mut host).unwrap();

        let host_ctrl3 = read_u32(base, 0x0500 + DWCMSHC_HOST_CTRL3);
        assert_eq!(host_ctrl3 & DWCMSHC_HOST_CTRL3_CMD_CONFLICT, 0);
        assert_eq!(
            host_ctrl3 & DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE,
            DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE,
            "RK3588 DWCMSHC must keep its internal clock ungated during identification"
        );
        assert_eq!(
            host_ctrl3 & !(DWCMSHC_HOST_CTRL3_CMD_CONFLICT | DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE),
            u32::MAX & !(DWCMSHC_HOST_CTRL3_CMD_CONFLICT | DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE),
            "clock setup must preserve unrelated vendor bits"
        );
        assert_eq!(
            read_u16(base, 0x0500 + DWCMSHC_EMMC_CONTROL) & DWCMSHC_CARD_IS_EMMC,
            DWCMSHC_CARD_IS_EMMC
        );
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_MISC_CON) & MISC_INTCLK_EN,
            MISC_INTCLK_EN
        );
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_DLL_CTRL),
            DWCMSHC_EMMC_DLL_BYPASS | DWCMSHC_EMMC_DLL_START
        );
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_RXCLK), DLL_RXCLK_ORI_GATE);
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_TXCLK), 0);
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_CMDOUT), 0);
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_DLL_STRBIN),
            DWCMSHC_EMMC_DLL_DLYENA
                | DLL_STRBIN_DELAY_NUM_SEL
                | (DLL_STRBIN_DELAY_NUM_DEFAULT << DLL_STRBIN_DELAY_NUM_OFFSET)
        );

        let phy_cfg = PHY_CNFG_RSTN_DEASSERT | (PHY_CNFG_PAD_SP << 16) | (PHY_CNFG_PAD_SN << 20);
        assert_eq!(read_u32(base, PHY_CNFG_R), phy_cfg);
        assert_eq!(read_u8(base, PHY_SDCLKDL_CNFG_R), 0);
        assert_eq!(read_u8(base, PHY_SDCLKDL_DC_R), PHY_SDCLKDL_DC_DEFAULT);
        assert_eq!(read_u8(base, PHY_DLL_CNFG2_R), PHY_DLL_CNFG2_JUMPSTEP);
        assert_eq!(read_u8(base, PHY_SMPLDL_CNFG_R), PHY_SMPLDL_CNFG_BYPASS_EN);
        assert_eq!(read_u8(base, PHY_DLL_CTRL_R), PHY_DLL_CTRL_ENABLE);

        let pad_pullup = PHY_PAD_RXSEL_3V3
            | (PHY_PAD_WEAKPULL_PULLUP << 3)
            | (PHY_PAD_TXSLEW_CTRL_P << 5)
            | (PHY_PAD_TXSLEW_CTRL_N << 9);
        assert_eq!(read_u16(base, PHY_CMDPAD_CNFG_R), pad_pullup);
        assert_eq!(read_u16(base, PHY_DATAPAD_CNFG_R), pad_pullup);
        assert_eq!(read_u16(base, PHY_RSTNPAD_CNFG_R), pad_pullup);

        let clk_pad = (PHY_PAD_TXSLEW_CTRL_P << 5) | (PHY_PAD_TXSLEW_CTRL_N << 9);
        assert_eq!(read_u16(base, PHY_CLKPAD_CNFG_R), clk_pad);

        let strobe_pad = PHY_PAD_RXSEL_3V3
            | (PHY_PAD_WEAKPULL_PULLDOWN << 3)
            | (PHY_PAD_TXSLEW_CTRL_P << 5)
            | (PHY_PAD_TXSLEW_CTRL_N << 9);
        assert_eq!(read_u16(base, PHY_STBPAD_CNFG_R), strobe_pad);
    }

    #[test]
    fn rk3588_dwcmshc_clock_stage_programs_low_speed_dll_bypass() {
        let mut mmio = [0xff_u8; 0x1000];
        let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
        write_u16(base, DWCMSHC_P_VENDOR_AREA1, 0x0500);
        write_u32(base, 0x0500 + DWCMSHC_HOST_CTRL3, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_CTRL, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_RXCLK, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_TXCLK, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_CMDOUT, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_STRBIN, u32::MAX);

        let mut host = unsafe { Sdhci::new(base) };
        configure_rk3588_dwcmshc_clock(&mut host, 400_000).unwrap();

        let host_ctrl3 = read_u32(base, 0x0500 + DWCMSHC_HOST_CTRL3);
        assert_eq!(host_ctrl3 & DWCMSHC_HOST_CTRL3_CMD_CONFLICT, 0);
        assert_eq!(
            host_ctrl3 & DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE,
            DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE,
            "clock programming must disable the DWCMSHC internal clock gate"
        );
        assert_eq!(
            host_ctrl3 & !(DWCMSHC_HOST_CTRL3_CMD_CONFLICT | DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE),
            u32::MAX & !(DWCMSHC_HOST_CTRL3_CMD_CONFLICT | DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE),
            "clock programming must preserve unrelated vendor bits"
        );
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_DLL_CTRL),
            DWCMSHC_EMMC_DLL_BYPASS | DWCMSHC_EMMC_DLL_START
        );
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_RXCLK), DLL_RXCLK_ORI_GATE);
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_TXCLK), 0);
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_CMDOUT), 0);
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_DLL_STRBIN),
            DWCMSHC_EMMC_DLL_DLYENA
                | DLL_STRBIN_DELAY_NUM_SEL
                | (DLL_STRBIN_DELAY_NUM_DEFAULT << DLL_STRBIN_DELAY_NUM_OFFSET)
        );
    }

    fn test_dma() -> dma_api::DeviceDma {
        dma_api::DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::NonCoherent,
                dma_api::DmaConstraints::new(u32::MAX as u64),
            ),
            &TEST_DMA,
        )
    }

    struct TestDma;
    static TEST_DMA: TestDma = TestDma;

    impl dma_api::DmaOp for TestDma {
        fn page_size(&self) -> usize {
            sdmmc_protocol::rdif::config::BLOCK_SIZE
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: dma_api::DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_contiguous(&self, _handle: dma_api::DmaAllocHandle) {}

        unsafe fn alloc_coherent(
            &self,
            _constraints: dma_api::DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_coherent(
            &self,
            _handle: dma_api::DmaAllocHandle,
        ) -> Result<(), dma_api::DmaError> {
            Ok(())
        }

        unsafe fn map_streaming(
            &self,
            _constraints: dma_api::DmaConstraints,
            _addr: core::ptr::NonNull<u8>,
            _size: core::num::NonZeroUsize,
            _direction: dma_api::DmaDirection,
        ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
            Err(dma_api::DmaError::NoMemory)
        }

        unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
    }
}
