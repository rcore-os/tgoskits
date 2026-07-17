// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::{format, vec::Vec};

use dwmmc_host::{CardDetect, DwMmc, HostClock, rdif as dwmmc_rdif};
use fdt_edit::{Node, Phandle};
use log::{info, warn};
use rdif_block::InitError;
use rdif_pinctrl::{FdtPinctrl, PinctrlDevice};
use rdrive::{
    probe::{OnProbeError, fdt::ClockLine},
    register::{FdtInfo, ProbeFdt},
};
use sdmmc_protocol::{
    Error,
    error::{ErrorContext, Phase},
    rdif::StagedBlockDevice,
    sdio::{CardInitPreference, OwnedSdioInit, SdioSdmmc},
};

use super::clock::{
    ScmiClockOps, StagedClockEnable, rdrive_named_clock, scmi_named_clock, staged_node_clocks,
};
use crate::{
    block::{
        ProbeFdtBlock,
        staged::{PlatformPrelude, StagedPlatformBlock},
    },
    mmio::iomap,
    soc::RockchipFdtPinctrlParser,
};

const DWMMC_STABLE_REFERENCE_CLOCK: u32 = 50_000_000;
const ROCKCHIP_DWMMC_CLKGEN_DIV: u32 = 2;
const ENABLE_SD_SPEED_SELECTION: bool = true;
const RK3588_CRU_BASE: usize = 0xfd7c_0000;
const RK3588_CRU_SIZE: usize = 0x5c000;
const RK3588_SDMMC_CON0: usize = 0x0c30;
const RK3588_SDMMC_CON1: usize = 0x0c34;
const RK3588_SDMMC_PHASE_SHIFT: u32 = 1;
const RK3588_SDMMC_DRV_PHASE_DEG: u32 = 90;
const RK3588_SDMMC_SAMPLE_PHASE_DEG: u32 = 0;
#[derive(Clone)]
enum RockchipDwMmcClock {
    Rdrive(ClockLine),
    Scmi(ScmiClockOps),
}

struct DwMmcClockSetup {
    clock: RockchipDwMmcClock,
}

struct RockchipSdResources {
    regulators: Vec<StagedRegulator>,
    clocks: Vec<StagedClockEnable>,
    ciu_clock: Option<RockchipDwMmcClock>,
    phase: phase::Rk3588PhaseSetup,
}

#[derive(Clone, Copy)]
struct StagedRegulator {
    supply: Phandle,
    startup_delay_ns: u64,
}

impl HostClock for RockchipDwMmcClock {
    fn set_clock(&self, target_hz: u32) -> Result<u32, Error> {
        if target_hz == 0 {
            return Err(Error::InvalidArgument);
        }
        let cclkin = u64::from(target_hz) * u64::from(ROCKCHIP_DWMMC_CLKGEN_DIV);
        let rate = self.enable_set_rate_and_read(cclkin)?;
        let bus_hz = rate / u64::from(ROCKCHIP_DWMMC_CLKGEN_DIV);
        let bus_hz = validate_bus_clock(bus_hz)?;
        info!(
            "rockchip-dwmmc: ciu clock set target={} Hz cclkin={} Hz bus={} Hz",
            target_hz, rate, bus_hz
        );
        Ok(bus_hz)
    }
}

impl RockchipDwMmcClock {
    fn enable_set_rate_and_read(&self, rate: u64) -> Result<u64, Error> {
        match self {
            Self::Rdrive(clock) => {
                clock
                    .enable()
                    .map_err(|_| Error::BadResponse(ErrorContext::new(Phase::Init)))?;
                clock
                    .set_rate(rate)
                    .map_err(|_| Error::BadResponse(ErrorContext::new(Phase::Init)))?;
                clock
                    .rate()
                    .map_err(|_| Error::BadResponse(ErrorContext::new(Phase::Init)))
            }
            Self::Scmi(clock) => {
                clock
                    .enable()
                    .ok_or_else(|| Error::BadResponse(ErrorContext::new(Phase::Init)))?;
                clock
                    .set_rate(rate)
                    .ok_or_else(|| Error::BadResponse(ErrorContext::new(Phase::Init)))?;
                clock
                    .rate()
                    .ok_or_else(|| Error::BadResponse(ErrorContext::new(Phase::Init)))
            }
        }
    }
}

mod phase;

impl RockchipSdResources {
    fn discover(
        info: &FdtInfo<'_>,
        ciu_clock: Option<RockchipDwMmcClock>,
    ) -> Result<Self, OnProbeError> {
        let regulators = staged_regulators(info)?;
        let clocks = staged_node_clocks(info)?;
        let phase = if is_rk3588_dwmmc(info) {
            phase::rk3588_phase_setup(info)
        } else {
            phase::Rk3588PhaseSetup::disabled()
        };
        Ok(Self {
            regulators,
            clocks,
            ciu_clock,
            phase,
        })
    }
}

