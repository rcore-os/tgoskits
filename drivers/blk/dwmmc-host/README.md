# dwmmc-host

`no_std` Synopsys DesignWare Mobile Storage Host Controller (DW_mshc)
backend for [`sdmmc-protocol`](../sdmmc-protocol).

This crate plugs the IP block known as `DWC_mobile_storage` / `dw_mshc` /
`dw_mmc` (Linux) into the physical `sdio_host2::SdioHost` trait so
`sdmmc_protocol::sdio::SdioSdmmc::new_host2_timed` can drive real hardware. The same core
appears in Rockchip RK33xx/RK35xx, Allwinner A-series, StarFive JH7110,
and a long tail of mid-range SoCs.

This crate implements `sdio_host2::SdioHost` for the controller while
leaving MMIO mapping, SoC clocks, resets, pinmux, power rails, IRQ routing, and
DMA cache policy to platform glue.

## Status

- Compiles as a `no_std` controller backend.
- Intended for use through `sdmmc_protocol::sdio::SdioSdmmc::new_host2_timed`.
- Board-specific clock, power, pinmux, and tuning policy must be supplied by
  the caller.
- Real hardware bring-up still depends on the surrounding SoC integration.

## Scope

| Area                | Implemented |
|---------------------|-------------|
| PIO read / write (FIFO) | ✅      |
| 1-bit / 4-bit / 8-bit bus | ✅    |
| Default speed       | ✅          |
| High Speed (50 MHz) | ✅          |
| UHS-I / HS200 clock targets | ✅    |
| 32-bit / 136-bit responses | ✅   |
| R3 / R4 (no CRC) responses | ✅   |
| Software reset / clock setup | ✅ |
| Configurable FIFO offset | ✅     |
| 1.8 V signaling register path | ✅ (board validation required) |
| DDR50 register bit path | ✅ (board validation required) |
| IDMAC descriptor read / write | ✅ |
| SdioHost IDMAC data path | ✅ |
| RDIF 0.12 owned IRQ queue | ✅ (IDMAC only) |
| External-DMA data path | ❌ |
| Controller-specific DLL/strobe/tuning windows | ❌ |

The protocol initialization path may use the FIFO while the caller explicitly
drives its state machine. Normal RDIF block I/O is different: it requires an
installed `dma_api::DeviceDma`, an IRQ binding, and the owned IDMAC queue. A
FIFO-only configuration cannot publish a runtime queue and never falls back to
task-side completion polling. The shared host2 API may carry an owned CPU PIO
buffer for other controllers; DWMMC rejects that unsupported transaction
without consuming the buffer, and keeps normal runtime I/O on the owned IDMAC
path.

The IRQ endpoint is the sole owner of runtime interrupt-status reads and W1C
acknowledgements. It publishes a stable event snapshot; task context advances
the serialized request only from the corresponding acknowledged event. The
controller exposes one live source lease as `DwMmcIrqEndpoint` plus
`DwMmcIrqControl`: OS glue registers the endpoint from its CPU-pinned
maintenance thread and retains the control capability on that same thread.
After explicit masking and action synchronization, retiring both halves makes
the source available to the next initialization, recovery, or runtime epoch.
FIFO-ready capture masks the exact RXDR/TXDR bits before publication; only the
generation-checked control endpoint may rearm them after service. The portable
driver never returns a deferred acknowledgement or asks a shared worker to
retry destructive status capture. IDMAC
`RI`/`TI` and controller `DATA_OVER` are generation-tagged independently, and a
DMA request succeeds only after both have been observed. Either arrival order
is valid, while any IDMAC or controller error wins over a combined completion
snapshot. IDMAC errors retain their exact `IDSTS` cause instead of being
translated into an unrelated controller status bit. Controller errors retain
the active IDMAC request until the bounded reset FSM has observed
controller/FIFO/DMA reset completion. Only then may the RDIF lifecycle return a
quiescence proof and reclaim DMA ownership.

The data buffer and coherent descriptor table cross into in-flight ownership at
the same hardware commit point. Admission failures release both normally;
dropping an accepted request quarantines both, and only terminal IRQ evidence or
a reset-derived quiescence proof may return either allocation to the DMA domain.

