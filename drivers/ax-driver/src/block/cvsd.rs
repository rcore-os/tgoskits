#[cfg(not(test))]
use alloc::format;

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
#[cfg(not(test))]
use sdhci_host::rdif as sdhci_rdif;
#[cfg(not(test))]
use sdmmc_protocol::{
    rdif::device::BlockDevice,
    sdio::{card::SdioSdmmc, host::BusWidth, init::CardInitPreference},
};

#[cfg(not(test))]
use crate::{block::ProbeFdtBlock, mmio::iomap};

#[cfg(not(test))]
pub const DEVICE_NAME: &str = "cvsd";

#[cfg(not(test))]
const DEFAULT_SDMMIF_SIZE: usize = 0x1000;
#[cfg(not(test))]
const DEFAULT_SYSCON_SIZE: usize = 0x8000;

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

    let mut host = unsafe { Cv181xSdhci::new(Cv181xMmio::new(core, syscon), config) };
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let block_config = sdhci_rdif::dma_config(DEVICE_NAME, 0, &dma);
    host.configure_dma(dma)
        .map_err(|err| OnProbeError::other(format!("cvsd ADMA2 configuration failed: {err:?}")))?;

    let mut card = SdioSdmmc::new(host);
    card.set_sd_uhs_selection_enabled(false);
    let dev = BlockDevice::new_initializing(card, block_config, card_init_preference(policy));
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
