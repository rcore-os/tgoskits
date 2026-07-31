use alloc::format;

use log::info;
use phytium_mci_host::{IDMAC_MAX_BLOCKS, IDMAC_MAX_TRANSFER_SIZE, PhytiumMci};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdmmc_protocol::{
    rdif::{config::BlockConfig, device::BlockDevice},
    sdio::{card::SdioSdmmc, init::CardInitPreference},
};

use crate::{block::ProbeFdtBlock, mmio::iomap};

crate::model_register!(
    name: "Phytium MCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["phytium,mci"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let node = info.node.as_node();
    let no_sd = node.get_property("no-sd").is_some();
    let no_mmc = node.get_property("no-mmc").is_some();
    if !supports_memory_card(no_sd, no_mmc) {
        info!(
            "phytium-mci: skip SDIO-only node {} in block probe",
            info.node.name()
        );
        return Ok(());
    }
    let non_removable = node.get_property("non-removable").is_some();
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
        "phytium-mci probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;

    let mut host = unsafe { PhytiumMci::new(mmio_base) };
    if !non_removable && !host.card_present() {
        info!(
            "phytium-mci: skip removable node {} without media",
            info.node.name()
        );
        return Ok(());
    }
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let block_config = phytium_block_config(&dma);
    host.configure_dma(dma).map_err(|err| {
        OnProbeError::other(format!("phytium-mci IDMAC configuration failed: {err:?}"))
    })?;

    info!("phytium-mci: defer card initialization to IRQ-driven hctx");
    let mut card = SdioSdmmc::new(host);
    card.set_sd_uhs_selection_enabled(false);
    let preference = card_init_preference(info);
    let dev = BlockDevice::new_initializing(card, block_config, preference);
    let irq = probe.register_block(dev)?;
    info!("phytium-mci block device registered irq={:?}", irq);
    Ok(())
}

const fn supports_memory_card(no_sd: bool, no_mmc: bool) -> bool {
    !(no_sd && no_mmc)
}

fn card_init_preference(info: &FdtInfo<'_>) -> CardInitPreference {
    let node = info.node.as_node();
    if node.get_property("no-sd").is_some() || node.get_property("non-removable").is_some() {
        CardInitPreference::MmcFirst
    } else {
        CardInitPreference::SdFirst
    }
}

fn phytium_block_config(dma: &dma_api::DeviceDma) -> BlockConfig {
    BlockConfig::dma("phytium-mci", 0, dma)
        .with_max_blocks_per_request(IDMAC_MAX_BLOCKS)
        .with_max_segment_size(IDMAC_MAX_TRANSFER_SIZE)
}

#[cfg(test)]
mod tests {
    use axklib::{
        AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle,
        IrqId, Klib, PhysAddr, VirtAddr, impl_trait,
    };

    use super::*;

    struct KlibImpl;

    impl_trait! {
        impl Klib for KlibImpl {
            fn mem_iomap(_addr: PhysAddr, _size: usize) -> AxResult<VirtAddr> {
                Err(AxError::Unsupported)
            }

            fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
                PhysAddr::from_usize(addr.as_usize())
            }

            fn mem_make_dma_coherent_uncached(
                _addr: VirtAddr,
                _size: usize,
            ) -> axklib::DmaCoherentMappingOutcome {
                axklib::DmaCoherentMappingOutcome::NotStarted(AxError::Unsupported)
            }

            fn mem_restore_dma_cached(_addr: VirtAddr, _size: usize) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn dma_cache_clean(_addr: VirtAddr, _size: usize) {}

            fn dma_cache_invalidate(_addr: VirtAddr, _size: usize) {}

            fn dma_cache_clean_invalidate(_addr: VirtAddr, _size: usize) {}

            fn dma_alloc_pages(
                _dma_mask: u64,
                _num_pages: usize,
                _align: usize,
            ) -> AxResult<VirtAddr> {
                Err(AxError::Unsupported)
            }

            fn dma_dealloc_pages(_addr: VirtAddr, _num_pages: usize) {}

            fn time_busy_wait(_dur: core::time::Duration) {}

            fn time_monotonic_nanos() -> u64 {
                0
            }

            fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool {
                false
            }

            fn irq_set_enable(_irq: IrqId, _enabled: bool) -> AxResult {
                Ok(())
            }

            fn irq_request_shared(
                _irq: IrqId,
                _handler: BoxedIrqHandler,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_request_shared_disabled(
                _irq: IrqId,
                _handler: BoxedIrqHandler,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_request_percpu(
                _irq: IrqId,
                _cpus: IrqCpuMask,
                _handler: ConcurrentBoxedIrqHandler,
            ) -> AxResult<IrqHandle> {
                Err(AxError::Unsupported)
            }

            fn irq_free(_handle: IrqHandle) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn irq_enable(_handle: IrqHandle) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn irq_disable(_handle: IrqHandle) -> AxResult {
                Err(AxError::Unsupported)
            }
        }
    }

    #[test]
    fn phytium_block_limits_match_persistent_idmac_ring() {
        let dma = axklib::dma::device_with_mask(u32::MAX as u64);
        let config = phytium_block_config(&dma);

        assert_eq!(config.name(), "phytium-mci");
        assert_eq!(config.limits.dma_mask, u32::MAX as u64);
        assert_eq!(config.limits.max_inflight, 1);
        assert_eq!(config.limits.max_submit_batch, 1);
        assert_eq!(config.limits.max_blocks_per_request, IDMAC_MAX_BLOCKS);
        assert_eq!(config.limits.max_segment_size, IDMAC_MAX_TRANSFER_SIZE);
    }

    #[test]
    fn sdio_only_node_is_not_registered_as_a_block_controller() {
        assert!(supports_memory_card(false, false));
        assert!(supports_memory_card(true, false));
        assert!(supports_memory_card(false, true));
        assert!(!supports_memory_card(true, true));
    }
}
