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
//! Brings the chip up from the FDT `cvitek,cv181x-sdio` node — the device
//! bring-up that previously lived in `starry-kernel`'s `probe_wifi` (MMIO
//! mapping, SDIO1 SoC init, SDHCI init and chip probe) happens here. The network
//! runtime exclusively owns controller IRQ registration and SoftAP startup.
//!
//! The chip probe returns a consumable [`aic8800::AicWifiNetDev`] with one poll
//! group and a move-only nested SDHCI IRQ endpoint. It is registered through the
//! same fixed-affinity network path as every other NIC.

use log::info;
use rd_net::{WifiLinkPolicy, WifiTransaction};
use rdrive::{probe::OnProbeError, register::ProbeFdt};
use sdhci_cv1800::{
    CviSdhci,
    hw_init::{Sdio1HwConfig, sdio1_hw_init},
};
use sdio_host::SdioHost;

use crate::{binding_info_from_fdt, net::PlatformDeviceNet};

// SG2002 SoC-level register bases (physical). These are *SoC* subsystem
// registers (clock/reset/pinmux), not part of the SDIO1 controller's own `reg`
// in the FDT node, so they stay as fixed silicon constants. Only the SDIO1
// controller base + IRQ come from the FDT node.
const SYSCON_PADDR: usize = 0x0300_0000;
const CRG_PADDR: usize = 0x0300_2000;
const RTCSYS_CTRL_PADDR: usize = 0x0502_5000;
const RTCSYS_IO_PADDR: usize = 0x0502_7000;
const MMIO_PAGE: usize = 0x1000;
// SYSCON spans two pages: `sdio1_hw_init` reaches the FMUX window at
// `SYSCON + 0x1000 + 0xE4` (pin-mux FSEL) on top of the SD_CTRL_OPT register at
// `0x294`, so a single page is not enough.
const SYSCON_SIZE: usize = 0x2000;

// SoftAP link policy for this board. Previously hard-coded inside the protocol
// stack; now produced here and carried as data to the stack-agnostic
// registration path.
const AP_SSID: &[u8] = b"PicoClaw-Car";
const AP_CHANNEL: u8 = 6;
const AP_SERVER_IP: [u8; 4] = [192, 168, 50, 1];
const AP_CLIENT_IP: [u8; 4] = [192, 168, 50, 2];
const AP_PREFIX_LEN: u8 = 24;

crate::model_register!(
    name: "AIC8800 WiFi (CV181x SDIO)",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["cvitek,cv181x-sdio"],
        on_probe: probe
    }],
);

/// Maps `size` bytes of MMIO at `paddr`, returning its kernel virtual address.
fn map_mmio(paddr: usize, size: usize) -> Result<usize, OnProbeError> {
    crate::mmio::iomap(paddr, size).map(|p| p.as_ptr() as usize)
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    // NOTE: the ArceOS runtime glue (timing / delay / yield / task spawn) for
    // the aic8800 and sdhci-cv1800 cores is installed by `axruntime` *before*
    // device probing — ax-driver sits below `ax-hal` in the crate graph and
    // cannot pull the `arceos` glue itself without forming a dependency cycle.
    let (info, plat_dev) = probe.into_parts();

    // The SDIO1 controller base + IRQ come from the FDT node; the SoC subsystem
    // bases are fixed silicon constants (not in this node's `reg`).
    let sdio1_reg =
        info.node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(alloc::format!("[{}] has no reg", info.node.name()))
        })?;
    let sdio1_paddr = sdio1_reg.address as usize;

    let cfg = Sdio1HwConfig::new(
        map_mmio(CRG_PADDR, MMIO_PAGE)?,
        map_mmio(SYSCON_PADDR, SYSCON_SIZE)?,
        map_mmio(RTCSYS_CTRL_PADDR, MMIO_PAGE)?,
        map_mmio(RTCSYS_IO_PADDR, MMIO_PAGE)?,
        map_mmio(sdio1_paddr, MMIO_PAGE)?,
        0,
    );

    info!(
        "[wifi] SDIO1 HW init (node={}, sdio1={:#x})",
        info.node.name(),
        sdio1_paddr
    );
    sdio1_hw_init(&cfg);

    let binding = binding_info_from_fdt(&info)?;

    // SDHCI init.
    let mut sdio = CviSdhci::new(cfg.sdio1_base_va);
    if let Err(e) = sdio.init() {
        return Err(OnProbeError::other(alloc::format!(
            "[wifi] SDIO1 init failed: {e:?}"
        )));
    }
    let (vid, did) = sdio.vendor_device_id();
    info!("[wifi] SDIO device: vendor={vid:#06x} device={did:#06x}");
    sdio.prepare_first_data_xfer();

    // Hand the initialized SDIO host to the chip driver. Probe only identifies
    // the chip and packages the host; firmware and FDRV startup remain deferred
    // until the fixed-CPU network queue worker owns the device.
    let wifi = match aic8800::probe(sdio) {
        Ok(wifi) => wifi,
        Err(e) => {
            return Err(OnProbeError::other(alloc::format!(
                "[wifi] chip probe failed: {e}"
            )));
        }
    };
    info!("[wifi] chip probe complete");

    // The fixed-CPU queue runtime executes this transaction only after worker
    // pinning and disabled IRQ registration succeed.
    let wifi = wifi.with_startup_transaction(WifiTransaction::open_access_point(
        AP_SSID.to_vec(),
        AP_CHANNEL,
        WifiLinkPolicy {
            ip: AP_SERVER_IP,
            prefix_len: AP_PREFIX_LEN,
            dhcp_server_client_ip: Some(AP_CLIENT_IP),
        },
    ));

    let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        crate::binding_resolver::dma_coherency_from_fdt(&info),
        dma_api::DmaConstraints::new(u64::MAX),
    ));
    plat_dev.register_net_with_info("wlan0", wifi, dma, binding);
    info!("[wifi] wlan0 device registered (probe stage done)");
    Ok(())
}
