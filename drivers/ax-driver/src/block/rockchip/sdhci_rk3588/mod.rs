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

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::{ptr::NonNull, time::Duration};

use fdt_edit::Node;
use log::{info, warn};
use rdif_pinctrl::PinctrlDevice;
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdhci_host::{HostClock, HostResetHook, Sdhci, rdif as sdhci_rdif};
use sdmmc_protocol::{
    Error, OperationPoll,
    error::{ErrorContext, Phase},
    sdio::{CardInfo, CardInitPreference, SdioHost2Adapter, SdioInitScratch, SdioSdmmc},
};
use spin::Once;

use super::clock::{RockchipClockOps, apply_assigned_clocks, enable_node_clocks};
use crate::{
    block::ProbeFdtBlock,
    mmio::iomap,
    soc::{
        RockchipPinCtrl, rk3588_enable_power_domain, rk3588_reset_assert, rk3588_reset_deassert,
    },
};

// RK3588 DWCMSHC follows Linux's normal SDHCI completion path: command/data
// status is acknowledged in the hard IRQ and task context advances the RDIF
// submit/poll queue.
const ROCKCHIP_SDHCI_IRQ_DRIVEN: bool = true;
const SDMMC_INIT_POLL_DELAY: Duration = Duration::from_micros(1);
const SDMMC_INIT_RETRY_DELAY: Duration = Duration::from_millis(10);
const RK3588_EMMC_PINCTRL_SYMBOLS: [&str; 5] = [
    "emmc_rstnout",
    "emmc_bus8",
    "emmc_clk",
    "emmc_cmd",
    "emmc_data_strobe",
];
const DWCMSHC_P_VENDOR_AREA1: usize = 0xe8;
const DWCMSHC_AREA1_MASK: u16 = 0x0fff;
const DWCMSHC_HOST_CTRL3: usize = 0x08;
const DWCMSHC_EMMC_CONTROL: usize = 0x2c;
const DWCMSHC_CARD_IS_EMMC: u16 = 1 << 0;
const DWCMSHC_EMMC_DLL_CTRL: usize = 0x800;
const DWCMSHC_EMMC_DLL_RXCLK: usize = 0x804;
const DWCMSHC_EMMC_DLL_TXCLK: usize = 0x808;
const DWCMSHC_EMMC_DLL_STRBIN: usize = 0x80c;
const DWCMSHC_EMMC_DLL_CMDOUT: usize = 0x810;
const DWCMSHC_EMMC_MISC_CON: usize = 0x81c;
const DWCMSHC_EMMC_DLL_BYPASS: u32 = 1 << 24;
const DWCMSHC_EMMC_DLL_START: u32 = 1 << 0;
const DWCMSHC_EMMC_DLL_DLYENA: u32 = 1 << 27;
const DLL_RXCLK_ORI_GATE: u32 = 1 << 31;
const DLL_STRBIN_DELAY_NUM_SEL: u32 = 1 << 26;
const DLL_STRBIN_DELAY_NUM_DEFAULT: u32 = 16;
const DLL_STRBIN_DELAY_NUM_OFFSET: u32 = 16;
const MISC_INTCLK_EN: u32 = 1 << 1;

const DWC_MSHC_PTR_PHY_R: usize = 0x300;
const PHY_CNFG_R: usize = DWC_MSHC_PTR_PHY_R;
const PHY_CMDPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x04;
const PHY_DATAPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x06;
const PHY_CLKPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x08;
const PHY_STBPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x0a;
const PHY_RSTNPAD_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x0c;
const PHY_SDCLKDL_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x1d;
const PHY_SDCLKDL_DC_R: usize = DWC_MSHC_PTR_PHY_R + 0x1e;
const PHY_SMPLDL_CNFG_R: usize = DWC_MSHC_PTR_PHY_R + 0x20;
const PHY_DLL_CTRL_R: usize = DWC_MSHC_PTR_PHY_R + 0x24;
const PHY_DLL_CNFG2_R: usize = DWC_MSHC_PTR_PHY_R + 0x26;
const PHY_CNFG_RSTN_DEASSERT: u32 = 1 << 0;
const PHY_CNFG_PAD_SP: u32 = 0x0c;
const PHY_CNFG_PAD_SN: u32 = 0x0c;
const PHY_PAD_RXSEL_3V3: u16 = 0x2;
const PHY_PAD_WEAKPULL_PULLUP: u16 = 0x1;
const PHY_PAD_WEAKPULL_PULLDOWN: u16 = 0x2;
const PHY_PAD_TXSLEW_CTRL_P: u16 = 0x3;
const PHY_PAD_TXSLEW_CTRL_N: u16 = 0x3;
const PHY_SDCLKDL_CNFG_UPDATE: u8 = 1 << 4;
const PHY_SDCLKDL_DC_DEFAULT: u8 = 0x32;
const PHY_SMPLDL_CNFG_BYPASS_EN: u8 = 1 << 1;
const PHY_DLL_CTRL_ENABLE: u8 = 0x1;
const PHY_DLL_CNFG2_JUMPSTEP: u8 = 0x0a;
static SDHCI_RESET_HOOK: RockchipSdhciResetHook = RockchipSdhciResetHook;
static RESET_SPECS: Once<Vec<ResetSpec>> = Once::new();

