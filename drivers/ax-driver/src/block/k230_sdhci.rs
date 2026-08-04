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

use log::info;
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
use sdhci_host::{Sdhci, rdif as sdhci_rdif};
use sdmmc_protocol::sdio::{card::SdioSdmmc, init::CardInitPreference};

use crate::{block::ProbeFdtBlock, mmio::iomap};

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
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    let config = sdhci_rdif::dma_config("k230-sdhci", 0, &dma);
    host.configure_dma(dma).map_err(|err| {
        OnProbeError::other(format!("k230-sdhci ADMA2 configuration failed: {err:?}"))
    })?;

    info!("k230-sdhci: defer protocol initialization to IRQ-driven hctx");
    let card = SdioSdmmc::new(host);
    let dev = sdhci_rdif::initializing_device(card, config, card_init_preference(info));
    let irq = probe.register_block(dev)?;
    info!("k230-sdhci block device registered irq={:?}", irq);
    Ok(())
}

fn card_init_preference(info: &FdtInfo<'_>) -> CardInitPreference {
    let node = info.node.as_node();
    if node.get_property("no-sd").is_some() || node.get_property("non-removable").is_some() {
        CardInitPreference::MmcFirst
    } else {
        CardInitPreference::SdFirst
    }
}
