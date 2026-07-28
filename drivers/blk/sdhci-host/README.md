# sdhci-host

`no_std` SD Host Controller Interface (SDHCI v3.x) backend for
[`sdmmc-protocol`](../sdmmc-protocol).

This crate plugs SDHCI register programming into the physical
`sdio_host2::SdioHost` trait so `SdioSdmmc::new_host2` can drive a real
controller through `sdmmc-protocol`. Platform code is still responsible for MMIO mapping, clock/reset
tree setup, power rails, pinmux, IRQ routing, and DMA cache coherency.

## Status

- Compiles as a `no_std` controller backend.
- Intended for use through `sdmmc_protocol::sdio::SdioSdmmc::new_host2`.
- Board-specific clock, power, pinmux, and DMA policy must be supplied by the
  caller.
- Real hardware bring-up still depends on the surrounding SoC integration.

## Scope

| Area                | Implemented |
|---------------------|-------------|
| PIO read / write    | ✅          |
| ADMA2 (32-bit) read / write | ✅  |
| 1-bit / 4-bit bus   | ✅          |
| Default speed       | ✅          |
| High Speed (50 MHz) | ✅          |
| 32-bit / 136-bit responses | ✅   |
| Software reset / clock setup | ✅ |
| External platform-clock callback | ✅ |
| 1.8 V signaling bit path | ✅ (board validation required) |
| Controller tuning entry points | ✅ (board validation required) |
| ADMA2 (64-bit / v4) | ❌          |
| 8-bit eMMC bus      | ✅          |
| eMMC EXT_CSD path   | ✅ (ADMA2)  |

## Usage

```rust,no_run
use core::ptr::NonNull;
use sdmmc_protocol::sdio::{SdioInitScratch, SdioSdmmc};
use sdhci_host::Sdhci;

// SAFETY: 0xFE31_0000 must point at a valid SDHCI register file the
// caller has exclusive access to.
let mmio = NonNull::new(0xFE31_0000 as *mut u8).unwrap();
let mut host = unsafe { Sdhci::new(mmio) };
// Optional platform capabilities such as HostClock, HostResetHook, DMA, and
// 1.8 V support are installed here before the protocol layer owns the host.

let mut card = SdioSdmmc::new_host2(host);
let mut scratch = SdioInitScratch::new();
let request = card.submit_init(&mut scratch)?;
// Transfer `card` and `request` to the maintenance task. Advance only after
// `request.wait_kind()` reports a satisfied register deadline or an
// acknowledged device IRQ; never drive command/data completion in a tight loop.
# Ok::<(), sdmmc_protocol::Error>(())
```

Construction is `unsafe` because the caller must guarantee that the supplied
address is a valid, exclusively-owned SDHCI register file for the lifetime of
the driver.

## Block Request Usage

Normal block-device integration should use `sdhci_host::rdif::device`, which
routes RDIF requests through `sdmmc-protocol` and the native `sdio-host2`
transaction path. The public boundary accepts owned DMA requests through
`rdif_block::HardwareQueue`; raw submit/poll block primitives are intentionally
not exposed.

```rust,ignore
use dma_api::DeviceDma;
use sdmmc_protocol::sdio::{
    SdioSdmmc,
    init::CardInitPreference,
    host2::SdioHost2Adapter,
};
use sdhci_host::{Sdhci, rdif};

# use platform::DmaImpl;
let dma = DeviceDma::new_legacy(u32::MAX as u64, &DmaImpl);
let mut host = unsafe { Sdhci::new_from_addr(0xFE31_0000) };
host.set_dma(dma.clone());
let card = SdioSdmmc::new_host2(host);
let config = rdif::dma_config("dwcmshc", 0, dma);
let controller =
    rdif::initializing_device(card, config, CardInitPreference::MmcFirst);
// Platform glue transfers `controller` and its pre-resolved IRQ source to the
// shared block runtime. The runtime owns the hctx task and completion channels.
```

Platform code should implement `dma_api::DmaOp` and keep OS-specific mapping
and cache maintenance there. `rdif::dma_config` publishes the 32-bit mask,
alignment, maximum transfer, descriptor, and DWCMSHC boundary constraints to
the shared transfer planner.

### Bring-up checklist

1. Map the SDHCI register file (RK3568: `0xFE31_0000`).
2. Configure the platform clock so the controller has a viable reference
   clock before calling `Sdhci::new` (RK3568 needs the CRU bringing
   `CLK_EMMC_CORE` up at ≥ 25 MHz).
3. Install optional capabilities such as `Sdhci::set_external_clock`,
   `Sdhci::set_reset_hook`, `Sdhci::set_dma`, and
   `Sdhci::enable_1v8_signaling` before handing the host to the protocol
   layer.
4. Build `SdioSdmmc::new_host2(host)`, submit initialization with
   `submit_init`, and transfer it to the IRQ-driven maintenance task. The protocol
   layer starts with native `sdio-host2` bus operations for `ResetAll`,
   `PowerOn`, initial voltage, 1-bit bus width, and 400 kHz identification
   clock before issuing SD/MMC commands, then ramps the card to 25 MHz /
   50 MHz via later bus ops. Register-only states use a unified bounded
   deadline; command/data states advance only after IRQ acknowledgement.

The register-only helpers such as `Sdhci::reset_all`,
`Sdhci::set_power`, and `Sdhci::enable_clock` remain useful for diagnostics,
but normal card initialization should let `SdioSdmmc::new_host2` drive those
steps through event-driven bus operations.

If the SoC requires external clock-tree programming for each SD speed, implement
`sdhci_host::HostClock` in platform glue and register it with
`Sdhci::set_external_clock`; the driver will gate the SD clock, call that clock
capability with the target frequency, and re-enable external-clock mode.

## Testing

From this crate directory:

```bash
cargo fmt --check
cargo test
cargo clippy --all-features -- -D warnings
```

In this workspace, prefer the project `xtask` flow for final validation:

```bash
cargo fmt
cargo xtask clippy --package sdhci-host
```

## License

Licensed under the Apache License, Version 2.0.
