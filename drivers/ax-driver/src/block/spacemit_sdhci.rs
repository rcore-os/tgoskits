// Copyright 2026 The Axvisor Team
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

use alloc::format;
use core::{ptr::NonNull, time::Duration};

use log::info;
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdhci_host::{HostResetHook, HostTimer, Sdhci, rdif as sdhci_rdif};
use sdmmc_protocol::{
    Error,
    sdio::{SdMmcCard, SdMmcIrqHost, init::CardInitPreference},
};

use crate::{block::ProbeFdtBlock, mmio::iomap};

const SPACEMIT_SDHCI_DMA_MASK: u64 = u64::MAX;
const K3_APMU_BASE: usize = 0xd428_2800;
const K3_APMU_SIZE: usize = 0x400;
const K3_APMU_SDH0_CLK_RES_CTRL: usize = 0x54;
const K3_APMU_SDH_AXI_RESET_DEASSERT: u32 = 1 << 0;
const K3_APMU_SDH0_RESET_DEASSERT: u32 = 1 << 1;
const K3_APMU_SDH_AXI_CLOCK_GATE: u32 = 1 << 3;
const K3_APMU_SDH0_CLOCK_GATE: u32 = 1 << 4;
const K3_APMU_SDHCI_RESET_DEASSERT_MASK: u32 =
    K3_APMU_SDH_AXI_RESET_DEASSERT | K3_APMU_SDH0_RESET_DEASSERT;
const K3_APMU_SDHCI_ENABLE_MASK: u32 =
    K3_APMU_SDHCI_RESET_DEASSERT_MASK | K3_APMU_SDH_AXI_CLOCK_GATE | K3_APMU_SDH0_CLOCK_GATE;
const K3_SDHCI_REFERENCE_CLOCK_HZ: u32 = 204_800_000;
// The K3 SDHCI divider is derived from the host-reported base clock. The
// removable SD slot is only reliable with a conservative identification clock,
// so report a doubled divider input while leaving the real MMIO clock setup
// unchanged.
const K3_SDHCI_SD_ONLY_DIVIDER_BASE_HZ: u32 = K3_SDHCI_REFERENCE_CLOCK_HZ * 2;

const SPACEMIT_SDHC_OP_EXT_REG: usize = 0x108;
const SDHC_OVRRD_CLK_OEN: u32 = 1 << 11;
const SDHC_FORCE_CLK_ON: u32 = 1 << 12;
const SPACEMIT_SDHC_LEGACY_CTRL_REG: usize = 0x10c;
const SDHC_GEN_PAD_CLK_ON: u32 = 1 << 6;
const SPACEMIT_SDHC_MMC_CTRL_REG: usize = 0x114;
const SDHC_MMC_CARD_MODE: u32 = 1 << 12;
const SPACEMIT_SDHC_TX_CFG_REG: usize = 0x11c;
const SDHC_TX_INT_CLK_SEL: u32 = 1 << 30;
const SPACEMIT_SDHC_DLINE_CTRL_REG: usize = 0x130;
const SDHC_DLINE_PU: u32 = 1 << 0;
const SPACEMIT_SDHC_PHY_CTRL_REG: usize = 0x160;
const SDHC_PHY_FUNC_EN: u32 = 1 << 0;
const SDHC_PHY_PLL_LOCK: u32 = 1 << 1;
const SPACEMIT_SDHC_PHY_PADCFG_REG: usize = 0x178;
const SDHC_PHY_DRIVE_SEL_MASK: u32 = 0x7;
const SDHC_PHY_DRIVE_SEL_DEFAULT: u32 = 4;
const SDHC_RX_BIAS_CTRL: u32 = 1 << 5;

struct AxKlibHostTimer;

static HOST_TIMER: AxKlibHostTimer = AxKlibHostTimer;

impl HostTimer for AxKlibHostTimer {
    fn now_ms(&self) -> u64 {
        axklib::time::monotonic_nanos() / 1_000_000
    }
}

#[derive(Clone, Copy)]
struct K3SdhciApmu {
    base: NonNull<u8>,
}

// The mapped APMU syscon is a shared SoC register block. This wrapper only
// touches the SDH0 reset/clock bits with single volatile read-modify-writes.
unsafe impl Send for K3SdhciApmu {}
unsafe impl Sync for K3SdhciApmu {}

struct SpacemitK3SdhciResetHook {
    apmu: K3SdhciApmu,
    media: K3SdhciMedia,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum K3SdhciMedia {
    SdOnly,
    MmcCapable,
}

impl K3SdhciApmu {
    const fn new(base: NonNull<u8>) -> Self {
        Self { base }
    }

    fn enable_sdhci(&self) {
        self.update_control(|value| value | K3_APMU_SDHCI_ENABLE_MASK);
    }

