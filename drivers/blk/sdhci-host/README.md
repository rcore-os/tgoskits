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
| 8-bit eMMC bus      | ❌ (returns `UnsupportedCommand`) |
| eMMC EXT_CSD path   | ❌          |

## Usage

```rust,no_run
use core::ptr::NonNull;
use sdmmc_protocol::{OperationPoll, sdio::{SdioInitScratch, SdioSdmmc}};
use sdhci_host::Sdhci;

// SAFETY: 0xFE31_0000 must point at a valid SDHCI register file the
// caller has exclusive access to.
let mmio = NonNull::new(0xFE31_0000 as *mut u8).unwrap();
let mut host = unsafe { Sdhci::new(mmio) };
// Optional platform capabilities such as HostClock, HostResetHook, DMA, and
// 1.8 V support are installed here before the protocol layer owns the host.

let mut card = SdioSdmmc::new_host2(host);
let mut scratch = SdioInitScratch::new();
let mut request = card.submit_init(&mut scratch)?;
while let OperationPoll::Pending = card.poll_init_request(&mut request)? {
    // Runtime policy belongs here: spin, yield, wait for IRQ, or sleep/timer
    // when request.take_needs_pace() is set.
}
# Ok::<(), sdmmc_protocol::Error>(())
```

Construction is `unsafe` because the caller must guarantee that the supplied
address is a valid, exclusively-owned SDHCI register file for the lifetime of
the driver.

## Block Request Usage

Normal block-device integration should use `sdhci_host::rdif::device`, which
routes RDIF requests through `sdmmc-protocol` and the native `sdio-host2`
transaction path. The lower-level primitives remain available for controller
bring-up: use `Sdhci::submit_read_blocks` / `Sdhci::submit_write_blocks`, then drive
completion with `Sdhci::poll_block_request`. `BlockTransferMode::Dma` uses
ADMA2 and owns request-buffer mapping, descriptor allocation, descriptor
cache sync, and completion sync. `BlockTransferMode::Fifo` uses the FIFO
path with the same submit/poll shape, so platform code can fall back when DMA
is unavailable.

```rust,ignore
use core::{num::NonZeroUsize, ptr::NonNull};
use dma_api::DeviceDma;
use sdhci_host::{BlockRequestSlot, BlockTransferMode, RequestId, Sdhci};

# use platform::DmaImpl;
let dma = DeviceDma::new_legacy(u32::MAX as u64, &DmaImpl);
let mut host = unsafe { Sdhci::new_from_addr(0xFE31_0000) };
let mut block = [0u8; 512];
let ptr = NonNull::new(block.as_mut_ptr()).unwrap();
let mut slot = BlockRequestSlot::default();
let mut request = Some(host.submit_read_blocks(
    0,
    ptr,
    NonZeroUsize::new(block.len()).unwrap(),
    Some(&dma),
    BlockTransferMode::Dma,
    &mut slot,
)?);
let id = RequestId::new(0);
while matches!(host.poll_block_request(&mut request, id, &mut slot), Ok(BlockPoll::Pending)) {}
```

Platform code should implement `dma_api::DmaOp` and keep OS-specific mapping
and cache maintenance there. `Sdhci::block_buffer_config` exposes the FIFO or
ADMA2 queue constraints so adapters can translate them into their runtime's
block-buffer contract.

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
   `submit_init`, and drive it with `poll_init_request`. The protocol
   layer starts with native `sdio-host2` bus operations for `ResetAll`,
   `PowerOn`, initial voltage, 1-bit bus width, and 400 kHz identification
   clock before issuing SD/MMC commands, then ramps the card to 25 MHz /
   50 MHz via later bus ops. Platform/runtime code chooses whether pending
   work spins, yields, or waits for an IRQ.

The lower-level blocking helpers such as `Sdhci::reset_all`,
`Sdhci::set_power`, and `Sdhci::enable_clock` remain useful for diagnostics,
but normal card initialization should let `SdioSdmmc::new_host2` drive those
steps through submit/poll bus operations.

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
