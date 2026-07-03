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
//! mapping, SDIO1 SoC init, controller IRQ, SDHCI init, chip probe + SoftAP
//! start) now happens here, behind the standard rdrive probe path.
//!
//! The chip probe returns a single [`aic8800::AicWifiNetDev`] that is *both* the
//! data plane (it implements `rd_net::Interface`) and the control plane (it
//! implements `rd_net::WifiControl`: STA/SoftAP control, RX wake, link policy).
//! It is registered through the ordinary [`register_net`] path used by every
//! NIC — no Wi-Fi-specific device type. The board's SoftAP link policy is
//! attached to the device via `WifiLinkPolicy`, which the runtime reads back
//! generically through `wifi_control()` once the network service is up.

use log::{error, info};
use rd_net::WifiLinkPolicy;
use rdrive::{probe::OnProbeError, register::ProbeFdt};
use sdhci_cv1800::{
    CviSdhci,
    hw_init::{Sdio1HwConfig, sdio1_hw_init},
};
use sdio_host::SdioHost;

use crate::net::PlatformDeviceNet;

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

fn resolve_fdt_irq(
    info: &rdrive::register::FdtInfo<'_>,
) -> Result<axklib::irq::IrqId, OnProbeError> {
    let interrupt =
        info.interrupts().into_iter().next().ok_or_else(|| {
            OnProbeError::other(alloc::format!("[{}] has no irq", info.node.name()))
        })?;
    let parent = info
        .phandle_to_device_id(interrupt.interrupt_parent)
        .ok_or_else(|| {
            OnProbeError::other(alloc::format!(
                "[{}] interrupt-parent {} is not registered",
                info.node.name(),
                interrupt.interrupt_parent
            ))
        })?;
    let mut intc = rdrive::get::<rdif_intc::Intc>(parent)
        .map_err(|err| OnProbeError::other(alloc::format!("failed to get IRQ parent: {err:?}")))?
        .lock()
        .map_err(|err| OnProbeError::other(alloc::format!("failed to lock IRQ parent: {err:?}")))?;
    let translation = intc
        .translate_fdt(&interrupt.specifier)
        .map_err(|err| OnProbeError::other(alloc::format!("failed to translate IRQ: {err:?}")))?;
    intc.configure(&translation)
        .map_err(|err| OnProbeError::other(alloc::format!("failed to configure IRQ: {err:?}")))?;
    Ok(translation.id)
}

/// IRQ trampoline: the SDIO1 controller interrupt is forwarded to the SDHCI
/// handler (CARD_INT detection). This is a *controller-level* IRQ, distinct from
/// a NIC's rx/tx-queue IRQ, so the net device itself registers with `irq=None`
/// and RX is driven out-of-band via the chip's RX-data callback.
fn sdio1_irq_handler(_ctx: axklib::irq::IrqContext) -> axklib::irq::IrqReturn {
    sdhci_cv1800::irq::sdhci_irq_handler(0);
    axklib::irq::IrqReturn::Handled
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

    // Register the SDIO1 controller IRQ (shared). Resolved from the FDT node.
    let irq = resolve_fdt_irq(&info)?;
    info!("[wifi] SDIO1 IRQ resolved to {irq:?}");
    let irq_handle = axklib::irq::request_shared_disabled(irq, sdio1_irq_handler).map_err(|e| {
        OnProbeError::other(alloc::format!(
            "[wifi] failed to register SDIO1 IRQ {irq:?}: {e:?}"
        ))
    })?;

    // SDHCI init.
    let mut sdio = CviSdhci::new(cfg.sdio1_base_va);
    if let Err(e) = sdio.init() {
        let _ = axklib::irq::free(irq_handle);
        return Err(OnProbeError::other(alloc::format!(
            "[wifi] SDIO1 init failed: {e:?}"
        )));
    }
    let (vid, did) = sdio.vendor_device_id();
    info!("[wifi] SDIO device: vendor={vid:#06x} device={did:#06x}");
    sdio.prepare_first_data_xfer();

    // Hand the initialized SDIO host to the chip driver. It returns a single
    // device that is both data plane (`Interface`) and control plane
    // (`WifiControl`).
    let mut wifi = match aic8800::probe(sdio) {
        Ok(wifi) => wifi,
        Err(e) => {
            let _ = axklib::irq::free(irq_handle);
            return Err(OnProbeError::other(alloc::format!(
                "[wifi] chip probe failed: {e}"
            )));
        }
    };
    info!("[wifi] chip probe complete");

    // Start an open SoftAP. SSID/channel are board policy expressed here, not in
    // the protocol stack. `start_ap_open` comes from the device's `WifiControl`
    // control plane.
    if let Err(e) = rd_net::WifiControl::start_ap_open(&mut wifi, AP_SSID, AP_CHANNEL) {
        error!("[wifi] AP start failed: {e:?}");
        let _ = axklib::irq::free(irq_handle);
        return Err(OnProbeError::other(alloc::format!(
            "[wifi] AP start failed: {e:?}"
        )));
    }
    info!("[wifi] SoftAP started, channel {AP_CHANNEL}");

    if let Err(e) = axklib::irq::enable(irq_handle) {
        let _ = axklib::irq::free(irq_handle);
        return Err(OnProbeError::other(alloc::format!(
            "[wifi] failed to enable SDIO1 IRQ {irq:?}: {e:?}"
        )));
    }
    info!("[wifi] SDIO1 IRQ {irq:?} registered and enabled");

    // Attach the board's SoftAP link policy to the device, then register it
    // through the ordinary net device path. The runtime reads the policy back
    // generically via `wifi_control()` — the stack stays Wi-Fi-agnostic.
    let wifi = wifi.with_link_policy(WifiLinkPolicy {
        ip: AP_SERVER_IP,
        prefix_len: AP_PREFIX_LEN,
        dhcp_server_client_ip: Some(AP_CLIENT_IP),
    });

    // Register through the ordinary net path with an empty binding (no
    // rx/tx-queue IRQ): the SDIO1 controller IRQ is registered manually above
    // and Wi-Fi RX is delivered out-of-band, so the net device itself carries
    // no IRQ.
    plat_dev.register_net("wlan0", wifi);
    info!("[wifi] wlan0 device registered (probe stage done)");
    Ok(())
}