    fn assert_sdhci_resets(&self) {
        self.update_control(|value| value & !K3_APMU_SDHCI_RESET_DEASSERT_MASK);
    }

    fn update_control(&self, update: impl FnOnce(u32) -> u32) {
        let value = self.read_control();
        self.write_control(update(value));
    }

    fn read_control(&self) -> u32 {
        // Safety: `base` is the mapped K3 APMU MMIO page and this offset is
        // within the 0x400-byte syscon region from the K3 device tree.
        unsafe { core::ptr::read_volatile(self.reg_ptr()) }
    }

    fn write_control(&self, value: u32) {
        // Safety: same mapped register as `read_control`; volatile preserves
        // the MMIO side effect and no reference to the register is created.
        unsafe { core::ptr::write_volatile(self.reg_mut_ptr(), value) }
    }

    fn reg_ptr(&self) -> *const u32 {
        // Safety: address arithmetic stays inside the mapped APMU region.
        unsafe { self.base.as_ptr().add(K3_APMU_SDH0_CLK_RES_CTRL).cast() }
    }

    fn reg_mut_ptr(&self) -> *mut u32 {
        // Safety: address arithmetic stays inside the mapped APMU region.
        unsafe { self.base.as_ptr().add(K3_APMU_SDH0_CLK_RES_CTRL).cast() }
    }
}

impl HostResetHook for SpacemitK3SdhciResetHook {
    fn before_reset_all(&self, _host: &mut Sdhci) -> Result<(), Error> {
        self.apmu.assert_sdhci_resets();
        axklib::time::busy_wait(Duration::from_micros(1));
        self.apmu.enable_sdhci();
        Ok(())
    }

    fn after_reset(&self, host: &mut Sdhci) -> Result<(), Error> {
        self.apmu.enable_sdhci();
        init_k3_sdhci_vendor_regs(host, self.media)?;
        Ok(())
    }
}

crate::model_register!(
    name: "SpacemiT SDHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "spacemit,k3-sdhci",
                "spacemit,k1-x-sdhci",
                "spacemit,k1-pro-sdhci",
            ],
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
        "spacemit-k3-sdhci probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;
    let is_k3 = is_k3_sdhci(info);
    let apmu = if is_k3 {
        let apmu = map_k3_sdhci_apmu()?;
        apmu.enable_sdhci();
        Some(apmu)
    } else {
        None
    };

    let mut host = unsafe { Sdhci::new(mmio_base) };
    host.disable_completion_irq();
    let dt_clock_frequency = node_clock_frequency(info);
    let media = k3_sdhci_media(info);
    if let Some(apmu) = apmu {
        init_k3_sdhci_vendor_regs(&mut host, media).map_err(|err| {
            OnProbeError::other(format!(
                "spacemit-k3-sdhci vendor initialization failed: {err:?}"
            ))
        })?;
        if matches!(media, K3SdhciMedia::SdOnly) {
            host.set_base_clock_hz_override(K3_SDHCI_SD_ONLY_DIVIDER_BASE_HZ);
        } else if let Some(clock_hz) = dt_clock_frequency.filter(|clock_hz| *clock_hz != 0) {
            host.set_base_clock_hz_override(clock_hz);
        }
        if matches!(media, K3SdhciMedia::SdOnly) {
            host.use_fifo_data_path().map_err(|err| {
                OnProbeError::other(format!(
                    "spacemit-k3-sdhci FIFO data path setup failed: {err:?}"
                ))
            })?;
        }
        host.set_reset_hook(SpacemitK3SdhciResetHook { apmu, media });
    }
    host.set_timer(&HOST_TIMER);
    let dma_coherency = if is_k3 {
        dma_api::DmaCoherency::NonCoherent
    } else {
        crate::binding_resolver::dma_coherency_from_fdt(info)
    };
    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        dma_coherency,
        dma_api::DmaConstraints::new(SPACEMIT_SDHCI_DMA_MASK),
    ));
    let config = sdhci_rdif::dma_config("spacemit-k3-sdhci", 0, &dma);
    host.configure_dma(dma).map_err(|err| {
        OnProbeError::other(format!(
            "spacemit-k3-sdhci ADMA2 configuration failed: {err:?}"
        ))
    })?;

    let parts = host.into_parts();
    let mut card = SdMmcCard::new(parts.bus);
    let identity = format!("spacemit-k3-sdhci:{}", info.node.path());
    card.set_diagnostic_identity(identity);
    if is_k3 && matches!(media, K3SdhciMedia::SdOnly) {
        card.set_sd_wide_bus_selection_enabled(false);
        card.set_sd_speed_selection_enabled(false);
    }
    let dev = sdhci_rdif::BlockDevice::new_initializing(
        card,
        parts.irq,
        config,
        card_init_preference(info),
    );
    probe.register_block(dev)?;
    Ok(())
}

