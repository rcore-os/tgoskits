# sdhci-host

`no_std` SD Host Controller Interface (SDHCI v3.x) backend for
[`sdmmc-protocol`](../sdmmc-protocol).

This crate plugs SDHCI register programming into the
`sdmmc_protocol::sdio::SdioHost` trait so `SdioSdmmc` can drive a real
controller. Platform code is still responsible for MMIO mapping, clock/reset
tree setup, power rails, pinmux, IRQ routing, and DMA cache coherency.

## Status

- Compiles as a `no_std` controller backend.
- Intended for use through `sdmmc_protocol::sdio::SdioSdmmc`.
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
| 8-bit eMMC bus      | ❌ (returns `UnsupportedCommand`) |
| eMMC EXT_CSD path   | ❌          |

## Usage

```rust,no_run
use core::ptr::NonNull;
use sdmmc_protocol::sdio::{DelayNs, SdioSdmmc};
use sdhci_host::Sdhci;

# fn make_delay() -> impl DelayNs { struct N; impl DelayNs for N { fn delay_ns(&mut self, _: u32) {} } N }
// SAFETY: 0xFE31_0000 must point at a valid SDHCI register file the
// caller has exclusive access to.
let mmio = NonNull::new(0xFE31_0000 as *mut u8).unwrap();
let mut host = unsafe { Sdhci::new(mmio) };
host.reset_all()?;
host.set_power(0x0f);
host.enable_interrupts();
host.enable_clock(150_000_000, 400_000)?;

let delay = make_delay();
let mut card = SdioSdmmc::new(host, delay);
// card.init()?;
# Ok::<(), sdmmc_protocol::Error>(())
```

Construction is `unsafe` because the caller must guarantee that the supplied
address is a valid, exclusively-owned SDHCI register file for the lifetime of
the driver.

## ADMA2 Usage

For ADMA2 request I/O, pass a `dma_api::DeviceDma` capability to
`Sdhci::dma_read_blocks_into`. The host crate owns request-buffer mapping,
ADMA2 descriptor allocation, descriptor cache sync, and completion sync.

```rust,ignore
use core::{num::NonZeroUsize, ptr::NonNull};
use dma_api::DeviceDma;
use sdhci_host::Sdhci;

# use platform::DmaImpl;
let dma = DeviceDma::new(u32::MAX as u64, &DmaImpl);
let mut host = unsafe { Sdhci::new_from_addr(0xFE31_0000) };
let mut block = [0u8; 512];
let ptr = NonNull::new(block.as_mut_ptr()).unwrap();
host.dma_read_blocks_into(0, ptr, NonZeroUsize::new(block.len()).unwrap(), &dma)?;
```

Platform code should implement `dma_api::DmaOp` and keep OS-specific mapping
and cache maintenance there.

### Bring-up checklist

1. Map the SDHCI register file (RK3568: `0xFE31_0000`).
2. Configure the platform clock so the controller has a viable reference
   clock before calling `Sdhci::new` (RK3568 needs the CRU bringing
   `CLK_EMMC_CORE` up at ≥ 25 MHz).
3. `host.reset_all()?` — clears CMD/DAT inhibits and the interrupt
   registers.
4. `host.set_power(POWER_330)` (or whatever your card needs).
5. `host.enable_interrupts()` — enables status flags. The driver polls;
   it does NOT enable signal-level IRQ delivery.
6. `host.enable_clock(base_hz, 400_000)` — start at 400 kHz for
   identification.
7. Build `SdioSdmmc::new(host, delay)` and call `init()`. The driver
   will ramp the clock up to 25 MHz / 50 MHz via `set_clock` for you.

If the SoC requires external clock-tree programming for each SD speed, register
it with `Sdhci::set_external_clock`; the driver will gate the SD clock, call
the platform callback with the target frequency, and re-enable external-clock
mode.

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
