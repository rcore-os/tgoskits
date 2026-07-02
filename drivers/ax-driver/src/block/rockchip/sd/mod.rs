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
use core::time::Duration;

use dwmmc_host::{CardDetect, DwMmc, HostClock, rdif as dwmmc_rdif};
use fdt_edit::{Node, Phandle};
use log::{info, warn};
use rdif_pinctrl::{FdtPinctrl, PinctrlDevice};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdmmc_protocol::{
    Error, OperationPoll,
    error::{ErrorContext, Phase},
    sdio::{CardInfo, SdioHost2Adapter, SdioInitScratch, SdioSdmmc},
};

use super::clock::{
    RockchipClockOps, ScmiClockOps, apply_assigned_clocks, enable_node_clocks, scmi_named_clock,
};
use crate::{
    block::ProbeFdtBlock,
    mmio::iomap,
    soc::{RockchipFdtPinctrlParser, rk3588_enable_power_domain},
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
const RK3588_SDMMC_SAMPLE_PHASE_CANDIDATES: [u32; 8] = [0, 45, 90, 135, 180, 225, 270, 315];
const SDMMC_INIT_POLL_DELAY: Duration = Duration::from_micros(1);
const SDMMC_INIT_RETRY_DELAY: Duration = Duration::from_millis(10);

type RockchipDwMmc = SdioSdmmc<SdioHost2Adapter<DwMmc>>;

enum RockchipDwMmcClock {
    Rdrive(RockchipClockOps),
    Scmi(ScmiClockOps),
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
        let rate = match self {
            Self::Rdrive(clock) => {
                clock
                    .set_rate(cclkin)
                    .map_err(|_| Error::BadResponse(ErrorContext::new(Phase::Init)))?;
                clock
                    .rate()
                    .map_err(|_| Error::BadResponse(ErrorContext::new(Phase::Init)))?
            }
            Self::Scmi(clock) => {
                clock
                    .set_rate(cclkin)
                    .ok_or_else(|| Error::BadResponse(ErrorContext::new(Phase::Init)))?;
                clock
                    .rate()
                    .ok_or_else(|| Error::BadResponse(ErrorContext::new(Phase::Init)))?
            }
        };
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

use phase::{init_rk3588_sdmmc_phase, tune_rk3588_sdmmc_sample_phase};

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
    let clock_setup = dwmmc_clock_setup(info);
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
    host.set_dma(dma.clone());

    info!("rockchip-dwmmc: initialize card through native host2 bus ops");
    let mut sd = SdioSdmmc::new_host2(host);
    sd.set_sd_speed_selection_enabled(ENABLE_SD_SPEED_SELECTION);
    let card_info = poll_card_init(&mut sd).map_err(|e| {
        warn!("rockchip-dwmmc: card init failed: {:?}", e);
        card_init_error(base_reg.address, mmio_size, e)
    })?;
    sd.host_mut()
        .with_host_mut(|host| host.clear_external_clock());
    info!(
        "rockchip-dwmmc card: kind={:?} high_capacity={} rca={} ocr={:#010x} capacity_blocks={:?} \
         cid={} ext_csd={}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
        card_info.cid.is_some(),
        card_info.ext_csd.is_some()
    );

    if let Some(reference_clock) = sd
        .host()
        .with_host(|host| validate_reference_clock(info, u64::from(host.reference_clock())))
        && is_rk3588_dwmmc(info)
    {
        tune_rk3588_sdmmc_sample_phase(&mut sd, reference_clock);
    }

    let dev = dwmmc_rdif::device(
        sd,
        dwmmc_rdif::dma_config(
            "rockchip-sd",
            card_info.capacity_blocks.unwrap_or(0),
            true,
            dma,
        ),
    );
    let irq = probe.register_block(dev)?;
    info!("rockchip-sd block device registered irq={:?}", irq);
    Ok(())
}

fn apply_rockchip_sd_resources(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    apply_assigned_clocks(info, "SDMMC")?;
    let Some(pinctrl) = rdrive::get_one::<PinctrlDevice>() else {
        warn!(
            "[{}] PinctrlDevice not found; skip SDMMC pinctrl and fixed regulators",
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
    enable_power_domains(parse_power_domains(info.node.as_node())?)?;
    enable_node_clocks(info, "SDMMC");
    Ok(())
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

fn parse_power_domains(node: &Node) -> Result<Vec<usize>, OnProbeError> {
    let Some(prop) = node.get_property("power-domains") else {
        return Ok(Vec::new());
    };
    let cells = prop.get_u32_iter().collect::<Vec<_>>();
    if cells.len() % 2 != 0 {
        return Err(OnProbeError::other(format!(
            "[{}] has malformed power-domains",
            node.name()
        )));
    }
    Ok(cells.chunks(2).map(|chunk| chunk[1] as usize).collect())
}

fn enable_power_domains(domains: Vec<usize>) -> Result<(), OnProbeError> {
    for domain in domains {
        rk3588_enable_power_domain(domain).map_err(|err| {
            OnProbeError::other(format!(
                "failed to enable RK3588 SDMMC power domain {domain}: {err}"
            ))
        })?;
    }
    Ok(())
}

fn poll_card_init(sd: &mut RockchipDwMmc) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request = sd.submit_init(&mut scratch)?;
    loop {
        match sd.poll_init_request(&mut request)? {
            OperationPoll::Pending => {
                if request.take_needs_pace() {
                    axklib::time::busy_wait(SDMMC_INIT_RETRY_DELAY);
                } else {
                    axklib::time::busy_wait(SDMMC_INIT_POLL_DELAY);
                }
            }
            OperationPoll::Complete(info) => return Ok(info),
            _ => return Err(Error::UnsupportedCommand),
        }
    }
}

fn init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    OnProbeError::other(format!(
        "failed to initialize DWMMC device at [PA:{:?}, SZ:0x{:x}): {err:?}",
        address, size
    ))
}

fn card_init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "rockchip-dwmmc: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping controller: \
             {err:?}",
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

fn dwmmc_clock_setup(info: &FdtInfo<'_>) -> Option<DwMmcClockSetup> {
    match RockchipClockOps::named(info, "ciu") {
        Ok(Some(clock)) => {
            if let Err(err) = clock.set_rate(DWMMC_STABLE_REFERENCE_CLOCK as u64) {
                warn!(
                    "[{}] failed to set ciu clock {:?} to {} Hz: {err}",
                    info.node.name(),
                    clock.id(),
                    DWMMC_STABLE_REFERENCE_CLOCK
                );
            }
            match clock.rate() {
                Ok(rate) => Some(DwMmcClockSetup {
                    reference_clock: validate_reference_clock(info, rate)?,
                    clock: RockchipDwMmcClock::Rdrive(clock),
                }),
                Err(err) => {
                    warn!("[{}] failed to read ciu clock: {err}", info.node.name());
                    None
                }
            }
        }
        Ok(None) | Err(_) => {
            if let Some(clock) = scmi_named_clock(info, "ciu") {
                clock.set_rate(DWMMC_STABLE_REFERENCE_CLOCK as u64)?;
                let rate = clock.rate()?;
                return Some(DwMmcClockSetup {
                    reference_clock: validate_reference_clock(info, rate)?,
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
    fn parse_power_domains_reads_rockchip_provider_cells() {
        let mut node = Node::new("mmc@fe2c0000");
        let mut raw = Vec::new();
        raw.extend_from_slice(&0x1111_u32.to_be_bytes());
        raw.extend_from_slice(&30_u32.to_be_bytes());
        node.add_property(fdt_edit::Property::new("power-domains", raw));

        assert_eq!(parse_power_domains(&node).unwrap(), vec![30]);
    }

    #[test]
    fn parse_power_domains_rejects_malformed_cells() {
        let mut node = Node::new("mmc@fe2c0000");
        node.add_property(fdt_edit::Property::new(
            "power-domains",
            0x1111_u32.to_be_bytes().to_vec(),
        ));

        assert!(parse_power_domains(&node).is_err());
    }
}
