use alloc::format;
use core::time::Duration;

use log::{info, warn};
use phytium_mci_host::PhytiumMci;
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdmmc_protocol::{
    Error, OperationPoll,
    error::Phase,
    sdio::{CardInfo, CardInitPreference, SdioInitScratch, SdioSdmmc},
};

use crate::{
    block::{
        ProbeFdtBlock, SharedDriver,
        sdmmc::{SdmmcBlockConfig, SdmmcBlockDevice},
    },
    mmio::iomap,
};

type PhytiumSdMmc = SdioSdmmc<PhytiumMci>;

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
    info!("phytium-mci: reset controller");
    host.reset_and_init()
        .map_err(|e| init_error(base_reg.address, mmio_size, e))?;

    info!("phytium-mci: initialize card");
    let mut card = SdioSdmmc::new(host);
    card.set_sd_uhs_selection_enabled(false);
    let preference = card_init_preference(info);
    let card_info = poll_card_init(&mut card, preference).map_err(|e| {
        warn!("phytium-mci: card init failed: {:?}", e);
        card_init_error(base_reg.address, mmio_size, e)
    })?;
    info!(
        "phytium-mci card: kind={:?} high_capacity={} rca={} ocr={:#010x} capacity_blocks={:?} \
         cid={} ext_csd={}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
        card_info.cid.is_some(),
        card_info.ext_csd.is_some()
    );

    let raw = SharedDriver::new(card);
    let dev = SdmmcBlockDevice::new(
        raw,
        SdmmcBlockConfig::dma("phytium-mci", card_info.capacity_blocks.unwrap_or(0), true),
    );
    let irq = probe.register_block(dev)?;
    info!("phytium-mci block device registered irq={:?}", irq);
    Ok(())
}

fn poll_card_init(
    card: &mut PhytiumSdMmc,
    preference: CardInitPreference,
) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request = card.submit_init_with_preference(preference, &mut scratch)?;
    loop {
        match card.poll_init_request(&mut request)? {
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

fn card_init_preference(info: &FdtInfo<'_>) -> CardInitPreference {
    let node = info.node.as_node();
    if node.get_property("no-sd").is_some() || node.get_property("non-removable").is_some() {
        CardInitPreference::MmcFirst
    } else {
        CardInitPreference::SdFirst
    }
}

fn init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    OnProbeError::other(format!(
        "failed to initialize Phytium MCI device at [PA:{:?}, SZ:0x{:x}): {err:?}",
        address, size
    ))
}

fn card_init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "phytium-mci: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping controller: {err:?}",
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
