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

use alloc::format;

use dwmmc_host::{CardDetect, DwMmc, HostClock, IDMAC_MAX_BLOCKS, IDMAC_MAX_TRANSFER_SIZE};
use fdt_edit::{Node, Phandle};
use log::{info, warn};
use rdif_pinctrl::{FdtPinctrl, PinctrlDevice};
use rdrive::{
    probe::{OnProbeError, fdt::ClockLine},
    register::{FdtInfo, ProbeFdt},
};
use sdmmc_protocol::{
    Error,
    error::{ErrorContext, Phase},
    rdif::{config::BlockConfig, device::BlockDevice},
    sdio::{card::SdioSdmmc, init::CardInitPreference},
};

use super::clock::enable_node_clocks;
use crate::{block::ProbeFdtBlock, mmio::iomap, soc::RockchipFdtPinctrlParser};

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

struct RockchipDwMmcClock {
    clock: ClockLine,
}

struct DwMmcClockSetup {
    reference_clock: u32,
    clock: RockchipDwMmcClock,
}

impl HostClock for RockchipDwMmcClock {
    fn set_clock(&self, target_hz: u32) -> Result<u32, Error> {
        if target_hz == 0 {
            return Err(Error::InvalidArgument);
        }
        let cclkin = u64::from(target_hz) * u64::from(ROCKCHIP_DWMMC_CLKGEN_DIV);
        self.clock
            .set_rate(cclkin)
            .map_err(|_| Error::BadResponse(ErrorContext::new(Phase::Init)))?;
        let rate = self
            .clock
            .rate()
            .map_err(|_| Error::BadResponse(ErrorContext::new(Phase::Init)))?;
        let bus_hz = rate / u64::from(ROCKCHIP_DWMMC_CLKGEN_DIV);
        let bus_hz = validate_bus_clock(bus_hz)?;
        info!(
            "rockchip-dwmmc: ciu clock set target={} Hz cclkin={} Hz bus={} Hz",
            target_hz, rate, bus_hz
        );
        Ok(bus_hz)
    }
}

mod phase;

use phase::init_rk3588_sdmmc_phase;

crate::model_register!(
    name: "Rockchip SD",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "rockchip,rk3568-dw-mshc",
                "rockchip,rk3588-dw-mshc",
                "rockchip,rk3288-dw-mshc"
            ],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    if !supports_block_card_protocol(info.node.as_node()) {
        info!(
            "rockchip-dwmmc: skip SDIO-only controller {}",
            info.node.name()
        );
        return Ok(());
    }
    apply_rockchip_sd_resources(info)?;
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
    let clock_setup = dwmmc_clock_setup(info)?;
    if let Some(setup) = clock_setup {
        info!(
            "rockchip-dwmmc: using ciu reference clock {} Hz",
            setup.reference_clock
        );
        host.set_reference_clock(setup.reference_clock);
        if is_rk3588_dwmmc(info) {
            init_rk3588_sdmmc_phase(info, setup.reference_clock)?;
        }
        host.set_external_clock(setup.clock);
    } else {
        warn!(
            "rockchip-dwmmc: ciu clock not found; leaving DWMMC divider bypassed and relying on \
             CRU rate"
        );
    }
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let block_config = BlockConfig::dma("rockchip-dwmmc", 0, &dma)
        .with_max_blocks_per_request(IDMAC_MAX_BLOCKS)
        .with_max_segment_size(IDMAC_MAX_TRANSFER_SIZE);
    host.configure_dma(dma).map_err(|err| {
        OnProbeError::other(format!(
            "rockchip-dwmmc IDMAC configuration failed: {err:?}"
        ))
    })?;

    info!("rockchip-dwmmc: defer protocol initialization to IRQ-driven hctx");
    let mut card = SdioSdmmc::new(host);
    card.set_sd_speed_selection_enabled(ENABLE_SD_SPEED_SELECTION);
    let dev = BlockDevice::new_initializing(card, block_config, card_init_preference(info));
    let irq = probe.register_block(dev)?;
    info!("rockchip-dwmmc block device registered irq={:?}", irq);
    Ok(())
}