impl PlatformPrelude for RockchipSdResources {
    fn prepare(&mut self) -> Result<u64, InitError> {
        let settle_ns = enable_staged_regulators(&self.regulators).map_err(|error| {
            warn!("rockchip-dwmmc: staged regulator enable failed: {error}");
            InitError::Hardware("Rockchip SD regulator prelude failed")
        })?;
        for clock in &self.clocks {
            clock.enable().map_err(|error| {
                warn!("rockchip-dwmmc: staged clock enable failed: {error}");
                InitError::Hardware("Rockchip SD clock prelude failed")
            })?;
        }
        if let Some(clock) = &self.ciu_clock {
            let rate = clock
                .enable_set_rate_and_read(u64::from(DWMMC_STABLE_REFERENCE_CLOCK))
                .map_err(|error| {
                    warn!("rockchip-dwmmc: staged ciu setup failed: {error:?}");
                    InitError::Hardware("Rockchip SD ciu prelude failed")
                })?;
            let rate = validate_bus_clock(rate).map_err(|error| {
                warn!("rockchip-dwmmc: invalid staged ciu rate: {error:?}");
                InitError::Hardware("Rockchip SD ciu rate is invalid")
            })?;
            self.phase.apply(rate).map_err(|error| {
                warn!("rockchip-dwmmc: staged phase setup failed: {error}");
                InitError::Hardware("Rockchip SD phase prelude failed")
            })?;
        }
        Ok(settle_ns)
    }
}

crate::model_register!(
    name: "Rockchip SD",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-dw-mshc", "rockchip,rk3288-dw-mshc"],
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
        "rockchip-dwmmc probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;

    let mut host = unsafe { DwMmc::new(mmio_base) };
    host.set_card_detect(CardDetect::ControllerActiveLow);
    let clock_setup = dwmmc_clock_setup(info);
    let ciu_clock = clock_setup.as_ref().map(|setup| setup.clock.clone());
    let resources = RockchipSdResources::discover(info, ciu_clock)?;
    if let Some(setup) = clock_setup {
        info!(
            "rockchip-dwmmc: using ciu reference clock {} Hz",
            DWMMC_STABLE_REFERENCE_CLOCK
        );
        host.set_reference_clock(DWMMC_STABLE_REFERENCE_CLOCK);
        host.set_external_clock(setup.clock);
    } else {
        warn!(
            "rockchip-dwmmc: ciu clock not found; leaving DWMMC divider bypassed and relying on \
             CRU rate"
        );
    }
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    host.set_dma(dma.clone());

    let mut sd = SdioSdmmc::new_host2_timed(host);
    sd.set_sd_speed_selection_enabled(ENABLE_SD_SPEED_SELECTION);
    let staged = StagedBlockDevice::new(
        OwnedSdioInit::new(sd, CardInitPreference::SdFirst),
        dwmmc_rdif::dma_config("rockchip-sd", 0, dma),
        dwmmc_rdif::device,
    );
    let staged = StagedPlatformBlock::new(staged, resources);
    let irq = probe.register_block(staged)?;
    info!("rockchip-sd controller staged irq={irq:?}");
    Ok(())
}

fn staged_regulators(info: &FdtInfo<'_>) -> Result<Vec<StagedRegulator>, OnProbeError> {
    let mut regulators = Vec::new();
    for (name, supply) in sd_supply_phandles(info.node.as_node()) {
        if supply_has_fixed_gpio_enable(info, supply)? {
            let regulator = info.get_by_phandle(supply).ok_or_else(|| {
                OnProbeError::other(format!("SDMMC regulator phandle {supply:?} not found"))
            })?;
            let startup_delay_ns = u64::from(
                regulator
                    .as_node()
                    .get_property("startup-delay-us")
                    .and_then(|property| property.get_u32())
                    .unwrap_or(0),
            )
            .saturating_mul(1_000);
            regulators.push(StagedRegulator {
                supply,
                startup_delay_ns,
            });
        } else {
            info!(
                "[{}] {name} phandle {:?} is not a fixed GPIO regulator; skip pinctrl enable",
                info.node.name(),
                supply
            );
        }
    }
    Ok(regulators)
}

