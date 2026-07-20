//! Broadcom BCM2835/BCM2711 interrupt-driven SDHCI integration.

use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicU8, Ordering},
};

use log::info;
use rdif_block::{
    BlkError, BlockIrqSource, ControllerInitEndpoint, DeviceInfo, Interface, IrqSourceList,
    LifecycleEndpoint, QueueHandle, QueueLimits,
};
use rdrive::{
    probe::{OnProbeError, fdt::ClockLine},
    register::{FdtInfo, ProbeFdt},
};
use sdhci_host::{BroadcomController, Sdhci, rdif as sdhci_rdif};
use sdmmc_protocol::{
    rdif::StagedBlockDevice,
    sdio::{CardInitPreference, OwnedSdioInit, SdioSdmmc},
};

use crate::{binding_info_from_fdt, block::ProbeFdtBlock, mmio::iomap};

pub const DEVICE_NAME: &str = "bcm-sdhci";

const BCM2835_COMPATIBLE: &str = "brcm,bcm2835-sdhci";
const BCM2711_COMPATIBLE: &str = "brcm,bcm2711-emmc2";
const SDHCI_REGISTER_BYTES: u64 = 0x100;
const CLOCK_UNPREPARED: u8 = 0;
const CLOCK_PREPARING: u8 = 1;
const CLOCK_READY: u8 = 2;

crate::model_register!(
    name: "Broadcom SDHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[BCM2835_COMPATIBLE, BCM2711_COMPATIBLE],
        on_probe: probe,
    }],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let node_name = alloc::string::String::from(info.node.name());
    let controller = broadcom_controller(info)?;
    ensure_completion_irq(info)?;
    let register =
        info.node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(alloc::format!("[{}] has no reg", info.node.name()))
        })?;
    let register_bytes = register.size.ok_or_else(|| {
        OnProbeError::other(alloc::format!(
            "[{}] SDHCI reg has no size",
            info.node.name()
        ))
    })?;
    if register_bytes < SDHCI_REGISTER_BYTES {
        return Err(OnProbeError::other(alloc::format!(
            "[{}] SDHCI reg size {register_bytes:#x} is smaller than {SDHCI_REGISTER_BYTES:#x}",
            info.node.name()
        )));
    }
    let clock = controller_clock(info)?;
    let base_clock_hz = controller_clock_hz(info, &clock)?;
    let mmio = iomap(register.address as usize, register_bytes as usize)?;

    // SAFETY: `iomap` returned the exclusive mapping described by this FDT
    // node, and the controller profile fixes its 32-bit-only register ABI.
    let mut host = unsafe { Sdhci::new_broadcom(mmio, controller) };
    host.set_base_clock_hz(base_clock_hz);
    let config = match controller {
        BroadcomController::Bcm2835 => sdhci_rdif::fifo_config(DEVICE_NAME, 0),
        BroadcomController::Bcm2711 => {
            let dma = axklib::dma::device_with_mask(u32::MAX as u64);
            host.set_dma(dma.clone());
            sdhci_rdif::dma_config(DEVICE_NAME, 0, dma)
        }
    };
    let mut card = SdioSdmmc::new_host2_timed(host);
    card.set_sd_uhs_selection_enabled(false);
    let staged = StagedBlockDevice::new(
        OwnedSdioInit::new(card, card_init_preference(info)?),
        config,
        sdhci_rdif::device,
    );
    let staged = ClockPreparedBlock::new(staged, clock);
    let registered = probe.register_block(staged)?;
    info!(
        "bcm-sdhci staged: node={} controller={controller:?} clock={}Hz device={registered:?}",
        node_name,
        base_clock_hz.get(),
    );
    Ok(())
}

fn broadcom_controller(info: &FdtInfo<'_>) -> Result<BroadcomController, OnProbeError> {
    info.node
        .as_node()
        .get_property("compatible")
        .and_then(|compatible| {
            compatible
                .as_str_iter()
                .find_map(controller_from_compatible)
        })
        .ok_or_else(|| {
            OnProbeError::other(alloc::format!(
                "[{}] has no supported Broadcom SDHCI compatible",
                info.node.name()
            ))
        })
}

fn controller_from_compatible(compatible: &str) -> Option<BroadcomController> {
    match compatible {
        BCM2835_COMPATIBLE => Some(BroadcomController::Bcm2835),
        BCM2711_COMPATIBLE => Some(BroadcomController::Bcm2711),
        _ => None,
    }
}

