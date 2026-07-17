# sdhci-host

`no_std` SD Host Controller Interface (SDHCI v3.x) backend for
[`sdmmc-protocol`](../sdmmc-protocol).

This crate plugs SDHCI register programming into the physical
`sdio_host2::SdioHost` trait so `SdioSdmmc::new_host2_timed` can drive a real
controller through `sdmmc-protocol`. Platform code is still responsible for MMIO mapping, clock/reset
tree setup, power rails, pinmux, IRQ routing, and DMA cache coherency.

## Status

- Compiles as a `no_std` controller backend.
- Intended for use through `sdmmc_protocol::sdio::SdioSdmmc::new_host2_timed`.
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
use sdmmc_protocol::sdio::{
    CardInitPreference, InitInput, InitPoll, OwnedSdioInit, SdioSdmmc,
};
use sdhci_host::Sdhci;

// SAFETY: 0xFE31_0000 must point at a valid SDHCI register file the
// caller has exclusive access to.
let Some(mmio) = NonNull::new(0xFE31_0000 as *mut u8) else {
    unreachable!()
};
let mut host = unsafe { Sdhci::new(mmio) };
// Optional platform capabilities such as HostClock, HostResetHook, DMA, and
// 1.8 V support are installed here before the protocol layer owns the host.
// OS glue moves this endpoint into its registered IRQ action before the
// initialization FSM is allowed to issue its first card command.
let _registered_irq = host.irq_endpoint();
host.enable_completion_irq();

let card = SdioSdmmc::new_host2_timed(host);
let mut init = OwnedSdioInit::new(card, CardInitPreference::SdFirst);
let InitPoll::Pending(schedule) = init.poll_init(InitInput::at(0)) else {
    unreachable!()
};
// Re-enter only for `schedule.run_again`, an acknowledged controller IRQ, or
// at `schedule.wake_at_ns`. An OS normally drives this through
// `sdmmc_protocol::rdif::StagedBlockDevice` and its shared worker pool.
# let _ = schedule;
# Ok::<(), sdmmc_protocol::Error>(())
```

Construction is `unsafe` because the caller must guarantee that the supplied
address is a valid, exclusively-owned SDHCI register file for the lifetime of
the driver.

## Block Request Usage

Normal block-device integration should use `sdhci_host::rdif::device`, which
routes RDIF requests through `sdmmc-protocol` and the native `sdio-host2`
transaction path. A platform selects either ADMA2 or owned interrupt-driven
PIO when it constructs the block configuration; this is an activation-time
capability choice, never a runtime fallback. Both modes require an IRQ source.
The lower-level `service_block_request` consumes one acknowledged IRQ snapshot
in serialized service context and never periodically reads controller state.
A runtime error keeps the request backing quarantined until the controller
reset FSM returns a typed quiescence proof.

The controller records interrupt-status ownership separately from signal
delivery. Temporarily masking the CPU IRQ does not return W1C or
`PRESENT_STATE` completion decisions to task context. Only the IRQ endpoint
may acknowledge normal runtime I/O; after the OS masks and synchronizes that
endpoint, recovery explicitly transfers status ownership to its initialization
state machine.

IRQ-owned submission checks the command/data inhibit bits once as an admission
condition. A busy engine rejects the request before ADMA registers are changed;
the accepted request is never parked for timer-driven re-submission. Subsequent
worker service consumes only the snapshot published by the IRQ endpoint, while
the watchdog can only fail the request and enter recovery.

Low-level raw-pointer FIFO/ADMA primitives are `unsafe`: the caller must retain
the allocation and prevent conflicting accesses even if the request moves to
another worker. Safe protocol calls retain the Rust buffer borrow, while
`sdhci_host::rdif::fifo_config` publishes an owned, IRQ-only PIO queue for
controllers such as BCM2835 that do not expose a usable ADMA2 path. Every
accepted RDIF request retains its `CpuDmaBuffer` until terminal completion or
proof-gated recovery; no timer or task-context probe may synthesize success.
An active ADMA descriptor table follows the same rule: an early request drop
quarantines it instead of returning memory that the controller may still
fetch.

Platform `HostResetHook` implementations are excluded from initial ResetAll and
runtime recovery by default. A hook may return
`ResetHookRecoveryMode::BoundedCallbacks` only when both callbacks are
guaranteed not to sleep or busy-wait. Otherwise the bus operation returns
`Unsupported` before invoking the hook or writing the software-reset register;
the platform must return `ResetHookRecoveryMode::Scheduled` and implement the
typed `begin_before_reset_all` / `poll_before_reset_all` state machine.
`ResetHookPoll::Pending` carries an absolute monotonic deadline, and
`cancel_before_reset_all` must undo an incomplete platform pulse. No hook may
obtain a hidden clock or delay in the callback.

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
4. Build `SdioSdmmc::new_host2_timed(host)` so every eventless controller
   transition consumes the runtime's absolute monotonic timestamp. Wrap it in
   `OwnedSdioInit`, and drive
   the returned absolute `InitSchedule` directly or use the RDIF
   `StagedBlockDevice`. The protocol
   layer starts with native `sdio-host2` bus operations for `ResetAll`,
   `PowerOn`, initial voltage, 1-bit bus width, and 400 kHz identification
   clock before issuing SD/MMC commands, then ramps the card to 25 MHz /
   50 MHz via later bus ops. Platform/runtime code chooses whether pending
   work runs again, waits for an IRQ, or waits for an absolute deadline.

The lower-level reset and power helpers remain for bounded diagnostics and
early bring-up. Clock programming, voltage settling, and tuning are exposed
only through timed host2 bus operations; normal card initialization must not
drive them through a synchronous compatibility path.

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
