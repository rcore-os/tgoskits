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
use core::time::Duration;

use log::{info, warn};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdhci_host::Sdhci;
use sdmmc_protocol::{
    Error, OperationPoll,
    error::Phase,
    sdio::{CardInfo, CardInitPreference, SdioInitScratch, SdioSdmmc},
};

use crate::{
    block::{
        ProbeFdtBlock, SharedDriver,
        sdmmc::{SdmmcBlockConfig, SdmmcBlockDevice},
    },
    mmio::iomap,
};

const SDHCI_POWER_330: u8 = 0x0e;

type K230Sdhci = SdioSdmmc<Sdhci>;

crate::model_register!(
    name: "K230 SDHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["canaan,k230-sdhci", "snps,dwcmshc-sdhci"],
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
        "k230-sdhci probe: node={}, addr={:#x}, size={:#x}",
        info.node.name(),
        base_reg.address as usize,
        mmio_size
    );
    let mmio_base = iomap(base_reg.address as usize, mmio_size as usize)?;

    let mut host = unsafe { Sdhci::new(mmio_base) };
    info!("k230-sdhci: reset controller");
    host.reset_all()
        .map_err(|e| init_error(base_reg.address, mmio_size, e))?;
    host.set_power(SDHCI_POWER_330);
    host.enable_interrupts();
    host.set_dma(axklib::dma::device_with_mask(u32::MAX as u64));

    info!("k230-sdhci: initialize card");
    let mut card = SdioSdmmc::new(host);
    let card_info = poll_card_init(&mut card, card_init_preference(info))
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

    let raw = SharedDriver::new(card);
    let dev = SdmmcBlockDevice::new(
        raw,
        SdmmcBlockConfig::dma("k230-sdhci", card_info.capacity_blocks.unwrap_or(0), false),
    );
    let irq = probe.register_block(dev)?;
    info!("k230-sdhci block device registered irq={:?}", irq);
    Ok(())
}

fn poll_card_init(card: &mut K230Sdhci, preference: CardInitPreference) -> Result<CardInfo, Error> {
    let mut scratch = SdioInitScratch::new();
    let mut request = card.submit_init_with_preference(preference, &mut scratch)?;
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

fn card_init_preference(info: &FdtInfo<'_>) -> CardInitPreference {
    let node = info.node.as_node();
    if node.get_property("no-sd").is_some() || node.get_property("non-removable").is_some() {
        CardInitPreference::MmcFirst
    } else {
        CardInitPreference::SdFirst
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
            "k230-sdhci: no responsive card at [PA:{:?}, SZ:0x{:x}); skipping controller: {err:?}",
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