fn map_k3_sdhci_apmu() -> Result<K3SdhciApmu, OnProbeError> {
    let base = iomap(K3_APMU_BASE, K3_APMU_SIZE)?;
    Ok(K3SdhciApmu::new(base))
}

fn is_k3_sdhci(info: &FdtInfo<'_>) -> bool {
    info.node
        .as_node()
        .compatibles()
        .any(|compatible| compatible == "spacemit,k3-sdhci")
}

fn init_k3_sdhci_vendor_regs(host: &mut Sdhci, media: K3SdhciMedia) -> Result<(), Error> {
    let base = NonNull::new(host.mmio_base() as *mut u8).ok_or(Error::InvalidArgument)?;
    init_k3_sdhci_vendor_regs_at(base, media);
    Ok(())
}

fn init_k3_sdhci_vendor_regs_at(base: NonNull<u8>, media: K3SdhciMedia) {
    set_bits(
        base,
        SPACEMIT_SDHC_PHY_CTRL_REG,
        SDHC_PHY_FUNC_EN | SDHC_PHY_PLL_LOCK,
    );
    update_u32(base, SPACEMIT_SDHC_PHY_PADCFG_REG, |value| {
        (value & !SDHC_PHY_DRIVE_SEL_MASK) | SDHC_RX_BIAS_CTRL | SDHC_PHY_DRIVE_SEL_DEFAULT
    });
    match media {
        K3SdhciMedia::SdOnly => {
            clear_bits(base, SPACEMIT_SDHC_MMC_CTRL_REG, SDHC_MMC_CARD_MODE);
            set_bits(
                base,
                SPACEMIT_SDHC_OP_EXT_REG,
                SDHC_OVRRD_CLK_OEN | SDHC_FORCE_CLK_ON,
            );
        }
        K3SdhciMedia::MmcCapable => {
            set_bits(base, SPACEMIT_SDHC_MMC_CTRL_REG, SDHC_MMC_CARD_MODE);
            clear_bits(
                base,
                SPACEMIT_SDHC_OP_EXT_REG,
                SDHC_OVRRD_CLK_OEN | SDHC_FORCE_CLK_ON,
            );
        }
    }
    set_bits(base, SPACEMIT_SDHC_LEGACY_CTRL_REG, SDHC_GEN_PAD_CLK_ON);
    set_bits(base, SPACEMIT_SDHC_TX_CFG_REG, SDHC_TX_INT_CLK_SEL);
    set_bits(base, SPACEMIT_SDHC_DLINE_CTRL_REG, SDHC_DLINE_PU);
}

fn node_clock_frequency(info: &FdtInfo<'_>) -> Option<u32> {
    info.node
        .as_node()
        .get_property("clock-frequency")
        .and_then(|prop| prop.get_u32())
}

fn k3_sdhci_media(info: &FdtInfo<'_>) -> K3SdhciMedia {
    let node = info.node.as_node();
    if node.get_property("no-mmc").is_some() || node.get_property("cd-gpios").is_some() {
        K3SdhciMedia::SdOnly
    } else {
        K3SdhciMedia::MmcCapable
    }
}

fn read_u32(base: NonNull<u8>, offset: usize) -> u32 {
    // Safety: callers pass an ioremapped SDHCI MMIO base and a register
    // offset within the 0x200-byte SpacemiT controller region.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(offset).cast()) }
}

fn write_u32(base: NonNull<u8>, offset: usize, value: u32) {
    // Safety: same MMIO region as `read_u32`; volatile preserves register
    // side effects and does not create references to device memory.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(offset).cast(), value) }
}

fn update_u32(base: NonNull<u8>, offset: usize, update: impl FnOnce(u32) -> u32) {
    let value = read_u32(base, offset);
    write_u32(base, offset, update(value));
}

fn set_bits(base: NonNull<u8>, offset: usize, bits: u32) {
    update_u32(base, offset, |value| value | bits);
}

fn clear_bits(base: NonNull<u8>, offset: usize, bits: u32) {
    update_u32(base, offset, |value| value & !bits);
}

