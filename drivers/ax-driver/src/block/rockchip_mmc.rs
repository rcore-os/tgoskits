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

use alloc::{format, string::ToString};
use core::time::Duration;

use log::{info, warn};
use rdif_clk::ClockId;
use rdrive::{
    Device,
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdhci_host::{HostClock, Sdhci, rdif as sdhci_rdif};
use sdmmc_protocol::{
    Error, OperationPoll,
    error::{ErrorContext, Phase},
    sdio::{CardInfo, CardInitPreference, SdioInitScratch, SdioSdmmc},
};
use spin::Once;

use crate::{block::ProbeFdtBlock, mmio::iomap};

const SDHCI_POWER_330: u8 = 0x0e;
static SDHCI_CLOCK: RockchipSdhciClock = RockchipSdhciClock;

type RockchipSdhci = SdioSdmmc<Sdhci>;

struct RockchipSdhciClock;

impl HostClock for RockchipSdhciClock {
    fn set_clock(&self, target_hz: u32) -> Result<(), Error> {
        set_sdhci_clock(target_hz)
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

    init_core_clock(info)?;

    let mut host = unsafe { Sdhci::new(mmio_base) };
    if CLK_DEV.is_completed() {
        info!("rockchip-sdhci: using external CRU clock");
        host.set_external_clock(&SDHCI_CLOCK);
    } else {
        warn!("rockchip-sdhci: no core clock found; using SDHCI internal clock divider");
    }
    info!("rockchip-sdhci: reset controller");
    host.reset_all()
        .map_err(|e| init_error(base_reg.address, mmio_size, e))?;
    host.set_power(SDHCI_POWER_330);
    host.enable_interrupts();
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    host.set_dma(dma.clone());

    info!("rockchip-sdhci: initialize card");
    let mut card = SdioSdmmc::new(host);
    let card_info = poll_card_init_mmc(&mut card)
        .map_err(|e| card_init_error(base_reg.address, mmio_size, e))?;
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
        sdhci_rdif::dma_config(
            "rockchip-sdhci",
            card_info.capacity_blocks.unwrap_or(0),
            true,
            dma,
        ),
    );
    let irq = probe.register_block(dev)?;
    info!("rockchip-sdhci block device registered irq={:?}", irq);
    Ok(())
}

fn poll_card_init_mmc(card: &mut RockchipSdhci) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request =
        card.submit_init_with_preference(CardInitPreference::MmcFirst, &mut scratch)?;
    loop {
        match card.poll_init_request(&mut request)? {
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

fn init_core_clock(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    for clk in info.node.clocks() {
        info!(
            "rockchip-sdhci clock: phandle <{}>, name: {:?}, cells: {}",
            clk.phandle, clk.name, clk.cells
        );
        if clk.name == Some("core".to_string()) {
            let device_id = info.phandle_to_device_id(clk.phandle).ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] core clock phandle {} has no device id",
                    info.node.name(),
                    clk.phandle
                ))
            })?;
            let clk_dev = rdrive::get::<rdif_clk::Clk>(device_id).map_err(|_| {
                OnProbeError::other(format!(
                    "[{}] core clock device {:?} is not registered",
                    info.node.name(),
                    device_id
                ))
            })?;
            CLK_DEV.call_once(|| ClkDev {
                inner: clk_dev,
                id: (clk.select().unwrap_or(0) as usize).into(),
            });
            return Ok(());
        }
    }
    Ok(())
}

fn set_sdhci_clock(target_hz: u32) -> Result<(), Error> {
    let clk = CLK_DEV.wait();
    let mut clk_dev = clk.inner.lock().map_err(|_| clock_error())?;
    clk_dev
        .set_rate(clk.id, target_hz as u64)
        .map_err(|_| clock_error())?;
    let rate = clk_dev.get_rate(clk.id).map_err(|_| clock_error())?;
    info!("rockchip-sdhci: core clock set to {} Hz", rate);
    Ok(())
}

fn clock_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Init))
}

static CLK_DEV: Once<ClkDev> = Once::new();

struct ClkDev {
    inner: Device<rdif_clk::Clk>,
    id: ClockId,
}
