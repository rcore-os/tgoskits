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

use alloc::{boxed::Box, vec::Vec};
use core::{ptr::NonNull, time::Duration};

use log::{info, warn};
use rdrive::{
    probe::{
        OnProbeError,
        fdt::{ClockLine, ResetLine},
    },
    register::{FdtInfo, ProbeFdt},
};
use sdhci_host::{HostClock, HostResetHook, Sdhci, rdif as sdhci_rdif};
use sdmmc_protocol::{
    Error,
    error::{ErrorContext, Phase},
    rdif::BlkError,
    sdio::{SdMmcIrqHost, native::SdMmcCard},
};

use super::{
    card_init_preference, clock::enable_node_clocks, media_name, supports_block_card_protocol,
};
use crate::{block::ProbeFdtBlock, mmio::iomap, sdhci_runtime::install_host_timer};

// RK3588 DWCMSHC follows Linux's normal SDHCI completion path: hard IRQ only
// acknowledges and caches status; the bound hctx advances command/data state.
const DWCMSHC_P_VENDOR_AREA1: usize = 0xe8;
const DWCMSHC_P_VENDOR_AREA2: usize = 0xea;
const DWCMSHC_AREA1_MASK: u16 = 0x0fff;
const DWCMSHC_CQHCI_REGISTER_BYTES: usize = 0x60;
const DWCMSHC_HOST_CTRL3: usize = 0x08;
const DWCMSHC_HOST_CTRL3_CMD_CONFLICT: u32 = 1 << 0;
const DWCMSHC_HOST_CTRL3_CLK_GATE_DISABLE: u32 = 1 << 4;
const DWCMSHC_EMMC_CONTROL: usize = 0x2c;
const DWCMSHC_CARD_IS_EMMC: u16 = 1 << 0;
const DWCMSHC_ENHANCED_STROBE: u16 = 1 << 8;
const DWCMSHC_EMMC_DLL_CTRL: usize = 0x800;
const DWCMSHC_EMMC_DLL_RXCLK: usize = 0x804;
const DWCMSHC_EMMC_DLL_TXCLK: usize = 0x808;
const DWCMSHC_EMMC_DLL_STRBIN: usize = 0x80c;
const DWCMSHC_EMMC_DLL_CMDOUT: usize = 0x810;
const DWCMSHC_EMMC_MISC_CON: usize = 0x81c;
const DWCMSHC_EMMC_DLL_STATUS0: usize = 0x840;
const DWCMSHC_VENDOR_AREA1_BYTES: usize = 0x44;
const DWCMSHC_EMMC_ATCTRL: usize = 0x40;
const DLL_LOCKED: u32 = 1 << 8;
const DLL_TIMEOUT: u32 = 1 << 9;
const DLL_TAPNUM_FROM_SW: u32 = 1 << 24;
const DLL_TXCLK_NO_INVERTER: u32 = 1 << 29;
const DWCMSHC_EMMC_DLL_BYPASS: u32 = 1 << 24;
const DWCMSHC_EMMC_DLL_START: u32 = 1 << 0;
const DWCMSHC_EMMC_DLL_DLYENA: u32 = 1 << 27;
const DLL_RXCLK_ORI_GATE: u32 = 1 << 31;
const DLL_STRBIN_DELAY_NUM_SEL: u32 = 1 << 26;
const DLL_STRBIN_DELAY_NUM_DEFAULT: u32 = 0x16;
const DLL_STRBIN_DELAY_NUM_OFFSET: u32 = 16;
const MISC_INTCLK_EN: u32 = 1 << 1;

