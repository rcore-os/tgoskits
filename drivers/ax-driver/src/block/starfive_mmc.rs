use alloc::format;
use core::time::Duration;

use dwmmc_host::{DwMmc, rdif as dwmmc_rdif};
use log::{info, warn};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdmmc_protocol::{
    Error, OperationPoll,
    error::Phase,
    sdio::{
        card::{CardInfo, SdioSdmmc},
        host2::SdioHost2Adapter,
        init::SdioInitScratch,
    },
};

use crate::{block::ProbeFdtBlock, mmio::iomap};

const STARFIVE_JH7110_MMC1_BASE: u64 = 0x1602_0000;
const DWMMC_STABLE_REFERENCE_CLOCK: u32 = 50_000_000;

type StarFiveDwMmc = SdioSdmmc<SdioHost2Adapter<DwMmc>>;

crate::model_register!(
    name: "StarFive JH7110 MMC",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["starfive,jh7110-mmc"],
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
        .ok_or(OnProbeError::other(format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let address = base_reg.address;
    let mmio_size = base_reg.size.unwrap_or(0x1000);
    if address != STARFIVE_JH7110_MMC1_BASE {
        info!(
            "starfive-jh7110-dwmmc: skipping non-microSD controller {} at {:#x}",
            info.node.name(),
            address
        );
        return Err(OnProbeError::NotMatch);
    }

    info!(
        "starfive-jh7110-dwmmc probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        address,
        mmio_size
    );
    let mmio_base = iomap(address as usize, mmio_size as usize)?;

    let mut host = unsafe { DwMmc::new(mmio_base) };
    host.set_reference_clock(reference_clock(info));

    info!("starfive-jh7110-dwmmc: reset controller");
    host.reset_and_init()
        .map_err(|err| init_error(address, mmio_size, err))?;

    info!("starfive-jh7110-dwmmc: initialize card");
    let mut sd = SdioSdmmc::new_host2(host);
    sd.set_sd_speed_selection_enabled(false);
    let card_info = poll_card_init(&mut sd).map_err(|err| {
        warn!("starfive-jh7110-dwmmc: card init failed: {:?}", err);
        card_init_error(address, mmio_size, err)
    })?;
    info!(
        "starfive-jh7110-dwmmc card: kind={:?} high_capacity={} rca={} ocr={:#010x} \
         capacity_blocks={:?} cid={} ext_csd={}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
        card_info.cid.is_some(),
        card_info.ext_csd.is_some()
    );

    let dev = dwmmc_rdif::device(
        sd,
        dwmmc_rdif::fifo_config(
            "starfive-jh7110-mmc",
            card_info.capacity_blocks.unwrap_or(0),
            false,
        ),
    );
    let irq = probe.register_block(dev)?;
    info!("starfive-jh7110-mmc block device registered irq={:?}", irq);
    Ok(())
}

fn reference_clock(info: &FdtInfo<'_>) -> u32 {
    let clock = info
        .find_clk_by_name("ciu")
        .map(|clk| clk.select().unwrap_or(0));
    info!(
        "starfive-jh7110-dwmmc: using {} Hz ciu reference clock hint {:?}",
        DWMMC_STABLE_REFERENCE_CLOCK, clock
    );
    DWMMC_STABLE_REFERENCE_CLOCK
}

fn poll_card_init(sd: &mut StarFiveDwMmc) -> Result<CardInfo, Error> {
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
        "failed to initialize StarFive JH7110 DWMMC device at [PA:{:?}, SZ:0x{:x}): {err:?}",
        address, size
    ))
}

fn card_init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "starfive-jh7110-dwmmc: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping \
             controller: {err:?}",
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
