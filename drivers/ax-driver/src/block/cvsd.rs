#[cfg(not(test))]
use alloc::format;
#[cfg(not(test))]
use core::time::Duration;

use cv181x_sdhci::rdif as cv181x_rdif;
#[cfg(not(test))]
use cv181x_sdhci::{
    CV181X_SYSCON_REQUIRED_SIZE, CV181X_TOP_SYSCON_BASE, Cv181xConfig, Cv181xMmio, Cv181xSdhci,
};
#[cfg(not(test))]
use log::{info, warn};
#[cfg(not(test))]
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdmmc_protocol::{Error, error::Phase};
#[cfg(not(test))]
use sdmmc_protocol::{
    OperationPoll,
    sdio::{
        card::{CardInfo, SdioSdmmc},
        host::BusWidth,
        host2::SdioHost2Adapter,
        init::{CardInitPreference, SdioInitScratch},
    },
};

#[cfg(not(test))]
use crate::{block::ProbeFdtBlock, mmio::iomap};

pub const DEVICE_NAME: &str = "cvsd";

#[cfg(not(test))]
const DEFAULT_SDMMIF_SIZE: usize = 0x1000;
#[cfg(not(test))]
const DEFAULT_SYSCON_SIZE: usize = 0x8000;
const CVSD_IRQ_DRIVEN: bool = true;

#[cfg(not(test))]
type CvsdCard = SdioSdmmc<SdioHost2Adapter<Cv181xSdhci>>;

#[cfg(not(test))]
#[derive(Clone, Copy)]
struct CvsdFdtPolicy {
    no_sd: bool,
    no_mmc: bool,
    no_sdio: bool,
    non_removable: bool,
}

#[cfg(not(test))]
crate::model_register!(
    name: "FDT CVSD",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["cvitek,cv181x-sd"],
        on_probe: probe_fdt,
    }],
);

#[cfg(not(test))]
fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let sdmmc =
        info.node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(alloc::format!("[{}] has no reg", info.node.name()))
        })?;
    let (syscon_addr, syscon_size) = cv181x_syscon(info)?;

    let core = iomap(
        sdmmc.address as usize,
        sdmmc.size.unwrap_or(DEFAULT_SDMMIF_SIZE as u64) as usize,
    )?;
    let syscon = iomap(syscon_addr, syscon_size)?;

    let config = cv181x_config(info);
    let policy = cvsd_fdt_policy(info);
    info!(
        "cvsd probe: node={}, src={}Hz min={}Hz max={}Hz bus_width={:?} no_1v8={} no_mmc={} \
         no_sdio={} cd_gpio={}",
        info.node.name(),
        config.src_frequency_hz,
        config.min_frequency_hz,
        config.max_frequency_hz,
        config.max_bus_width,
        config.no_1v8,
        policy.no_mmc,
        policy.no_sdio,
        config.has_card_detect_gpio,
    );

    let host = unsafe { Cv181xSdhci::new(Cv181xMmio::new(core, syscon), config) };
    let mut card = SdioSdmmc::new_host2(host);
    card.set_sd_uhs_selection_enabled(false);

    let card_info = poll_card_init(&mut card, card_init_preference(policy)).map_err(|err| {
        warn!("cvsd: card init failed: {:?}", err);
        card_init_error(
            sdmmc.address,
            sdmmc.size.unwrap_or(DEFAULT_SDMMIF_SIZE as u64),
            err,
        )
    })?;
    info!(
        "cvsd card: kind={:?} high_capacity={} rca={} ocr={:#010x} capacity_blocks={:?} cid={} \
         ext_csd={}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
        card_info.cid.is_some(),
        card_info.ext_csd.is_some()
    );

    let dev = cv181x_rdif::device(
        card,
        cvsd_block_config(card_info.capacity_blocks.unwrap_or(0)),
    );
    let irq = probe.register_block(dev)?;
    info!("cvsd block device registered irq={:?}", irq);
    Ok(())
}

#[cfg(not(test))]
fn cv181x_syscon(info: &FdtInfo<'_>) -> Result<(usize, usize), OnProbeError> {
    for node in info.find_compatible(&["syscon"]) {
        let Some(reg) = node.regs().into_iter().next() else {
            continue;
        };
        if reg.address == CV181X_TOP_SYSCON_BASE {
            return Ok((reg.address as usize, cv181x_syscon_map_size(reg.size)?));
        }
    }

    Err(OnProbeError::other(format!(
        "CVSD TOP syscon at PA:0x{CV181X_TOP_SYSCON_BASE:x} not found in FDT"
    )))
}