type RockchipSdhci = SdioSdmmc<SdioHost2Adapter<Sdhci>>;

struct RockchipSdhciClock {
    clock: RockchipClockOps,
}
struct RockchipSdhciResetHook;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResetSpec {
    name: Option<String>,
    id: u64,
}

impl HostClock for RockchipSdhciClock {
    fn effective_clock_hz(&self, target_hz: u32) -> u32 {
        if target_hz <= 400_000 {
            375_000
        } else {
            target_hz
        }
    }

    fn clock_div_zero_broken(&self) -> bool {
        true
    }

    fn set_clock(&self, target_hz: u32) -> Result<(), Error> {
        self.clock
            .set_rate(u64::from(target_hz))
            .map_err(|_| clock_error())?;
        let rate = self.clock.rate().map_err(|_| clock_error())?;
        info!("rockchip-sdhci: core clock set to {} Hz", rate);
        Ok(())
    }

    fn prepare_host_clock(&self, host: &mut Sdhci, target_hz: u32) -> Result<(), Error> {
        configure_rk3588_dwcmshc_clock(host, target_hz)
    }
}

impl HostResetHook for RockchipSdhciResetHook {
    fn before_reset_all(&self, _host: &mut Sdhci) -> Result<(), Error> {
        let Some(resets) = RESET_SPECS.get() else {
            return Ok(());
        };
        assert_resets(resets).map_err(|_| reset_error())?;
        axklib::time::busy_wait(Duration::from_micros(1));
        deassert_resets(resets).map_err(|_| reset_error())?;
        Ok(())
    }

    fn after_reset(&self, host: &mut Sdhci) -> Result<(), Error> {
        init_rk3588_dwcmshc_after_reset(host)
    }
}

crate::model_register!(
    name: "Rockchip sdhci",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-dwcmshc"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    apply_rockchip_sdhci_resources(info)?;
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
        "rockchip-sdhci probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;

    let mut host = unsafe { Sdhci::new(mmio_base) };
    if let Some(clock) = sdhci_core_clock(info)? {
        info!("rockchip-sdhci: using external CRU clock");
        host.set_external_clock(RockchipSdhciClock { clock });
    } else {
        warn!("rockchip-sdhci: no core clock found; using SDHCI internal clock divider");
    }
    host.set_reset_hook(&SDHCI_RESET_HOOK);
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    host.set_dma(dma.clone());

    info!("rockchip-sdhci: initialize card through native host2 bus ops");
    let mut card = SdioSdmmc::new_host2(host);
    let card_info = poll_card_init_mmc(&mut card)
        .map_err(|e| card_init_error(base_reg.address, mmio_size, e))?;
    card.host_mut()
        .with_host_mut(|host| host.clear_external_clock());
    info!(
        "SDHCI card: kind={:?} high_capacity={} rca={} ocr={:#010x} capacity_blocks={:?} cid={} \
         ext_csd={}",
        card_info.kind,
        card_info.high_capacity,
        card_info.rca,
        card_info.ocr,
        card_info.capacity_blocks,
        card_info.cid.is_some(),
        card_info.ext_csd.is_some()
    );

    let dev = sdhci_rdif::device(
        card,
        rockchip_sdhci_rdif_config(card_info.capacity_blocks.unwrap_or(0), dma),
    );
    let irq = probe.register_block(dev)?;
    info!("rockchip-sdhci block device registered irq={:?}", irq);
    Ok(())
}