const SDHCI_BLOCK_SIZE: usize = 0x04;
const SDHCI_TRANSFER_MODE: usize = 0x0c;
const SDHCI_BUFFER: usize = 0x20;
const SDHCI_PRESENT_STATE: usize = 0x24;
const SDHCI_HOST_CONTROL: usize = 0x28;
const SDHCI_TIMEOUT_CONTROL: usize = 0x2e;
const SDHCI_INT_STATUS: usize = 0x30;
const SDHCI_INT_ENABLE: usize = 0x34;
const SDHCI_SIGNAL_ENABLE: usize = 0x38;
const SDHCI_DATA_AVAILABLE: u32 = 1 << 11;
const SDHCI_INT_RESPONSE: u32 = 1;
const SDHCI_DMA_SELECT_MASK: u8 = 0b11 << 3;
const SDHCI_DMA_SELECT_ADMA32: u8 = 0b10 << 3;
const SDHCI_BLOCK_SIZE_MASK: u16 = 0x0fff;
const SDHCI_CQE_BLOCK_SIZE: u16 = 512;
const SDHCI_INT_CQE: u32 = 1 << 14;
const SDHCI_CQE_ERROR_MASK: u32 = 0x02ff_0000;
// Match Linux SDHCI_CQE_INT_MASK. Bit 15 summarizes enabled error sources;
// it is not an independently enabled interrupt source.
const SDHCI_CQE_INTERRUPT_MASK: u32 = SDHCI_CQE_ERROR_MASK | SDHCI_INT_CQE;
const DWCMSHC_SDHCI_CQE_TRANSFER_MODE: u16 = (1 << 5) | (1 << 1) | 1;
const CQHCI_SSC1: usize = 0x40;
const CQHCI_SSC1_CIT_MASK: u32 = 0xffff;
const CQHCI_SSC1_CIT_RK35XX: u32 = 0x0100;
const CQHCI_DATA_DRAIN_LIMIT: usize = 4096;

struct RockchipSdhciClock {
    clock: ClockLine,
    max_hz: u32,
    txclk_tap: u8,
}
struct RockchipSdhciResetHook {
    resets: Vec<ResetLine>,
}

struct Rk3588CqhciPlatform {
    sdhci_base: NonNull<u8>,
    legacy_interrupt_masks: Option<(u32, u32)>,
}

// SAFETY: the platform object owns no CPU-local state. Its MMIO capability is
// moved with the single hardware-queue owner and never accessed concurrently
// outside the IRQ endpoint's disjoint status registers.
unsafe impl Send for Rk3588CqhciPlatform {}

impl cqhci_host::CqhciPlatform for Rk3588CqhciPlatform {
    fn now_ns(&self) -> u64 {
        axklib::time::monotonic_nanos()
    }

    fn prepare(&mut self, cqhci_base: usize) -> Result<(), BlkError> {
        let cqhci_base = NonNull::new(cqhci_base as *mut u8).ok_or(BlkError::InvalidRequest)?;
        let ssc1 = read_u32(cqhci_base, CQHCI_SSC1);
        write_u32(
            cqhci_base,
            CQHCI_SSC1,
            (ssc1 & !CQHCI_SSC1_CIT_MASK) | CQHCI_SSC1_CIT_RK35XX,
        );
        Ok(())
    }

    fn enable_data_path(&mut self) -> Result<(), BlkError> {
        for _ in 0..CQHCI_DATA_DRAIN_LIMIT {
            if read_u32(self.sdhci_base, SDHCI_PRESENT_STATE) & SDHCI_DATA_AVAILABLE == 0 {
                write_u16(
                    self.sdhci_base,
                    SDHCI_TRANSFER_MODE,
                    DWCMSHC_SDHCI_CQE_TRANSFER_MODE,
                );
                let host_control = read_u8(self.sdhci_base, SDHCI_HOST_CONTROL);
                write_u8(
                    self.sdhci_base,
                    SDHCI_HOST_CONTROL,
                    (host_control & !SDHCI_DMA_SELECT_MASK) | SDHCI_DMA_SELECT_ADMA32,
                );
                let block_size = read_u16(self.sdhci_base, SDHCI_BLOCK_SIZE);
                write_u16(
                    self.sdhci_base,
                    SDHCI_BLOCK_SIZE,
                    (block_size & !SDHCI_BLOCK_SIZE_MASK) | SDHCI_CQE_BLOCK_SIZE,
                );
                write_u8(self.sdhci_base, SDHCI_TIMEOUT_CONTROL, 0x0e);
                if self.legacy_interrupt_masks.is_none() {
                    self.legacy_interrupt_masks = Some((
                        read_u32(self.sdhci_base, SDHCI_INT_ENABLE),
                        read_u32(self.sdhci_base, SDHCI_SIGNAL_ENABLE),
                    ));
                }
                write_u32(self.sdhci_base, SDHCI_INT_ENABLE, SDHCI_CQE_INTERRUPT_MASK);
                write_u32(
                    self.sdhci_base,
                    SDHCI_SIGNAL_ENABLE,
                    SDHCI_CQE_INTERRUPT_MASK,
                );
                return Ok(());
            }
            let _ = read_u32(self.sdhci_base, SDHCI_BUFFER);
        }
        Err(BlkError::Io)
    }