#[cfg(not(test))]
fn cv181x_syscon_map_size(size: Option<u64>) -> Result<usize, OnProbeError> {
    let map_size = size.unwrap_or(DEFAULT_SYSCON_SIZE as u64);
    if map_size < CV181X_SYSCON_REQUIRED_SIZE as u64 {
        return Err(OnProbeError::other(format!(
            "CVSD TOP syscon reg size 0x{map_size:x} is smaller than required 0x{:x}",
            CV181X_SYSCON_REQUIRED_SIZE
        )));
    }
    Ok(map_size as usize)
}

#[cfg(not(test))]
fn cv181x_config(info: &FdtInfo<'_>) -> Cv181xConfig {
    let node = info.node.as_node();
    Cv181xConfig {
        src_frequency_hz: fdt_u32(info, "src-frequency", 375_000_000),
        min_frequency_hz: fdt_u32(info, "min-frequency", 400_000),
        max_frequency_hz: fdt_u32(info, "max-frequency", 25_000_000),
        max_bus_width: cv181x_bus_width(info),
        no_1v8: node.get_property("no-1-8-v").is_some(),
        has_card_detect_gpio: node.get_property("cvi-cd-gpios").is_some()
            || node.get_property("cd-gpios").is_some(),
        touch_power_enable_pin: false,
    }
    .normalized()
}

#[cfg(not(test))]
fn cvsd_fdt_policy(info: &FdtInfo<'_>) -> CvsdFdtPolicy {
    let node = info.node.as_node();
    CvsdFdtPolicy {
        no_sd: node.get_property("no-sd").is_some(),
        no_mmc: node.get_property("no-mmc").is_some(),
        no_sdio: node.get_property("no-sdio").is_some(),
        non_removable: node.get_property("non-removable").is_some(),
    }
}

#[cfg(not(test))]
fn cv181x_bus_width(info: &FdtInfo<'_>) -> BusWidth {
    match fdt_u32(info, "bus-width", 4) {
        1 => BusWidth::Bit1,
        4 => BusWidth::Bit4,
        8 => {
            warn!("cvsd: 8-bit bus-width requested for 4-bit SD0 pads; clamping to 4-bit");
            BusWidth::Bit4
        }
        other => {
            warn!("cvsd: unsupported bus-width {other}; using 4-bit");
            BusWidth::Bit4
        }
    }
}

#[cfg(not(test))]
fn fdt_u32(info: &FdtInfo<'_>, name: &str, default: u32) -> u32 {
    info.node
        .as_node()
        .get_property(name)
        .and_then(|prop| prop.get_u32())
        .unwrap_or(default)
}

fn cvsd_block_config(capacity_blocks: u64) -> cv181x_rdif::BlockConfig {
    cv181x_rdif::fifo_config(DEVICE_NAME, capacity_blocks, CVSD_IRQ_DRIVEN)
}

#[cfg(not(test))]
fn poll_card_init(card: &mut CvsdCard, preference: CardInitPreference) -> Result<CardInfo, Error> {
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

#[cfg(not(test))]
fn card_init_preference(policy: CvsdFdtPolicy) -> CardInitPreference {
    if policy.no_sd || policy.non_removable {
        if policy.no_mmc {
            warn!("cvsd: FDT has both no-sd/non-removable and no-mmc; probing SD only");
            return CardInitPreference::SdOnly;
        }
        CardInitPreference::MmcFirst
    } else if policy.no_mmc {
        CardInitPreference::SdOnly
    } else {
        CardInitPreference::SdFirst
    }
}

#[cfg(not(test))]
fn init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    OnProbeError::other(format!(
        "failed to initialize CVSD device at [PA:{:?}, SZ:0x{:x}): {err:?}",
        address, size
    ))
}

#[cfg(not(test))]
fn card_init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "cvsd: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping controller: {err:?}",
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

    #[test]
    fn cvsd_block_io_uses_irq_driven_sdmmc_rdif_fifo_config() {
        let config = cvsd_block_config(8);

        assert_eq!(config.name, DEVICE_NAME);
        assert_eq!(config.capacity_blocks, 8);
        assert!(!config.uses_dma());
        assert!(config.irq_driven);
    }
}