fn apply_rockchip_sdhci_resources(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    apply_assigned_clocks(info, "SDHCI")?;
    if let Some(pinctrl) = rdrive::get_one::<PinctrlDevice>() {
        let mut pinctrl = pinctrl
            .lock()
            .map_err(|err| OnProbeError::other(format!("failed to lock PinctrlDevice: {err}")))?;
        let pinctrl = pinctrl
            .typed_mut::<RockchipPinCtrl>()
            .ok_or_else(|| OnProbeError::other("PinctrlDevice is not backed by RockchipPinCtrl"))?;
        let configured = pinctrl.apply_default_pinctrl(info.node)?;
        if configured.is_empty() {
            let fallback = rk3588_emmc_pinctrl_symbol_paths();
            if !fallback.is_empty() {
                let mut total = 0;
                for path in &fallback {
                    total += pinctrl.apply_pinctrl_path(path.as_str())?.len();
                }
                info!(
                    "[{}] applied {} RK3588 eMMC symbol fallback pinctrl pins",
                    info.node.name(),
                    total
                );
            }
        }
    }
    enable_power_domains(parse_power_domains(info.node.as_node())?)?;
    let resets = parse_resets(info.node.as_node())?;
    if !resets.is_empty() {
        RESET_SPECS.call_once(|| resets);
    }
    enable_node_clocks(info, "SDHCI");
    Ok(())
}

fn rk3588_emmc_pinctrl_symbol_paths() -> Vec<String> {
    rdrive::with_fdt(|fdt| {
        fdt.get_by_path("/__symbols__")
            .map(|symbols| rk3588_emmc_pinctrl_paths_from_symbols(symbols.as_node()))
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

fn rk3588_emmc_pinctrl_paths_from_symbols(symbols: &Node) -> Vec<String> {
    RK3588_EMMC_PINCTRL_SYMBOLS
        .into_iter()
        .filter_map(|symbol| {
            symbols
                .get_property(symbol)?
                .as_str()
                .map(ToString::to_string)
        })
        .collect()
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
                "failed to enable RK3588 SDHCI power domain {domain}: {err}"
            ))
        })?;
    }
    Ok(())
}

