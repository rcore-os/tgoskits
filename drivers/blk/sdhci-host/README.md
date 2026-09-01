# sdhci-host

`no_std` SD Host Controller Interface backend for
[`sdmmc-protocol`](../sdmmc-protocol).

The crate owns SDHCI command, response, ADMA2, interrupt-status, and
register-transition state. Platform glue owns MMIO mapping, clock/reset trees,
power rails, pinmux, IRQ routing, and construction of the `dma-api`
capability.

## Data-path contract

- Production block I/O is ADMA2-only. There is no PIO or FIFO fallback.
- `configure_dma` installs the owned DMA capability and preallocates the
  controller-lifetime descriptor table.
- 32-bit, 64-bit, and SDHCI v4 ADMA2 addressing are selected from hardware
  capabilities and the device DMA mask.
- Queue limits expose the DMA mask, 512-byte alignment, descriptor capacity,
  maximum transfer, and the DWCMSHC 128 MiB boundary.
- Command and data completion advance only after the boxed hard-IRQ endpoint
  has acknowledged and cached status. Register-only reset/clock transitions
  use bounded `RegisterPending` retries.
- `SdMmcIrqHost` controls the physical signal-enable registers.
  `CompletionIrqRearmHost` additionally restores completion delivery and
  publishes status captured from the masked window. The hard IRQ never copies
  DMA data or completes an RDIF request.
- Interrupt status, status-enable, and signal-enable are adjacent normal/error
  pairs and use one 32-bit MMIO transaction, matching Linux `sdhci.c`.

Traditional SDHCI has one in-flight hardware request, so
`queue_depth = max_submit_batch = 1`. CQHCI/CQE is outside this crate.

## Usage

```rust,no_run
use core::ptr::NonNull;

use dma_api::DeviceDma;
use sdhci_host::{Sdhci, rdif};
use sdmmc_protocol::sdio::{
    init::CardInitPreference,
    native::SdMmcCard,
};

// SAFETY: the mapped register file is valid and exclusively owned.
let mmio = NonNull::new(0xFE31_0000 as *mut u8).unwrap();
let mut host = unsafe { Sdhci::new(mmio) };
let dma: DeviceDma = todo!("construct from the platform DMA domain");
let config = rdif::dma_config("dwcmshc", 0, &dma);
host.configure_dma(dma)?;

let card = SdMmcCard::new(host);
let controller =
    rdif::initializing_device(card, config, CardInitPreference::MmcFirst);
// Transfer `controller` and its resolved IRQ source to the shared block
// runtime. The hctx owns initialization, submission, and completion.
# Ok::<(), sdmmc_protocol::Error>(())
```

Construction is `unsafe` because the caller must guarantee the MMIO lifetime
and exclusive ownership. The DMA capability is configured before the host is
transferred to the protocol/runtime owner.

## Platform checklist

1. Map the register window and prepare clocks, resets, power, and pinmux.
2. Install optional `HostClock`, `HostResetHook`, timer, and voltage hooks.
3. Build the correct `DeviceDma` domain/mask and call `configure_dma`.
4. Keep completion IRQ signaling masked until the runtime owns the boxed IRQ
   handler. `BlockQueue` enables it through `SdMmcIrqHost`.
5. Require `CompletionIrqRearmHost` only for an SDIO owner that closes the
   masked completion-delivery window before rearming `CARD_INT`.
6. Use `SdMmcCard::new` plus `rdif::initializing_device`; do not run a local
   initialization or completion polling loop.

## Validation

```bash
cargo test -p sdhci-host
cargo xtask clippy --package sdhci-host
```

Board validation must additionally cover DMA boundary splitting, maximum
transfer, concurrent submitters, fsync, checksum verification, IRQ quiescence,
and submit/completion accounting.

## License

Licensed under the Apache License, Version 2.0.