Card initialization uses the protocol crate's explicit controller-IRQ and
absolute-deadline schedule. RDIF integrations should retain it in
`OwnedSdioInit`/`StagedBlockDevice`; board probe code must not run a synchronous
poll loop.

## Usage

```rust,no_run
use core::ptr::NonNull;
use sdmmc_protocol::sdio::{
    CardInitPreference, InitInput, InitPoll, OwnedSdioInit, SdioSdmmc,
};
use dwmmc_host::DwMmc;

// SAFETY: 0xFE2B_0000 must point at a valid DW_mshc register file the
// caller has exclusive access to.
let mmio = NonNull::new(0xFE2B_0000 as *mut u8).unwrap();
let mut host = unsafe { DwMmc::new(mmio) };
host.set_reference_clock(50_000_000);
// Optional DMA capability can be installed here before the protocol layer owns
// the host.
let source = host.take_irq_source().expect("unique DWMMC IRQ source");
let (_capture_endpoint, _owner_control) = source.into_parts();
// OS glue registers `_capture_endpoint` disabled on this maintenance thread's
// fixed CPU and retains `_owner_control` before polling initialization.

let card = SdioSdmmc::new_host2_timed(host);
let mut init = OwnedSdioInit::new(card, CardInitPreference::SdFirst);
let InitPoll::Pending(schedule) = init.poll_init(InitInput::at(0)) else {
    unreachable!()
};
// Re-enter only for the schedule's in-memory work, acknowledged IRQ, or
// absolute deadline. RDIF normally drives this through StagedBlockDevice.
# let _ = schedule;
# Ok::<(), sdmmc_protocol::Error>(())
```

Construction is `unsafe` because the caller must guarantee that the supplied
address is a valid, exclusively-owned DW_mshc register file for the lifetime of
the driver. Construction masks the controller's internal interrupt output, but
does not acknowledge stale status, reset the controller, or issue a command.
Platform code must bind the external IRQ service before driving controller/card
initialization and enabling runtime interrupts.

### Bring-up checklist (for real-hardware validation)

1. Map the DW_mshc register file (e.g. RK3568 SDMMC0 at `0xFE2B_0000`).
2. Configure the platform clock so the controller has a viable
   reference clock before calling `DwMmc::new`. Most SoCs route a
   selectable mux through the CRU; pick a rate that divides cleanly
   to 400 kHz for ID mode.
3. Pass that rate to `DwMmc::set_reference_clock` so the divider
   programmed by `set_clock` lands on the right frequency.
4. Install optional capabilities such as `DwMmc::set_dma`, take the unique IRQ
   source, and register its capture endpoint disabled from the CPU-pinned
   maintenance thread. Retain the source-control endpoint on that thread;
   initialization must not issue its first command before registration.
5. Build `SdioSdmmc::new_host2_timed(host)`, retain it in `OwnedSdioInit`, and drive
   its `InitSchedule` directly or through RDIF `StagedBlockDevice`. The protocol layer
   starts with native `sdio-host2` bus operations for `ResetAll`, `PowerOn`,
   initial voltage, 1-bit bus width, and 400 kHz identification clock before
   issuing SD/MMC commands, then ramps the clock up via later bus ops.
   Platform/runtime code chooses when to call the initialization state machine
   again. This is separate from the RDIF runtime queue, whose completion path
   is IRQ-only.
6. Add board-specific tuning before relying on SDR50, SDR104, DDR50, or HS200
   modes.

There is no synchronous reset-and-initialize runtime path. Card initialization,
request recovery, and ownership return must let the explicit state machines
drive reset, power, and clock setup through bounded bus-operation states.

### FIFO offset

The data FIFO sits at a fixed offset that varies by IP revision /
integration:

- `0x100`: very old DWC_mobile_storage builds.
- `0x200` (default): Rockchip RK33xx/RK35xx, StarFive JH7110.
- `0x400`: some Allwinner integrations.

Use `DwMmc::new_with_fifo_offset` if your SoC differs from the default.

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
cargo xtask clippy --package dwmmc-host
```

## License

Licensed under the Apache License, Version 2.0.