fn parse_resets(node: &Node) -> Result<Vec<ResetSpec>, OnProbeError> {
    let Some(prop) = node.get_property("resets") else {
        return Ok(Vec::new());
    };
    let cells = prop.get_u32_iter().collect::<Vec<_>>();
    if cells.len() % 2 != 0 {
        return Err(OnProbeError::other(format!(
            "[{}] has malformed resets",
            node.name()
        )));
    }
    let names = node
        .get_property("reset-names")
        .map(|prop| {
            prop.as_str_iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(cells
        .chunks(2)
        .enumerate()
        .map(|(index, chunk)| ResetSpec {
            name: names.get(index).cloned(),
            id: u64::from(chunk[1]),
        })
        .collect())
}

fn assert_resets(resets: &[ResetSpec]) -> Result<(), OnProbeError> {
    for reset in resets {
        rk3588_reset_assert(reset.id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to assert RK3588 SDHCI reset {:?} ({:#x}): {err}",
                reset.name, reset.id
            ))
        })?;
    }
    Ok(())
}

fn deassert_resets(resets: &[ResetSpec]) -> Result<(), OnProbeError> {
    for reset in resets {
        rk3588_reset_deassert(reset.id).map_err(|err| {
            OnProbeError::other(format!(
                "failed to deassert RK3588 SDHCI reset {:?} ({:#x}): {err}",
                reset.name, reset.id
            ))
        })?;
    }
    Ok(())
}

fn init_rk3588_dwcmshc_after_reset(host: &mut Sdhci) -> Result<(), Error> {
    let base = NonNull::new(host.mmio_base() as *mut u8).ok_or(Error::InvalidArgument)?;
    let area1 = dwcmshc_vendor_area1(base);

    // Match Linux rk3588 DWCMSHC low-speed setup before CMD1/CMD8:
    // keep internal clock alive, mark the vendor area as eMMC, disable
    // command-conflict checks, and bypass DLL while identification runs.
    write_u32(
        base,
        DWCMSHC_EMMC_MISC_CON,
        read_u32(base, DWCMSHC_EMMC_MISC_CON) | MISC_INTCLK_EN,
    );
    write_u32(base, area1 + DWCMSHC_HOST_CTRL3, 0);
    write_u16(
        base,
        area1 + DWCMSHC_EMMC_CONTROL,
        read_u16(base, area1 + DWCMSHC_EMMC_CONTROL) | DWCMSHC_CARD_IS_EMMC,
    );
    configure_rk3588_dwcmshc_clock_regs(base, area1, 400_000);
    init_rk3588_dwcmshc_phy_3v3(base);
    info!("rockchip-sdhci: RK3588 DWCMSHC vendor reset area1={area1:#x}");
    Ok(())
}

fn configure_rk3588_dwcmshc_clock(host: &mut Sdhci, target_hz: u32) -> Result<(), Error> {
    let base = NonNull::new(host.mmio_base() as *mut u8).ok_or(Error::InvalidArgument)?;
    let area1 = dwcmshc_vendor_area1(base);
    configure_rk3588_dwcmshc_clock_regs(base, area1, target_hz);
    Ok(())
}

fn configure_rk3588_dwcmshc_clock_regs(base: NonNull<u8>, area1: usize, target_hz: u32) {
    // Linux's rk35xx set_clock path disables command-conflict checks and
    // programs the low-speed DLL bypass while SDHCI clock output is gated.
    write_u32(base, area1 + DWCMSHC_HOST_CTRL3, 0);
    if target_hz <= 52_000_000 {
        write_u32(base, DWCMSHC_EMMC_DLL_CTRL, 0);
        write_u32(
            base,
            DWCMSHC_EMMC_DLL_CTRL,
            DWCMSHC_EMMC_DLL_BYPASS | DWCMSHC_EMMC_DLL_START,
        );
        write_u32(base, DWCMSHC_EMMC_DLL_RXCLK, DLL_RXCLK_ORI_GATE);
        write_u32(base, DWCMSHC_EMMC_DLL_TXCLK, 0);
        write_u32(base, DWCMSHC_EMMC_DLL_CMDOUT, 0);
        write_u32(
            base,
            DWCMSHC_EMMC_DLL_STRBIN,
            DWCMSHC_EMMC_DLL_DLYENA
                | DLL_STRBIN_DELAY_NUM_SEL
                | (DLL_STRBIN_DELAY_NUM_DEFAULT << DLL_STRBIN_DELAY_NUM_OFFSET),
        );
    }
}

fn init_rk3588_dwcmshc_phy_3v3(base: NonNull<u8>) {
    let phy_cfg = PHY_CNFG_RSTN_DEASSERT | (PHY_CNFG_PAD_SP << 16) | (PHY_CNFG_PAD_SN << 20);
    write_u32(base, PHY_CNFG_R, phy_cfg);
    write_u8(base, PHY_SDCLKDL_CNFG_R, PHY_SDCLKDL_CNFG_UPDATE);
    write_u8(base, PHY_SDCLKDL_DC_R, PHY_SDCLKDL_DC_DEFAULT);
    write_u8(base, PHY_DLL_CNFG2_R, PHY_DLL_CNFG2_JUMPSTEP);
    write_u8(base, PHY_SDCLKDL_CNFG_R, 0);

    let pad_pullup = PHY_PAD_RXSEL_3V3
        | (PHY_PAD_WEAKPULL_PULLUP << 3)
        | (PHY_PAD_TXSLEW_CTRL_P << 5)
        | (PHY_PAD_TXSLEW_CTRL_N << 9);
    write_u16(base, PHY_CMDPAD_CNFG_R, pad_pullup);
    write_u16(base, PHY_DATAPAD_CNFG_R, pad_pullup);
    write_u16(base, PHY_RSTNPAD_CNFG_R, pad_pullup);

    let clk_pad = (PHY_PAD_TXSLEW_CTRL_P << 5) | (PHY_PAD_TXSLEW_CTRL_N << 9);
    write_u16(base, PHY_CLKPAD_CNFG_R, clk_pad);

    let strobe_pad = PHY_PAD_RXSEL_3V3
        | (PHY_PAD_WEAKPULL_PULLDOWN << 3)
        | (PHY_PAD_TXSLEW_CTRL_P << 5)
        | (PHY_PAD_TXSLEW_CTRL_N << 9);
    write_u16(base, PHY_STBPAD_CNFG_R, strobe_pad);
    write_u8(base, PHY_SMPLDL_CNFG_R, PHY_SMPLDL_CNFG_BYPASS_EN);
    write_u8(base, PHY_DLL_CTRL_R, PHY_DLL_CTRL_ENABLE);
}

fn reset_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Init))
}

