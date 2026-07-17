use alloc::format;

use cv181x_sdhci::{
    CV181X_SYSCON_REQUIRED_SIZE, CV181X_TOP_SYSCON_BASE, Cv181xConfig, Cv181xMmio, Cv181xSdhci,
    rdif as cv181x_rdif,
};
use log::{info, warn};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdmmc_protocol::{
    rdif::StagedBlockDevice,
    sdio::{BusWidth, CardInitPreference, OwnedSdioInit, SdioSdmmc},
};

use crate::{block::ProbeFdtBlock, mmio::iomap};

pub const DEVICE_NAME: &str = "cvsd";

const DEFAULT_SDMMIF_SIZE: usize = 0x1000;
const DEFAULT_SYSCON_SIZE: usize = 0x8000;

#[derive(Clone, Copy)]
struct CvsdFdtPolicy {
    no_sd: bool,
    no_mmc: bool,
    no_sdio: bool,
    non_removable: bool,
}

crate::model_register!(
    name: "FDT CVSD",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["cvitek,cv181x-sd"],
        on_probe: probe_fdt,
    }],
);

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
    host.set_dma(dma.clone());
    let mut card = SdioSdmmc::new_host2_timed(host);
    card.set_sd_uhs_selection_enabled(false);
    let staged = StagedBlockDevice::new(
        OwnedSdioInit::new(card, card_init_preference(policy)),
        cv181x_rdif::dma_config(DEVICE_NAME, 0, dma),
        cv181x_rdif::device,
    );
    let irq = probe.register_block(staged)?;
    info!("cvsd controller staged irq={irq:?}");
    Ok(())
}

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

fn cvsd_fdt_policy(info: &FdtInfo<'_>) -> CvsdFdtPolicy {
    let node = info.node.as_node();
    CvsdFdtPolicy {
        no_sd: node.get_property("no-sd").is_some(),
        no_mmc: node.get_property("no-mmc").is_some(),
        no_sdio: node.get_property("no-sdio").is_some(),
        non_removable: node.get_property("non-removable").is_some(),
    }
}

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

fn fdt_u32(info: &FdtInfo<'_>, name: &str, default: u32) -> u32 {
    info.node
        .as_node()
        .get_property(name)
        .and_then(|prop| prop.get_u32())
        .unwrap_or(default)
}

fn card_init_preference(policy: CvsdFdtPolicy) -> CardInitPreference {
    if policy.no_sd || policy.non_removable {
        if policy.no_mmc {
            warn!("cvsd: FDT disables both SD and MMC; retaining SD-only probe semantics");
            return CardInitPreference::SdOnly;
        }
        CardInitPreference::MmcFirst
    } else if policy.no_mmc {
        CardInitPreference::SdOnly
    } else {
        CardInitPreference::SdFirst
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cvsd_block_io_uses_dma_irq_queue() {
        let config = cv181x_rdif::dma_config(
            DEVICE_NAME,
            8,
            axklib::dma::device_with_mask(u32::MAX as u64),
        );

        assert!(config.uses_dma());
        assert!(config.supports_runtime_queue());
    }

    #[test]
    fn removable_sd_slot_prefers_sd_without_disabling_mmc_fallback() {
        let policy = CvsdFdtPolicy {
            no_sd: false,
            no_mmc: false,
            no_sdio: true,
            non_removable: false,
        };

        assert_eq!(card_init_preference(policy), CardInitPreference::SdFirst);
    }
}