    fn disable_data_path(&mut self) -> Result<(), BlkError> {
        write_u32(self.sdhci_base, SDHCI_INT_STATUS, SDHCI_INT_RESPONSE);
        if let Some((status_enable, signal_enable)) = self.legacy_interrupt_masks.take() {
            write_u32(self.sdhci_base, SDHCI_INT_ENABLE, status_enable);
            write_u32(self.sdhci_base, SDHCI_SIGNAL_ENABLE, signal_enable);
        }
        Ok(())
    }
}

impl HostClock for RockchipSdhciClock {
    fn effective_clock_hz(&self, target_hz: u32) -> u32 {
        if target_hz == 0 {
            0
        } else if target_hz <= 400_000 {
            375_000
        } else {
            target_hz.min(self.max_hz)
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
        configure_rk3588_dwcmshc_clock(host, target_hz, self.txclk_tap)
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
    let mmio_size = usize::try_from(mmio_size)
        .map_err(|_| OnProbeError::other("SDHCI mapping size exceeds address space"))?;
    if mmio_size < DWCMSHC_EMMC_DLL_STATUS0 + 4 {
        return Err(OnProbeError::other(
            "SDHCI mapping does not cover the Rockchip DLL",
        ));
    }
    let mmio_base = iomap(base_reg.address as usize, mmio_size)?;
    let area1 = dwcmshc_vendor_area1(mmio_base);
    if area1 < 0x100
        || area1 & 3 != 0
        || area1 + DWCMSHC_VENDOR_AREA1_BYTES > mmio_size
        || ranges_overlap(
            area1,
            DWCMSHC_VENDOR_AREA1_BYTES,
            DWCMSHC_EMMC_DLL_CTRL,
            DWCMSHC_EMMC_DLL_STATUS0 + 4 - DWCMSHC_EMMC_DLL_CTRL,
        )
    {
        return Err(OnProbeError::other("invalid SDHCI vendor register window"));
    }

    // SAFETY: iomap retains the aligned controller mapping for the device's
    // lifetime; the standard, vendor and DLL windows have been bounded above.
    let mut host = unsafe { Sdhci::new(mmio_base) };
    // A previous owner (for example the hypervisor host) may leave completion
    // IRQ signaling enabled. Keep signal delivery masked until the runtime
    // owns the boxed handler; command/data initialization cannot advance until
    // that IRQ endpoint is registered and enabled.
    host.disable_completion_irq();
    if let Some(clock) = sdhci_core_clock(info)? {
        info!("rockchip-sdhci: using external CRU clock");
        let node = info.node.as_node();
        let max_hz = node
            .get_property("max-frequency")
            .and_then(|prop| prop.get_u32())
            .unwrap_or(200_000_000)
            .min(200_000_000);
        let txclk_tap = match node.get_property("rockchip,txclk-tapnum") {
            None => 0x10,
            Some(prop) => match prop.data.as_slice() {
                [tap] => *tap,
                _ => prop
                    .get_u32()
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or(OnProbeError::other("invalid eMMC TX clock tap"))?,
            },
        };
        // These board properties describe a wired 1.8 V eMMC bus. Do not
        // enable signaling solely because the controller advertises it.
        let fixed_emmc_1v8 = node.get_property("non-removable").is_some()
            && node.get_property("no-1-8-v").is_none()
            && (node.get_property("mmc-hs200-1_8v").is_some()
                || node.get_property("mmc-hs400-1_8v").is_some());
        if fixed_emmc_1v8 && fixed_1v8_supply(info) && max_hz > 52_000_000 {
            host.enable_1v8_signaling();
        }
        host.set_external_clock(RockchipSdhciClock {
            clock,
            max_hz,
            txclk_tap,
        });
    } else {
        warn!("rockchip-sdhci: no core clock found; using SDHCI internal clock divider");
    }
    host.set_reset_hook(RockchipSdhciResetHook { resets });
    install_host_timer(&mut host);
    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        crate::binding_resolver::dma_coherency_from_fdt(info),
        dma_api::DmaConstraints::new(u32::MAX as u64),
    ));
    let config = rockchip_sdhci_rdif_config(0, &dma);
    host.configure_dma(dma.clone()).map_err(|err| {
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
    let supports_cqe = info.node.as_node().get_property("supports-cqe").is_some();
    let cqhci_base = if supports_cqe {
        match dwcmshc_cqhci_base(mmio_base, mmio_size) {
            Ok(base) => Some(base),
            Err(error) => {
                warn!("rockchip-sdhci: invalid CQHCI vendor area ({error:?}); using legacy SDHCI");
                None
            }
        }
    } else {
        None
    };
    let parts = host.into_parts();
    let mut card = SdMmcCard::new(parts.bus);
    card.set_diagnostic_identity(identity);
    let dev = if let Some(cqhci_base) = cqhci_base {
        let platform = Rk3588CqhciPlatform {
            sdhci_base: mmio_base,
            legacy_interrupt_masks: None,
        };
        // SAFETY: iomap retains this controller mapping for the device's
        // lifetime. Both endpoints share its registered owner-CPU IRQ; the
        // validated vendor window is disjoint from the legacy status words.
        match unsafe { cqhci_host::queue_and_irq(cqhci_base, dma, Box::new(platform), parts.irq) } {
            Ok((queue, irq)) => {
                info!(
                    "rockchip-sdhci: CQHCI available at offset={:#x}",
                    cqhci_base.as_ptr() as usize - mmio_base.as_ptr() as usize
                );
                sdhci_rdif::BlockDevice::new_initializing_accelerated(
                    card,
                    irq,
                    config,
                    preference,
                    Box::new(queue),
                )
            }
            Err(failure) => {
                warn!(
                    "rockchip-sdhci: CQHCI allocation failed ({:?}); using legacy SDHCI",
                    failure.error
                );
                sdhci_rdif::BlockDevice::new_initializing(
                    card,
                    failure.legacy_irq,
                    config,
                    preference,
                )
            }
        }
    } else {
        sdhci_rdif::BlockDevice::new_initializing(card, parts.irq, config, preference)
    };
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
        (read_u16(base, area1 + DWCMSHC_EMMC_CONTROL) & !DWCMSHC_ENHANCED_STROBE)
            | DWCMSHC_CARD_IS_EMMC,
    );
    configure_rk3588_dwcmshc_clock_regs(base, area1, 400_000);
    info!("rockchip-sdhci: RK3588 DWCMSHC vendor reset area1={area1:#x}");
    Ok(())
}

fn configure_rk3588_dwcmshc_clock(
    host: &mut Sdhci,
    target_hz: u32,
    txclk_tap: u8,
) -> Result<(), Error> {
    let base = NonNull::new(host.mmio_base() as *mut u8).ok_or(Error::InvalidArgument)?;
    let area1 = dwcmshc_vendor_area1(base);
    configure_rk3588_dwcmshc_clock_regs(base, area1, target_hz);
    if target_hz > 52_000_000 {
        configure_high_speed_dll(base, area1, txclk_tap)?;
    }
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

fn configure_high_speed_dll(base: NonNull<u8>, area1: usize, txclk_tap: u8) -> Result<(), Error> {
    // RK3588 uses the Rockchip DLL at 0x800, not the generic Synopsys PHY
    // pad controls at 0x300 used by other DWCMSHC integrations.
    write_u32(base, DWCMSHC_EMMC_DLL_CTRL, 1 << 1);
    axklib::time::busy_wait(Duration::from_micros(1));
    write_u32(base, DWCMSHC_EMMC_DLL_CTRL, 0);
    write_u32(base, DWCMSHC_EMMC_DLL_RXCLK, DWCMSHC_EMMC_DLL_DLYENA);
    write_u32(
        base,
        DWCMSHC_EMMC_DLL_CTRL,
        (5 << 16) | (2 << 8) | DWCMSHC_EMMC_DLL_START,
    );
    let deadline = axklib::time::monotonic_nanos().saturating_add(500_000_000);
    loop {
        let status = read_u32(base, DWCMSHC_EMMC_DLL_STATUS0);
        if status & (DLL_LOCKED | DLL_TIMEOUT) == DLL_LOCKED {
            break;
        }
        if status & DLL_TIMEOUT != 0 || axklib::time::monotonic_nanos() >= deadline {
            write_u32(base, DWCMSHC_EMMC_DLL_CTRL, 0);
            return Err(Error::Timeout(ErrorContext::new(Phase::Init)));
        }
        core::hint::spin_loop();
    }
    write_u32(
        base,
        area1 + DWCMSHC_EMMC_ATCTRL,
        (1 << 16) | (3 << 17) | (3 << 19),
    );
    write_u32(base, DWCMSHC_EMMC_DLL_CMDOUT, 0);
    write_u32(
        base,
        DWCMSHC_EMMC_DLL_TXCLK,
        DWCMSHC_EMMC_DLL_DLYENA | DLL_TAPNUM_FROM_SW | DLL_TXCLK_NO_INVERTER | u32::from(txclk_tap),
    );
    write_u32(
        base,
        DWCMSHC_EMMC_DLL_STRBIN,
        DWCMSHC_EMMC_DLL_DLYENA | DLL_TAPNUM_FROM_SW | 4,
    );
    Ok(())
}

fn fixed_1v8_supply(info: &FdtInfo<'_>) -> bool {
    let Some(supply) = info.node.as_node().get_property("vqmmc-supply") else {
        // No controllable rail is described: the board's explicit HS200/400
        // 1.8 V property is the firmware contract for its fixed wiring.
        return true;
    };
    let Some(regulator) = supply
        .get_u32()
        .and_then(|phandle| info.get_by_phandle(phandle.into()))
    else {
        return false;
    };
    let regulator = regulator.as_node();
    regulator.get_property("regulator-always-on").is_some()
        && regulator
            .get_property("regulator-min-microvolt")
            .and_then(|p| p.get_u32())
            == Some(1_800_000)
        && regulator
            .get_property("regulator-max-microvolt")
            .and_then(|p| p.get_u32())
            == Some(1_800_000)
}

fn ranges_overlap(start: usize, len: usize, other: usize, other_len: usize) -> bool {
    start < other.saturating_add(other_len) && other < start.saturating_add(len)
}

fn reset_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Init))
}

