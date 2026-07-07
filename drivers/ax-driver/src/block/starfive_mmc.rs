use alloc::format;
use core::time::Duration;

use log::{info, warn};
use rdrive::{probe::OnProbeError, register::ProbeFdt};
use sdmmc_protocol::{
    Error, OperationPoll,
    error::Phase,
    rdif::config::BlockConfig,
    sdio::{
        BusWidth,
        card::{CardInfo, SdioSdmmc},
        host2::SdioHost2Adapter,
        init::{CardInitPreference, SdioInitScratch},
    },
};
use starfive_jh7110_dwmmc::{
    JH7110_STABLE_REFERENCE_CLOCK_HZ, Jh7110DwMmc, Jh7110DwMmcConfig, rdif as starfive_rdif,
};

use crate::{block::ProbeFdtBlock, mmio::iomap};

type StarFiveDwMmc = SdioSdmmc<SdioHost2Adapter<Jh7110DwMmc>>;

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
    info!(
        "starfive-jh7110-dwmmc probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        address,
        mmio_size
    );
    let resources = info.prepare_resources(
        rdrive::probe::fdt::ResourcePrepareConfig::default().with_named_clock_rate("ciu"),
    )?;
    let reference_clock_hz = prepared_reference_clock_hz(resources.clock_rate("ciu"));
    let profile = StarFiveMmcNodeProfile::from_info(info, reference_clock_hz);
    let mmio_base = iomap(address as usize, mmio_size as usize)?;

    let mut host = unsafe { Jh7110DwMmc::new(mmio_base, profile.host_config) };

    info!("starfive-jh7110-dwmmc: reset controller");
    host.reset_and_init()
        .map_err(|err| init_error(address, mmio_size, err))?;

    info!("starfive-jh7110-dwmmc: initialize card");
    let mut sd = SdioSdmmc::new_host2(host);
    sd.set_sd_speed_selection_enabled(false);
    let card_info = poll_card_init(&mut sd, profile.init_preference).map_err(|err| {
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

    let dev = starfive_rdif::device(
        sd,
        starfive_block_config(card_info.capacity_blocks.unwrap_or(0)),
    );
    let irq = probe.register_block(dev)?;
    info!("starfive-jh7110-mmc block device registered irq={:?}", irq);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StarFiveMmcNodeProfile {
    host_config: Jh7110DwMmcConfig,
    init_preference: CardInitPreference,
}

impl StarFiveMmcNodeProfile {
    fn from_info(info: &rdrive::probe::fdt::FdtInfo<'_>, reference_clock_hz: u32) -> Self {
        let node = info.node.as_node();
        Self::from_dt_flags(
            reference_clock_hz,
            node.get_property("bus-width")
                .and_then(|prop| prop.get_u32())
                .unwrap_or(1),
            node.get_property("mmc-hs200-1_8v").is_some()
                || node.get_property("mmc-ddr-1_8v").is_some()
                || node.get_property("sd-uhs-sdr104").is_some()
                || node.get_property("sd-uhs-sdr50").is_some()
                || node.get_property("sd-uhs-ddr50").is_some()
                || node.get_property("sd-uhs-sdr25").is_some()
                || node.get_property("sd-uhs-sdr12").is_some(),
            node.get_property("no-sd").is_some(),
            node.get_property("no-mmc").is_some(),
            node.get_property("non-removable").is_some(),
            node.get_property("cap-mmc-hw-reset").is_some()
                || node.get_property("mmc-hs200-1_8v").is_some()
                || node.get_property("mmc-hs400-1_8v").is_some()
                || node.get_property("mmc-ddr-1_8v").is_some(),
        )
    }

    fn from_dt_flags(
        reference_clock_hz: u32,
        bus_width: u32,
        supports_1v8: bool,
        no_sd: bool,
        no_mmc: bool,
        non_removable: bool,
        has_mmc_capability: bool,
    ) -> Self {
        let max_bus_width = match bus_width {
            8.. => BusWidth::Bit8,
            4.. => BusWidth::Bit4,
            _ => BusWidth::Bit1,
        };
        let host_config = Jh7110DwMmcConfig::default()
            .with_reference_clock_hz(reference_clock_hz)
            .with_max_bus_width(max_bus_width)
            .with_1v8_support(supports_1v8);
        let init_preference = if no_mmc {
            CardInitPreference::SdOnly
        } else if no_sd
            || matches!(max_bus_width, BusWidth::Bit8)
            || (non_removable && has_mmc_capability)
        {
            CardInitPreference::MmcFirst
        } else {
            CardInitPreference::SdFirst
        };

        Self {
            host_config,
            init_preference,
        }
    }
}

fn starfive_block_config(capacity_blocks: u64) -> BlockConfig {
    starfive_rdif::fifo_config(capacity_blocks, true)
}

fn prepared_reference_clock_hz(clock_rate: Option<u64>) -> u32 {
    let reference_clock_hz = clock_rate
        .and_then(|hz| u32::try_from(hz).ok())
        .filter(|hz| *hz != 0)
        .unwrap_or(JH7110_STABLE_REFERENCE_CLOCK_HZ);
    info!(
        "starfive-jh7110-dwmmc: using {} Hz ciu reference clock prepared rate {:?}",
        reference_clock_hz, clock_rate
    );
    reference_clock_hz
}

fn poll_card_init(
    sd: &mut StarFiveDwMmc,
    init_preference: CardInitPreference,
) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request = sd.submit_init_with_preference(init_preference, &mut scratch)?;
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
            matches!(ctx.phase, Phase::Unspecified) && ctx.cmd.is_none()
                || ctx.cmd.is_some()
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

            fn mem_make_dma_coherent_uncached(_addr: VirtAddr, _size: usize) -> AxResult {
                Err(AxError::Unsupported)
            }

            fn mem_restore_dma_cached(_addr: VirtAddr, _size: usize) -> AxResult {
                Err(AxError::Unsupported)
            }

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
    fn starfive_block_config_is_irq_driven_fifo() {
        let config = starfive_block_config(16);

        assert_eq!(config.name, "starfive-jh7110-mmc");
        assert_eq!(config.capacity_blocks, 16);
        assert!(config.irq_driven);
        assert!(!config.uses_dma());
    }

    #[test]
    fn starfive_profiles_are_dt_capability_driven_not_base_driven() {
        let emmc =
            StarFiveMmcNodeProfile::from_dt_flags(50_000_000, 8, true, false, false, true, true);
        let microsd =
            StarFiveMmcNodeProfile::from_dt_flags(50_000_000, 4, false, false, true, false, false);

        assert_eq!(emmc.host_config.max_bus_width(), BusWidth::Bit8);
        assert!(emmc.host_config.supports_1v8());
        assert_eq!(emmc.init_preference, CardInitPreference::MmcFirst);

        assert_eq!(microsd.host_config.max_bus_width(), BusWidth::Bit4);
        assert!(!microsd.host_config.supports_1v8());
        assert_eq!(microsd.init_preference, CardInitPreference::SdOnly);
    }

    #[test]
    fn contextless_init_timeout_is_treated_as_absent_card() {
        assert!(is_absent_card_init_error(Error::Timeout(
            sdmmc_protocol::ErrorContext::default()
        )));
    }
}
