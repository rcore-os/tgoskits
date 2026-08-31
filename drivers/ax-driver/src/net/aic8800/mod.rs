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

//! AIC8800 Wi-Fi (CV181x/SG2002 SDIO) platform probe.
//!
//! Brings the chip up from an FDT `cvitek,cv181x-sdio` node. Controller and SoC
//! integration MMIO, clock/reset resources, bus policy, DMA width, deadlines,
//! and optional startup network policy are firmware inputs; portable crates do
//! not parse firmware or retain board addresses.
//!
//! The probe constructs a portable [`aic8800::AicRdifDevice`] with one
//! poll group and a move-only nested SDHCI IRQ endpoint. SDIO enumeration,
//! firmware startup and data-plane work begin only after the fixed owner CPU
//! and physical IRQ registration are ready.

use aic8800::AicRdifDevice;
use cv181x_sdhci::{Cv181xMmio, Cv181xSdhci, Cv181xSdio1Mmio};
use log::info;
use rdrive::{
    probe::{OnProbeError, fdt::ResourcePrepareConfig},
    register::ProbeFdt,
};

use crate::{binding_info_from_fdt, net::PlatformDeviceNet};

mod fdt;
mod startup_config;

use fdt::AicFdtProfile;

crate::model_register!(
    name: "AIC8800 WiFi (CV181x SDIO)",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["cvitek,cv181x-sdio"],
        on_probe: probe
    }],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let resources = info.prepare_resources(
        ResourcePrepareConfig::default()
            .with_assigned_clocks()
            .with_power_domains()
            .with_named_clock_rate("sdio"),
    )?;
    let profile = AicFdtProfile::from_info(&info, resources.clock_rate("sdio"))?;
    let controller_address = profile.controller.address;
    let host_mmio = Cv181xMmio::new(profile.controller.map()?, profile.syscon.map()?);
    let sdio1_mmio = Cv181xSdio1Mmio::new(
        host_mmio,
        profile.crg.map()?,
        profile.rtcsys_ctrl.map()?,
        profile.rtcsys_io.map()?,
    );

    info!(
        "[wifi] SDIO1 resources prepared (node={}, controller={:#x}, src={}Hz, bus={:?})",
        info.node.name(),
        controller_address,
        profile.host_config.src_frequency_hz,
        profile.host_config.max_bus_width,
    );
    let binding = binding_info_from_fdt(&info)?;

    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        crate::binding_resolver::dma_coherency_from_fdt(&info),
        dma_api::DmaConstraints::new(profile.dma_address_mask),
    ));
    // SAFETY: every mapping above is exclusive to this controller instance and
    // covers the register windows required by `Cv181xSdio1Mmio`.
    let mut sdio = unsafe { Cv181xSdhci::new_sdio1(sdio1_mmio, profile.host_config) };
    sdio.configure_dma(dma.clone()).map_err(|error| {
        OnProbeError::other(alloc::format!("[wifi] configure SDIO DMA failed: {error}"))
    })?;

    let wifi = AicRdifDevice::new(sdio, profile.options).map_err(|error| {
        OnProbeError::other(alloc::format!(
            "[wifi] construct AIC adapter failed: {error}"
        ))
    })?;

    plat_dev.register_net_with_info("wlan0", wifi, dma, binding);
    info!("[wifi] wlan0 device registered (probe stage done)");
    Ok(())
}