fn dwcmshc_vendor_area1(base: NonNull<u8>) -> usize {
    (read_u16(base, DWCMSHC_P_VENDOR_AREA1) & DWCMSHC_AREA1_MASK) as usize
}

fn read_u32(base: NonNull<u8>, off: usize) -> u32 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u32) }
}

#[cfg(test)]
fn read_u8(base: NonNull<u8>, off: usize) -> u8 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off)) }
}

fn read_u16(base: NonNull<u8>, off: usize) -> u16 {
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u16) }
}

fn write_u32(base: NonNull<u8>, off: usize, val: u32) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u32, val) }
}

fn write_u16(base: NonNull<u8>, off: usize, val: u16) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u16, val) }
}

fn write_u8(base: NonNull<u8>, off: usize, val: u8) {
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off), val) }
}

fn rockchip_sdhci_rdif_config(
    capacity_blocks: u64,
    dma: dma_api::DeviceDma,
) -> sdhci_rdif::BlockConfig {
    sdhci_rdif::dma_config(
        "rockchip-sdhci",
        capacity_blocks,
        ROCKCHIP_SDHCI_IRQ_DRIVEN,
        dma,
    )
}

fn poll_card_init_mmc(card: &mut RockchipSdhci) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request =
        card.submit_init_with_preference(CardInitPreference::MmcFirst, &mut scratch)?;
    loop {
        match card.poll_init_request(&mut request)? {
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
        "failed to initialize SDHCI device at [PA:{:?}, SZ:0x{:x}): {err:?}",
        address, size
    ))
}