fn card_init_preference(info: &FdtInfo<'_>) -> CardInitPreference {
    let node = info.node.as_node();
    if node.get_property("no-sd").is_some() || node.get_property("non-removable").is_some() {
        CardInitPreference::MmcFirst
    } else {
        CardInitPreference::SdFirst
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    fn fake_apmu() -> (Vec<u32>, K3SdhciApmu) {
        let mut regs = alloc::vec![0_u32; K3_APMU_SIZE / core::mem::size_of::<u32>()];
        let base = NonNull::new(regs.as_mut_ptr().cast::<u8>()).unwrap();
        (regs, K3SdhciApmu::new(base))
    }

    fn fake_sdhci_regs() -> (Vec<u32>, NonNull<u8>) {
        let mut regs = alloc::vec![0_u32; 0x200 / core::mem::size_of::<u32>()];
        let base = NonNull::new(regs.as_mut_ptr().cast::<u8>()).unwrap();
        (regs, base)
    }

    #[test]
    fn spacemit_sdhci_dma_mask_allows_high_memory_descriptors() {
        assert!(SPACEMIT_SDHCI_DMA_MASK > u32::MAX as u64);
    }

    #[test]
    fn k3_sdhci_apmu_enable_deasserts_resets_and_gates_clocks() {
        let (regs, apmu) = fake_apmu();

        apmu.enable_sdhci();

        assert_eq!(
            regs[K3_APMU_SDH0_CLK_RES_CTRL / core::mem::size_of::<u32>()]
                & K3_APMU_SDHCI_ENABLE_MASK,
            K3_APMU_SDHCI_ENABLE_MASK
        );
    }

    #[test]
    fn k3_sdhci_apmu_assert_resets_preserves_clock_gates() {
        let (mut regs, apmu) = fake_apmu();
        regs[K3_APMU_SDH0_CLK_RES_CTRL / core::mem::size_of::<u32>()] = K3_APMU_SDHCI_ENABLE_MASK;

        apmu.assert_sdhci_resets();

        let ctrl = regs[K3_APMU_SDH0_CLK_RES_CTRL / core::mem::size_of::<u32>()];
        assert_eq!(ctrl & K3_APMU_SDHCI_RESET_DEASSERT_MASK, 0);
        assert_eq!(
            ctrl & (K3_APMU_SDH_AXI_CLOCK_GATE | K3_APMU_SDH0_CLOCK_GATE),
            K3_APMU_SDH_AXI_CLOCK_GATE | K3_APMU_SDH0_CLOCK_GATE
        );
    }

    #[test]
    fn k3_sdhci_vendor_init_programs_identification_defaults() {
        let (regs, base) = fake_sdhci_regs();

        init_k3_sdhci_vendor_regs_at(base, K3SdhciMedia::MmcCapable);

        assert_eq!(
            regs[SPACEMIT_SDHC_PHY_CTRL_REG / core::mem::size_of::<u32>()]
                & (SDHC_PHY_FUNC_EN | SDHC_PHY_PLL_LOCK),
            SDHC_PHY_FUNC_EN | SDHC_PHY_PLL_LOCK
        );
        assert_eq!(
            regs[SPACEMIT_SDHC_PHY_PADCFG_REG / core::mem::size_of::<u32>()]
                & (SDHC_PHY_DRIVE_SEL_MASK | SDHC_RX_BIAS_CTRL),
            SDHC_PHY_DRIVE_SEL_DEFAULT | SDHC_RX_BIAS_CTRL
        );
        assert_eq!(
            regs[SPACEMIT_SDHC_MMC_CTRL_REG / core::mem::size_of::<u32>()] & SDHC_MMC_CARD_MODE,
            SDHC_MMC_CARD_MODE
        );
        assert_eq!(
            regs[SPACEMIT_SDHC_LEGACY_CTRL_REG / core::mem::size_of::<u32>()] & SDHC_GEN_PAD_CLK_ON,
            SDHC_GEN_PAD_CLK_ON
        );
        assert_eq!(
            regs[SPACEMIT_SDHC_TX_CFG_REG / core::mem::size_of::<u32>()] & SDHC_TX_INT_CLK_SEL,
            SDHC_TX_INT_CLK_SEL
        );
        assert_eq!(
            regs[SPACEMIT_SDHC_OP_EXT_REG / core::mem::size_of::<u32>()]
                & (SDHC_OVRRD_CLK_OEN | SDHC_FORCE_CLK_ON),
            0
        );
        assert_eq!(
            regs[SPACEMIT_SDHC_DLINE_CTRL_REG / core::mem::size_of::<u32>()] & SDHC_DLINE_PU,
            SDHC_DLINE_PU
        );
    }

    #[test]
    fn k3_sdhci_vendor_init_uses_sd_only_mode_for_removable_slots() {
        let (mut regs, base) = fake_sdhci_regs();
        regs[SPACEMIT_SDHC_MMC_CTRL_REG / core::mem::size_of::<u32>()] = SDHC_MMC_CARD_MODE;

        init_k3_sdhci_vendor_regs_at(base, K3SdhciMedia::SdOnly);

        assert_eq!(
            regs[SPACEMIT_SDHC_MMC_CTRL_REG / core::mem::size_of::<u32>()] & SDHC_MMC_CARD_MODE,
            0
        );
        assert_eq!(
            regs[SPACEMIT_SDHC_OP_EXT_REG / core::mem::size_of::<u32>()]
                & (SDHC_OVRRD_CLK_OEN | SDHC_FORCE_CLK_ON),
            SDHC_OVRRD_CLK_OEN | SDHC_FORCE_CLK_ON
        );
    }
}
