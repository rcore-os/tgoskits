use alloc::format;

use log::info;
use rdrive::{
    probe::{OnProbeError, fdt::ResourcePrepareConfig},
    register::ProbeFdt,
};
use sdmmc_protocol::{
    rdif::{config::BlockConfig, device::BlockDevice},
    sdio::{BusWidth, card::SdioSdmmc, init::CardInitPreference},
};
use starfive_jh7110_dwmmc::{
    DEVICE_NAME, JH7110_FIFO_CONFIG, JH7110_STABLE_REFERENCE_CLOCK_HZ, Jh7110DwMmc,
    Jh7110DwMmcConfig,
};

use crate::{block::ProbeFdtBlock, mmio::iomap};

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
        ResourcePrepareConfig::default()
            .without_assigned_clocks()
            .with_named_clock_rate("ciu"),
    )?;
    let reference_clock_hz = prepared_reference_clock_hz(resources.clock_rate("ciu"));
    let profile = StarFiveMmcNodeProfile::from_info(info, reference_clock_hz)?;
    info!(
        "starfive-jh7110-dwmmc: fifo depth={} words watermark_aligned={}",
        profile.host_config.fifo_config().depth_words(),
        profile.fifo_watermark_aligned
    );
    let mmio_base = iomap(address as usize, mmio_size as usize)?;

    let mut host = unsafe { Jh7110DwMmc::new(mmio_base, profile.host_config) };
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let block_config = starfive_block_config(&dma);
    host.inner_mut().configure_dma(dma).map_err(|err| {
        OnProbeError::other(format!(
            "starfive-jh7110-dwmmc IDMAC configuration failed: {err:?}"
        ))
    })?;

    info!("starfive-jh7110-dwmmc: defer card initialization to IRQ-driven hctx");
    let mut sd = SdioSdmmc::new(host);
    sd.set_sd_speed_selection_enabled(false);
    let dev = BlockDevice::new_initializing(sd, block_config, profile.init_preference);
    let irq = probe.register_block(dev)?;
    info!("starfive-jh7110-mmc block device registered irq={:?}", irq);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StarFiveMmcNodeProfile {
    host_config: Jh7110DwMmcConfig,
    init_preference: CardInitPreference,
    // Linux applies this quirk only to PIO RX/TX interrupt thresholds. Keep
    // the DT value visible for diagnostics, but do not pass it into the
    // IDMAC-only portable data path.
    fifo_watermark_aligned: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StarFiveMmcDtProperties {
    fifo_config: dwmmc_host::FifoConfig,
    fifo_watermark_aligned: bool,
    bus_width: u32,
    supports_1v8: bool,
    no_sd: bool,
    no_mmc: bool,
    non_removable: bool,
    has_mmc_capability: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FifoDepthPropertyError {
    Malformed,
    Invalid(u32),
}

impl StarFiveMmcNodeProfile {
    fn from_info(
        info: &rdrive::probe::fdt::FdtInfo<'_>,
        reference_clock_hz: u32,
    ) -> Result<Self, OnProbeError> {
        let node = info.node.as_node();
        let fifo_config = fifo_config_from_property(
            node.get_property("fifo-depth")
                .map(|property| property.get_u32()),
        )
        .map_err(|error| {
            let detail = match error {
                FifoDepthPropertyError::Malformed => "malformed encoding".into(),
                FifoDepthPropertyError::Invalid(depth) => format!("invalid value {depth}"),
            };
            OnProbeError::other(format!(
                "[{}] has invalid fifo-depth: {detail}",
                info.node.name()
            ))
        })?;
        Ok(Self::from_dt_properties(
            reference_clock_hz,
            StarFiveMmcDtProperties {
                fifo_config,
                fifo_watermark_aligned: node.get_property("fifo-watermark-aligned").is_some(),
                bus_width: node
                    .get_property("bus-width")
                    .and_then(|prop| prop.get_u32())
                    .unwrap_or(1),
                supports_1v8: node.get_property("mmc-hs200-1_8v").is_some()
                    || node.get_property("mmc-ddr-1_8v").is_some()
                    || node.get_property("sd-uhs-sdr104").is_some()
                    || node.get_property("sd-uhs-sdr50").is_some()
                    || node.get_property("sd-uhs-ddr50").is_some()
                    || node.get_property("sd-uhs-sdr25").is_some()
                    || node.get_property("sd-uhs-sdr12").is_some(),
                no_sd: node.get_property("no-sd").is_some(),
                no_mmc: node.get_property("no-mmc").is_some(),
                non_removable: node.get_property("non-removable").is_some(),
                has_mmc_capability: node.get_property("cap-mmc-hw-reset").is_some()
                    || node.get_property("mmc-hs200-1_8v").is_some()
                    || node.get_property("mmc-hs400-1_8v").is_some()
                    || node.get_property("mmc-ddr-1_8v").is_some(),
            },
        ))
    }

    fn from_dt_properties(reference_clock_hz: u32, properties: StarFiveMmcDtProperties) -> Self {
        let max_bus_width = match properties.bus_width {
            8.. => BusWidth::Bit8,
            4.. => BusWidth::Bit4,
            _ => BusWidth::Bit1,
        };
        let host_config = Jh7110DwMmcConfig::default()
            .with_reference_clock_hz(reference_clock_hz)
            .with_fifo_config(properties.fifo_config)
            .with_max_bus_width(max_bus_width)
            .with_1v8_support(properties.supports_1v8);
        let init_preference = if properties.no_mmc {
            CardInitPreference::SdOnly
        } else if properties.no_sd
            || matches!(max_bus_width, BusWidth::Bit8)
            || (properties.non_removable && properties.has_mmc_capability)
        {
            CardInitPreference::MmcFirst
        } else {
            CardInitPreference::SdFirst
        };

        Self {
            host_config,
            init_preference,
            fifo_watermark_aligned: properties.fifo_watermark_aligned,
        }
    }
}

fn fifo_config_from_property(
    property: Option<Option<u32>>,
) -> Result<dwmmc_host::FifoConfig, FifoDepthPropertyError> {
    let depth = match property {
        None => return Ok(JH7110_FIFO_CONFIG),
        Some(None) => return Err(FifoDepthPropertyError::Malformed),
        Some(Some(depth)) => depth,
    };
    let depth_words = u16::try_from(depth).map_err(|_| FifoDepthPropertyError::Invalid(depth))?;
    dwmmc_host::FifoConfig::new(depth_words, dwmmc_host::FifoDataWidth::Bits32)
        .ok_or(FifoDepthPropertyError::Invalid(depth))
}

fn starfive_block_config(dma: &dma_api::DeviceDma) -> BlockConfig {
    BlockConfig::dma(DEVICE_NAME, 0, dma)
        .with_max_blocks_per_request(dwmmc_host::IDMAC_MAX_BLOCKS)
        .with_max_segment_size(dwmmc_host::IDMAC_MAX_TRANSFER_SIZE)
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

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "pci"))]
    use axklib::{
        AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle,
        IrqId, Klib, PhysAddr, VirtAddr, impl_trait,
    };

    use super::*;

    #[cfg(not(feature = "pci"))]
    struct KlibImpl;

    #[cfg(not(feature = "pci"))]
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
    fn starfive_profiles_are_dt_capability_driven_not_base_driven() {
        let emmc = StarFiveMmcNodeProfile::from_dt_properties(
            50_000_000,
            StarFiveMmcDtProperties {
                fifo_config: JH7110_FIFO_CONFIG,
                fifo_watermark_aligned: true,
                bus_width: 8,
                supports_1v8: true,
                no_sd: false,
                no_mmc: false,
                non_removable: true,
                has_mmc_capability: true,
            },
        );
        let microsd = StarFiveMmcNodeProfile::from_dt_properties(
            50_000_000,
            StarFiveMmcDtProperties {
                fifo_config: JH7110_FIFO_CONFIG,
                fifo_watermark_aligned: true,
                bus_width: 4,
                supports_1v8: false,
                no_sd: false,
                no_mmc: true,
                non_removable: false,
                has_mmc_capability: false,
            },
        );

        assert_eq!(emmc.host_config.max_bus_width(), BusWidth::Bit8);
        assert!(emmc.host_config.supports_1v8());
        assert_eq!(emmc.host_config.fifo_config().depth_words(), 32);
        assert!(emmc.fifo_watermark_aligned);
        assert_eq!(emmc.init_preference, CardInitPreference::MmcFirst);

        assert_eq!(microsd.host_config.max_bus_width(), BusWidth::Bit4);
        assert!(!microsd.host_config.supports_1v8());
        assert_eq!(microsd.init_preference, CardInitPreference::SdOnly);
    }

    #[test]
    fn malformed_fifo_depth_is_not_treated_as_a_missing_property() {
        assert_eq!(
            fifo_config_from_property(Some(None)),
            Err(FifoDepthPropertyError::Malformed)
        );
        assert_eq!(fifo_config_from_property(None), Ok(JH7110_FIFO_CONFIG));
        assert_eq!(
            fifo_config_from_property(Some(Some(1))),
            Err(FifoDepthPropertyError::Invalid(1))
        );
        assert_eq!(
            fifo_config_from_property(Some(Some(32))),
            Ok(JH7110_FIFO_CONFIG)
        );
    }
}
