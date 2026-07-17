use alloc::{format, vec::Vec};

use fdt_edit::Phandle;
use log::info;
use rdif_block::InitError;
use rdif_pinctrl::{PinctrlDevice, PinctrlError};
use rdrive::{
    probe::{
        OnProbeError,
        fdt::{ClockLine, FdtInfo, ResetLine},
    },
    register::ProbeFdt,
};
use sdmmc_protocol::{
    rdif::StagedBlockDevice,
    sdio::{BusWidth, CardInitPreference, OwnedSdioInit, SdioSdmmc},
};
use starfive_jh7110_dwmmc::{
    JH7110_STABLE_REFERENCE_CLOCK_HZ, Jh7110DwMmc, Jh7110DwMmcConfig, rdif as starfive_rdif,
};

use crate::{
    block::{
        ProbeFdtBlock,
        staged::{PlatformPrelude, StagedPlatformBlock},
    },
    mmio::iomap,
};

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
    let resources = StarFiveMmcResources::discover(info)?;
    let reference_clock_hz = prepared_reference_clock_hz(resources.ciu_clock_rate()?);
    let profile = StarFiveMmcNodeProfile::from_info(info, reference_clock_hz);
    let mmio_base = iomap(address as usize, mmio_size as usize)?;

    let mut host = unsafe { Jh7110DwMmc::new(mmio_base, profile.host_config) };
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    host.set_dma(dma.clone());
    let mut card = SdioSdmmc::new_host2_timed(host);
    card.set_sd_speed_selection_enabled(false);
    let staged = StagedBlockDevice::new(
        OwnedSdioInit::new(card, profile.init_preference),
        starfive_rdif::dma_config(0, dma),
        starfive_rdif::device,
    );
    let staged = StagedPlatformBlock::new(staged, resources);
    let irq = probe.register_block(staged)?;
    info!("starfive-jh7110-mmc controller staged irq={irq:?}");
    Ok(())
}

struct StarFiveMmcResources {
    clocks: Vec<ClockLine>,
    resets: Vec<ResetLine>,
    regulators: Vec<StarFiveRegulator>,
    ciu_clock: Option<ClockLine>,
}

impl StarFiveMmcResources {
    fn discover(info: &FdtInfo<'_>) -> Result<Self, OnProbeError> {
        let clocks = info.clock_lines()?;
        let ciu_clock = clocks
            .iter()
            .find(|clock| clock.name() == Some("ciu"))
            .cloned();
        Ok(Self {
            clocks,
            resets: info.reset_lines()?,
            regulators: StarFiveRegulator::discover_all(info)?,
            ciu_clock,
        })
    }

    fn ciu_clock_rate(&self) -> Result<Option<u64>, OnProbeError> {
        self.ciu_clock.as_ref().map(ClockLine::rate).transpose()
    }

    fn activate_resources(&self) -> Result<u64, OnProbeError> {
        let mut settle_ns = 0;
        for regulator in &self.regulators {
            regulator.enable()?;
            settle_ns = settle_ns.max(regulator.startup_delay_ns);
        }
        for clock in &self.clocks {
            clock.enable()?;
        }
        for reset in &self.resets {
            reset.deassert()?;
        }
        Ok(settle_ns)
    }
}

impl PlatformPrelude for StarFiveMmcResources {
    fn prepare(&mut self) -> Result<u64, InitError> {
        self.activate_resources().map_err(|error| {
            log::warn!("starfive-jh7110-dwmmc: staged resource setup failed: {error}");
            InitError::Hardware("StarFive MMC platform resource prelude failed")
        })
    }
}

struct StarFiveRegulator {
    name: &'static str,
    phandle: Phandle,
    startup_delay_ns: u64,
    requires_control: bool,
}

impl StarFiveRegulator {
    fn discover_all(info: &FdtInfo<'_>) -> Result<Vec<Self>, OnProbeError> {
        ["vmmc-supply", "vqmmc-supply"]
            .into_iter()
            .filter_map(|name| {
                info.node
                    .as_node()
                    .get_property(name)
                    .and_then(|property| property.get_u32())
                    .map(|phandle| Self::discover(info, name, Phandle::from(phandle)))
            })
            .collect()
    }

    fn discover(
        info: &FdtInfo<'_>,
        name: &'static str,
        phandle: Phandle,
    ) -> Result<Self, OnProbeError> {
        let regulator = info.get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] {name} phandle {phandle:?} was not found",
                info.node.name()
            ))
        })?;
        let node = regulator.as_node();
        let requires_control = node
            .compatibles()
            .any(|compatible| compatible == "regulator-fixed")
            && (node.get_property("gpios").is_some()
                || node.get_property("gpio").is_some()
                || node.get_property("pinctrl-0").is_some());
        let startup_delay_ns = node
            .get_property("startup-delay-us")
            .and_then(|property| property.get_u32())
            .map(u64::from)
            .unwrap_or(0)
            .saturating_mul(1_000);
        Ok(Self {
            name,
            phandle,
            startup_delay_ns,
            requires_control,
        })
    }

    fn enable(&self) -> Result<(), OnProbeError> {
        if !self.requires_control {
            return Ok(());
        }
        rdrive::with_fdt(|fdt| {
            let regulator = fdt.get_by_phandle(self.phandle).ok_or_else(|| {
                OnProbeError::other(format!(
                    "StarFive MMC {} phandle {:?} disappeared before activation",
                    self.name, self.phandle
                ))
            })?;
            let pinctrl = rdrive::get_one::<PinctrlDevice>().ok_or_else(|| {
                OnProbeError::other(format!(
                    "StarFive MMC {} requires a fixed-regulator pinctrl provider",
                    self.name
                ))
            })?;
            let mut pinctrl = pinctrl.lock().map_err(|error| {
                OnProbeError::other(format!(
                    "failed to lock StarFive regulator pinctrl: {error}"
                ))
            })?;
            match pinctrl.apply_fdt_fixed_regulator(fdt, regulator.as_node(), "starfive-mmc") {
                Ok(()) | Err(PinctrlError::NotAvailable) => Ok(()),
                Err(error) => Err(OnProbeError::other(format!(
                    "failed to enable StarFive MMC {}: {error}",
                    self.name
                ))),
            }
        })
        .ok_or_else(|| OnProbeError::other("live FDT unavailable during StarFive MMC activation"))?
    }
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
    use super::*;

    #[test]
    fn starfive_block_io_uses_dma_irq_queue() {
        let config = starfive_rdif::dma_config(16, axklib::dma::device_with_mask(u32::MAX as u64));

        assert_eq!(config.name, "starfive-jh7110-mmc");
        assert!(config.uses_dma());
        assert!(config.supports_runtime_queue());
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
}