fn apply_rockchip_sd_resources(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    enable_node_clocks(info, "SDMMC")?;
    let Some(pinctrl) = rdrive::get_one::<PinctrlDevice>() else {
        warn!(
            "[{}] PinctrlDevice not found; SDMMC clocks are enabled but pinctrl and fixed \
             regulators remain firmware-owned",
            info.node.name()
        );
        return Ok(());
    };
    let mut pinctrl = pinctrl
        .lock()
        .map_err(|err| OnProbeError::other(format!("failed to lock PinctrlDevice: {err}")))?;
    for (name, supply) in sd_supply_phandles(info.node.as_node()) {
        if supply_has_fixed_gpio_enable(info, supply)? {
            enable_fixed_regulator_with_pinctrl(&mut pinctrl, info, supply)?;
        } else {
            info!(
                "[{}] {name} phandle {:?} is not a fixed GPIO regulator; skip pinctrl enable",
                info.node.name(),
                supply
            );
        }
    }
    Ok(())
}

fn supports_block_card_protocol(node: &Node) -> bool {
    node.get_property("no-sd").is_none() || node.get_property("no-mmc").is_none()
}

fn enable_fixed_regulator_with_pinctrl(
    pinctrl: &mut PinctrlDevice,
    info: &FdtInfo<'_>,
    supply: Phandle,
) -> Result<(), OnProbeError> {
    let regulator = info.get_by_phandle(supply).ok_or_else(|| {
        OnProbeError::other(format!("SDMMC regulator phandle {supply:?} not found"))
    })?;
    let fdt = rdrive::with_fdt(Clone::clone)
        .ok_or_else(|| OnProbeError::other("live FDT not found for SDMMC regulator"))?;
    FdtPinctrl::apply_fixed_regulator(
        pinctrl,
        &fdt,
        regulator.as_node(),
        &RockchipFdtPinctrlParser,
        "rockchip-sd-regulator",
    )
    .map_err(|err| {
        OnProbeError::other(format!(
            "failed to enable SDMMC regulator {supply:?} via pinctrl: {err}"
        ))
    })?;

    let startup_delay_us = regulator
        .as_node()
        .get_property("startup-delay-us")
        .and_then(|prop| prop.get_u32())
        .unwrap_or(0);
    if startup_delay_us != 0 {
        axklib::time::busy_wait(core::time::Duration::from_micros(u64::from(
            startup_delay_us,
        )));
    }
    Ok(())
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

fn card_init_preference(info: &FdtInfo<'_>) -> CardInitPreference {
    let node = info.node.as_node();
    if node.get_property("no-mmc").is_some() {
        CardInitPreference::SdOnly
    } else if node.get_property("no-sd").is_some() || node.get_property("non-removable").is_some() {
        CardInitPreference::MmcFirst
    } else {
        CardInitPreference::SdFirst
    }
}

fn dwmmc_clock_setup(info: &FdtInfo<'_>) -> Result<Option<DwMmcClockSetup>, OnProbeError> {
    let Some(clock) = info.find_clock_line_by_name("ciu")? else {
        warn!("[{}] ciu clock provider is not available", info.node.name());
        return Ok(None);
    };
    clock.set_rate(DWMMC_STABLE_REFERENCE_CLOCK as u64)?;
    let rate = clock.rate()?;
    let reference_clock = validate_reference_clock(info, rate).ok_or_else(|| {
        OnProbeError::other(format!(
            "[{}] invalid ciu clock rate {rate} Hz",
            info.node.name()
        ))
    })?;
    Ok(Some(DwMmcClockSetup {
        reference_clock,
        clock: RockchipDwMmcClock { clock },
    }))
}

fn is_rk3588_dwmmc(info: &FdtInfo<'_>) -> bool {
    info.node
        .as_node()
        .compatibles()
        .any(|compatible| compatible == "rockchip,rk3588-dw-mshc")
}

fn validate_reference_clock(info: &FdtInfo<'_>, rate: u64) -> Option<u32> {
    if rate == 0 || rate > u32::MAX as u64 {
        warn!("[{}] invalid ciu clock rate {} Hz", info.node.name(), rate);
        return None;
    }
    Some(rate as u32)
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

    #[test]
    fn sdio_only_controller_is_not_registered_as_a_block_device() {
        let mut node = Node::new("dwmmc@fe000000");
        node.add_property(fdt_edit::Property::new("no-sd", Vec::new()));
        node.add_property(fdt_edit::Property::new("no-mmc", Vec::new()));

        assert!(!supports_block_card_protocol(&node));
    }

    #[test]
    fn removable_sd_controller_remains_block_capable() {
        let mut node = Node::new("dwmmc@fe2b0000");
        node.add_property(fdt_edit::Property::new("no-mmc", Vec::new()));

        assert!(supports_block_card_protocol(&node));
    }
}
