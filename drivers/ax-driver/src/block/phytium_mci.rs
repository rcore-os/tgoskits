use log::info;
use phytium_mci_host::{PhytiumMci, rdif as phytium_rdif};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdmmc_protocol::{
    rdif::StagedBlockDevice,
    sdio::{CardInitPreference, OwnedSdioInit, SdioSdmmc},
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
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    host.set_dma(dma.clone());

    let mut card = SdioSdmmc::new_host2_timed(host);
    card.set_sd_uhs_selection_enabled(false);
    let staged = StagedBlockDevice::new(
        OwnedSdioInit::new(card, card_init_preference(info)),
        phytium_rdif::dma_config("phytium-mci", 0, dma),
        phytium_rdif::device,
    );
    let irq = probe.register_block(staged)?;
    info!("phytium-mci controller staged irq={irq:?}");
    Ok(())
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
    use super::*;

    #[test]
    fn phytium_mci_block_io_uses_dma_config_with_irq_completion() {
        let config = phytium_rdif::dma_config(
            "phytium-mci",
            8,
            axklib::dma::device_with_mask(u32::MAX as u64),
        );

        assert!(config.uses_dma());
        assert!(config.supports_runtime_queue());
    }
}
