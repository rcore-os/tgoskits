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

//! Side-effect-free AIC8800/CV1800 discovery.
//!
//! Probe owns only resource mapping, validation and capability composition.
//! The generic network activation path moves this object to its fixed-CPU
//! maintenance owner, installs the disabled IRQ action, and only then starts
//! the controller/card/firmware state machine.

use core::convert::TryFrom;

use aic8800::{AicDiscoveryConfig, AicWifiNetDev, SoftApPolicy};
use log::info;
use mmio_api::Mmio;
use rd_net::WifiLinkPolicy;
use rdrive::{probe::OnProbeError, register::ProbeFdt};
use sdhci_cv1800::{
    CviSdhci,
    hw_init::{Sdio1MappedResources, Sdio1Policy},
};

use crate::{binding_info_from_fdt, net::PlatformDeviceNet};

const SYSCON_PADDR: u64 = 0x0300_0000;
const CRG_PADDR: u64 = 0x0300_2000;
const RTCSYS_CTRL_PADDR: u64 = 0x0502_5000;
const RTCSYS_IO_PADDR: u64 = 0x0502_7000;
const MMIO_PAGE: usize = 0x1000;
const SYSCON_SIZE: usize = 0x2000;
const SDHCI_REGISTER_BYTES: u64 = 0x100;

const AP_SERVER_IP: [u8; 4] = [192, 168, 50, 1];
const AP_CLIENT_IP: [u8; 4] = [192, 168, 50, 2];
const AP_PREFIX_LEN: u8 = 24;
const AP_SSID: &[u8] = b"PicoClaw-Car";
const AP_CHANNEL: u8 = 6;

crate::model_register!(
    name: "AIC8800 WiFi (CV181x SDIO)",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["cvitek,cv181x-sdio"],
        on_probe: probe
    }],
);

fn map_mmio(physical_address: u64, size: usize, name: &str) -> Result<Mmio, OnProbeError> {
    axklib::mmio::ioremap(physical_address.into(), size).map_err(|error| {
        OnProbeError::other(alloc::format!(
            "failed to map AIC8800 {name} register window: {error}"
        ))
    })
}

fn map_cv_resources(
    controller_address: u64,
    controller_size: usize,
) -> Result<Sdio1MappedResources, OnProbeError> {
    let resources = Sdio1MappedResources::new(
        map_mmio(CRG_PADDR, MMIO_PAGE, "clock/reset")?,
        map_mmio(SYSCON_PADDR, SYSCON_SIZE, "system control")?,
        map_mmio(RTCSYS_CTRL_PADDR, MMIO_PAGE, "RTC control")?,
        map_mmio(RTCSYS_IO_PADDR, MMIO_PAGE, "RTC IO")?,
        map_mmio(controller_address, controller_size, "SDIO1")?,
    )
    .map_err(|error| {
        OnProbeError::other(alloc::format!(
            "invalid AIC8800/CV1800 mapped resources: {error}"
        ))
    })?;
    Ok(resources)
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let binding = binding_info_from_fdt(probe.info())?;
    let register = probe
        .info()
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("AIC8800 SDIO FDT node has no register aperture"))?;
    let register_bytes = register.size.ok_or_else(|| {
        OnProbeError::other("AIC8800 SDIO FDT register aperture has no declared size")
    })?;
    if register_bytes < SDHCI_REGISTER_BYTES {
        return Err(OnProbeError::other(alloc::format!(
            "AIC8800 SDIO register aperture {register_bytes:#x} is smaller than \
             {SDHCI_REGISTER_BYTES:#x}"
        )));
    }
    let register_bytes = usize::try_from(register_bytes)
        .map_err(|_| OnProbeError::other("AIC8800 SDIO register aperture does not fit usize"))?;

    let resources = map_cv_resources(register.address, register_bytes)?;
    let host = CviSdhci::discover(resources, Sdio1Policy::default()).map_err(|error| {
        OnProbeError::other(alloc::format!(
            "failed to assemble AIC8800 SDIO discovery object: {error}"
        ))
    })?;
    let soft_ap = SoftApPolicy::try_new(AP_SSID, AP_CHANNEL).map_err(|error| {
        OnProbeError::other(alloc::format!(
            "invalid board-default AIC8800 SoftAP policy: {error}"
        ))
    })?;
    let config = AicDiscoveryConfig::new(
        [0; 6],
        Some(WifiLinkPolicy {
            ip: AP_SERVER_IP,
            prefix_len: AP_PREFIX_LEN,
            dhcp_server_client_ip: Some(AP_CLIENT_IP),
        }),
    )
    .with_soft_ap(soft_ap);
    let wifi =
        AicWifiNetDev::discover(host, axklib::dma::device_with_mask(u32::MAX as u64), config)
            .map_err(|error| {
                OnProbeError::other(alloc::format!(
                    "failed to split AIC8800 SDIO IRQ capabilities: {error}"
                ))
            })?;

    probe
        .into_platform_device()
        .register_owned_net_with_info("wlan0", wifi, binding);
    info!("AIC8800 wlan0 discovery object registered for owner-side activation");
    Ok(())
}