fn ensure_completion_irq(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    let binding = binding_info_from_fdt(info)?;
    if binding.irq_for_source(0).is_none() {
        return Err(OnProbeError::other(alloc::format!(
            "[{}] has no completion interrupt",
            info.node.name()
        )));
    }
    Ok(())
}

fn controller_clock(info: &FdtInfo<'_>) -> Result<ClockLine, OnProbeError> {
    info.clock_lines()?.into_iter().next().ok_or_else(|| {
        OnProbeError::other(alloc::format!(
            "[{}] has no SDHCI input clock",
            info.node.name()
        ))
    })
}

fn controller_clock_hz(info: &FdtInfo<'_>, clock: &ClockLine) -> Result<NonZeroU32, OnProbeError> {
    let rate = clock.rate()?;
    let rate = u32::try_from(rate).map_err(|_| {
        OnProbeError::other(alloc::format!(
            "[{}] SDHCI input clock {rate}Hz exceeds u32",
            info.node.name()
        ))
    })?;
    NonZeroU32::new(rate).ok_or_else(|| {
        OnProbeError::other(alloc::format!(
            "[{}] SDHCI input clock is zero",
            info.node.name()
        ))
    })
}

fn card_init_preference(info: &FdtInfo<'_>) -> Result<CardInitPreference, OnProbeError> {
    let node = info.node.as_node();
    let no_sd = node.get_property("no-sd").is_some();
    let no_mmc = node.get_property("no-mmc").is_some();
    if no_sd && no_mmc {
        return Err(OnProbeError::other(alloc::format!(
            "[{}] disables both SD and MMC",
            info.node.name()
        )));
    }
    Ok(if no_sd || node.get_property("non-removable").is_some() {
        CardInitPreference::MmcFirst
    } else if no_mmc {
        CardInitPreference::SdOnly
    } else {
        CardInitPreference::MmcFirst
    })
}

/// Enables the controller input clock only after the runtime has installed
/// the initialization IRQ action and asks the interface to unmask delivery.
struct ClockPreparedBlock<T> {
    inner: T,
    clock: ClockLine,
    clock_state: AtomicU8,
}

impl<T> ClockPreparedBlock<T> {
    fn new(inner: T, clock: ClockLine) -> Self {
        Self {
            inner,
            clock,
            clock_state: AtomicU8::new(CLOCK_UNPREPARED),
        }
    }

    fn prepare_clock(&self) -> Result<(), BlkError> {
        match self.clock_state.compare_exchange(
            CLOCK_UNPREPARED,
            CLOCK_PREPARING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => match self.clock.enable() {
                Ok(()) => {
                    self.clock_state.store(CLOCK_READY, Ordering::Release);
                    Ok(())
                }
                Err(error) => {
                    self.clock_state.store(CLOCK_UNPREPARED, Ordering::Release);
                    log::warn!("bcm-sdhci: failed to enable input clock: {error}");
                    Err(BlkError::Other("BCM SDHCI input clock enable failed"))
                }
            },
            Err(CLOCK_READY) => Ok(()),
            Err(_) => Err(BlkError::Busy),
        }
    }
}

impl<T: Interface> rdif_block::DriverGeneric for ClockPreparedBlock<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }
}

impl<T: Interface> Interface for ClockPreparedBlock<T> {
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
        self.inner.controller_init()
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        self.inner.lifecycle()
    }

    fn device_info(&self) -> DeviceInfo {
        self.inner.device_info()
    }

    fn queue_limits(&self) -> QueueLimits {
        self.inner.queue_limits()
    }

    fn create_queue(&mut self) -> Option<QueueHandle> {
        self.inner.create_queue()
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        self.prepare_clock()?;
        self.inner.enable_irq()
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        self.inner.disable_irq()
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.is_irq_enabled()
    }

    fn irq_sources(&self) -> IrqSourceList {
        self.inner.irq_sources()
    }

    fn take_irq_source(&mut self, source_id: usize) -> Option<BlockIrqSource> {
        self.inner.take_irq_source(source_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_compatible_selects_distinct_broadcom_profile() {
        assert_eq!(
            controller_from_compatible(BCM2835_COMPATIBLE),
            Some(BroadcomController::Bcm2835)
        );
        assert_eq!(
            controller_from_compatible(BCM2711_COMPATIBLE),
            Some(BroadcomController::Bcm2711)
        );
        assert_eq!(controller_from_compatible("brcm,unknown-sdhci"), None);
    }
}
