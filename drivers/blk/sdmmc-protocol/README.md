# sdmmc-protocol

`sdmmc-protocol` provides `no_std` SD/MMC protocol building blocks for
embedded systems and kernel drivers.

The crate owns protocol-level command construction, response parsing, card
initialization flow, and block I/O sequencing. It does not own board setup,
MMIO mapping, IRQ routing, DMA allocation, or controller clock-tree setup; keep
those in the host-controller crate or OS/platform glue.

It provides:

- SD/MMC command definitions and SPI command packet encoding
- Response types and parsers for common SD, MMC, and SDIO responses
- EXT_CSD helpers for eMMC capacity, bus-width, and timing capability fields
- A SPI-mode SD card driver over a small transport trait
- A SDIO/native-mode host-controller abstraction and driver skeleton
- An optional RDIF block-device bridge for SDIO-backed host crates
- One shared `Error` type with command/phase context for protocol and host errors

The SPI path has protocol-level unit tests and basic block read/write support.
The SDIO path is the integration boundary used by the host crates in this
workspace and has been validated end-to-end on the controller / SoC
combinations listed under [Validated host backends](#validated-host-backends).

## Features

```toml
[features]
default = ["spi"]
spi = []
sdio = []
rdif = ["sdio", "dep:rdif-block"]
```

- `spi`: enables the SPI transport and `SpiSdmmc` driver.
- `sdio`: enables the SDIO host abstraction and `SdMmcCard` driver.
- `rdif`: enables the RDIF block-device adapter for SDIO-backed host crates.

Diagnostics use the `log` crate. Configure a logger in the caller if runtime
messages are needed.

## SPI Mode

The SPI path is built around `SpiTransport` plus an `embedded_hal::delay::DelayNs`
implementation that the driver uses for wall-clock timeouts:

```rust
use embedded_hal::delay::DelayNs;
use sdmmc_protocol::Error;
use sdmmc_protocol::spi::{SpiSdmmc, SpiTransport};

struct MySpi;

impl SpiTransport for MySpi {
    fn transfer_byte(&mut self, byte: u8) -> Result<u8, Error> {
        // Send one byte on your platform SPI peripheral and return the byte read.
        // Chip-select handling depends on your board/HAL design.
        let _ = byte;
        todo!()
    }
}

fn example<D: DelayNs>(spi: MySpi, delay: D) -> Result<(), Error> {
    let mut card = SpiSdmmc::new(spi, delay);
    let info = card.init()?;

    let mut block = [0u8; 512];
    card.read_block(0, &mut block)?;

    let _is_sdhc_or_sdxc = info.high_capacity;
    let _capacity_blocks = info.capacity_blocks; // Some(blocks) for known CSD versions
    Ok(())
}
```

If your platform already exposes an `embedded-hal` 1.0 `SpiDevice<u8>`, wrap it with `SpiDeviceWrapper`:

```rust
use embedded_hal::delay::DelayNs;
use sdmmc_protocol::spi::{SpiDeviceWrapper, SpiSdmmc};

fn create_driver<SPI, D>(spi: SPI, delay: D) -> SpiSdmmc<SpiDeviceWrapper<SPI>, D>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
    D: DelayNs,
{
    SpiSdmmc::new(SpiDeviceWrapper::new(spi), delay)
}
```

### SPI Operations

`SpiSdmmc` currently exposes:

- `init()`
- `read_block(addr, &mut [u8; 512])`
- `write_block(addr, &[u8; 512])`
- `read_blocks(addr, count, handler)`
- `write_blocks(addr, blocks)`
- `switch_function(cmd)`
- `switch_to_high_speed()`

For SDHC/SDXC cards, block addresses are passed through directly. For SDSC cards, block addresses are converted to byte addresses internally.
CRC16 verification for read data is enabled by default and can be changed with
`set_verify_data_crc`.

## SDIO Mode

Physical controllers implement `sdmmc_host::SdMmcHost` plus
`sdio::host::SdMmcIrqHost`. The latter transfers move-only bus, controller-IRQ,
and optional card-IRQ capabilities through `sdmmc_host::HostParts`; it does not
accept a task, waker, channel, or scheduling callback. Construct native-card
protocol state with `SdMmcCard::new` and IO-card state with `SdioCard::new`.
An SDIO device owner that must restore completion delivery and synchronously
capture status from the masked window additionally requires
`CompletionIrqRearmHost`. Ordinary block hosts do not implement that narrower
capability.

Initialization is a request state machine. Its `wait_kind()` result is a
mandatory execution contract:

- `SdMmcInitWait::Irq`: advance only after the hard-IRQ endpoint acknowledged
  and latched a matching controller event.
- `SdMmcInitWait::Register`: the maintenance task may inspect register state
  under one caller-owned deadline.

Methods named `poll_*` advance one already-authorized state-machine step. They
are not completion-polling APIs and must not be called from a tight loop for a
command or data phase. The hard IRQ only acknowledges hardware and publishes
an event; protocol progress, DMA finalization, and request completion all stay
in the maintenance task.

`SdMmcCard` detects SD versus eMMC during initialization. SD cards are widened
through ACMD6; eMMC cards use EXT_CSD plus CMD6 SWITCH to negotiate bus width
and timing where the host supports those modes. The eMMC path also parses
`EXT_CSD_REV`, `CACHE_SIZE`, and `CACHE_CTRL`. A nonzero advertised cache is
enabled with an IRQ-completed CMD6 before initialization finishes.

The RDIF flush path sends `FLUSH_CACHE` only when that cache is enabled. A card
without an enabled cache instead receives an IRQ-completed CMD13
transfer-state barrier. Neither path polls for command completion.

Protocol operations expose request objects because the portable crate does not
own a scheduler. The block runtime owns the waiting policy and invokes these
step methods only from its hctx maintenance task after the required event.

### Optional wall-clock timeouts

Hosts should expose a monotonic clock through
`SdMmcHost::now_ms() -> Option<u64>`. The protocol layer then enforces
wall-clock deadlines for ACMD41/CMD1 power-up and MMC `CMD6 SWITCH` in addition
to its bounded step budgets. The owning runtime must also apply one outer
deadline to the complete initialization or recovery transaction.

### SDIO module boundaries

The `sdio` feature is split by capability:

- `sdio::host`: host-controller capabilities, IRQ events, and bus operations.
- `sdio::native`: `SdMmcCard`, card information, and ordinary SD/eMMC
  command/block I/O request wrappers.
- `sdio::io`: `SdioCard`, typed Function/CCCR/FBR/CIS state, and CMD52/CMD53
  request ownership. The physical-host adapter remains private to the protocol
  crate.
- `sdio::init`: initialization scratch storage, probe preference, and the
  event-gated initialization state machine.

## RDIF Block Bridge

The `rdif` feature adapts an initialized `SdMmcCard` card to `rdif-block`
without pulling OS runtime policy into the protocol crate. Its public modules
match the ownership boundary:

- `rdif::config`: block size constants, `BlockConfig`, queue limits, device
  info, card-address translation, and error/transfer-mode helpers.
- `rdif::host`: the private `ProtocolBlockSlot` and owned physical-host request
  lifecycle used by the depth-one hardware queue.
- `rdif::device`: `BlockDevice` and `rdif_block::Interface` integration.
- `rdif::queue`: the depth-one `HardwareQueue` implementation, including
  owned-DMA batch submission, IRQ-gated completion, and shutdown recovery.
- `rdif::irq`: the top-half IRQ bridge, which consumes a host IRQ endpoint and
  never enters the shared card core.

## Command Helpers

The `cmd` module contains helpers for common commands:

- `CMD0`, `CMD2`, `CMD3_SD`, `CMD12`, `CMD38`, `CMD58`
- `cmd8(voltage, check_pattern)`
- `cmd17(addr)`, `cmd18(addr)`
- `cmd24(addr)`, `cmd25(addr)`
- `cmd55(rca)`, `cmd41(hcs, voltage_window)`
- SDIO helpers such as `cmd52(...)` and `cmd53(...)`

Commands can be encoded for SPI with:

```rust
let bytes = sdmmc_protocol::cmd::CMD0.to_spi_bytes();
assert_eq!(bytes, [0x40, 0x00, 0x00, 0x00, 0x00, 0x95]);
```

## Testing

Run the default SPI-enabled test suite:

```bash
cargo test
```

Run SDIO-only compilation and tests:

```bash
cargo test --no-default-features --features sdio
```

Run the SDIO plus RDIF block-bridge tests:

```bash
cargo test --no-default-features --features sdio,rdif
```

Run all feature combinations used during development:

```bash
cargo fmt --check
cargo test
cargo test --no-default-features --features sdio
cargo test --no-default-features --features sdio,rdif
cargo test --all-features
```

In this workspace, prefer the project `xtask` flow for final validation when
the crate is part of a larger change:

```bash
cargo fmt
cargo xtask clippy --package sdmmc-protocol
```

## Current Limitations

- No real hardware examples are included yet.
- SPI mode targets SD cards; MMC-over-SPI is not a current target.
- SDIO/native mode has card init and block I/O plumbing, but advanced eMMC
  mode switching is still incomplete.
- UHS-I and HS200 entry depends on host support for voltage switching and
  tuning. Unsupported host operations return `Error::UnsupportedCommand`.

## Validated host backends

The protocol layer has been exercised on the controller / SoC combinations
below through dedicated host crates in this workspace. Modes not listed are
either unimplemented in the host backend or have not yet been signed off on
real hardware.

| Host crate         | SoC / controller       | Mode                  | Notes                                       |
|--------------------|------------------------|-----------------------|---------------------------------------------|
| `sdhci-host`       | RK3568 (dwcmshc)       | eMMC HS@52, FIFO/DMA  | HS200 path exists; not yet signed off       |
| `sdhci-host`       | RK3588 (dwcmshc)       | eMMC HS@52, FIFO/DMA  | HS200 path exists; not yet signed off       |
| `dwmmc-host`       | RK3568 SD (dw_mshc)    | SD HS, DMA            |                                             |
| `phytium-mci-host` | Phytium MCI            | SD HS, DMA            |                                             |

See `drivers/blk/sdmmc-protocol/docs/REVIEW.md` for the remaining 1.0 roadmap
(non_exhaustive enums, `Display` impls, time-base contract, fuzz coverage,
SDIO IO-card support, etc.).

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