fn enable_staged_regulators(regulators: &[StagedRegulator]) -> Result<u64, OnProbeError> {
    if regulators.is_empty() {
        return Ok(0);
    }
    let pinctrl = rdrive::get_one::<PinctrlDevice>().ok_or_else(|| {
        OnProbeError::other("PinctrlDevice unavailable for staged SDMMC regulator enable")
    })?;
    let mut pinctrl = pinctrl
        .lock()
        .map_err(|error| OnProbeError::other(format!("failed to lock PinctrlDevice: {error}")))?;
    let fdt = rdrive::with_fdt(Clone::clone)
        .ok_or_else(|| OnProbeError::other("live FDT not found for SDMMC regulator"))?;
    let mut startup_delay_ns = 0;
    for regulator in regulators {
        let node = fdt.get_by_phandle(regulator.supply).ok_or_else(|| {
            OnProbeError::other(format!(
                "SDMMC regulator phandle {:?} disappeared before activation",
                regulator.supply
            ))
        })?;
        FdtPinctrl::apply_fixed_regulator(
            &mut *pinctrl,
            &fdt,
            node.as_node(),
            &RockchipFdtPinctrlParser,
            "rockchip-sd-regulator",
        )
        .map_err(|error| {
            OnProbeError::other(format!(
                "failed to enable SDMMC regulator {:?} via pinctrl: {error}",
                regulator.supply
            ))
        })?;
        startup_delay_ns = startup_delay_ns.max(regulator.startup_delay_ns);
    }
    Ok(startup_delay_ns)
}

fn sd_supply_phandles(node: &Node) -> impl Iterator<Item = (&'static str, Phandle)> + '_ {
    ["vmmc-supply", "vqmmc-supply"]
        .into_iter()
        .filter_map(|name| {
            node.get_property(name)
                .and_then(|prop| prop.get_u32())
                .map(|phandle| (name, Phandle::from(phandle)))
        })
}

fn supply_has_fixed_gpio_enable(
    info: &FdtInfo<'_>,
    phandle: Phandle,
) -> Result<bool, OnProbeError> {
    let node = info
        .get_by_phandle(phandle)
        .ok_or_else(|| OnProbeError::other(format!("SD supply phandle {phandle:?} not found")))?;
    Ok(regulator_has_fixed_gpio_enable(node.as_node()))
}

fn regulator_has_fixed_gpio_enable(node: &Node) -> bool {
    node.compatibles()
        .any(|compatible| compatible == "regulator-fixed")
        && (node.get_property("gpios").is_some()
            || node.get_property("gpio").is_some()
            || node.get_property("pinctrl-0").is_some())
}

fn dwmmc_clock_setup(info: &FdtInfo<'_>) -> Option<DwMmcClockSetup> {
    match rdrive_named_clock(info, "ciu") {
        Ok(Some(clock)) => Some(DwMmcClockSetup {
            clock: RockchipDwMmcClock::Rdrive(clock),
        }),
        Ok(None) | Err(_) => {
            if let Some(clock) = scmi_named_clock(info, "ciu") {
                return Some(DwMmcClockSetup {
                    clock: RockchipDwMmcClock::Scmi(clock),
                });
            }
            warn!(
                "[{}] ciu clock provider is not available through CRU or SCMI",
                info.node.name()
            );
            None
        }
    }
}

fn is_rk3588_dwmmc(info: &FdtInfo<'_>) -> bool {
    info.node
        .as_node()
        .compatibles()
        .any(|compatible| compatible == "rockchip,rk3588-dw-mshc")
}

fn validate_bus_clock(rate: u64) -> Result<u32, Error> {
    if rate == 0 || rate > u32::MAX as u64 {
        return Err(Error::BadResponse(ErrorContext::new(Phase::Init)));
    }
    Ok(rate as u32)
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;

    #[test]
    fn sd_supply_phandles_reads_optional_vmmc_and_vqmmc() {
        let mut node = Node::new("mmc@fe2c0000");
        node.add_property(fdt_edit::Property::new(
            "vmmc-supply",
            0x1234_u32.to_be_bytes().to_vec(),
        ));
        node.add_property(fdt_edit::Property::new(
            "vqmmc-supply",
            0x5678_u32.to_be_bytes().to_vec(),
        ));

        let supplies = sd_supply_phandles(&node).collect::<Vec<_>>();

        assert_eq!(
            supplies,
            vec![
                ("vmmc-supply", Phandle::from(0x1234)),
                ("vqmmc-supply", Phandle::from(0x5678))
            ]
        );
    }

    #[test]
    fn sd_supply_phandles_allows_absent_supplies() {
        let node = Node::new("mmc@fe2c0000");

        assert_eq!(sd_supply_phandles(&node).count(), 0);
    }

    #[test]
    fn fixed_regulator_with_gpio_enable_is_pinctrl_controlled() {
        let mut node = Node::new("vcc-3v3-sd-s0");
        node.add_property(fdt_edit::Property::new(
            "compatible",
            b"regulator-fixed\0".to_vec(),
        ));
        node.add_property(fdt_edit::Property::new("gpios", Vec::new()));

        assert!(regulator_has_fixed_gpio_enable(&node));
    }

    #[test]
    fn pmic_regulator_supply_is_not_pinctrl_controlled() {
        let mut node = Node::new("PLDO_REG5");
        node.add_property(fdt_edit::Property::new(
            "regulator-name",
            b"vccio_sd_s0\0".to_vec(),
        ));

        assert!(!regulator_has_fixed_gpio_enable(&node));
    }
}