fn card_init_error(address: u64, size: u64, err: Error) -> OnProbeError {
    if is_absent_card_init_error(err) {
        warn!(
            "rockchip-sdhci: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping controller: \
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

fn sdhci_core_clock(info: &FdtInfo<'_>) -> Result<Option<RockchipClockOps>, OnProbeError> {
    for clk in info.node.clocks() {
        info!(
            "rockchip-sdhci clock: phandle <{}>, name: {:?}, cells: {}",
            clk.phandle, clk.name, clk.cells
        );
        if clk.name == Some("core".to_string()) {
            return RockchipClockOps::from_node_clock(info, &clk);
        }
    }
    Ok(None)
}

fn clock_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Init))
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn rk3588_block_io_uses_adma_config_with_irq_completion() {
        let config = rockchip_sdhci_rdif_config(8, test_dma());

        assert_eq!(config.name, "rockchip-sdhci");
        assert_eq!(config.capacity_blocks, 8);
        assert!(config.uses_dma());
        assert!(config.irq_driven);
    }

    #[test]
    fn rk3588_adma_queue_limits_expose_sdhci_window() {
        let config = rockchip_sdhci_rdif_config(8, test_dma());
        let limits = sdmmc_protocol::rdif::queue_limits(&config, config.dma_mask);

        assert_eq!(limits.max_blocks_per_request, sdhci_host::ADMA2_MAX_BLOCKS);
        assert_eq!(limits.max_segment_size, sdhci_host::ADMA2_MAX_TRANSFER_SIZE);
        assert_eq!(limits.max_segments, 1);
    }

    #[test]
    fn parse_resets_reads_rk3588_sdhci_reset_cells_and_names() {
        let mut node = Node::new("mmc@fe2e0000");
        let mut resets = Vec::new();
        for id in [10_u32, 11] {
            resets.extend_from_slice(&0x1000_u32.to_be_bytes());
            resets.extend_from_slice(&id.to_be_bytes());
        }
        node.add_property(fdt_edit::Property::new("resets", resets));
        node.add_property(fdt_edit::Property::new(
            "reset-names",
            b"core\0bus\0".to_vec(),
        ));

        assert_eq!(
            parse_resets(&node).unwrap(),
            vec![
                ResetSpec {
                    name: Some(String::from("core")),
                    id: 10
                },
                ResetSpec {
                    name: Some(String::from("bus")),
                    id: 11
                }
            ]
        );
    }

    #[test]
    fn parse_power_domains_accepts_absent_sdhci_domain() {
        let node = Node::new("mmc@fe2e0000");

        assert_eq!(parse_power_domains(&node).unwrap(), Vec::<usize>::new());
    }

    #[test]
    fn emmc_symbol_fallback_reads_linux_pinctrl_paths_in_order() {
        let mut symbols = Node::new("__symbols__");
        symbols.add_property(fdt_edit::Property::new(
            "emmc_cmd",
            b"/pinctrl/emmc/emmc-cmd\0".to_vec(),
        ));
        symbols.add_property(fdt_edit::Property::new(
            "emmc_rstnout",
            b"/pinctrl/emmc/emmc-rstnout\0".to_vec(),
        ));
        symbols.add_property(fdt_edit::Property::new(
            "emmc_clk",
            b"/pinctrl/emmc/emmc-clk\0".to_vec(),
        ));
        symbols.add_property(fdt_edit::Property::new(
            "emmc_bus8",
            b"/pinctrl/emmc/emmc-bus8\0".to_vec(),
        ));
        symbols.add_property(fdt_edit::Property::new(
            "emmc_data_strobe",
            b"/pinctrl/emmc/emmc-data-strobe\0".to_vec(),
        ));

        assert_eq!(
            rk3588_emmc_pinctrl_paths_from_symbols(&symbols),
            vec![
                String::from("/pinctrl/emmc/emmc-rstnout"),
                String::from("/pinctrl/emmc/emmc-bus8"),
                String::from("/pinctrl/emmc/emmc-clk"),
                String::from("/pinctrl/emmc/emmc-cmd"),
                String::from("/pinctrl/emmc/emmc-data-strobe"),
            ]
        );
    }

    #[test]
    fn dwcmshc_vendor_area_masks_pointer_register() {
        let mut mmio = [0u8; 0x1000];
        let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
        unsafe {
            core::ptr::write_volatile(
                mmio.as_mut_ptr().add(DWCMSHC_P_VENDOR_AREA1) as *mut u16,
                0xfabc,
            );
        }

        assert_eq!(dwcmshc_vendor_area1(base), 0x0abc);
    }

    #[test]
    fn rk3588_dwcmshc_after_reset_programs_low_speed_vendor_and_phy_defaults() {
        let mut mmio = [0xff_u8; 0x1000];
        let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
        write_u16(base, DWCMSHC_P_VENDOR_AREA1, 0x0500);
        write_u32(base, DWCMSHC_EMMC_MISC_CON, 0);
        write_u16(base, 0x0500 + DWCMSHC_EMMC_CONTROL, 0);

        let mut host = unsafe { Sdhci::new(base) };
        init_rk3588_dwcmshc_after_reset(&mut host).unwrap();

        assert_eq!(read_u32(base, 0x0500 + DWCMSHC_HOST_CTRL3), 0);
        assert_eq!(
            read_u16(base, 0x0500 + DWCMSHC_EMMC_CONTROL) & DWCMSHC_CARD_IS_EMMC,
            DWCMSHC_CARD_IS_EMMC
        );
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_MISC_CON) & MISC_INTCLK_EN,
            MISC_INTCLK_EN
        );
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_DLL_CTRL),
            DWCMSHC_EMMC_DLL_BYPASS | DWCMSHC_EMMC_DLL_START
        );
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_RXCLK), DLL_RXCLK_ORI_GATE);
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_TXCLK), 0);
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_CMDOUT), 0);
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_DLL_STRBIN),
            DWCMSHC_EMMC_DLL_DLYENA
                | DLL_STRBIN_DELAY_NUM_SEL
                | (DLL_STRBIN_DELAY_NUM_DEFAULT << DLL_STRBIN_DELAY_NUM_OFFSET)
        );

        let phy_cfg = PHY_CNFG_RSTN_DEASSERT | (PHY_CNFG_PAD_SP << 16) | (PHY_CNFG_PAD_SN << 20);
        assert_eq!(read_u32(base, PHY_CNFG_R), phy_cfg);
        assert_eq!(read_u8(base, PHY_SDCLKDL_CNFG_R), 0);
        assert_eq!(read_u8(base, PHY_SDCLKDL_DC_R), PHY_SDCLKDL_DC_DEFAULT);
        assert_eq!(read_u8(base, PHY_DLL_CNFG2_R), PHY_DLL_CNFG2_JUMPSTEP);
        assert_eq!(read_u8(base, PHY_SMPLDL_CNFG_R), PHY_SMPLDL_CNFG_BYPASS_EN);
        assert_eq!(read_u8(base, PHY_DLL_CTRL_R), PHY_DLL_CTRL_ENABLE);

        let pad_pullup = PHY_PAD_RXSEL_3V3
            | (PHY_PAD_WEAKPULL_PULLUP << 3)
            | (PHY_PAD_TXSLEW_CTRL_P << 5)
            | (PHY_PAD_TXSLEW_CTRL_N << 9);
        assert_eq!(read_u16(base, PHY_CMDPAD_CNFG_R), pad_pullup);
        assert_eq!(read_u16(base, PHY_DATAPAD_CNFG_R), pad_pullup);
        assert_eq!(read_u16(base, PHY_RSTNPAD_CNFG_R), pad_pullup);

        let clk_pad = (PHY_PAD_TXSLEW_CTRL_P << 5) | (PHY_PAD_TXSLEW_CTRL_N << 9);
        assert_eq!(read_u16(base, PHY_CLKPAD_CNFG_R), clk_pad);

        let strobe_pad = PHY_PAD_RXSEL_3V3
            | (PHY_PAD_WEAKPULL_PULLDOWN << 3)
            | (PHY_PAD_TXSLEW_CTRL_P << 5)
            | (PHY_PAD_TXSLEW_CTRL_N << 9);
        assert_eq!(read_u16(base, PHY_STBPAD_CNFG_R), strobe_pad);
    }

    #[test]
    fn rk3588_dwcmshc_clock_stage_programs_low_speed_dll_bypass() {
        let mut mmio = [0xff_u8; 0x1000];
        let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
        write_u16(base, DWCMSHC_P_VENDOR_AREA1, 0x0500);
        write_u32(base, 0x0500 + DWCMSHC_HOST_CTRL3, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_CTRL, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_RXCLK, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_TXCLK, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_CMDOUT, u32::MAX);
        write_u32(base, DWCMSHC_EMMC_DLL_STRBIN, u32::MAX);

        let mut host = unsafe { Sdhci::new(base) };
        configure_rk3588_dwcmshc_clock(&mut host, 400_000).unwrap();

        assert_eq!(read_u32(base, 0x0500 + DWCMSHC_HOST_CTRL3), 0);
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_DLL_CTRL),
            DWCMSHC_EMMC_DLL_BYPASS | DWCMSHC_EMMC_DLL_START
        );
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_RXCLK), DLL_RXCLK_ORI_GATE);
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_TXCLK), 0);
        assert_eq!(read_u32(base, DWCMSHC_EMMC_DLL_CMDOUT), 0);
        assert_eq!(
            read_u32(base, DWCMSHC_EMMC_DLL_STRBIN),
            DWCMSHC_EMMC_DLL_DLYENA
                | DLL_STRBIN_DELAY_NUM_SEL
                | (DLL_STRBIN_DELAY_NUM_DEFAULT << DLL_STRBIN_DELAY_NUM_OFFSET)
        );
    }

    fn test_dma() -> dma_api::DeviceDma {
        dma_api::DeviceDma::new_legacy(u32::MAX as u64, &TEST_DMA)
    }

    struct TestDma;
    static TEST_DMA: TestDma = TestDma;

    impl dma_api::DmaOp for TestDma {
        fn page_size(&self) -> usize {
            sdmmc_protocol::rdif::BLOCK_SIZE
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: dma_api::DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_contiguous(&self, _handle: dma_api::DmaAllocHandle) {}

        unsafe fn alloc_coherent(
            &self,
            _constraints: dma_api::DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_coherent(&self, _handle: dma_api::DmaAllocHandle) {}

        unsafe fn map_streaming(
            &self,
            _constraints: dma_api::DmaConstraints,
            _addr: core::ptr::NonNull<u8>,
            _size: core::num::NonZeroUsize,
            _direction: dma_api::DmaDirection,
        ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
            Err(dma_api::DmaError::NoMemory)
        }

        unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
    }
}
