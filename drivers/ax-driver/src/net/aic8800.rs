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
//! The data plane (`rd_net::Net`) and the control plane
//! (`Box<dyn WifiDriver>`) plus the board's link policy ([`ApConfig`]) are
//! stashed in a [`PlatformWifiDevice`]; the runtime takes them out after the
//! network service comes up (see `axruntime::devices`).

use core::ptr::NonNull;

use log::{error, info};
use rdrive::{PlatformDevice, probe::OnProbeError, register::FdtInfo};
use sdhci_cv1800::{
    CviSdhci,
    hw_init::{Sdio1HwConfig, sdio1_hw_init},
};
use sdio_host::SdioHost;

use crate::net::{ApConfig, PlatformWifiDevice};

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

/// Decodes the SDIO1 controller IRQ from the FDT node.
///
/// The SG2002 interrupt-parent is the RISC-V PLIC, whose `#interrupt-cells = 2`
/// and whose specifier is `<irq-number, trigger-flags>` — the IRQ number is the
/// **first** cell. (This differs from the ARM GIC's `<type, number, flags>`
/// three-cell form used by the `block`/`usb` helpers, which must not be applied
/// here: doing so would read the trigger flags as the IRQ number.)
fn decode_fdt_irq(interrupts: &[rdrive::probe::fdt::InterruptRef]) -> Option<usize> {
    let interrupt = interrupts.first()?;
    match &*interrupt.specifier {
        // PLIC: [irq] or [irq, flags] — first cell is the IRQ number.
        [irq] | [irq, _] => Some(*irq as usize),
        _ => None,
    }
}

/// Raw IRQ trampoline: the SDIO1 controller interrupt is forwarded to the SDHCI
/// handler (CARD_INT detection). This is a *controller-level* IRQ, distinct from
/// a NIC's rx/tx-queue IRQ, so the net device itself registers with `irq=None`
/// and RX is driven out-of-band via the chip's RX-data callback.
unsafe fn sdio1_raw_irq_handler(
    _ctx: axklib::irq::IrqContext,
    _data: NonNull<()>,
) -> axklib::irq::IrqReturn {
    sdhci_cv1800::irq::sdhci_irq_handler(0);
    axklib::irq::IrqReturn::Handled
}

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    // NOTE: the ArceOS runtime glue (timing / delay / yield / task spawn) for
    // the aic8800 and sdhci-cv1800 cores is installed by `axruntime` *before*
    // device probing — ax-driver sits below `ax-hal` in the crate graph and
    // cannot pull the `arceos` glue itself without forming a dependency cycle.

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
    let irq = decode_fdt_irq(&info.interrupts())
        .ok_or_else(|| OnProbeError::other(alloc::format!("[{}] has no irq", info.node.name())))?;
    info!("[wifi] SDIO1 IRQ resolved to {irq}");
    if let Err(e) = axklib::irq::request_shared(irq, sdio1_raw_irq_handler, NonNull::dangling()) {
        return Err(OnProbeError::other(alloc::format!(
            "[wifi] failed to register SDIO1 IRQ {irq}: {e:?}"
        )));
    }

    // SDHCI init.
    let mut sdio = CviSdhci::new(cfg.sdio1_base_va);
    sdio.init()
        .map_err(|e| OnProbeError::other(alloc::format!("[wifi] SDIO1 init failed: {e:?}")))?;
    let (vid, did) = sdio.vendor_device_id();
    info!("[wifi] SDIO device: vendor={vid:#06x} device={did:#06x}");
    sdio.prepare_first_data_xfer();

    // Hand the initialized SDIO host to the chip driver. From here the rest of
    // the system only talks to the generic `WifiDriver` trait.
    let mut wifi = aic8800::probe(sdio)
        .map_err(|e| OnProbeError::other(alloc::format!("[wifi] chip probe failed: {e:?}")))?;
    info!("[wifi] chip probe complete");

    // Start an open SoftAP. The link policy (SSID/channel and the IP/DHCP
    // config below) is board policy expressed here, not in the protocol stack.
    if let Err(e) = wifi.start_ap_open(AP_SSID, AP_CHANNEL) {
        error!("[wifi] AP start failed: {e:?}");
        return Err(OnProbeError::other(alloc::format!(
            "[wifi] AP start failed: {e:?}"
        )));
    }
    info!("[wifi] SoftAP started, channel {AP_CHANNEL}");

    // Take the data-plane device and register it together with the control
    // handle and the AP policy. The runtime wires it into the stack later.
    let net = wifi
        .take_net(axklib::dma::op())
        .ok_or_else(|| OnProbeError::other("[wifi] no net device"))?;

    plat_dev.register(PlatformWifiDevice::new(
        "wlan0",
        net,
        wifi,
        ApConfig {
            server_ip: AP_SERVER_IP,
            client_ip: AP_CLIENT_IP,
            prefix_len: AP_PREFIX_LEN,
        },
    ));
    info!("[wifi] wlan0 device registered (probe stage done)");
    Ok(())
}
