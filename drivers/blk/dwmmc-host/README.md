# dwmmc-host

`no_std` Synopsys DesignWare Mobile Storage Host Controller (DW_mshc)
backend for [`sdmmc-protocol`](../sdmmc-protocol).

This crate plugs the IP block known as `DWC_mobile_storage` / `dw_mshc` /
`dw_mmc` (Linux) into the `SdioHost` trait so
`sdmmc_protocol::sdio::SdioSdmmc` can drive real hardware. The same core
appears in Rockchip RK33xx/RK35xx, Allwinner A-series, StarFive JH7110,
and a long tail of mid-range SoCs.

This crate implements `sdmmc_protocol::sdio::SdioHost` for the controller while
leaving MMIO mapping, SoC clocks, resets, pinmux, power rails, IRQ routing, and
DMA cache policy to platform glue.

## Status

- Compiles as a `no_std` controller backend.
- Intended for use through `sdmmc_protocol::sdio::SdioSdmmc`.
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
| SdioHost IDMAC data path | ✅ (FIFO fallback) |
| External-DMA data path | ❌ |
| Controller-specific DLL/strobe/tuning windows | ❌ |

The `SdioHost` implementation tries IDMAC for 512-byte CMD17/CMD18/CMD24/CMD25
block I/O when a `dma_api::DeviceDma` capability is installed with
`DwMmc::set_dma`; otherwise it uses the FIFO path. `DwMmc::block_buffer_config`
exposes the selected queue constraints for block-device adapters.

## Usage

```rust,no_run
use core::ptr::NonNull;
use sdmmc_protocol::sdio::{DelayNs, SdioSdmmc};
use dwmmc_host::DwMmc;

# fn make_delay() -> impl DelayNs { struct N; impl DelayNs for N { fn delay_ns(&mut self, _: u32) {} } N }
// SAFETY: 0xFE2B_0000 must point at a valid DW_mshc register file the
// caller has exclusive access to.
let mmio = NonNull::new(0xFE2B_0000 as *mut u8).unwrap();
let mut host = unsafe { DwMmc::new(mmio) };
host.set_reference_clock(50_000_000);
host.reset_and_init().expect("controller reset");

let mut card = SdioSdmmc::new(host, make_delay());
// card.init()?;
# Ok::<(), sdmmc_protocol::Error>(())
```

Construction is `unsafe` because the caller must guarantee that the supplied
address is a valid, exclusively-owned DW_mshc register file for the lifetime of
the driver.

### Bring-up checklist (for real-hardware validation)

1. Map the DW_mshc register file (e.g. RK3568 SDMMC0 at `0xFE2B_0000`).
2. Configure the platform clock so the controller has a viable
   reference clock before calling `DwMmc::new`. Most SoCs route a
   selectable mux through the CRU; pick a rate that divides cleanly
   to 400 kHz for ID mode.
3. Pass that rate to `DwMmc::set_reference_clock` so the divider
   programmed by `set_clock` lands on the right frequency.
4. `host.reset_and_init()?` — clears the controller / FIFO / DMA
   state and arms a 400 kHz ID-mode clock.
5. Build `SdioSdmmc::new(host, delay)` and call `init()`. The
   protocol layer will ramp the clock up via `set_clock`.
6. Add board-specific tuning before relying on SDR50, SDR104, DDR50, or HS200
   modes.

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