fn dwcmshc_vendor_area1(base: NonNull<u8>) -> usize {
    (read_u16(base, DWCMSHC_P_VENDOR_AREA1) & DWCMSHC_AREA1_MASK) as usize
}

fn dwcmshc_cqhci_base(base: NonNull<u8>, mmio_size: usize) -> Result<NonNull<u8>, BlkError> {
    let area2 = usize::from(read_u16(base, DWCMSHC_P_VENDOR_AREA2));
    let area1 = dwcmshc_vendor_area1(base);
    if area2 < 0x100
        || area2 & 3 != 0
        || ranges_overlap(
            area2,
            DWCMSHC_CQHCI_REGISTER_BYTES,
            area1,
            DWCMSHC_VENDOR_AREA1_BYTES,
        )
        || ranges_overlap(
            area2,
            DWCMSHC_CQHCI_REGISTER_BYTES,
            DWCMSHC_EMMC_DLL_CTRL,
            DWCMSHC_EMMC_DLL_STATUS0 + 4 - DWCMSHC_EMMC_DLL_CTRL,
        )
        || area2
            .checked_add(DWCMSHC_CQHCI_REGISTER_BYTES)
            .is_none_or(|end| end > mmio_size)
    {
        return Err(BlkError::InvalidRequest);
    }
    // SAFETY: area2 is aligned, fully mapped, and disjoint from both vendor
    // clock registers and the standard SDHCI register file.
    NonNull::new(unsafe { base.as_ptr().add(area2) }).ok_or(BlkError::InvalidRequest)
}

