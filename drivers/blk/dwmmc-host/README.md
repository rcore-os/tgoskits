# dwmmc-host

`no_std` Synopsys DesignWare Mobile Storage Host Controller (`DW_mshc`)
backend for [`sdmmc-protocol`](../sdmmc-protocol).

The portable core owns command/response state, one persistent 4 KiB IDMAC
descriptor ring, interrupt acknowledgement, and bounded register transitions.
SoC wrappers retain only clock, reset, pad, voltage, and tuning policy.

## Data-path contract

- Production block I/O is IDMAC-only. There is no FIFO/PIO fallback.
- `configure_dma` installs the controller-owned `DeviceDma` and allocates the
  descriptor ring once.
- Each descriptor carries at most 4 KiB, matching the Linux DW MMC policy;
  for example 4608 bytes is represented by 4096-byte and 512-byte descriptors.
- Normal submission does not reset DMA and does not allocate descriptors.
- Command/data state advances only from an acknowledged IRQ event. Reset and
  clock-stable register states use bounded `RegisterPending` retries.
- Abort, recovery, and shutdown return the owned DMA object only after the
  controller is quiescent; an unprovable failure is quarantined.
- The hard IRQ only reads/W1C status and merges it into the atomic mailbox.

Traditional DW MMC has one in-flight hardware request, so
`queue_depth = max_submit_batch = 1`. DW CQE/CQHCI is a separate future
migration.

## Usage

```rust,no_run
use core::ptr::NonNull;

use dma_api::DeviceDma;
use dwmmc_host::{DwMmc, IDMAC_MAX_BLOCKS, IDMAC_MAX_TRANSFER_SIZE};
use sdmmc_protocol::{
    rdif::{config::BlockConfig, device::BlockDevice},
    sdio::{SdMmcIrqHost, init::CardInitPreference, native::SdMmcCard},
};

// SAFETY: the mapped register file is valid and exclusively owned.
let mmio = NonNull::new(0xFE2B_0000 as *mut u8).unwrap();
let mut host = unsafe { DwMmc::new(mmio) };
host.set_reference_clock(50_000_000);
let dma: DeviceDma = todo!("construct from the platform DMA domain");
let config = BlockConfig::dma("dwmmc", 0, &dma)
    .with_max_blocks_per_request(IDMAC_MAX_BLOCKS)
    .with_max_segment_size(IDMAC_MAX_TRANSFER_SIZE);
host.configure_dma(dma)?;

let parts = host.into_parts();
let card = SdMmcCard::new(parts.bus);
let controller = BlockDevice::new_initializing(
    card,
    parts.irq,
    config,
    CardInitPreference::SdFirst,
);
// Transfer `controller` and its resolved IRQ source to the block runtime.
# Ok::<(), sdmmc_protocol::Error>(())
```

Construction is `unsafe` because the caller owns the MMIO lifetime and
exclusive access. Platform glue must prepare clocks, resets, regulators,
pinmux, and the correct DMA domain before transferring ownership.

## Validation

```bash
cargo test -p dwmmc-host
cargo xtask clippy --package dwmmc-host
```

Board validation must cover descriptor splitting, maximum transfer, removable
and non-removable media policy, concurrent submitters, fsync, checksum
verification, IRQ quiescence, and submit/completion accounting.

## License

Licensed under the Apache License, Version 2.0.