fn read_u32(base: NonNull<u8>, off: usize) -> u32 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u32) }
}

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
        let mut mmio = [0u32; 0x400];
        let base = NonNull::new(mmio.as_mut_ptr().cast::<u8>()).unwrap();
        write_u16(base, DWCMSHC_P_VENDOR_AREA1, 0xfabc);

        assert_eq!(dwcmshc_vendor_area1(base), 0x0abc);
    }

    #[test]
    fn dwcmshc_cqhci_pointer_must_cover_the_complete_register_window() {
        let mut mmio = [0u32; 0x400];
        let base = NonNull::new(mmio.as_mut_ptr().cast::<u8>()).unwrap();
        write_u16(base, DWCMSHC_P_VENDOR_AREA1, 0x0500);
        write_u16(base, DWCMSHC_P_VENDOR_AREA2, 0x0400);

        let cqhci = dwcmshc_cqhci_base(base, core::mem::size_of_val(&mmio)).unwrap();
        assert_eq!(cqhci.as_ptr() as usize - base.as_ptr() as usize, 0x0400);

        write_u16(base, DWCMSHC_P_VENDOR_AREA2, 0x0fc0);
        assert_eq!(
            dwcmshc_cqhci_base(base, core::mem::size_of_val(&mmio)),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn rk3588_cqhci_platform_programs_linux_compatible_data_path() {
        let mut mmio = [0u32; 0x400];
        let base = NonNull::new(mmio.as_mut_ptr().cast::<u8>()).unwrap();
        // SAFETY: offset 0x400 remains within the live test MMIO array.
        let cqhci = unsafe { NonNull::new_unchecked(base.as_ptr().add(0x400)) };
        write_u32(cqhci, CQHCI_SSC1, u32::MAX);
        write_u32(base, SDHCI_INT_ENABLE, 0x1234_5678);
        write_u32(base, SDHCI_SIGNAL_ENABLE, 0x8765_4321);
        let mut platform = Rk3588CqhciPlatform {
            sdhci_base: base,
            legacy_interrupt_masks: None,
        };

        cqhci_host::CqhciPlatform::prepare(&mut platform, cqhci.as_ptr() as usize).unwrap();
        cqhci_host::CqhciPlatform::enable_data_path(&mut platform).unwrap();

        assert_eq!(
            read_u32(cqhci, CQHCI_SSC1) & CQHCI_SSC1_CIT_MASK,
            CQHCI_SSC1_CIT_RK35XX
        );
        assert_eq!(
            read_u16(base, SDHCI_TRANSFER_MODE),
            DWCMSHC_SDHCI_CQE_TRANSFER_MODE
        );
        assert_eq!(
            read_u8(base, SDHCI_HOST_CONTROL) & SDHCI_DMA_SELECT_MASK,
            SDHCI_DMA_SELECT_ADMA32
        );
        assert_eq!(
            read_u16(base, SDHCI_BLOCK_SIZE) & SDHCI_BLOCK_SIZE_MASK,
            SDHCI_CQE_BLOCK_SIZE
        );
        // Check the programmed registers against Linux's mask, independently
        // of the constant used by enable_data_path().
        assert_eq!(read_u32(base, SDHCI_INT_ENABLE), 0x02ff_4000);
        assert_eq!(read_u32(base, SDHCI_SIGNAL_ENABLE), 0x02ff_4000);

        cqhci_host::CqhciPlatform::disable_data_path(&mut platform).unwrap();
        assert_eq!(read_u32(base, SDHCI_INT_ENABLE), 0x1234_5678);
        assert_eq!(read_u32(base, SDHCI_SIGNAL_ENABLE), 0x8765_4321);
    }

    #[test]
    fn rk3588_dwcmshc_after_reset_programs_low_speed_vendor_defaults() {
        let mut mmio = [u32::MAX; 0x400];
        let base = NonNull::new(mmio.as_mut_ptr().cast::<u8>()).unwrap();
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
    }

    #[test]
    fn rk3588_dwcmshc_clock_stage_programs_low_speed_dll_bypass() {
        let mut mmio = [u32::MAX; 0x400];
        let base = NonNull::new(mmio.as_mut_ptr().cast::<u8>()).unwrap();
        write_u16(base, DWCMSHC_P_VENDOR_AREA1, 0x0500);
        write_u32(base, 0x0500 + DWCMSHC_HOST_CTRL3, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_CTRL, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_RXCLK, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_TXCLK, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_CMDOUT, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_STRBIN, u32::MAX);

        let mut host = unsafe { Sdhci::new(base) };
        configure_rk3588_dwcmshc_clock(&mut host, 400_000, 0x10).unwrap();

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
